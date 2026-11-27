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
    AccessKind, ActiveStatement, AfterExpression, BindingPolicy, CatchBinding, CatchSlot,
    DirectReference, DotName, Error, ExpressionEnd, ExpressionInput, ExpressionResume, Follow,
    Instruction, Limit, Limits, Operator, ParseAction, ParseErrorType, ParseOwner, Parser, Pending,
    QuotedDecimalEscape, RealmParseLimit, Span, StatementFrame, StatementKind, Token, TokenKind,
    TokenPosition, UNPATCHED, WriteAccess, new_parser, try_push,
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
    definition_scope: usize,
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
    restricted_catch: Option<Span>,
    definition_scope: usize,
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
        let definition_scope = self.current_scope();
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
            definition_scope,
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
                restricted_catch: owner.restricted_catch,
                definition_scope: function.definition_scope,
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

    pub(super) fn parse_throw(&mut self, keyword: Span) -> Result<ParseAction, Error> {
        let first = self.next_throw_operand()?.ok_or_else(|| {
            Error::syntax(Span::at(self.lexer.offset), "expected a throw operand")
        })?;
        if matches!(first.kind, TokenKind::Close) {
            return Err(Error::syntax(first.span, "expected a throw operand"));
        }
        Ok(ParseAction::expression(
            first.into(),
            ExpressionEnd::Throw,
            AfterExpression::Throw(keyword),
        ))
    }

    pub(super) fn pack_region_target(index: usize, span: Span) -> Result<u16, Error> {
        u16::try_from(index)
            .ok()
            .filter(|&index| index != u16::MAX)
            .ok_or_else(|| Error::realm_limit(span, RealmParseLimit::RegionTarget))
    }

    pub(super) fn open_try(&mut self, keyword: Span) -> Result<(), Error> {
        let brace = self.abrupt_token(TokenPosition::Plain, "expected '{' after try")?;
        if !matches!(brace.kind, TokenKind::OpenBrace) {
            return Err(Error::syntax(brace.span, "expected '{' after try"));
        }
        let enter = Self::pack_region_target(self.instructions.len(), keyword)?;
        self.emit(
            Instruction::EnterTry {
                catch: u16::MAX,
                after: u16::MAX,
            },
            keyword,
        )?;
        self.open_block(
            StatementFrame {
                kind: StatementKind::Try { enter },
                target: None,
            },
            brace.span,
        )
    }

    pub(super) fn open_catch(
        &mut self,
        owner: &mut ParseOwner,
        index: usize,
        enter: u16,
        close: Span,
    ) -> Result<ParseAction, Error> {
        self.emit(Instruction::LeaveTry { enter }, close)?;
        let clause = self.abrupt_token(TokenPosition::Plain, "expected catch after try block")?;
        match clause.kind {
            TokenKind::Catch => {}
            TokenKind::Finally => return Err(Self::excluded_finally(clause.span)),
            _ => return Err(Error::syntax(clause.span, "expected catch after try block")),
        }
        let open = self.abrupt_token(TokenPosition::Plain, "expected '(' after catch")?;
        match open.kind {
            TokenKind::Open => {}
            TokenKind::OpenBrace => {
                return Err(Error::unsupported(
                    open.span,
                    "optional catch binding is outside the subset",
                ));
            }
            _ => return Err(Error::syntax(open.span, "expected '(' after catch")),
        }
        let name = self.abrupt_token(
            TokenPosition::CatchParameter,
            "expected a catch binding name",
        )?;
        match name.kind {
            TokenKind::Identifier | TokenKind::RestrictedCatchName => {}
            TokenKind::OpenBrace => {
                return Err(Error::unsupported(
                    name.span,
                    "catch binding patterns are outside the subset",
                ));
            }
            TokenKind::Declaration(super::BindingKind::Let) => {
                return Err(Error::unsupported(
                    name.span,
                    "let as a catch binding is outside the subset",
                ));
            }
            _ => return Err(Error::syntax(name.span, "expected a catch binding name")),
        }
        if name.span.end - name.span.start > MAX_NAME_BYTES {
            return Err(Error::realm_limit(
                name.span,
                RealmParseLimit::CatchNameBytes,
            ));
        }
        let end = self.abrupt_token(TokenPosition::Plain, "expected ')' after catch binding")?;
        if !matches!(end.kind, TokenKind::Close) {
            return Err(Error::syntax(end.span, "expected ')' after catch binding"));
        }
        let brace = self.abrupt_token(TokenPosition::Plain, "expected '{' after catch binding")?;
        if !matches!(brace.kind, TokenKind::OpenBrace) {
            return Err(Error::syntax(
                brace.span,
                "expected '{' after catch binding",
            ));
        }
        if matches!(name.kind, TokenKind::RestrictedCatchName) {
            owner.restricted_catch.get_or_insert(name.span);
        }
        let frame = &self.statement_frames[index];
        let parent = self.scopes[frame.scope]
            .parent
            .expect("braced try has a parent");
        let slot = CatchSlot {
            enter,
            depth: frame.regions,
        };
        let marker = self.catches.len();
        self.lexer.charge(1, name.span)?;
        grow_parse_pool(&mut self.lexer, &mut self.catches, marker + 1, name.span)?;
        self.catches.push(CatchBinding {
            name: name.span,
            slot,
            parent: self.scopes[parent].nearest_catch,
        });
        // Each arm is a sibling Block. The simple parameter lives on the
        // catch Block's parse scope (no local lexical declarations here).
        let scope = self.push_scope(Some(parent), Some(marker), brace.span)?;
        let catch = Self::pack_region_target(self.instructions.len(), clause.span)?;
        self.emit(Instruction::EnterCatch { enter }, clause.span)?;
        self.lexer.charge(1, clause.span)?;
        let Instruction::EnterTry { catch: target, .. } =
            &mut self.instructions[usize::from(enter)]
        else {
            unreachable!("try opener retained until catch");
        };
        *target = catch;
        self.statement_frames[index].statement.kind = StatementKind::Catch { enter };
        self.statement_frames[index].scope = scope;
        Ok(ParseAction::Item)
    }

    pub(super) fn close_catch(&mut self, enter: u16, close: Span) -> Result<(), Error> {
        self.emit(Instruction::LeaveCatch { enter }, close)?;
        let after = Self::pack_region_target(self.instructions.len(), close)?;
        self.lexer.charge(1, close)?;
        let Instruction::EnterTry { after: target, .. } =
            &mut self.instructions[usize::from(enter)]
        else {
            unreachable!("try opener retained until catch close");
        };
        *target = after;
        let next = self.next_token(TokenPosition::Operand)?;
        if let Some(Token {
            kind: TokenKind::Finally,
            span,
            ..
        }) = next
        {
            return Err(Self::excluded_finally(span));
        }
        self.lookahead = Some(next);
        Ok(())
    }

    fn abrupt_token(
        &mut self,
        position: TokenPosition,
        message: &'static str,
    ) -> Result<Token, Error> {
        self.next_token(position)?
            .ok_or_else(|| Error::syntax(Span::at(self.lexer.offset), message))
    }

    fn excluded_finally(span: Span) -> Error {
        Error::unsupported(span, "finally is outside the subset")
    }

    pub(super) fn find_catch(
        &mut self,
        mut marker: Option<usize>,
        name: Span,
    ) -> Result<Option<CatchSlot>, Error> {
        while let Some(index) = marker {
            self.lexer.charge(1, name)?;
            let CatchBinding {
                name: candidate,
                slot,
                parent,
            } = self.catches[index];
            if self.names_equal(name, candidate)? {
                return Ok(Some(slot));
            }
            marker = parent;
        }
        Ok(None)
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
        grow_parse_pool(&mut self.lexer, &mut self.dot.sites, sites + 1, dot)?;
        grow_parse_pool(&mut self.lexer, &mut self.dot.keys, end, dot)?;
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
// dot limits are powers of two, so no request can round up beyond its cap.
// Catch markers are bounded by the aggregate token ceiling instead.
fn grow_parse_pool<T>(
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

#[derive(Clone, Copy)]
enum DirectSlot {
    Local(u8),
    Catch(CatchSlot),
}

impl DirectSlot {
    fn load(self) -> Instruction {
        match self {
            Self::Local(slot) => Instruction::LoadLocal(slot),
            Self::Catch(slot) => Instruction::LoadCatch(slot),
        }
    }

    fn store(self, keep: bool) -> Instruction {
        match self {
            Self::Local(slot) => Instruction::StoreLocal { slot, keep },
            Self::Catch(slot) => Instruction::StoreCatch { slot, keep },
        }
    }

    fn update(self, op: super::Update, result: super::UpdateResult) -> Instruction {
        match self {
            Self::Local(slot) => Instruction::UpdateLocal { slot, op, result },
            Self::Catch(slot) => Instruction::UpdateCatch { slot, op, result },
        }
    }

    fn role(self) -> Role {
        match self {
            Self::Local(_) => Role::Local,
            Self::Catch(_) => Role::Catch,
        }
    }
}

fn patch_direct(
    ops: &mut [Instruction],
    access: super::Access,
    slot: DirectSlot,
) -> Result<(), CompileError> {
    let op = ops
        .get_mut(access.instruction)
        .ok_or(CompileError::InvalidScript)?;
    *op = match (access.kind, *op) {
        (AccessKind::Read | AccessKind::TypeofRead, Instruction::LoadBinding(UNPATCHED)) => {
            slot.load()
        }
        (
            AccessKind::Write(WriteAccess { load: None, .. }),
            Instruction::UpdateBinding {
                slot: UNPATCHED,
                op,
                result,
            },
        ) => slot.update(op, result),
        (AccessKind::Write(_), Instruction::StoreBinding(UNPATCHED)) => slot.store(false),
        (AccessKind::Write(_), Instruction::StoreBindingKeep(UNPATCHED)) => slot.store(true),
        _ => return Err(CompileError::InvalidScript),
    };
    if let AccessKind::Write(write) = access.kind
        && let Some(load) = write.load
    {
        let Some(op @ Instruction::LoadBinding(UNPATCHED)) = ops.get_mut(load) else {
            return Err(CompileError::InvalidScript);
        };
        *op = slot.load();
    }
    Ok(())
}

pub(crate) fn compile_realm(source: &str) -> Result<CompiledScript, CompileError> {
    let mut parser = new_parser(source, Limits::default(), BindingPolicy::Realm)?;
    let (exit, mode) = parser.parse(true).map_err(|error| {
        let limit = match error.realm_limit {
            Some(RealmParseLimit::Arguments) => CompileLimit::Arguments,
            Some(RealmParseLimit::Parameters) => CompileLimit::Parameters,
            Some(RealmParseLimit::FormalNameBytes | RealmParseLimit::CatchNameBytes) => {
                CompileLimit::NameBytes
            }
            Some(RealmParseLimit::RegionTarget) => CompileLimit::Instructions,
            Some(RealmParseLimit::DotSites) => CompileLimit::DotSites,
            Some(RealmParseLimit::DotKeyUnits) => CompileLimit::DotKeyUnits,
            Some(RealmParseLimit::PooledDotKeyUnits) => CompileLimit::PooledDotKeyUnits,
            None => return CompileError::Parse(error),
        };
        cap(limit, error.span())
    })?;
    // Validate the entire body, including earlier directives and dead code,
    // after parsing (including streaming limits) but before final lowering.
    let witnesses = |mode, witness: &QuotedDecimalEscape, restricted| {
        let quoted = match witness {
            QuotedDecimalEscape::Found(span) => Some(*span),
            _ => None,
        };
        match mode {
            CodeMode::Strict => (
                quoted
                    .into_iter()
                    .chain(restricted)
                    .min_by_key(|span| span.start),
                None,
            ),
            CodeMode::Sloppy => (None, restricted),
        }
    };
    let (mut strict, mut sloppy) = witnesses(
        mode,
        &parser.lexer.quoted_decimal_escape,
        parser.script_restricted_catch,
    );
    for f in &parser.functions {
        let (body_strict, body_sloppy) = witnesses(f.body.mode, &f.witness, f.restricted_catch);
        strict = strict
            .into_iter()
            .chain(body_strict)
            .min_by_key(|span| span.start);
        sloppy = sloppy
            .into_iter()
            .chain(body_sloppy)
            .min_by_key(|span| span.start);
    }
    if let Some(span) = strict {
        return Err(CompileError::SyntaxError { span });
    }
    if let Some(span) = sloppy {
        return Err(Error::unsupported(
            span,
            "eval and arguments catch bindings are outside the subset",
        )
        .into());
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
        let direct = if let Some(slot) =
            parser.find_catch(parser.scopes[access.scope].nearest_catch, access.name)?
        {
            Some(DirectSlot::Catch(slot))
        } else if let Some(function) = body.as_deref() {
            if let Some(slot) = parser.find_local(&function.locals, access.name)? {
                Some(DirectSlot::Local(slot))
            } else if parser
                .find_catch(
                    parser.scopes[function.definition_scope].nearest_catch,
                    access.name,
                )?
                .is_some()
            {
                return Err(Error::unsupported(
                    access.name,
                    "capturing a catch binding across a function body is outside the subset",
                )
                .into());
            } else {
                None
            }
        } else {
            None
        };
        let role = match access.kind {
            AccessKind::Read => Role::Read,
            AccessKind::TypeofRead => Role::Typeof,
            AccessKind::Write(_) => Role::Write,
        };
        let slot = sites.len();
        sites.push(if let Some(direct) = direct {
            // Keep local and catch access numbers dense, but never link the
            // sentinel or retain name bytes for it (including in root IR).
            Site {
                span: access.name,
                start: 0,
                end: 0,
                role: direct.role(),
            }
        } else {
            copy_name(&mut parser, &mut names, access.name, role)?
        });
        parser.lexer.charge(1, access.name)?;
        if let AccessKind::Write(write) = access.kind
            && write.load.is_some()
        {
            parser.lexer.charge(1, access.name)?;
        }
        let instructions = body.map_or(&mut parser.instructions, |f| &mut f.body.instructions);
        if let Some(direct) = direct {
            patch_direct(instructions, access, direct)?;
        } else {
            Parser::patch_access_in(instructions, access, Some(slot))
                .map_err(|_| CompileError::InvalidScript)?;
        }
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

#[cfg(test)]
mod abrupt_lowering_tests {
    use super::*;
    use crate::{
        ErrorKind, ParseErrorType,
        parser::{ExitStack, THROW_SEMICOLON, Update, UpdateResult},
    };
    use Instruction::*;

    fn span(source: &str, text: &str) -> Span {
        let start = source.find(text).unwrap();
        Span {
            start,
            end: start + text.len(),
        }
    }

    fn error(source: &str) -> Error {
        match compile_realm(source) {
            Err(CompileError::Parse(error)) => error,
            Err(other) => panic!("{source}: {other:?}"),
            Ok(_) => panic!("unexpectedly compiled: {source}"),
        }
    }

    #[test]
    fn throw_restriction_precedes_operand_and_eof_and_does_not_apply_later() {
        for lt in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
            for (prefix, suffix) in [("", ""), (" //comment", ""), (" /*comment", "end*/")] {
                for operand in ["", "new F;", "[1];", "/regexp/;", "1;"] {
                    let trivia = format!("throw{prefix}{lt}{suffix} ");
                    assert_eq!(
                        error(&format!("{trivia}{operand}")),
                        Error::syntax(
                            Span::at(trivia.len()),
                            "line terminator before a restricted operand"
                        )
                    );
                }
            }
        }
        let text = "throw /*no break*/ (1\r\n + 2), f(`a${3}`)\n;";
        let code = compile_realm(text).unwrap();
        assert_eq!(
            code.instructions.last(),
            Some(&ThrowValue { start: 0, end: 5 })
        );
        assert_eq!(code.exit, ExitStack::Empty);
        for source in [
            "throw 1",
            "{ throw 1 }",
            "throw 1\n2;",
            "throw x\n++x;",
            "throw 1\n!x;",
            "if (true) throw 1\nelse ;",
            "throw 1\ntry {} catch (e) {}",
        ] {
            let err = error(source);
            assert_eq!(err.kind(), ErrorKind::Unsupported);
            assert_eq!(err.parse_error_type(), None);
            assert_eq!(err.message(), THROW_SEMICOLON);
        }
        for (source, bad) in [
            ("throw;", ";"),
            ("throw )", ")"),
            ("throw *1;", "*"),
            ("throw =1;", "="),
            ("throw 1 + );", ")"),
            ("{throw }", "}"),
            ("{throw (1 }", "}"),
            ("{throw 1 + }", "}"),
            ("throw 1 }", "}"),
            ("throw 1 2;", "2"),
            ("throw 1 const x = 0;", "const"),
            ("throw 1 else ;", "else"),
            ("throw 1);", ")"),
            ("throw 1 !x;", "!"),
        ] {
            let err = error(source);
            assert_eq!(
                (err.parse_error_type(), err.span()),
                (Some(ParseErrorType::SyntaxError), span(source, bad)),
                "{source}"
            );
        }
        for source in ["throw", "throw //no newline", "throw 1 +"] {
            let err = error(source);
            assert_eq!(err.parse_error_type(), Some(ParseErrorType::SyntaxError));
            assert_eq!(err.span(), Span::at(source.len()));
        }
    }

    #[test]
    fn cached_operand_preserves_original_trivia_without_replay() {
        for (text, newline) in [("throw /* x */1;", false), ("throw /*\u{2029} */1;", true)] {
            let mut parser = new_parser(text, Limits::default(), BindingPolicy::Realm).unwrap();
            let keyword = parser.next_token(TokenPosition::Operand).unwrap().unwrap();
            parser.lookahead = Some(parser.next_token(TokenPosition::Operand).unwrap());
            let work = parser.lexer.work_left;
            let tokens = parser.lexer.tokens_left;
            let offset = parser.lexer.offset;
            if newline {
                assert_eq!(
                    parser.parse_throw(keyword.span).err(),
                    Some(Error::syntax(
                        Span::at(span(text, "1").start),
                        "line terminator before a restricted operand"
                    ))
                );
                assert!(parser.lookahead.is_some());
            } else {
                assert!(parser.parse_throw(keyword.span).is_ok());
                assert!(parser.lookahead.is_none());
            }
            assert_eq!(
                (
                    parser.lexer.work_left,
                    parser.lexer.tokens_left,
                    parser.lexer.offset
                ),
                (work, tokens, offset)
            );
        }
    }

    #[test]
    fn dot_identifier_names_and_legacy_exclusions_keep_their_own_frontiers() {
        for key in ["throw", "try", "catch", "finally"] {
            let text = format!("throw obj.{key};");
            let code = compile_realm(&text).unwrap();
            assert!(matches!(
                code.instructions.as_slice(),
                [LoadBinding(_), GetDot(0), ThrowValue { start: 0, end: 5 }]
            ));
            let site = code.dot.sites[0];
            let dot = span(&text, ".");
            assert_eq!(
                (site.dot_start as usize, site.dot_end as usize),
                (dot.start, dot.end)
            );
            let text = format!("a; /*comment*/ {key};");
            let mut legacy = new_parser(&text, Limits::default(), BindingPolicy::Legacy).unwrap();
            assert_eq!(
                legacy.parse(false).err(),
                Some(Error::unsupported(
                    span(&text, key),
                    "reserved or excluded name is outside the subset"
                ))
            );
            for tail in ["é", "\\u0030"] {
                let text = format!("{key}{tail}");
                let mut legacy =
                    new_parser(&text, Limits::default(), BindingPolicy::Legacy).unwrap();
                let err = legacy.parse(false).err().unwrap();
                assert_eq!(err.kind(), ErrorKind::Unsupported);
                assert_eq!(err.span().start, key.len());
            }
        }
    }

    #[test]
    fn braces_clauses_and_parameter_fences_have_exact_diagnostics() {
        for (source, bad) in [
            ("try ;", ";"),
            ("try {} 1;", "1"),
            ("catch(e) {}", "catch"),
            ("finally {}", "finally"),
            ("try {} catch 1", "1"),
            ("try {} catch() {}", ")"),
            ("try {} catch('e') {}", "'e'"),
            ("try {} catch(e,e) {}", ","),
            ("try {} catch(e=0) {}", "="),
            ("try {} catch(e) ;", ";"),
            ("try {} catch(new) {}", "new"),
            ("throw 0; try {} catch(null) {}", "null"),
        ] {
            let err = error(source);
            assert_eq!(
                (err.parse_error_type(), err.span()),
                (Some(ParseErrorType::SyntaxError), span(source, bad)),
                "{source}"
            );
        }
        for source in [
            "try {",
            "try {}",
            "try {} catch",
            "try {} catch (e)",
            "try {} catch (e) {",
        ] {
            let err = error(source);
            assert_eq!(
                err.parse_error_type(),
                Some(ParseErrorType::SyntaxError),
                "{source}"
            );
            assert_eq!(err.span(), Span::at(source.len()));
        }
        for (source, bad) in [
            ("try {} finally {}", "finally"),
            ("try {} catch(e) {} finally {}", "finally"),
            ("try { } catch {}", "{}"),
            ("try {} catch({e}) {}", "{e}"),
            ("try {} catch([e]) {}", "["),
            ("try {} catch(let) {}", "let"),
            ("try {} catch(await) {}", "await"),
            ("try {} catch(yield) {}", "yield"),
            ("try {} catch(e\\u0030) {}", "\\"),
            ("label: try {} catch(e) {}", "try"),
        ] {
            let err = error(source);
            let mut bad_span = span(source, bad);
            if bad.starts_with('{') {
                bad_span.end = bad_span.start + 1;
            }
            assert_eq!(
                (err.kind(), err.parse_error_type(), err.span()),
                (ErrorKind::Unsupported, None, bad_span),
                "{source}"
            );
        }
    }

    #[test]
    fn inline_markers_keep_empty_arms_and_dangling_else_distinct() {
        assert_eq!(
            compile_realm("try {} catch(e) {}").unwrap().instructions,
            [
                EnterTry { catch: 2, after: 4 },
                LeaveTry { enter: 0 },
                EnterCatch { enter: 0 },
                LeaveCatch { enter: 0 },
            ]
        );
        let text = "if (true) try { throw 7; } catch(e) { throw e; } else throw 0;";
        let code = compile_realm(text).unwrap();
        assert_eq!(
            code.instructions,
            [
                Boolean(true),
                JumpIfFalseStatement(11),
                EnterTry {
                    catch: 6,
                    after: 10
                },
                Number(7.),
                ThrowValue { start: 16, end: 21 },
                LeaveTry { enter: 2 },
                EnterCatch { enter: 2 },
                LoadCatch(CatchSlot { enter: 2, depth: 1 }),
                ThrowValue { start: 38, end: 43 },
                LeaveCatch { enter: 2 },
                JumpStatement(13),
                Number(0.),
                ThrowValue { start: 54, end: 59 },
            ]
        );
        assert!(code.names.is_empty());
        assert_eq!(code.sites.len(), 1);
        assert_eq!((code.sites[0].start, code.sites[0].end), (0, 0));
        assert!(matches!(code.sites[0].role, Role::Catch));
        assert!(compile_realm("if (true) if (false) try {} catch(e) {} else ; else ;").is_ok());
        assert!(compile_realm("try {} catch(e) {} `a${1}`;").is_ok());
    }

    #[test]
    fn cells_shadow_locals_and_builtins_and_all_patch_shapes_stay_direct() {
        let text = "var e; function f(e) {var x; try { e; } catch(e) {\
                    typeof e; e += 1; e = 2; (e = 3); (++e); (e++); \
                    try { e; } catch(undefined) { undefined; e; } } return e; } e;";
        let code = compile_realm(text).unwrap();
        let body = &code.functions[0];
        let ops = &body.instructions;
        let outer = CatchSlot { enter: 0, depth: 1 };
        assert_eq!(ops[1], LoadLocal(0));
        assert_eq!(
            ops[5..22],
            [
                LoadCatch(outer),
                Typeof,
                Discard,
                LoadCatch(outer),
                Number(1.),
                Add,
                StoreCatch {
                    slot: outer,
                    keep: false
                },
                Number(2.),
                StoreCatch {
                    slot: outer,
                    keep: false
                },
                Number(3.),
                StoreCatch {
                    slot: outer,
                    keep: true
                },
                Discard,
                UpdateCatch {
                    slot: outer,
                    op: Update::Increment,
                    result: UpdateResult::New
                },
                Discard,
                UpdateCatch {
                    slot: outer,
                    op: Update::Increment,
                    result: UpdateResult::Old
                },
                Discard,
                EnterTry {
                    catch: 25,
                    after: 31
                },
            ]
        );
        assert_eq!(ops[22], LoadCatch(outer));
        assert_eq!(
            ops[26],
            LoadCatch(CatchSlot {
                enter: 21,
                depth: 2
            })
        );
        assert_eq!(ops[28], LoadCatch(outer));
        assert_eq!(ops[32..], [LoadLocal(0), Return { value: true }]);
        let sites = &code.sites[body.accesses.clone()];
        assert_eq!(sites.len(), 11);
        for (i, site) in sites.iter().enumerate() {
            assert!(if i == 0 || i == sites.len() - 1 {
                matches!(site.role, Role::Local)
            } else {
                matches!(site.role, Role::Catch)
            });
            assert_eq!((site.start, site.end), (0, 0));
        }
        let code = compile_realm("try {} catch(e) {try {} catch(e) {e;} e;} e;").unwrap();
        assert!(matches!(
            code.instructions[6],
            LoadCatch(CatchSlot { enter: 3, depth: 2 })
        ));
        assert!(matches!(
            code.instructions[9],
            LoadCatch(CatchSlot { enter: 0, depth: 1 })
        ));
        assert!(matches!(code.sites[2].role, Role::Read));
    }

    #[test]
    fn catch_hoists_and_capture_resolution_obey_body_ownership() {
        for source in [
            "try {} catch(e) {throw 1; var e;}",
            "try {} catch(e) {for(var e;false;) {}}",
            "try {} catch(e) {try {} catch(x) {var a,e;}}",
            "function f(){ try{}catch(x){var x;} }",
        ] {
            let err = error(source);
            assert_eq!(
                err.message(),
                "var with the same name as an enclosing catch binding is outside the subset"
            );
            assert_eq!(err.span().end - err.span().start, 1);
        }
        for rhs in ["e;", "typeof e;", "e = 0;", "e += 1;", "e++;", "undefined;"] {
            let name = if rhs == "undefined;" {
                "undefined"
            } else {
                "e"
            };
            let text =
                format!("try {{ throw 1; }} catch({name}) {{ (function() {{ return; {rhs} }}); }}");
            let err = error(&text);
            assert_eq!(
                err.message(),
                "capturing a catch binding across a function body is outside the subset"
            );
            assert_eq!(&text[err.span().start..err.span().end], name);
        }
        let text = "var e; try { var e; } catch(e) { var x; \
                     (function() {e; var e;}); (function(e) {e;}); \
                     (function() { try {} catch(e) {e;} g; }); \
                     e; } try {e;} catch(x) {e;}";
        let code = compile_realm(text).unwrap();
        let [var_body, formal_body, guarded_body] = &code.functions[..] else {
            panic!()
        };
        assert_eq!(var_body.instructions[0], LoadLocal(0));
        assert_eq!(formal_body.instructions[0], LoadLocal(0));
        assert_eq!(
            guarded_body.instructions[3],
            LoadCatch(CatchSlot { enter: 0, depth: 1 })
        );
        assert!(matches!(
            code.sites[guarded_body.accesses.end - 1].role,
            Role::Read
        ));
        assert!(matches!(
            code.sites[guarded_body.accesses.end].role,
            Role::Catch
        ));
        let code = compile_realm("function f(){var x; try{}catch(x){x;} return x;}").unwrap();
        assert_eq!(
            code.functions[0].instructions[3],
            LoadCatch(CatchSlot { enter: 0, depth: 1 })
        );
        assert_eq!(code.functions[0].instructions[6], LoadLocal(0));
    }

    #[test]
    fn restriction_witnesses_survive_parking_order_and_owner_modes() {
        for (text, bad) in [
            ("'use strict'; try{}catch(eval){} '\\8';", "eval"),
            ("'\\8'; 'use strict'; try{}catch(eval){}", "\\8"),
            (
                "'use strict'; try{}catch(e){ (function(){try{}catch(arguments){}}); }",
                "arguments",
            ),
            (
                "(function(){'use strict'; try{}catch(eval){} }); try{}catch(arguments){}",
                "eval",
            ),
            (
                "try{}catch(eval){ (function(){'use strict'; '\\8';}); }",
                "\\8",
            ),
        ] {
            let Some(CompileError::SyntaxError { span: actual }) = compile_realm(text).err() else {
                panic!("{text}");
            };
            assert_eq!(actual, span(text, bad), "{text}");
        }
        for (text, bad) in [
            ("try{}catch(eval){'use strict';}", "eval"),
            ("try{}catch(eval){ (function(){'use strict';}); }", "eval"),
            (
                "(function(){'use strict';}); (function(){try{}catch(arguments){}});",
                "arguments",
            ),
            ("try{}catch(e){ (function(){try{}catch(eval){}}); }", "eval"),
        ] {
            let err = error(text);
            assert_eq!(
                (err.kind(), err.parse_error_type(), err.span()),
                (ErrorKind::Unsupported, None, span(text, bad))
            );
        }
        let text = "'use strict'; try{}catch(eval){} throw;";
        assert_eq!(
            error(text).span(),
            Span {
                start: text.len() - 1,
                end: text.len()
            }
        );
    }
}

// Parser-only tests: deliberately never submit the split ABI to Realm execution.
#[cfg(test)]
mod abrupt_control_tests {
    use super::*;
    use crate::parser::MAX_STATEMENT_FRAMES;
    use Instruction::*;

    #[test]
    fn loop_in_try_keeps_region_but_nested_continue_leaves_one_region() {
        let code =
            compile_realm("try {while(true) {if(false) break; continue;}} catch(e) {}").unwrap();
        assert_eq!(
            code.instructions,
            [
                EnterTry {
                    catch: 9,
                    after: 11
                },
                Boolean(true),
                JumpIfFalseStatement(8),
                Boolean(false),
                JumpIfFalseStatement(6),
                JumpStatement(8),
                LoopBack(1),
                LoopBack(1),
                LeaveTry { enter: 0 },
                EnterCatch { enter: 0 },
                LeaveCatch { enter: 0 },
            ]
        );
        let code =
            compile_realm("label: while(true) { try {continue label;} catch(e) {break label;} }")
                .unwrap();
        assert_eq!(
            code.instructions,
            [
                Boolean(true),
                JumpIfFalseStatement(9),
                EnterTry { catch: 5, after: 8 },
                ExitRegions {
                    target: 0,
                    pop: 1,
                    backward: true
                },
                LeaveTry { enter: 2 },
                EnterCatch { enter: 2 },
                ExitRegions {
                    target: 9,
                    pop: 1,
                    backward: false
                },
                LeaveCatch { enter: 2 },
                LoopBack(0),
            ]
        );
    }

    #[test]
    fn mixed_forward_lists_with_do_and_labeled_block_and_disconnected_for_update() {
        let code = compile_realm(
            "label: do {if(true) continue; try {\
            try { continue label; } catch(e) {continue label;} \
            } catch(e) {break label;} continue; } while(false);",
        )
        .unwrap();
        assert_eq!(
            code.instructions,
            [
                Boolean(true),
                JumpIfFalseStatement(3),
                JumpStatement(15),
                EnterTry {
                    catch: 11,
                    after: 14
                },
                EnterTry {
                    catch: 7,
                    after: 10
                },
                ExitRegions {
                    target: 15,
                    pop: 2,
                    backward: false
                },
                LeaveTry { enter: 4 },
                EnterCatch { enter: 4 },
                ExitRegions {
                    target: 15,
                    pop: 2,
                    backward: false
                },
                LeaveCatch { enter: 4 },
                LeaveTry { enter: 3 },
                EnterCatch { enter: 3 },
                ExitRegions {
                    target: 18,
                    pop: 1,
                    backward: false
                },
                LeaveCatch { enter: 3 },
                JumpStatement(15),
                Boolean(false),
                JumpIfFalseStatement(18),
                LoopBack(0),
            ]
        );
        let code =
            compile_realm("label: {try {break label;} catch(e){ break label;} break label;}")
                .unwrap();
        assert_eq!(
            code.instructions,
            [
                EnterTry { catch: 3, after: 6 },
                ExitRegions {
                    target: 7,
                    pop: 1,
                    backward: false
                },
                LeaveTry { enter: 0 },
                EnterCatch { enter: 0 },
                ExitRegions {
                    target: 7,
                    pop: 1,
                    backward: false
                },
                LeaveCatch { enter: 0 },
                JumpStatement(7),
            ]
        );
        let code = compile_realm(
            "try { for (;true; i++) {try {continue;} catch(e) {continue;}} } catch(e) {}",
        )
        .unwrap();
        let ops = &code.instructions;
        assert_eq!(ops[3], JumpStatement(9));
        assert_eq!(ops[8], LoopBack(1));
        assert_eq!(
            ops[10],
            ExitRegions {
                target: 4,
                pop: 1,
                backward: true
            }
        );
        assert_eq!(
            ops[13],
            ExitRegions {
                target: 4,
                pop: 1,
                backward: true
            }
        );
        assert_eq!(ops[15], LoopBack(4));
    }

    #[test]
    fn return_and_dead_expression_islands_preserve_scope_and_operand_owners() {
        let code = compile_realm("function f() {try {return h(); throw `a${false ? 0 : true && 2}`;} catch(e) {return e;} return;}").unwrap();
        let ops = &code.functions[0].instructions;
        let EnterTry { catch, after } = ops[0] else {
            panic!()
        };
        let catch = usize::from(catch);
        let after = usize::from(after);
        assert_eq!(ops[3], Return { value: true });
        assert!(matches!(ops[catch - 2], ThrowValue { .. }));
        assert_eq!(
            ops[catch - 1..after],
            [
                LeaveTry { enter: 0 },
                EnterCatch { enter: 0 },
                LoadCatch(CatchSlot { enter: 0, depth: 1 }),
                Return { value: true },
                LeaveCatch { enter: 0 }
            ]
        );
        assert_eq!(ops[after..], [Return { value: false }]);
        assert!(!ops.iter().any(|op| matches!(op, ExitRegions { .. })));
        let text =
            "try {} catch(e) {throw `x${(function(){try {} catch (x) {return x;}})() ? e : e}`;}";
        let code = compile_realm(text).unwrap();
        assert_eq!(code.functions.len(), 1);
        assert_eq!(
            code.instructions
                .iter()
                .filter(|op| matches!(op, LoadCatch(CatchSlot { enter: 0, depth: 1 })))
                .count(),
            2
        );
        assert_eq!(
            code.functions[0].instructions[3],
            LoadCatch(CatchSlot { enter: 0, depth: 1 })
        );
        assert_eq!(code.functions[0].accesses.len(), 1);
    }

    #[test]
    fn catch_name_search_spends_only_actual_catch_chain_work() {
        let text = "try {} catch(outer) { try {} catch(e) { {{{ outer; }}} } } outer;";
        let mut parser = parse_with(text, Limits::default()).unwrap_or_else(|e| panic!("{e:?}"));
        let access = parser.accesses[0];
        let arm = parser.scopes[access.scope].nearest_catch;
        parser.lexer.work_left = 7; // two markers, five matched name bytes
        assert_eq!(
            parser.find_catch(arm, access.name).unwrap(),
            Some(CatchSlot { enter: 0, depth: 1 })
        );
        assert_eq!(parser.lexer.work_left, 0);
        let sibling = parser.accesses[1];
        assert!(
            parser
                .find_catch(parser.scopes[sibling.scope].nearest_catch, sibling.name)
                .unwrap()
                .is_none()
        );
        parser.lexer.work_left = 6;
        assert_eq!(
            parser.find_catch(arm, access.name).err(),
            Some(Error::limit(access.name, Limit::Work))
        );
    }

    fn parse_with(text: &str, limits: Limits) -> Result<Parser<'_>, Error> {
        let mut parser = new_parser(text, limits, BindingPolicy::Realm)?;
        parser.parse(true)?;
        Ok(parser)
    }

    #[test]
    fn token_work_and_parked_statement_frame_bounds_have_exact_frontiers() {
        let text = "try{}catch(e){ (function(){try{}catch(x){throw x;}}); throw e; }";
        let ok = parse_with(text, Limits::default()).unwrap_or_else(|e| panic!("{e:?}"));
        let defaults = Limits::default();
        let tokens = defaults.max_tokens - ok.lexer.tokens_left;
        let work = defaults.max_work - ok.lexer.work_left;
        assert_eq!((tokens, work), (32, 89));
        assert!(
            parse_with(
                text,
                Limits {
                    max_tokens: tokens,
                    max_work: work,
                    ..defaults
                }
            )
            .is_ok()
        );
        let err = parse_with(
            text,
            Limits {
                max_tokens: tokens - 1,
                ..defaults
            },
        )
        .err()
        .unwrap();
        assert_eq!(
            err,
            Error::limit(
                Span {
                    start: text.len() - 1,
                    end: text.len()
                },
                Limit::Tokens
            )
        );
        let err = parse_with(
            text,
            Limits {
                max_work: work - 1,
                ..defaults
            },
        )
        .err()
        .unwrap();
        assert_eq!(
            err,
            Error::limit(
                Span {
                    start: text.len() - 1,
                    end: text.len()
                },
                Limit::Work
            )
        );
        // One root catch remains parked while the independent callee opens blocks.
        let make = |count| {
            format!(
                "try {{}} catch(e) {{ (function() {{ {}{} }}); }}",
                "{".repeat(count),
                "}".repeat(count)
            )
        };
        assert!(compile_realm(&make(MAX_STATEMENT_FRAMES - 1)).is_ok());
        let bad = make(MAX_STATEMENT_FRAMES);
        let start = bad.find("{{").unwrap() + MAX_STATEMENT_FRAMES - 1;
        assert!(
            matches!(compile_realm(&bad), Err(CompileError::Parse(error))
            if error == Error { message: "statement nesting ceiling exceeded", ..Error::limit(Span{start,end:start+1},Limit::NestingDepth) })
        );
    }
}
