use crate::{
    eval::MAX_INSTRUCTIONS,
    realm::{
        BodyKind, CodeMode, CompileError, CompileLimit, CompiledScript, DOT_ROOT, DotCode,
        DotOccurrence, FrameLayout, FunctionBody, MAX_CODE_NAME_BYTES, MAX_DECLARATIONS,
        MAX_DOT_CODE_UNITS, MAX_DOT_KEY_UNITS, MAX_DOT_SITES, MAX_FRAME_LOCALS, MAX_NAME_BYTES,
        MAX_PARAMETERS, MAX_SITES, NameRange, Role, Site,
    },
};

use super::{
    AccessKind, ActiveStatement, AfterExpression, BindingPolicy, DirectReference, DotName, Error,
    ExpressionEnd, ExpressionInput, ExpressionResume, Follow, Instruction, Limit, Limits, Operator,
    ParseAction, ParseErrorType, ParseOwner, Parser, Pending, QuotedDecimalEscape, RealmParseLimit,
    Span, Token, TokenKind, TokenPosition, UNPATCHED, WriteAccess, new_parser, try_push,
};

// Shared source, cursor, fuel, literal and dot pools, access numbering and
// scope allocation remain live. Only the active code-owner state is exchanged.
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

    pub(super) fn record_dot(&mut self, key: DotName, dot: Span) -> Result<u16, Error> {
        debug_assert!(self.lexer.policy == BindingPolicy::Realm);
        let key_span = Span {
            start: key.start as usize,
            end: key.end as usize,
        };
        let bytes = self
            .lexer
            .source
            .as_bytes()
            .get(key_span.start..key_span.end)
            .filter(|bytes| !bytes.is_empty())
            .ok_or_else(|| Error::malformed(dot, "invalid dot key range"))?;
        if bytes.len() > MAX_DOT_KEY_UNITS {
            return Err(Error::realm_limit(key_span, RealmParseLimit::DotKeyUnits));
        }
        let sites = self.dot.sites.len();
        if sites == MAX_DOT_SITES {
            return Err(Error::realm_limit(dot, RealmParseLimit::DotSites));
        }
        let start = self.dot.keys.len();
        let end = start + bytes.len();
        if end > MAX_DOT_CODE_UNITS {
            return Err(Error::realm_limit(dot, RealmParseLimit::PooledDotKeyUnits));
        }
        let pack_key =
            |index| u16::try_from(index).map_err(|_| Error::limit(dot, Limit::Allocation));
        let pack_dot =
            |index| u32::try_from(index).map_err(|_| Error::limit(dot, Limit::SourceBytes));
        let occurrence = DotOccurrence {
            owner: self
                .function
                .as_ref()
                .map_or(DOT_ROOT, |function| function.body),
            key_start: pack_key(start)?,
            key_end: pack_key(end)?,
            dot_start: pack_dot(dot.start)?,
            dot_end: pack_dot(dot.end)?,
        };
        let site = pack_key(sites)?;
        self.lexer.charge(1 + bytes.len(), dot)?;
        grow_dot_pool(&mut self.lexer, &mut self.dot.sites, sites + 1, dot)?;
        grow_dot_pool(&mut self.lexer, &mut self.dot.keys, end, dot)?;
        // The scanner already authenticated the ASCII IdentifierName. Store
        // one immutable range per occurrence, not a binding or a cache entry.
        self.dot.keys.extend(bytes.iter().copied().map(u16::from));
        self.dot.sites.push(occurrence);
        Ok(site)
    }

    pub(super) fn patch_dot(
        &mut self,
        site: u16,
        instruction: usize,
        replacement: Instruction,
        span: Span,
    ) -> Result<(), Error> {
        if self.stack_depth != 0
            && self.instructions.len().checked_sub(1) == Some(instruction)
            && self.instructions[instruction] == Instruction::GetDot(site)
        {
            // Retain the Get's emission charge and ordinal, including when a
            // group closes without having emitted any converting instructions.
            self.instructions[instruction] = replacement;
            Ok(())
        } else {
            Err(Error::malformed(span, "invalid terminal dot reference"))
        }
    }

    pub(super) fn finish_call(&mut self, close: Span, last: bool) -> Result<(), Error> {
        let Some((
            Pending::Call {
                base,
                completed,
                member,
            },
            open,
        )) = self.pending.pop()
        else {
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
                member,
            },
            open,
        )
    }
}

