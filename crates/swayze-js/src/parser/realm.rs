use crate::{
    eval::MAX_INSTRUCTIONS,
    realm::{
        BodyKind, CodeMode, CompileError, CompileLimit, CompiledScript, FrameLayout, FunctionBody,
        MAX_CODE_NAME_BYTES, MAX_DECLARATIONS, MAX_FRAME_LOCALS, MAX_NAME_BYTES, MAX_PARAMETERS,
        MAX_SITES, NameRange, Role, Site,
    },
};

use super::{
    AccessKind, ActiveStatement, AfterExpression, BindingPolicy, DirectReference, Error,
    ExpressionEnd, ExpressionInput, ExpressionResume, Follow, Instruction, Limit, Limits, Operator,
    ParseAction, ParseErrorType, ParseOwner, Parser, Pending, QuotedDecimalEscape, RealmParseLimit,
    Span, Token, TokenKind, TokenPosition, UNPATCHED, WriteAccess, new_parser, try_push,
};

// Shared source, cursor, fuel, literal pool, access numbering and scope
// allocation remain live. Only the active code-owner state is exchanged.
pub(super) struct OpenFunction {
    root_ops: Vec<Instruction>,
    root_peak: usize,
    root_stack: usize,
    root_owner: ParseOwner,
    root_witness: QuotedDecimalEscape,
    root_pending: Vec<(Pending, Span)>,
    root_statement_frames: Vec<ActiveStatement>,
    root_labels: Vec<Span>,
    root_innermost_loop: Option<usize>,
    root_innermost_target: Option<usize>,
    root_header_scope: Option<usize>,
    root_lookahead: Option<Option<Token>>,
    pub(super) root_depth: usize,
    expression: Option<ExpressionContinuation>,
    body: u16,
    params: Vec<Span>,
    vars: Vec<Span>,
    accesses: usize,
}

impl OpenFunction {
    pub(super) fn parked_statement_count(&self) -> usize {
        self.root_statement_frames.len()
    }
}

pub(super) struct ExpressionContinuation {
    pub(super) end: ExpressionEnd,
    pub(super) after: AfterExpression,
    pub(super) keyword: Span,
    pub(super) base_depth: usize,
    pub(super) template_depth: usize,
}

pub(super) struct ParsedFunction {
    body: FunctionBody,
    locals: Vec<Span>,
    vars: Vec<Span>,
    witness: QuotedDecimalEscape,
    initial_name: Option<Span>,
}

