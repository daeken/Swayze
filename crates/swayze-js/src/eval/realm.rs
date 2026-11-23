//! Opt-in, bounded Script execution with persistent root lexical scalars.
//!
//! ```
//! use swayze_js::realm::{CompiledScript, Completion, Realm};
//!
//! let consumer = CompiledScript::compile("count += 2; if (count !== 42) { fail; }").unwrap();
//! let producer = CompiledScript::compile("let count = 40;").unwrap();
//! let mut realm = Realm::new();
//! assert_eq!(realm.instantiate(&producer).unwrap().execute(), Ok(Completion::Normal));
//! drop(producer); // The Realm owns its lexical names and cells, not the code.
//! assert_eq!(realm.instantiate(&consumer).unwrap().execute(), Ok(Completion::Normal));
//! ```
//!
//! No global object, host bindings, `var`, strict directives, nested lexical
//! declarations, or persistent Strings. `undefined` is the sole intrinsic
//! fallback. The host chooses which checked Scripts to retain; the Realm does
//! not own or cache them. No Script results or binding handles leave the Realm.

use std::fmt;

use crate::{Error, Span, parser};

use super::{
    BindingKind, Bindings, EvalError, EvalScratch, ExitStack, Instruction, Operand, Update,
    UpdateNumeric, UpdateResult, Value, dispatch, init_operand, new_stack, store_operand,
    update_number, update_result, validate_layout,
};

pub(crate) const MAX_NAME_BYTES: usize = 256;
pub(crate) const MAX_CODE_NAME_BYTES: usize = 16 * 1024;
pub(crate) const MAX_DECLARATIONS: usize = 256;
pub(crate) const MAX_SITES: usize = 4096;
const MAX_GLOBALS: usize = 512;
const PREFLIGHT_WORK: usize = 128 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum Role {
    Declaration(BindingKind),
    Read,
    Typeof,
    Write,
}

pub(crate) struct Site {
    pub(crate) span: Span,
    pub(crate) start: u16,
    pub(crate) end: u16,
    pub(crate) role: Role,
}

/// Immutable policy-compiled code with no source borrow or Realm dependency.
pub struct CompiledScript {
    pub(crate) instructions: Vec<Instruction>,
    pub(crate) string_units: Vec<u16>,
    pub(crate) max_stack_depth: usize,
    pub(crate) exit: ExitStack,
    pub(crate) names: Vec<u8>,
    pub(crate) sites: Vec<Site>,
    pub(crate) declarations: usize,
}

impl CompiledScript {
    /// Compile under fixed ceilings, without checking or modifying any Realm.
    pub fn compile(source: &str) -> Result<Self, CompileError> {
        parser::compile_realm(source)
    }

    fn name(&self, site: &Site) -> Result<&[u8], InstantiateError> {
        self.names
            .get(usize::from(site.start)..usize::from(site.end))
            .ok_or(InstantiateError::InvalidScript)
    }
}

impl fmt::Debug for CompiledScript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledScript")
            .field("instructions", &self.instructions.len())
            .field("declarations", &self.declarations)
            .finish_non_exhaustive()
    }
}

/// Resource ceilings specific to Realm compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CompileLimit {
    Instructions,
    NameBytes,
    PooledNameBytes,
    Declarations,
    Sites,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CompileError {
    /// Usual coarse parser diagnostic (not a typed JavaScript exception).
    Parse(Error),
    /// A proven duplicate lexical declaration in this Script.
    SyntaxError {
        span: Span,
    },
    LimitExceeded {
        limit: CompileLimit,
        span: Span,
    },
    InvalidScript,
}