// Logical tiers are inferred from the initialized length. Debit possible
// moves even if the allocator donated spare space; no key is removed. The
// two limits are powers of two, so no request can round up beyond its cap.
fn grow_dot_pool<T>(
    lexer: &mut super::Lexer<'_>,
    pool: &mut Vec<T>,
    needed: usize,
    span: Span,
) -> Result<(), Error> {
    let len = pool.len();
    if len == 0 || needed > len.next_power_of_two() {
        lexer.charge(len, span)?;
        pool.try_reserve_exact(needed.next_power_of_two() - len)
            .map_err(|_| Error::limit(span, Limit::Allocation))?;
    }
    Ok(())
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
            Some(RealmParseLimit::DotSites) => CompileLimit::DotSites,
            Some(RealmParseLimit::DotKeyUnits) => CompileLimit::DotKeyUnits,
            Some(RealmParseLimit::PooledDotKeyUnits) => CompileLimit::PooledDotKeyUnits,
            None => return CompileError::Parse(error),
        };
        cap(limit, error.span())
    })?;
    // Validate the entire body, including earlier directives and dead code,
    // after parsing (including streaming limits) but before final lowering.
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
        dot: DotCode {
            source_len: source
                .len()
                .try_into()
                .map_err(|_| CompileError::InvalidScript)?,
            ..parser.dot
        },
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

#[cfg(test)]
mod dot_lowering_tests {
    use super::*;
    use crate::{ErrorKind, parser::BARE_CALL};
    use Instruction::*;

    fn one_byte(start: usize) -> Span {
        Span {
            start,
            end: start + 1,
        }
    }

    fn call(source: &str, text: &str, argc: u8, member: u16) -> Instruction {
        let start = source.find(text).unwrap() as u32;
        Call {
            start,
            end: start + text.len() as u32,
            argc,
            member,
        }
    }

    fn key(dot: &DotCode, site: &DotOccurrence) -> std::string::String {
        std::string::String::from_utf16(
            &dot.keys[usize::from(site.key_start)..usize::from(site.key_end)],
        )
        .unwrap()
    }