impl Parser<'_> {
    pub(super) fn open_function(
        &mut self,
        root_owner: ParseOwner,
        keyword: Span,
        expression: Option<ExpressionContinuation>,
    ) -> Result<(), Error> {
        let name = if expression.is_none() {
            let name =
                self.require_token(TokenPosition::Plain, "expected an ordinary function name")?;
            if !matches!(name.kind, TokenKind::Identifier) {
                return Err(Error::unsupported(
                    name.span,
                    "expected a simple ordinary function name",
                ));
            }
            Some(name.span)
        } else {
            None
        };
        let open = self.require_token(
            TokenPosition::Plain,
            "expected '(' before formal parameters",
        )?;
        if expression.is_some() && matches!(open.kind, TokenKind::Operator(Operator::Multiply)) {
            return Err(Error::unsupported(
                keyword,
                "generator function expressions are outside the subset",
            ));
        }
        if !matches!(open.kind, TokenKind::Open) {
            return Err(
                if expression.is_some() && matches!(open.kind, TokenKind::Identifier) {
                    Error::unsupported(
                        open.span,
                        "named function expressions are outside the subset",
                    )
                } else {
                    Error::malformed(open.span, "expected '(' before formal parameters")
                },
            );
        }
        // The boundary and any nested body delimiters spend from the
        // Script's nesting limit, without sharing its pendings or targets.
        if expression.is_some() {
            self.enter_nesting(keyword)?;
        }
        let mut params = Vec::new();
        let mut formal =
            self.require_token(TokenPosition::Plain, "expected a formal parameter or ')'")?;
        if !matches!(formal.kind, TokenKind::Close) {
            loop {
                if !matches!(formal.kind, TokenKind::Identifier) {
                    return Err(Error::unsupported(
                        formal.span,
                        "only distinct simple formal parameters are supported",
                    ));
                }
                if params.len() == usize::from(MAX_PARAMETERS) {
                    return Err(Error::realm_limit(formal.span, RealmParseLimit::Parameters));
                }
                if formal.span.end - formal.span.start > MAX_NAME_BYTES {
                    return Err(Error::realm_limit(
                        formal.span,
                        RealmParseLimit::FormalNameBytes,
                    ));
                }
                for &other in &params {
                    self.lexer.charge(1, formal.span)?;
                    if self.names_equal(formal.span, other)? {
                        return Err(Error::unsupported(
                            formal.span,
                            "duplicate formal parameters are outside the subset",
                        ));
                    }
                }
                try_push(&mut params, formal.span, formal.span)?;
                let tail = self.require_token(
                    TokenPosition::Plain,
                    "expected ',' or ')' after formal parameter",
                )?;
                match tail.kind {
                    TokenKind::Close => break,
                    TokenKind::Comma => {
                        formal =
                            self.require_token(TokenPosition::Plain, "expected a formal parameter")?
                    }
                    _ => {
                        return Err(Error::unsupported(
                            tail.span,
                            "only simple formal parameters are supported",
                        ));
                    }
                }
            }
        }
        let brace = self.require_token(
            TokenPosition::Plain,
            "expected '{' to open the function body",
        )?;
        if !matches!(brace.kind, TokenKind::OpenBrace) {
            return Err(Error::malformed(
                brace.span,
                "expected '{' to open the function body",
            ));
        }
        self.lexer.charge(1, keyword)?;
        // There cannot be more bodies than chargeable header tokens, so u16
        // is ample under the aggregate token and declaration ceilings.
        let body = u16::try_from(self.functions.len())
            .map_err(|_| Error::limit(keyword, Limit::Allocation))?;
        if let Some(name) = name {
            try_push(
                &mut self.realm_declarations,
                (name, Role::Function { body }),
                name,
            )?;
        }
        // Nothing can peek into the body before '{'. The lexer cursor and
        // aggregate ledgers are shared (no relex); lookahead is owner-local.
        debug_assert!(self.lookahead.is_none());
        self.function = Some(OpenFunction {
            root_ops: std::mem::take(&mut self.instructions),
            root_peak: std::mem::take(&mut self.max_stack_depth),
            root_stack: std::mem::take(&mut self.stack_depth),
            root_owner,
            root_witness: std::mem::replace(
                &mut self.lexer.quoted_decimal_escape,
                QuotedDecimalEscape::Pending,
            ),
            root_pending: std::mem::take(&mut self.pending),
            root_statement_frames: std::mem::take(&mut self.statement_frames),
            root_labels: std::mem::take(&mut self.labels),
            root_innermost_loop: self.innermost_loop.take(),
            root_innermost_target: self.innermost_target.take(),
            root_header_scope: self.for_header_scope.take(),
            root_lookahead: self.lookahead.take(),
            root_depth: std::mem::take(&mut self.nesting_depth),
            expression,
            body,
            params,
            vars: Vec::new(),
            accesses: self.accesses.len(),
        });
        Ok(())
    }

    pub(super) fn close_function(
        &mut self,
        owner: &mut ParseOwner,
        span: Span,
    ) -> Result<ParseAction, Error> {
        let function = self
            .function
            .take()
            .ok_or_else(|| Error::malformed(span, "invalid function boundary"))?;
        // A body if-tail can peek at its closing brace, never beyond it.
        // Thus the lexer witness before/after either fence is unambiguous.
        debug_assert!(
            self.lookahead.is_none()
                && self.statement_frames.is_empty()
                && self.pending.is_empty()
                && self.stack_depth == 0
                && self.labels.is_empty()
                && self.for_header_scope.is_none()
                && self.innermost_target.is_none()
                && self.innermost_loop.is_none()
                && self.nesting_depth == 0
        );
        let body = FunctionBody {
            instructions: std::mem::replace(&mut self.instructions, function.root_ops),
            max_stack_depth: std::mem::replace(&mut self.max_stack_depth, function.root_peak),
            mode: if function.root_owner.mode == CodeMode::Strict {
                CodeMode::Strict
            } else {
                owner.mode
            },
            layout: FrameLayout::new(function.params.len(), function.params.len())
                .ok_or_else(|| Error::malformed(span, "invalid formal layout"))?,
            accesses: function.accesses..self.accesses.len(),
            kind: if function.expression.is_some() {
                BodyKind::Expression(None)
            } else {
                BodyKind::Declaration
            },
        };
        let witness =
            std::mem::replace(&mut self.lexer.quoted_decimal_escape, function.root_witness);
        try_push(
            &mut self.functions,
            ParsedFunction {
                body,
                locals: function.params,
                vars: function.vars,
                witness,
                initial_name: None,
            },
            span,
        )?;
        *owner = function.root_owner;
        self.stack_depth = function.root_stack;
        self.nesting_depth = function.root_depth;
        self.pending = function.root_pending;
        self.statement_frames = function.root_statement_frames;
        self.labels = function.root_labels;
        self.innermost_loop = function.root_innermost_loop;
        self.innermost_target = function.root_innermost_target;
        self.for_header_scope = function.root_header_scope;
        self.lookahead = function.root_lookahead;
        if let Some(saved) = function.expression {
            self.nesting_depth -= 1;
            let creator = self.instructions.len();
            self.emit(Instruction::CreateFunction(function.body), saved.keyword)?;
            Ok(ParseAction::Expression {
                input: ExpressionInput::Resume(ExpressionResume {
                    base_depth: saved.base_depth,
                    template_depth: saved.template_depth,
                    creator,
                }),
                end: saved.end,
                after: saved.after,
            })
        } else {
            Ok(ParseAction::Item)
        }
    }

    pub(super) fn infer_function_name(
        &mut self,
        reference: Option<DirectReference>,
        name: Span,
    ) -> Result<(), Error> {
        let Some(DirectReference::AnonymousDefinition(creator)) = reference else {
            return Ok(());
        };
        self.lexer.charge(1, name)?;
        if let Some(Instruction::CreateFunction(body)) = self.instructions.get(creator)
            && let Some(function) = self.functions.get_mut(usize::from(*body))
            && matches!(function.body.kind, BodyKind::Expression(None))
            && function.initial_name.is_none()
        {
            function.initial_name = Some(name);
            return Ok(());
        }
        Err(Error::malformed(name, "invalid anonymous function creator"))
    }

    pub(super) fn record_local_var(&mut self, name: Span) -> Result<(), Error> {
        let vars = &mut self.function.as_mut().expect("open function").vars;
        // Every occurrence consumes at least a name and a separator token.
        // Thus the aggregate token cap bounds even duplicate/no-op backing.
        push_span(&mut self.lexer, vars, name)
    }

    fn find_local(&mut self, locals: &[Span], name: Span) -> Result<Option<u8>, Error> {
        for (slot, &candidate) in locals.iter().enumerate() {
            self.lexer.charge(1, name)?;
            if self.names_equal(name, candidate)? {
                return Ok(Some(slot as u8));
            }
        }
        Ok(None)
    }

    pub(super) fn parse_return(&mut self, keyword: Span) -> Result<ParseAction, Error> {
        if self.function.is_none() {
            let mut error = Error::malformed(keyword, "return is not allowed in Script code");
            error.parse_error_type = Some(ParseErrorType::SyntaxError);
            return Err(error);
        }
        let first = self.require_token(
            TokenPosition::Operand,
            "return requires an explicit semicolon",
        )?;
        if matches!(first.kind, TokenKind::Semicolon) {
            self.emit(Instruction::Return { value: false }, keyword)?;
            return Ok(ParseAction::Follow(
                Follow::Item,
                Span::at(self.lexer.offset),
            ));
        }
        if first.after_line_break {
            return Err(Error::unsupported(
                first.span,
                "line-separated return requires an explicit semicolon before the next statement",
            ));
        }
        Ok(ParseAction::expression(
            first.into(),
            ExpressionEnd::Return,
            AfterExpression::Return(keyword),
        ))
    }

    pub(super) fn finish_call(&mut self, close: Span, last: bool) -> Result<(), Error> {
        let Some((Pending::Call { base, completed }, open)) = self.pending.pop() else {
            return Err(Error::malformed(close, "unmatched call delimiter"));
        };
        let argc = completed + u8::from(last);
        if self.stack_depth != base + usize::from(argc) {
            return Err(Error::malformed(close, "invalid call operand count"));
        }
        self.nesting_depth -= 1;
        let start =
            u32::try_from(open.start).map_err(|_| Error::limit(open, Limit::SourceBytes))?;
        let end = u32::try_from(close.end).map_err(|_| Error::limit(close, Limit::SourceBytes))?;
        self.emit(
            Instruction::Call {
                start,
                end,
                argc,
                member: super::BARE_CALL,
            },
            open,
        )
    }
}

