use crate::{
    eval::MAX_INSTRUCTIONS,
    realm::{
        CompileError, CompileLimit, CompiledScript, MAX_CODE_NAME_BYTES, MAX_DECLARATIONS,
        MAX_NAME_BYTES, MAX_SITES, Role, Site,
    },
};

use super::{AccessKind, Binding, BindingPolicy, Error, Limit, Limits, Parser, Span, new_parser};

pub(crate) fn compile_realm(source: &str) -> Result<CompiledScript, CompileError> {
    let mut parser = new_parser(source, Limits::default(), BindingPolicy::Realm)?;
    let exit = parser.parse(true)?;
    let end = Span::at(parser.lexer.offset);
    let declarations = parser.bindings.len();
    let count = declarations + parser.accesses.len();
    if parser.instructions.len() > MAX_INSTRUCTIONS {
        return Err(cap(CompileLimit::Instructions, end));
    }
    if declarations > MAX_DECLARATIONS {
        return Err(cap(
            CompileLimit::Declarations,
            parser.bindings[MAX_DECLARATIONS].name,
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
        let Binding { name, kind, .. } = parser.bindings[index];
        sites.push(copy_name(
            &mut parser,
            &mut names,
            name,
            Role::Declaration(kind),
        )?);
        // Global conflicts are deferred, but a duplicate declaration within
        // this single Script does not depend on a Realm. Reuse the same
        // comparison charging as the legacy static resolver.
        for other in 0..index {
            parser.lexer.charge(1, name)?;
            if parser.names_equal(name, parser.bindings[other].name)? {
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