impl From<Error> for CompileError {
    fn from(error: Error) -> Self {
        Self::Parse(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstantiateError {
    /// A declaration conflicts with an existing lexical or restricted name.
    SyntaxError {
        span: Span,
    },
    BindingLimitExceeded,
    WorkLimitExceeded,
    AllocationFailed,
    InvalidScript,
}

/// A language outcome, not an Error object or a host-exposed guest value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Completion {
    Normal,
    Throw(Throw),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Throw {
    ReferenceError { span: Span },
    TypeError { span: Span },
}

/// A VM stop, distinct from a language throw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EngineFault {
    Evaluation(EvalError),
    ImplicitGlobal { span: Span },
    PersistentString { span: Span },
}

struct Cell {
    name: Vec<u8>,
    kind: BindingKind,
    // Only scalars can enter this field. None is the TDZ, not undefined.
    value: Option<Value>,
}

/// Owns one bounded lexical environment. No handles, callbacks, or reentry.
#[derive(Default)]
pub struct Realm {
    globals: Vec<Cell>,
}

impl Realm {
    pub fn new() -> Self {
        Self::default()
    }

    /// Preflight and atomically install the declarations as TDZ cells. A
    /// failed instantiation installs nothing. Missing or uninitialized uses
    /// are resolved for execution, not rejected by this check.
    pub fn instantiate<'realm, 'code>(
        &'realm mut self,
        code: &'code CompiledScript,
    ) -> Result<ReadyScript<'realm, 'code>, InstantiateError> {
        self.instantiate_with_work(code, PREFLIGHT_WORK)
    }

    fn instantiate_with_work<'realm, 'code>(
        &'realm mut self,
        code: &'code CompiledScript,
        work: usize,
    ) -> Result<ReadyScript<'realm, 'code>, InstantiateError> {
        use InstantiateError::{
            AllocationFailed, BindingLimitExceeded, InvalidScript, SyntaxError,
        };
        let mut work = Work(work);
        let base = self.globals.len();
        let (declarations, accesses) = code
            .sites
            .split_at_checked(code.declarations)
            .ok_or(InvalidScript)?;
        if declarations.len() > MAX_DECLARATIONS || code.sites.len() > MAX_SITES {
            return Err(InvalidScript);
        }
        // Conflict checks precede capacity checks, in declaration source order.
        for site in declarations {
            work.charge(1)?;
            let name = code.name(site)?;
            if work.equal(name, b"undefined")? || work.find(name, &self.globals)?.is_some() {
                return Err(SyntaxError { span: site.span });
            }
        }
        let total = base + declarations.len(); // Both terms are independently bounded.
        if total > MAX_GLOBALS {
            return Err(BindingLimitExceeded);
        }

        let mut staged = Vec::new();
        staged
            .try_reserve_exact(declarations.len())
            .map_err(|_| AllocationFailed)?;
        let mut links = Vec::new();
        links
            .try_reserve_exact(code.sites.len())
            .map_err(|_| AllocationFailed)?;
        for (i, site) in declarations.iter().enumerate() {
            let Role::Declaration(kind) = site.role else {
                return Err(InvalidScript);
            };
            let name = code.name(site)?;
            // Prepare an owned name and stage a cell and a link. Charge the
            // publication write too; commit does no remaining fallible work.
            work.charge(name.len() + 3)?;
            let mut owned = Vec::new();
            owned
                .try_reserve_exact(name.len())
                .map_err(|_| AllocationFailed)?;
            owned.extend_from_slice(name);
            staged.push(Cell {
                name: owned,
                kind,
                value: None,
            });
            links.push(Target::Lexical(base + i));
        }
        for site in accesses {
            work.charge(1)?;
            let name = code.name(site)?;
            let target = if let Some(slot) = work.find(name, &self.globals)? {
                Target::Lexical(slot)
            } else if let Some(slot) = work.find(name, &staged)? {
                Target::Lexical(base + slot)
            } else if work.equal(name, b"undefined")? {
                Target::Undefined
            } else {
                Target::Missing
            };
            links.push(target);
        }

        // Request bounded geometric backing. Names already belong to staged
        // cells; extending only moves validated rows, without copying names.
        if total > self.globals.capacity() {
            work.charge(base)?; // Existing cells may move; their owned names do not.
            self.globals
                .try_reserve_exact(total.next_power_of_two() - base)
                .map_err(|_| AllocationFailed)?;
        }
        self.globals.extend(staged);
        Ok(ReadyScript {
            realm: self,
            code,
            links,
        })
    }
}

impl fmt::Debug for Realm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Realm")
            .field("bindings", &self.globals.len())
            .finish_non_exhaustive()
    }
}