// Bound requested backing (not allocator rounding or overhead) geometrically.
// Charge the record write and any move of initialized spans before reserving.
// No name bytes are copied here.
fn push_span(lexer: &mut super::Lexer<'_>, spans: &mut Vec<Span>, name: Span) -> Result<(), Error> {
    lexer.charge(1, name)?;
    let len = spans.len();
    if len == spans.capacity() {
        lexer.charge(len, name)?;
        let next = (len + 1).next_power_of_two();
        spans
            .try_reserve_exact(next - len)
            .map_err(|_| Error::limit(name, Limit::Allocation))?;
    }
    spans.push(name);
    Ok(())
}

impl ParsedFunction {
    fn plan_locals(&mut self, parser: &mut Parser<'_>) -> Result<(), CompileError> {
        if self.vars.is_empty() {
            return Ok(());
        }
        let formals = self.locals.len();
        for name in std::mem::take(&mut self.vars) {
            parser.lexer.charge(1, name)?;
            if name.end - name.start > MAX_NAME_BYTES {
                return Err(cap(CompileLimit::NameBytes, name));
            }
            if parser.find_local(&self.locals, name)?.is_none() {
                if self.locals.len() == usize::from(MAX_FRAME_LOCALS) {
                    return Err(cap(CompileLimit::LocalSlots, name));
                }
                push_span(&mut parser.lexer, &mut self.locals, name)?;
            }
        }
        self.body.layout =
            FrameLayout::new(formals, self.locals.len()).ok_or(CompileError::InvalidScript)?;
        Ok(())
    }
}

