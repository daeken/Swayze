use crate::{
    eval::MAX_INSTRUCTIONS,
    realm::{
        CodeMode, CompileError, CompileLimit, CompiledScript, MAX_CODE_NAME_BYTES,
        MAX_DECLARATIONS, MAX_NAME_BYTES, MAX_SITES, Role, Site,
    },
};

use super::{
    AccessKind, BindingPolicy, Error, Limit, Limits, Parser, QuotedDecimalEscape, Span, new_parser,
};

pub(crate) fn compile_realm(source: &str) -> Result<CompiledScript, CompileError> {
    let mut parser = new_parser(source, Limits::default(), BindingPolicy::Realm)?;
    let (exit, mode) = parser.parse(true)?;
    // Validate the entire body, including earlier directives and dead code,
    // after structural diagnostics but before Realm-only caps/name lowering.
    if mode == CodeMode::Strict
        && let QuotedDecimalEscape::Found(span) = parser.lexer.quoted_decimal_escape
    {
        return Err(CompileError::SyntaxError { span });
    }
    let end = Span::at(parser.lexer.offset);
    let declarations = parser.realm_declarations.len();
    let count = declarations + parser.accesses.len();
    if parser.instructions.len() > MAX_INSTRUCTIONS {
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
    for index in 0..parser.accesses.len() {
        let access = parser.accesses[index];
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
        parser
            .patch_access(access, Some(slot))
            .map_err(|_| CompileError::InvalidScript)?;
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