    #[test]
    fn exact_chained_get_put_and_prepared_call_ir() {
        let source = "a. /*é*/if.b = c.d = (e.m)(a, q.n()).z;";
        let code = compile_realm(source).unwrap();
        assert_eq!(
            code.instructions,
            [
                LoadBinding(0),
                GetDot(0),
                FormDot(1),
                LoadBinding(1),
                FormDot(2),
                LoadBinding(2),
                PrepareDotCall(3),
                LoadBinding(3),
                LoadBinding(4),
                PrepareDotCall(4),
                call(source, "()", 0, 4),
                call(source, "(a, q.n())", 2, 3),
                GetDot(5),
                PutDot(2),
                PutDot(1),
            ]
        );
        assert_eq!(code.max_stack_depth, 3);
        assert_eq!(code.exit, crate::parser::ExitStack::Value);
        assert_eq!(code.names, b"aceaq");
        assert_eq!(code.sites.len(), 5);
        assert!(
            code.sites
                .iter()
                .all(|site| matches!(site.role, Role::Read))
        );
        assert!(code.string_units.is_empty());
        assert_eq!(code.dot.source_len as usize, source.len());
        let offsets = source.match_indices('.').map(|(i, _)| i as u32);
        let mut key_start = 0;
        for ((site, offset), expected) in code
            .dot
            .sites
            .iter()
            .zip(offsets)
            .zip(["if", "b", "d", "m", "n", "z"])
        {
            assert_eq!(key(&code.dot, site), expected);
            assert_eq!(
                (site.owner, site.key_start, site.dot_start, site.dot_end),
                (DOT_ROOT, key_start, offset, offset + 1)
            );
            key_start += expected.len() as u16;
            assert_eq!(site.key_end, key_start);
        }
        assert_eq!(code.dot.sites.len(), 6);
        assert_eq!(code.dot.keys.len(), usize::from(key_start));
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<Instruction>(), 16);
        assert_eq!(std::mem::size_of::<DotOccurrence>(), 16);
    }

    #[test]
    fn put_keeps_its_rhs_slot_across_an_expression_join() {
        let code = compile_realm("a.p = true ? b.q = 1 : c.r = 2;").unwrap();
        assert_eq!(
            code.instructions,
            [
                LoadBinding(0),
                FormDot(0),
                Boolean(true),
                JumpIfFalse(9),
                LoadBinding(1),
                FormDot(1),
                Number(1.0),
                PutDot(1),
                Jump(13),
                LoadBinding(2),
                FormDot(2),
                Number(2.0),
                PutDot(2),
                PutDot(0),
            ]
        );
        assert_eq!(code.max_stack_depth, 1);
        assert!(code.functions.is_empty());
    }

    #[test]
    fn both_call_closes_continue_into_dot_and_call_suffixes() {
        for (source, prefix, close, argc) in [
            ("f()().x.m();", "()()", 2, 0),
            ("f(1)(2).x.m(3);", "(1)(2)", 3, 1),
        ] {
            let code = compile_realm(source).unwrap();
            let mut expected = vec![LoadBinding(0)];
            for i in 0..2 {
                let start = 1 + i * close;
                if argc != 0 {
                    expected.push(Number(f64::from(i + 1)));
                }
                expected.push(Call {
                    start,
                    end: start + close,
                    argc,
                    member: BARE_CALL,
                });
            }
            assert_eq!(&source[1..1 + 2 * close as usize], prefix);
            expected.extend([GetDot(0), PrepareDotCall(1)]);
            let final_pair = &source[source.rfind('(').unwrap()..source.len() - 1];
            if argc != 0 {
                expected.push(Number(3.0));
            }
            expected.push(Call {
                start: source.rfind('(').unwrap() as u32,
                end: source.len() as u32 - 1,
                argc,
                member: 1,
            });
            assert_eq!(final_pair.len(), close as usize);
            assert_eq!(code.instructions, expected);
            assert_eq!(code.max_stack_depth, 1 + usize::from(argc));
        }
    }

    #[test]
    fn member_arguments_share_the_bare_call_ceiling() {
        let prefix = format!("f.m({}1", "1,".repeat(usize::from(MAX_PARAMETERS) - 1));
        let source = format!("{prefix});");
        let code = compile_realm(&source).unwrap();
        assert_eq!(code.instructions[1], PrepareDotCall(0));
        assert_eq!(code.max_stack_depth, usize::from(MAX_PARAMETERS) + 1);
        assert_eq!(
            code.instructions.last(),
            Some(&call(
                &source,
                &source[3..source.len() - 1],
                MAX_PARAMETERS,
                0
            ))
        );
        assert_eq!(
            compile_realm(&format!("{prefix},1);")).unwrap_err(),
            cap(CompileLimit::Arguments, one_byte(prefix.len()))
        );
    }

    #[test]
    fn grouping_preserves_only_unconverted_property_references() {
        for source in ["(f.p)();", "(((f.p)))();", "(0, f).p();", "(f.p, f.p).p();"] {
            let code = compile_realm(source).unwrap();
            let member = code.dot.sites.len() as u16 - 1;
            assert!(
                code.instructions
                    .ends_with(&[PrepareDotCall(member), call(source, "()", 0, member)]),
                "{source}"
            );
        }
        for source in [
            "(0, f.p)();",
            "(true && f.p)();",
            "(false || f.p)();",
            "(null ?? f.p)();",
            "(true ? g.p : f.p)();",
            "(v = f.p)();",
            "(v ||= f.p)();",
            "(g.p = f.p)();",
            "(+f.p)();",
            "(typeof f.p)();",
        ] {
            let code = compile_realm(source).unwrap();
            assert!(
                code.instructions
                    .ends_with(&[call(source, "()", 0, BARE_CALL)]),
                "{source}"
            );
            assert!(
                !code
                    .instructions
                    .iter()
                    .any(|op| matches!(op, PrepareDotCall(_))),
                "{source}"
            );
        }
        let code = compile_realm("typeof (missing.p);").unwrap();
        assert_eq!(code.instructions, [LoadBinding(0), GetDot(0), Typeof]);
        assert!(matches!(code.sites[0].role, Role::Read));
        let code = compile_realm("typeof (missing).p();").unwrap();
        assert!(matches!(code.sites[0].role, Role::Read));
        assert_eq!(code.instructions[1], PrepareDotCall(0));
    }

    #[test]
    fn property_lhs_precedence_and_delimiter_ownership() {
        for source in [
            "a+f.p=x;",
            "a||f.p=x;",
            "a&&f.p=x;",
            "a??(f.p)=x;",
            "+(f.p)=x;",
            "typeof f.p=x;",
        ] {
            let CompileError::Parse(err) = compile_realm(source).unwrap_err() else {
                panic!("{source}");
            };
            assert_eq!(err.kind(), ErrorKind::Malformed, "{source}");
            assert_eq!(err.span(), one_byte(source.find('=').unwrap()));
        }
        for source in [
            "(0, f.p)=x;",
            "(a?b:f.p)=x;",
            "(a||f.p)=x;",
            "(a=f.p)=x;",
            "f.p()=x;",
            "(f.p=x)=y;",
        ] {
            assert!(compile_realm(source).is_err(), "{source}");
        }
        for (source, puts) in [
            ("(f.p)=x;", 1),
            ("(+x).p=y;", 1),
            ("(0, f.p=x);", 1),
            ("a || (f.p=x);", 1),
            ("a?f.p=x:g.p=y;", 2),
            ("f.p = a? x : y;", 1),
            ("f.p = v = x;", 1),
            ("v = f.p = x;", 1),
            ("f.p = x, g.p = y;", 2),
            ("if (f.p = x, g.p = y) f.q = z;", 3),
            ("while (f.p=x) { f.q=y; }", 2),
            ("f.m(g.p=x, h.p=y);", 2),
            ("let a=f.p=x, b=g.q=y;", 2),
            ("for (f.p=x; f.q=y; f.r=z, f.s=x) { f.t=y; }", 5),
            ("for (var a=f.p=x, b=g.q=y; f.r=x; f.s=y) {}", 4),
        ] {
            let code = compile_realm(source).unwrap_or_else(|e| panic!("{source}: {e:?}"));
            assert_eq!(
                code.instructions
                    .iter()
                    .filter(|op| matches!(op, PutDot(_)))
                    .count(),
                puts,
                "{source}"
            );
        }
    }

    #[test]
    fn parked_put_call_and_template_keep_actual_body_owners_and_stack_peaks() {
        let source = "function d(p){ var v=p.if; return (v.m)(); }\n\
            a.p = function(q){ 'use strict'; return q.return; }.b;\n\
            a.m(`L${function(){return z.x;}.k}R`).z;";
        let code = compile_realm(source).unwrap();
        let keys_and_owners: Vec<_> = code
            .dot
            .sites
            .iter()
            .map(|site| (key(&code.dot, site), site.owner))
            .collect();
        let expected = [
            ("if", 0),
            ("m", 0),
            ("p", DOT_ROOT),
            ("return", 1),
            ("b", DOT_ROOT),
            ("m", DOT_ROOT),
            ("x", 2),
            ("k", DOT_ROOT),
            ("z", DOT_ROOT),
        ];
        assert_eq!(
            keys_and_owners,
            expected.map(|(key, owner)| (key.to_owned(), owner))
        );
        assert_eq!(code.functions.len(), 3);
        assert_eq!(
            code.functions[0].instructions,
            [
                LoadLocal(0),
                GetDot(0),
                StoreLocal {
                    slot: 1,
                    keep: false
                },
                LoadLocal(1),
                PrepareDotCall(1),
                call(source, "()", 0, 1),
                Return { value: true }
            ]
        );
        assert_eq!(code.functions[0].max_stack_depth, 1);
        assert!(code.functions[1].mode == CodeMode::Strict);
        assert!(code.functions[1].instructions.ends_with(&[
            LoadLocal(0),
            GetDot(3),
            Return { value: true }
        ]));
        assert!(code.functions[2].mode == CodeMode::Sloppy);
        assert!(
            code.functions[2]
                .instructions
                .ends_with(&[GetDot(6), Return { value: true }])
        );
        assert_eq!(
            &code.instructions[..6],
            [
                LoadBinding(4),
                FormDot(2),
                CreateFunction(1),
                GetDot(4),
                PutDot(2),
                Discard
            ]
        );
        assert!(matches!(
            &code.instructions[6..],
            [
                LoadBinding(_),
                PrepareDotCall(5),
                String(_),
                CreateFunction(2),
                GetDot(7),
                TemplateAppend(_),
                Call {
                    argc: 1,
                    member: 5,
                    ..
                },
                GetDot(8)
            ]
        ));
        assert_eq!(code.max_stack_depth, 3);
    }

    #[test]
    fn name_inference_and_strict_witnesses_follow_existing_owners() {
        let code = compile_realm(
            "let a=(function(){}), b=function(){}.p; a.p=(function(){}); \
            a.p = v = function(){}; v = a.p = function(){}; \
            v=(0,function(){}); v=function(){}();",
        )
        .unwrap();
        let names: Vec<_> =
            code.functions
                .iter()
                .map(|f| match f.kind {
                    BodyKind::Expression(name) => name
                        .map(|range| &code.names[usize::from(range.start)..usize::from(range.end)]),
                    BodyKind::Declaration => panic!("expected an anonymous definition"),
                })
                .collect();
        assert_eq!(
            names,
            [
                Some(b"a".as_slice()),
                None,
                None,
                Some(b"v".as_slice()),
                None,
                None,
                None
            ]
        );

        for (source, first) in [
            (
                r"'use strict'; f.m('\8'.p, function(){ return '\9'.p; });",
                r"\8",
            ),
            (
                r"f.p = function(){ 'use strict'; return '\9'.p; }; '\8'.p;",
                r"\9",
            ),
            (
                r"'use strict'; f.p = function(){ return x.p; }; '\8'.p;",
                r"\8",
            ),
        ] {
            let start = source.find(first).unwrap();
            assert_eq!(
                compile_realm(source).unwrap_err(),
                CompileError::SyntaxError {
                    span: Span {
                        start,
                        end: start + 2
                    }
                }
            );
        }
        let code = compile_realm(
            r"'use strict' /* cache */ .if; f.p = function(){return '\9'.p;}; '\8'.p;",
        )
        .unwrap();
        assert!(code.mode == CodeMode::Sloppy);
        assert!(code.functions[0].mode == CodeMode::Sloppy);
    }

    #[test]
    fn requested_pool_sizes_token_and_work_debits_are_exact() {
        let source = "f. /*é*/ab.c = g();";
        // Two loads, two dot emissions (one patched in place), two markers,
        // call and Put. Two occurrence records, three copied units, and one
        // site + two initialized key units moved on the second append.
        let work = source.len() + 8 + 2 + 3 + 1 + 2;
        let limits = Limits {
            max_tokens: 10,
            max_work: work,
            ..Limits::default()
        };
        let mut parser = new_parser(source, limits, BindingPolicy::Realm).unwrap();
        parser.parse(true).unwrap();
        assert_eq!((parser.lexer.tokens_left, parser.lexer.work_left), (0, 0));
        for (max_tokens, max_work, limit) in [(9, work, Limit::Tokens), (10, work - 1, Limit::Work)]
        {
            let mut parser = new_parser(
                source,
                Limits {
                    max_tokens,
                    max_work,
                    ..limits
                },
                BindingPolicy::Realm,
            )
            .unwrap();
            let error = parser.parse(true).err().unwrap();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
            assert_eq!(error.span(), one_byte(source.len() - 1));
        }
    }

    #[test]
    fn key_site_and_parked_nesting_caps_are_inclusive() {
        let key = "k".repeat(MAX_DOT_KEY_UNITS);
        let code = compile_realm(&format!("f.{key};")).unwrap();
        assert_eq!(code.dot.keys.len(), MAX_DOT_KEY_UNITS);
        let source = format!("f.{key}x;");
        assert_eq!(
            compile_realm(&source).unwrap_err(),
            cap(
                CompileLimit::DotKeyUnits,
                Span {
                    start: 2,
                    end: source.len() - 1
                }
            )
        );

        let full = format!(
            "f{}",
            format!(".{key}").repeat(MAX_DOT_CODE_UNITS / MAX_DOT_KEY_UNITS)
        );
        let code = compile_realm(&format!("{full};")).unwrap();
        assert_eq!(code.dot.keys.len(), MAX_DOT_CODE_UNITS);
        assert_eq!(
            compile_realm(&format!("{full}.x;")).unwrap_err(),
            cap(CompileLimit::PooledDotKeyUnits, one_byte(full.len()))
        );

        // At least two tokens per dot means the default token cap wins.
        // Raise only that ceiling on the private parser to reach all site ids.
        let limits = Limits {
            max_tokens: 2 * MAX_DOT_SITES + 4,
            ..Limits::default()
        };
        let full = format!("f{}", ".p".repeat(MAX_DOT_SITES));
        let source = format!("{full};");
        let mut parser = new_parser(&source, limits, BindingPolicy::Realm).unwrap();
        parser.parse(true).unwrap();
        assert_eq!(parser.dot.sites.len(), MAX_DOT_SITES);
        assert_eq!(
            parser.instructions.last(),
            Some(&GetDot(MAX_DOT_SITES as u16 - 1))
        );
        let source = format!("{full}.p;");
        let err = new_parser(&source, limits, BindingPolicy::Realm)
            .unwrap()
            .parse(true)
            .err()
            .unwrap();
        assert_eq!(err.realm_limit, Some(RealmParseLimit::DotSites));
        assert_eq!(err.span(), one_byte(full.len()));

        for (prefix, suffix) in [
            ("f.p = function(){return ", " ;};"),
            ("f.m(function(){return ", " ;});"),
        ] {
            let make = |n| format!("{prefix}{}f.p{}{suffix}", "(".repeat(n), ")".repeat(n));
            // One parked property assignment/call + one parked creator.
            compile_realm(&make(126)).unwrap();
            let CompileError::Parse(err) = compile_realm(&make(127)).unwrap_err() else {
                panic!();
            };
            assert_eq!(err.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
            assert_eq!(err.span(), one_byte(prefix.len() + 126));
        }
    }

    #[test]
    fn unsupported_suffixes_fail_in_the_right_grammar_and_legacy_is_unchanged() {
        for source in [
            "f.;",
            "f[0];",
            "f?.x;",
            "f?.();",
            "f.x++;",
            "++f.x;",
            "f.x += 1;",
            "f.x ||= 1;",
            "f.#x;",
            "f.é;",
            r"f.if\u0061;",
            "f.x`tagged`;",
            "1..if;",
            "f.5;",
            "f.x(;",
            "f.x = ;",
        ] {
            assert!(
                matches!(compile_realm(source), Err(CompileError::Parse(_))),
                "{source}"
            );
        }
        assert_eq!(
            compile_realm("(1).if;").unwrap().instructions,
            [Number(1.0), GetDot(0)]
        );
        assert_eq!(compile_realm(".5;").unwrap().instructions, [Number(0.5)]);
        let mut legacy =
            new_parser("'a'.length;", Limits::default(), BindingPolicy::Legacy).unwrap();
        legacy.parse(false).unwrap();
        assert!(matches!(
            &legacy.instructions[..],
            [String(_), StringLength]
        ));
        assert!(legacy.dot.keys.is_empty() && legacy.dot.sites.is_empty());
        for (source, start) in [("f.if;", 1), ("f. /* gap */ if;", 1), ("f.length.x;", 8)] {
            let err = new_parser(source, Limits::default(), BindingPolicy::Legacy)
                .unwrap()
                .parse(false)
                .err()
                .unwrap();
            assert_eq!(err.kind(), ErrorKind::Unsupported);
            assert_eq!(err.span(), one_byte(start));
        }
    }
}