fn patch_local(
    ops: &mut [Instruction],
    access: super::Access,
    slot: u8,
) -> Result<(), CompileError> {
    let op = ops
        .get_mut(access.instruction)
        .ok_or(CompileError::InvalidScript)?;
    *op = match (access.kind, *op) {
        (AccessKind::Read | AccessKind::TypeofRead, Instruction::LoadBinding(UNPATCHED)) => {
            Instruction::LoadLocal(slot)
        }
        (
            AccessKind::Write(WriteAccess { load: None, .. }),
            Instruction::UpdateBinding {
                slot: UNPATCHED,
                op,
                result,
            },
        ) => Instruction::UpdateLocal { slot, op, result },
        (AccessKind::Write(_), Instruction::StoreBinding(UNPATCHED)) => {
            Instruction::StoreLocal { slot, keep: false }
        }
        (AccessKind::Write(_), Instruction::StoreBindingKeep(UNPATCHED)) => {
            Instruction::StoreLocal { slot, keep: true }
        }
        _ => return Err(CompileError::InvalidScript),
    };
    if let AccessKind::Write(write) = access.kind
        && let Some(load) = write.load
    {
        let Some(op @ Instruction::LoadBinding(UNPATCHED)) = ops.get_mut(load) else {
            return Err(CompileError::InvalidScript);
        };
        *op = Instruction::LoadLocal(slot);
    }
    Ok(())
}

