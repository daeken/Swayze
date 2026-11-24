use crate::{
    eval::MAX_INSTRUCTIONS,
    realm::{
        CodeMode, CompileError, CompileLimit, CompiledScript, FunctionBody, MAX_CODE_NAME_BYTES,
        MAX_DECLARATIONS, MAX_NAME_BYTES, MAX_PARAMETERS, MAX_SITES, Role, Site,
    },
};

use super::{
    AccessKind, BindingPolicy, Error, ExpressionEnd, Instruction, Limit, Limits, ParseErrorType,
    Parser, Pending, QuotedDecimalEscape, Span, TokenKind, TokenPosition, new_parser, try_push,
};

pub(super) struct OpenFunction {
    root_ops: Vec<Instruction>,
    root_peak: usize,
    root_mode: CodeMode,
    root_witness: QuotedDecimalEscape,
    params: Vec<Span>,
    accesses: usize,
}

pub(super) struct ParsedFunction {
    body: FunctionBody,
    params: Vec<Span>,
    witness: QuotedDecimalEscape,
}

impl Parser<'_> {
    pub(super) fn open_function(
        &mut self,
        root_mode: CodeMode,
        keyword: Span,
    ) -> Result<(), Error> {
        let name =
            self.require_token(TokenPosition::Plain, "expected an ordinary function name")?;
        if !matches!(name.kind, TokenKind::Identifier) {
            return Err(Error::unsupported(
                name.span,
                "expected a simple ordinary function name",
            ));
        }
        let open = self.require_token(
            TokenPosition::Plain,
            "expected '(' before formal parameters",
        )?;
        if !matches!(open.kind, TokenKind::Open) {
            return Err(Error::malformed(
                open.span,
                "expected '(' before formal parameters",
            ));
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
                    return Err(Error::limit(formal.span, Limit::Parameters));
                }
                if formal.span.end - formal.span.start > MAX_NAME_BYTES {
                    return Err(Error::limit(formal.span, Limit::FormalNameBytes));
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
        try_push(
            &mut self.realm_declarations,
            (name.span, Role::Function { body }),
            name.span,
        )?;
        self.function = Some(OpenFunction {
            root_ops: std::mem::take(&mut self.instructions),
            root_peak: std::mem::take(&mut self.max_stack_depth),
            root_mode,
            root_witness: std::mem::replace(
                &mut self.lexer.quoted_decimal_escape,
                QuotedDecimalEscape::Pending,
            ),
            params,
            accesses: self.accesses.len(),
        });
        Ok(())
    }

    pub(super) fn close_function(&mut self, mode: CodeMode, span: Span) -> Result<CodeMode, Error> {
        let function = self
            .function
            .take()
            .ok_or_else(|| Error::malformed(span, "invalid function boundary"))?;
        // Only this outer loop consumes the body's closing brace. An if-tail
        // can peek at that brace, never beyond it. No quoted token can be
        // decoded across either boundary under this grammar.
        debug_assert!(
            self.lookahead.is_none()
                && self.statement_frames.is_empty()
                && self.labels.is_empty()
                && self.for_header_scope.is_none()
                && self.innermost_target.is_none()
                && self.innermost_loop.is_none()
        );
        let body = FunctionBody {
            instructions: std::mem::replace(&mut self.instructions, function.root_ops),
            max_stack_depth: std::mem::replace(&mut self.max_stack_depth, function.root_peak),
            mode: if function.root_mode == CodeMode::Strict {
                CodeMode::Strict
            } else {
                mode
            },
            formals: function.params.len() as u8,
            accesses: function.accesses..self.accesses.len(),
        };
        let witness =
            std::mem::replace(&mut self.lexer.quoted_decimal_escape, function.root_witness);
        try_push(
            &mut self.functions,
            ParsedFunction {
                body,
                params: function.params,
                witness,
            },
            span,
        )?;
        Ok(function.root_mode)
    }

    pub(super) fn parse_return(&mut self, keyword: Span) -> Result<(), Error> {
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
            return self.emit(Instruction::Return { value: false }, keyword);
        }
        if first.after_line_break {
            return Err(Error::unsupported(
                first.span,
                "line-separated return requires an explicit semicolon before the next statement",
            ));
        }
        self.parse_expression(first.into(), ExpressionEnd::Return)?;
        self.emit(Instruction::Return { value: true }, keyword)
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
        self.emit(Instruction::Call { start, end, argc }, open)
    }
}

pub(crate) fn compile_realm(source: &str) -> Result<CompiledScript, CompileError> {
    let mut parser = new_parser(source, Limits::default(), BindingPolicy::Realm)?;
    let (exit, mode) = parser.parse(true)?;
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
    let mut parsed = std::mem::take(&mut parser.functions).into_iter().peekable();
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
        if let Some(function) = body {
            let mut formal = None;
            for (slot, &candidate) in function.params.iter().enumerate() {
                parser.lexer.charge(1, access.name)?;
                if parser.names_equal(access.name, candidate)? {
                    formal = Some(slot as u8);
                    break;
                }
            }
            if let Some(formal) = formal {
                // Keep access numbering dense and bounded. Formal sites have
                // source provenance but are never linked or looked up globally.
                parser.lexer.charge(1, access.name)?;
                let role = Role::Formal;
                sites.push(Site {
                    span: access.name,
                    start: 0,
                    end: 0,
                    role,
                });
                let ops = &mut function.body.instructions;
                let op = ops
                    .get_mut(access.instruction)
                    .ok_or(CompileError::InvalidScript)?;
                *op = match *op {
                    Instruction::LoadBinding(_) => Instruction::LoadFormal(formal),
                    Instruction::UpdateBinding { op, result, .. } => Instruction::UpdateFormal {
                        slot: formal,
                        op,
                        result,
                    },
                    Instruction::StoreBinding(_) => Instruction::StoreFormal {
                        slot: formal,
                        keep: false,
                    },
                    Instruction::StoreBindingKeep(_) => Instruction::StoreFormal {
                        slot: formal,
                        keep: true,
                    },
                    _ => return Err(CompileError::InvalidScript),
                };
                if let AccessKind::Write(write) = access.kind
                    && let Some(load) = write.load
                {
                    parser.lexer.charge(1, access.name)?;
                    let Some(op @ Instruction::LoadBinding(_)) = ops.get_mut(load) else {
                        return Err(CompileError::InvalidScript);
                    };
                    *op = Instruction::LoadFormal(formal);
                }
                continue;
            }
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
    Ok(Site {
        span,
        start: u16::try_from(start).map_err(|_| CompileError::InvalidScript)?,
        end: u16::try_from(end).map_err(|_| CompileError::InvalidScript)?,
        role,
    })
}

fn cap(limit: CompileLimit, span: Span) -> CompileError {
    CompileError::LimitExceeded { limit, span }
}