struct Work(usize);

impl Work {
    fn charge(&mut self, credits: usize) -> Result<(), InstantiateError> {
        self.0 = self
            .0
            .checked_sub(credits)
            .ok_or(InstantiateError::WorkLimitExceeded)?;
        Ok(())
    }

    fn equal(&mut self, name: &[u8], candidate: &[u8]) -> Result<bool, InstantiateError> {
        self.charge(1)?;
        if name.len() != candidate.len() {
            return Ok(false);
        }
        self.charge(name.len())?;
        Ok(name == candidate)
    }

    fn find(&mut self, name: &[u8], globals: &[Cell]) -> Result<Option<usize>, InstantiateError> {
        for (index, cell) in globals.iter().enumerate() {
            if self.equal(name, &cell.name)? {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }
}

#[derive(Clone, Copy)]
enum Target {
    Lexical(usize),
    Undefined,
    Missing,
}

/// A one-shot execution guard. Dropping the guard abandons the body, but
/// leaves its successfully installed lexical bindings in TDZ.
///
/// ```compile_fail
/// use swayze_js::realm::{CompiledScript, Realm};
/// let code = CompiledScript::compile("let a = 1;").unwrap();
/// let mut realm = Realm::new();
/// let ready = realm.instantiate(&code).unwrap();
/// let _other = realm.instantiate(&code);
/// let _ = ready.execute();
/// ```
///
/// ```compile_fail
/// use swayze_js::realm::{CompiledScript, Realm};
/// let code = CompiledScript::compile("let a = 1;").unwrap();
/// let mut realm = Realm::new();
/// let ready = realm.instantiate(&code).unwrap();
/// let _ = ready.execute();
/// let _ = ready.execute();
/// ```
#[must_use = "dropping an instantiated Script leaves its lexical declarations uninitialized"]
pub struct ReadyScript<'realm, 'code> {
    realm: &'realm mut Realm,
    code: &'code CompiledScript,
    links: Vec<Target>,
}

impl fmt::Debug for ReadyScript<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadyScript").finish_non_exhaustive()
    }
}

impl ReadyScript<'_, '_> {
    /// Execute once. Normal completion discards the result; a throw or VM stop
    /// preserves declarations and all effects that occurred before it.
    pub fn execute(mut self) -> Result<Completion, EngineFault> {
        let code = self.code;
        validate_layout(&code.instructions, code.max_stack_depth, code.exit)
            .map_err(EngineFault::Evaluation)?;
        let stack = new_stack(code.max_stack_depth).map_err(EngineFault::Evaluation)?;
        let mut scratch = EvalScratch::new();
        match dispatch(
            &code.instructions,
            stack,
            code.max_stack_depth,
            &code.string_units,
            code.exit,
            &mut scratch,
            &mut self,
        ) {
            Ok(_) => Ok(Completion::Normal),
            Err(Stop::Throw(error)) => Ok(Completion::Throw(error)),
            Err(Stop::Engine(error)) => Err(error),
        }
    }

    fn site(&self, index: usize) -> Result<(Target, Span, Role), Stop> {
        let site = self.code.sites.get(index).ok_or(EvalError::InvalidScript)?;
        let target = *self.links.get(index).ok_or(EvalError::InvalidScript)?;
        Ok((target, site.span, site.role))
    }

    fn put(&mut self, index: usize, value: Operand) -> Result<(), Stop> {
        let (target, span, Role::Write) = self.site(index)? else {
            return Err(EvalError::InvalidScript.into());
        };
        let cell = match target {
            // Sloppy writes to the readonly intrinsic are ignored, *after*
            // the RHS/coercion. No value escapes, including String operands.
            Target::Undefined => return Ok(()),
            Target::Missing => return Err(Stop::Engine(EngineFault::ImplicitGlobal { span })),
            Target::Lexical(slot) => self
                .realm
                .globals
                .get_mut(slot)
                .ok_or(EvalError::InvalidScript)?,
        };
        if cell.value.is_none() {
            return Err(Stop::Throw(Throw::ReferenceError { span }));
        }
        if cell.kind == BindingKind::Const {
            return Err(Stop::Throw(Throw::TypeError { span }));
        }
        cell.value = Some(scalar(value, span)?);
        Ok(())
    }
}