pub(crate) fn compile_realm(source: &str) -> Result<CompiledScript, CompileError> {
    let mut parser = new_parser(source, Limits::default(), BindingPolicy::Realm)?;
    let (exit, mode) = parser.parse(true).map_err(|error| {
        let limit = match error.realm_limit {
            Some(RealmParseLimit::Arguments) => CompileLimit::Arguments,
            Some(RealmParseLimit::Parameters) => CompileLimit::Parameters,
            Some(RealmParseLimit::FormalNameBytes) => CompileLimit::NameBytes,
            None => return CompileError::Parse(error),
        };
        cap(limit, error.span())
    })?;
    // Validate the entire body, including earlier directives and dead code,
    // after structural diagnostics but before Realm-only caps/name lowering.
    let strict_witness = |mode, witness: &QuotedDecimalEscape| match (mode, witness) {
        (CodeMode::Strict, QuotedDecimalEscape::Found(span)) => Some(*span),
        _ => None,
    };
    if let Some(span) = strict_witness(mode, &parser.lexer.quoted_decimal_escape)
        .into_iter()
        .chain(
            parser
                .functions
                .iter()
                .filter_map(|f| strict_witness(f.body.mode, &f.witness)),
        )
        .min_by_key(|span| span.start)
    {
        return Err(CompileError::SyntaxError { span });
    }
    let end = Span::at(parser.lexer.offset);
    let declarations = parser.realm_declarations.len();
    let count = declarations + parser.accesses.len();
    if parser.instructions.len()
        + parser
            .functions
            .iter()
            .map(|f| f.body.instructions.len())
            .sum::<usize>()
        > MAX_INSTRUCTIONS
    {
        return Err(cap(CompileLimit::Instructions, end));
    }
    if declarations > MAX_DECLARATIONS {
        return Err(cap(
            CompileLimit::Declarations,
            parser.realm_declarations[MAX_DECLARATIONS].0,
        ));
    }
    if count > MAX_SITES {
        return Err(cap(CompileLimit::Sites, end));
    }

    let mut sites = Vec::new();
    sites
        .try_reserve_exact(count)
        .map_err(|_| Error::limit(end, Limit::Allocation))?;
    let mut names = Vec::new();
    for index in 0..declarations {
        let (name, role) = parser.realm_declarations[index];
        sites.push(copy_name(&mut parser, &mut names, name, role)?);
        // Only a pair involving a lexical can conflict within this Script.
        // Charge every visited occurrence, even when comparing no name bytes.
        for other in 0..index {
            parser.lexer.charge(1, name)?;
            let (candidate, other_role) = parser.realm_declarations[other];
            if (matches!(role, Role::Declaration(_)) || matches!(other_role, Role::Declaration(_)))
                && parser.names_equal(name, candidate)?
            {
                return Err(CompileError::SyntaxError { span: name });
            }
        }
    }
    let mut functions = Vec::new();
    functions
        .try_reserve_exact(parser.functions.len())
        .map_err(|_| Error::limit(end, Limit::Allocation))?;
    let mut parsed = std::mem::take(&mut parser.functions);
    for function in &mut parsed {
        function.plan_locals(&mut parser)?;
        if let Some(span) = function.initial_name {
            function.body.kind =
                BodyKind::Expression(Some(copy_name_range(&mut parser, &mut names, span)?));
        }
    }
    let mut parsed = parsed.into_iter().peekable();
    for index in 0..parser.accesses.len() {
        while parsed.peek().is_some_and(|f| f.body.accesses.end <= index) {
            let mut body = parsed.next().unwrap().body;
            body.accesses =
                (declarations + body.accesses.start)..(declarations + body.accesses.end);
            functions.push(body);
        }
        let access = parser.accesses[index];
        let body = parsed
            .peek_mut()
            .filter(|f| f.body.accesses.contains(&index));
        if let Some(function) = body
            && let Some(slot) = parser.find_local(&function.locals, access.name)?
        {
            // Keep access numbering dense and bounded. Local sites have
            // source provenance but are never linked or looked up globally.
            parser.lexer.charge(1, access.name)?;
            if matches!(access.kind, AccessKind::Write(write) if write.load.is_some()) {
                parser.lexer.charge(1, access.name)?;
            }
            patch_local(&mut function.body.instructions, access, slot)?;
            sites.push(Site {
                span: access.name,
                start: 0,
                end: 0,
                role: Role::Local,
            });
            continue;
        }
        let role = match access.kind {
            AccessKind::Read => Role::Read,
            AccessKind::TypeofRead => Role::Typeof,
            AccessKind::Write(_) => Role::Write,
        };
        let slot = sites.len();
        sites.push(copy_name(&mut parser, &mut names, access.name, role)?);
        parser.lexer.charge(1, access.name)?;
        if let AccessKind::Write(write) = access.kind
            && write.load.is_some()
        {
            parser.lexer.charge(1, access.name)?;
        }
        let instructions = parsed
            .peek_mut()
            .filter(|f| f.body.accesses.contains(&index))
            .map_or(&mut parser.instructions, |f| &mut f.body.instructions);
        Parser::patch_access_in(instructions, access, Some(slot))
            .map_err(|_| CompileError::InvalidScript)?;
    }
    for function in parsed {
        let mut body = function.body;
        body.accesses = (declarations + body.accesses.start)..(declarations + body.accesses.end);
        functions.push(body);
    }
    Ok(CompiledScript {
        instructions: parser.instructions,
        string_units: parser.lexer.string_units,
        max_stack_depth: parser.max_stack_depth,
        exit,
        mode,
        names,
        sites,
        declarations,
        functions,
    })
}

fn copy_name(
    parser: &mut Parser<'_>,
    names: &mut Vec<u8>,
    span: Span,
    role: Role,
) -> Result<Site, CompileError> {
    let NameRange { start, end } = copy_name_range(parser, names, span)?;
    Ok(Site {
        span,
        start,
        end,
        role,
    })
}

fn copy_name_range(
    parser: &mut Parser<'_>,
    names: &mut Vec<u8>,
    span: Span,
) -> Result<NameRange, CompileError> {
    let source = parser.lexer.source;
    let name = source
        .as_bytes()
        .get(span.start..span.end)
        .ok_or(CompileError::InvalidScript)?;
    if name.len() > MAX_NAME_BYTES {
        return Err(cap(CompileLimit::NameBytes, span));
    }
    let start = names.len();
    let end = start + name.len();
    if end > MAX_CODE_NAME_BYTES {
        return Err(cap(CompileLimit::PooledNameBytes, span));
    }
    parser.lexer.charge(name.len() + 1, span)?;
    if end > names.capacity() {
        parser.lexer.charge(start, span)?; // Existing initialized name bytes may move.
        names
            .try_reserve_exact(end.next_power_of_two() - start)
            .map_err(|_| Error::limit(span, Limit::Allocation))?;
    }
    names.extend_from_slice(name);
    Ok(NameRange {
        start: u16::try_from(start).map_err(|_| CompileError::InvalidScript)?,
        end: u16::try_from(end).map_err(|_| CompileError::InvalidScript)?,
    })
}

fn cap(limit: CompileLimit, span: Span) -> CompileError {
    CompileError::LimitExceeded { limit, span }
}