fn scalar(value: Operand, span: Span) -> Result<Value, Stop> {
    match value {
        Operand::Scalar(value) => Ok(value),
        Operand::String(_) => Err(Stop::Engine(EngineFault::PersistentString { span })),
    }
}

pub(super) enum Stop {
    Throw(Throw),
    Engine(EngineFault),
}

impl From<EvalError> for Stop {
    fn from(error: EvalError) -> Self {
        Self::Engine(EngineFault::Evaluation(error))
    }
}

impl Bindings for ReadyScript<'_, '_> {
    type Error = Stop;

    fn load(&self, index: usize) -> Result<Operand, Stop> {
        let (target, span, role) = self.site(index)?;
        let value = match target {
            Target::Undefined => Value::Undefined,
            Target::Missing if matches!(role, Role::Typeof) => Value::Undefined,
            Target::Missing => return Err(Stop::Throw(Throw::ReferenceError { span })),
            Target::Lexical(slot) => self
                .realm
                .globals
                .get(slot)
                .ok_or(EvalError::InvalidScript)?
                .value
                .ok_or(Stop::Throw(Throw::ReferenceError { span }))?,
        };
        Ok(Operand::Scalar(value))
    }

    fn init(
        &mut self,
        index: usize,
        kind: BindingKind,
        _: usize,
        stack: &mut Vec<Operand>,
    ) -> Result<(), Stop> {
        let (Target::Lexical(slot), span, Role::Declaration(declared)) = self.site(index)? else {
            return Err(EvalError::InvalidScript.into());
        };
        let cell = self
            .realm
            .globals
            .get_mut(slot)
            .ok_or(EvalError::InvalidScript)?;
        if declared != kind || cell.kind != kind || cell.value.is_some() {
            return Err(EvalError::InvalidScript.into());
        }
        cell.value = Some(scalar(init_operand(stack)?, span)?);
        Ok(())
    }

    fn store(&mut self, index: usize, keep: bool, stack: &mut Vec<Operand>) -> Result<(), Stop> {
        self.put(index, store_operand(stack, keep)?)
    }

    fn update(
        &mut self,
        index: usize,
        op: Update,
        result: UpdateResult,
        stack: &mut Vec<Operand>,
        max: usize,
        numeric: UpdateNumeric<'_, '_>,
    ) -> Result<(), Stop> {
        let old = self
            .load(index)?
            .to_number(numeric.pools, numeric.credits)?;
        let new = update_number(op, old);
        self.put(index, Operand::Scalar(Value::Number(new)))?;
        update_result(stack, max, result, old, new)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_work_boundary_after_staging_does_not_leave_a_tdz_cell() {
        let code = CompiledScript::compile("let planned; missing;").unwrap();
        // Declaration check: 1 row + 1 unequal-length candidate. Stage:
        // 7 bytes + 3 writes. Access: 1 row + (1 candidate + 7 bytes) +
        // 1 unequal-length intrinsic comparison. No existing rows to move.
        let mut realm = Realm::new();
        assert_eq!(
            realm.instantiate_with_work(&code, 21).unwrap_err(),
            InstantiateError::WorkLimitExceeded
        );
        assert!(realm.globals.is_empty());
        let ready = realm.instantiate_with_work(&code, 22).unwrap();
        assert!(matches!(
            ready.execute(),
            Ok(Completion::Throw(Throw::ReferenceError { .. }))
        ));
        assert_eq!(realm.globals[0].value, Some(Value::Undefined));
    }

    #[test]
    fn debug_only_exposes_shape() {
        let code = CompiledScript::compile("let secretBinding = 123456789;").unwrap();
        let mut realm = Realm::new();
        let mut rendered = format!("{code:?}");
        let ready = realm.instantiate(&code).unwrap();
        rendered.push_str(&format!("{ready:?}"));
        assert_eq!(ready.execute(), Ok(Completion::Normal));
        rendered.push_str(&format!("{realm:?}"));
        assert!(!rendered.contains("secretBinding") && !rendered.contains("123456789"));
    }
}
