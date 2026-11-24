//! Opt-in, bounded Script execution with persistent lexical and `var` bindings.
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
//! Root lexical bindings and Script-scoped simple `var` declarations are
//! separate private records, not a global object. No host bindings or nested
//! lexical declarations. `undefined` is the sole intrinsic fallback. An exact Use Strict Directive
//! in the root prologue affects only its own Script. The host chooses which
//! checked Scripts to retain; the Realm does not own or cache them. No Script
//! results or binding handles leave the Realm.
//!
//! An allowed binding write copies a nonempty Script-local or temporary String
//! into the Realm. Each execution can copy up to 16,384 UTF-16 units; the Realm
//! retains up to 32,768 copied units over its lifetime, even after overwrite.
//! Aliasing an existing Realm String, storing a static `typeof` spelling, or
//! storing an empty String copies nothing. These ceilings are independent of
//! the existing per-String and evaluation ceilings.

use std::{collections::TryReserveError, fmt};

use crate::{
    Error, Span,
    parser::{self, MAX_STRING_LITERAL_CODE_UNITS},
};

use super::{
    BindingKind, Bindings, EvalError, EvalScratch, ExitStack, Instruction, Operand, StringOperand,
    TransientStrings, TypeName, Update, UpdateNumeric, UpdateResult, Value, dispatch, init_operand,
    new_stack, store_operand, update_number, update_result, validate_layout,
};

pub(crate) const MAX_NAME_BYTES: usize = 256;
pub(crate) const MAX_CODE_NAME_BYTES: usize = 16 * 1024;
pub(crate) const MAX_DECLARATIONS: usize = 256;
pub(crate) const MAX_SITES: usize = 4096;
const MAX_GLOBALS: usize = 512;
const PREFLIGHT_WORK: usize = 128 * 1024;
const MAX_COPIED_UNITS: usize = 16_384;
const MAX_RETAINED_UNITS: usize = 32_768;
const CHUNK_UNITS: usize = 256;
const _: () = assert!(MAX_RETAINED_UNITS <= u16::MAX as usize);
const _: () = assert!(MAX_RETAINED_UNITS.is_multiple_of(CHUNK_UNITS));
const _: () = assert!((MAX_RETAINED_UNITS / CHUNK_UNITS).is_power_of_two());
type Chunk = [u16; CHUNK_UNITS];

#[derive(Clone, Copy)]
pub(crate) enum Role {
    Declaration(BindingKind),
    Var { initialized: bool },
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeMode {
    Sloppy,
    Strict,
}

/// Immutable policy-compiled code with no source borrow or Realm dependency.
pub struct CompiledScript {
    pub(crate) instructions: Vec<Instruction>,
    pub(crate) string_units: Vec<u16>,
    pub(crate) max_stack_depth: usize,
    pub(crate) exit: ExitStack,
    pub(crate) mode: CodeMode,
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
    /// A declaration conflict within the Script or quoted `\8`/`\9` in strict code.
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
    /// A lexical or `var` declaration conflicts with the Realm's existing names.
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
    ImplicitGlobal {
        span: Span,
    },
    /// Kept for compatibility; String persistence no longer emits this fault.
    PersistentString {
        span: Span,
    },
    /// This execution would copy more than 16,384 UTF-16 units.
    StringCopyLimitExceeded {
        span: Span,
    },
    /// This Realm would retain more than 32,768 UTF-16 units.
    StringRetentionLimitExceeded {
        span: Span,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum StoredValue {
    Scalar(Value),
    TypeName(TypeName),
    Empty,
    Realm(RealmRange),
}

// Not a public handle: only successful appends issue offsets, and the private
// operand stack and cells never cross Realm boundaries. Reborrowing uses the
// owner's logical prefix, not initialized padding or allocator capacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RealmRange {
    start: u16,
    end: u16,
}

impl RealmRange {
    pub(super) fn get(self, retained: &[u16]) -> Option<&[u16]> {
        let (start, end) = (usize::from(self.start), usize::from(self.end));
        retained
            .get(start..end)
            .filter(|units| !units.is_empty() && units.len() <= MAX_STRING_LITERAL_CODE_UNITS)
    }
}

#[derive(Default)]
struct Arena {
    chunks: Vec<Chunk>,
    used: usize,
}

impl Arena {
    fn units(&self) -> &[u16] {
        &self.chunks.as_flattened()[..self.used]
    }

    #[inline(never)]
    fn append(
        &mut self,
        units: &[u16],
        copied: &mut usize,
        span: Span,
        reserve: impl FnOnce(&mut Vec<Chunk>, usize) -> Result<(), TryReserveError>,
    ) -> Result<StoredValue, Stop> {
        if units.is_empty() {
            return Ok(StoredValue::Empty);
        }
        if units.len() > MAX_STRING_LITERAL_CODE_UNITS {
            return Err(EvalError::InvalidScript.into());
        }
        let next_copied = copied
            .checked_add(units.len())
            .filter(|&total| total <= MAX_COPIED_UNITS)
            .ok_or(Stop::Engine(EngineFault::StringCopyLimitExceeded { span }))?;
        let end = self
            .used
            .checked_add(units.len())
            .filter(|&end| end <= MAX_RETAINED_UNITS)
            .ok_or(Stop::Engine(EngineFault::StringRetentionLimitExceeded {
                span,
            }))?;
        let needed = end.div_ceil(CHUNK_UNITS);
        let old = self.chunks.len();
        if old == 0 || needed > old.next_power_of_two() {
            // Follow logical power-of-two tiers even if the allocator supplies
            // extra capacity. At most 128 chunks (64 KiB) requested, at most
            // 64 old initialized chunks moved at once, 1+2+...+64 = 127
            // across the lifetime (thus <=32,512 old units in any execute).
            // Requested old+new overlap is <=96 KiB, or <=160 KiB with
            // live scratch backing, excluding allocator overhead/rounding.
            reserve(&mut self.chunks, needed.next_power_of_two() - old)
                .map_err(|_| EvalError::AllocationFailed)?;
        }
        // Only initialize reached chunks, at most 16 here and 128 over the
        // Realm lifetime. There are no fallible steps
        // from here through publication of the new logical length and cell.
        assert!(self.chunks.capacity() >= needed);
        while self.chunks.len() < needed {
            self.chunks.push([0; CHUNK_UNITS]);
        }
        let start = self.used;
        self.chunks.as_flattened_mut()[start..end].copy_from_slice(units);
        self.used = end;
        *copied = next_copied;
        Ok(StoredValue::Realm(RealmRange {
            start: start as u16,
            end: end as u16,
        }))
    }
}

struct Cell {
    name: Vec<u8>,
    kind: BindingKind,
    // No Script or scratch references. None is the TDZ, not undefined.
    value: Option<StoredValue>,
}

struct VarCell {
    name: Vec<u8>,
    value: StoredValue,
}

#[derive(Default)]
struct ObjectBindings {
    // All dynamic rows are declared vars; the readonly intrinsic is present
    // even before a Script declares it. Neither is exposed as a property.
    vars: Vec<VarCell>,
    undefined_declared: bool,
}

/// Owns two private binding records. No global object, handles, callbacks, or reentry.
#[derive(Default)]
pub struct Realm {
    globals: Vec<Cell>,
    object: ObjectBindings,
    strings: Arena,
}

impl Realm {
    pub fn new() -> Self {
        Self::default()
    }

    /// Preflight and atomically install new lexical cells in TDZ and new `var`
    /// cells initialized to `undefined`. A failed instantiation changes no
    /// bindings. Missing or uninitialized uses are not rejected by this check.
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
        let lexical_base = self.globals.len();
        let var_base = self.object.vars.len();
        let (declarations, accesses) = code
            .sites
            .split_at_checked(code.declarations)
            .ok_or(InvalidScript)?;
        if declarations.len() > MAX_DECLARATIONS || code.sites.len() > MAX_SITES {
            return Err(InvalidScript);
        }
        // GlobalDeclarationInstantiation: lexical names first, checking
        // declared vars, existing lexicals, then restricted names per name.
        // The only restricted name is the intrinsic, even when undeclared.
        let mut lexical_count = 0;
        for site in declarations {
            work.charge(1)?;
            match site.role {
                Role::Declaration(_) => lexical_count += 1,
                Role::Var { .. } => continue,
                _ => return Err(InvalidScript),
            }
            let name = code.name(site)?;
            let seed = self.object.undefined_declared;
            if (seed && work.equal(name, b"undefined")?)
                || work
                    .find(name, self.object.vars.iter().map(|c| c.name.as_slice()))?
                    .is_some()
                || work
                    .find(name, self.globals.iter().map(|c| c.name.as_slice()))?
                    .is_some()
                || (!seed && work.equal(name, b"undefined")?)
            {
                return Err(SyntaxError { span: site.span });
            }
        }
        let var_count = declarations.len() - lexical_count;
        if var_count != 0 {
            for site in declarations {
                work.charge(1)?;
                if matches!(site.role, Role::Var { .. })
                    && work
                        .find(
                            code.name(site)?,
                            self.globals.iter().map(|c| c.name.as_slice()),
                        )?
                        .is_some()
                {
                    return Err(SyntaxError { span: site.span });
                }
            }
        }
        // All absent names are declarable, and existing object-side names
        // (including readonly undefined) are reused. Dedup installation only.
        let available = MAX_GLOBALS - lexical_base - var_base;
        if lexical_count > available {
            return Err(BindingLimitExceeded);
        }
        let mut lexicals = Vec::<Cell>::new();
        let mut vars = Vec::<VarCell>::new();
        let mut declare_undefined = false;
        lexicals
            .try_reserve_exact(lexical_count)
            .map_err(|_| AllocationFailed)?;
        vars.try_reserve_exact(var_count.min(available - lexical_count))
            .map_err(|_| AllocationFailed)?;
        let mut links = Vec::new();
        links
            .try_reserve_exact(code.sites.len())
            .map_err(|_| AllocationFailed)?;
        for site in declarations {
            let name = code.name(site)?;
            let target = match site.role {
                Role::Declaration(kind) => {
                    let slot = lexical_base + lexicals.len();
                    // Name copy + staging, link, and publication writes.
                    work.charge(name.len() + 3)?;
                    lexicals.push(Cell {
                        name: owned_name(name)?,
                        kind,
                        value: None,
                    });
                    Target::Lexical(slot)
                }
                Role::Var { .. } => {
                    work.charge(1)?; // Link, even on reuse.
                    if work.equal(name, b"undefined")? {
                        if !self.object.undefined_declared && !declare_undefined {
                            work.charge(2)?; // Stage and publish membership.
                            declare_undefined = true;
                        }
                        Target::Undefined
                    } else if let Some(slot) = work.find(
                        name,
                        self.object
                            .vars
                            .iter()
                            .chain(&vars)
                            .map(|c| c.name.as_slice()),
                    )? {
                        Target::Var(slot)
                    } else {
                        if vars.len() == available - lexical_count {
                            return Err(BindingLimitExceeded);
                        }
                        let slot = var_base + vars.len();
                        work.charge(name.len() + 2)?; // Copy, stage and publish.
                        vars.push(VarCell {
                            name: owned_name(name)?,
                            value: StoredValue::Scalar(Value::Undefined),
                        });
                        Target::Var(slot)
                    }
                }
                _ => return Err(InvalidScript),
            };
            links.push(target);
        }
        for site in accesses {
            work.charge(1)?;
            if !matches!(site.role, Role::Read | Role::Typeof | Role::Write) {
                return Err(InvalidScript);
            }
            let name = code.name(site)?;
            let target = if let Some(slot) = work.find(
                name,
                self.globals
                    .iter()
                    .chain(&lexicals)
                    .map(|c| c.name.as_slice()),
            )? {
                Target::Lexical(slot)
            } else if work.equal(name, b"undefined")? {
                Target::Undefined
            } else if let Some(slot) = work.find(
                name,
                self.object
                    .vars
                    .iter()
                    .chain(&vars)
                    .map(|c| c.name.as_slice()),
            )? {
                Target::Var(slot)
            } else {
                Target::Missing
            };
            links.push(target);
        }

        // Reserve both records before publishing either. Prepare new backing
        // separately: failed plans cannot leave speculative capacity behind
        // in one record that would weaken the joint geometric bound.
        let lexical_growth = prepare_growth(&self.globals, lexicals.len(), &mut work)?;
        let var_growth = prepare_growth(&self.object.vars, vars.len(), &mut work)?;
        commit_rows(&mut self.globals, lexicals, lexical_growth);
        commit_rows(&mut self.object.vars, vars, var_growth);
        if declare_undefined {
            self.object.undefined_declared = true;
        }
        // Ready's exclusive borrow keeps these indices and misses valid
        // until this execution finishes or is abandoned.
        Ok(ReadyScript {
            realm: self,
            code,
            links,
        })
    }
}

fn owned_name(name: &[u8]) -> Result<Vec<u8>, InstantiateError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(name.len())
        .map_err(|_| InstantiateError::AllocationFailed)?;
    owned.extend_from_slice(name);
    Ok(owned)
}

fn prepare_growth<T>(
    rows: &Vec<T>,
    additional: usize,
    work: &mut Work,
) -> Result<Option<Vec<T>>, InstantiateError> {
    // With monotonic lengths totaling at most 512, the two power-of-two
    // tiers total at most 768 rows. Including old backing if both grow,
    // the peak is at most 1152 requested rows, plus <=256 pending rows.
    let total = rows.len() + additional;
    if total > rows.capacity() {
        work.charge(rows.len())?; // Existing rows move, not their owned names.
        let mut destination = Vec::new();
        destination
            .try_reserve_exact(total.next_power_of_two())
            .map_err(|_| InstantiateError::AllocationFailed)?;
        return Ok(Some(destination));
    }
    Ok(None)
}

fn commit_rows<T>(rows: &mut Vec<T>, staged: Vec<T>, growth: Option<Vec<T>>) {
    if let Some(mut destination) = growth {
        assert!(destination.capacity() >= rows.len() + staged.len());
        destination.append(rows);
        *rows = destination;
    }
    assert!(rows.capacity() - rows.len() >= staged.len());
    rows.extend(staged);
}

impl fmt::Debug for Realm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Realm")
            .field("bindings", &(self.globals.len() + self.object.vars.len()))
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

    fn find<'a>(
        &mut self,
        name: &[u8],
        names: impl Iterator<Item = &'a [u8]>,
    ) -> Result<Option<usize>, InstantiateError> {
        for (index, candidate) in names.enumerate() {
            if self.equal(name, candidate)? {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }
}

#[derive(Clone, Copy)]
enum Target {
    Lexical(usize),
    Var(usize),
    Undefined,
    Missing,
}

/// A one-shot execution guard. Dropping the guard abandons the body, but
/// leaves new lexical bindings in TDZ and new `var` bindings `undefined`.
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
#[must_use = "dropping an instantiated Script leaves its declarations installed"]
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
        let mut execution = self.execution();
        match dispatch(
            &code.instructions,
            stack,
            code.max_stack_depth,
            &code.string_units,
            code.exit,
            &mut scratch,
            &mut execution,
        ) {
            Ok(_) => Ok(Completion::Normal),
            Err(Stop::Throw(error)) => Ok(Completion::Throw(error)),
            Err(Stop::Engine(error)) => Err(error),
        }
    }

    fn execution(&mut self) -> Execution<'_, '_> {
        Execution {
            realm: self.realm,
            sites: &self.code.sites,
            links: &self.links,
            mode: self.code.mode,
            copied: 0,
        }
    }
}

struct Execution<'realm, 'code> {
    realm: &'realm mut Realm,
    sites: &'code [Site],
    links: &'code [Target],
    mode: CodeMode,
    copied: usize,
}

impl Execution<'_, '_> {
    fn site(&self, index: usize) -> Result<(Target, Span, Role), Stop> {
        let site = self.sites.get(index).ok_or(EvalError::InvalidScript)?;
        let target = *self.links.get(index).ok_or(EvalError::InvalidScript)?;
        Ok((target, site.span, site.role))
    }

    fn write_target(&self, index: usize) -> Result<(Target, Span), Stop> {
        let (target, span, Role::Write) = self.site(index)? else {
            return Err(EvalError::InvalidScript.into());
        };
        Ok((target, span))
    }

    fn load_target(
        &self,
        target: Target,
        span: Span,
        direct_typeof: bool,
    ) -> Result<Operand, Stop> {
        let value = match target {
            Target::Undefined => StoredValue::Scalar(Value::Undefined),
            Target::Missing if direct_typeof => StoredValue::Scalar(Value::Undefined),
            Target::Missing => return Err(Stop::Throw(Throw::ReferenceError { span })),
            Target::Lexical(slot) => self
                .realm
                .globals
                .get(slot)
                .ok_or(EvalError::InvalidScript)?
                .value
                .ok_or(Stop::Throw(Throw::ReferenceError { span }))?,
            Target::Var(slot) => {
                self.realm
                    .object
                    .vars
                    .get(slot)
                    .ok_or(EvalError::InvalidScript)?
                    .value
            }
        };
        Ok(match value {
            StoredValue::Scalar(value) => Operand::Scalar(value),
            StoredValue::TypeName(name) => Operand::String(StringOperand::TypeName(name)),
            StoredValue::Empty => Operand::String(StringOperand::Empty),
            StoredValue::Realm(range) => {
                range
                    .get(self.retained_units())
                    .ok_or(EvalError::InvalidScript)?;
                Operand::String(StringOperand::Realm(range))
            }
        })
    }

    fn put(
        &mut self,
        target: Target,
        span: Span,
        value: Operand,
        source: TransientStrings<'_>,
    ) -> Result<(), Stop> {
        let destination = match target {
            // Only reached writes get here, after RHS evaluation/coercion.
            Target::Undefined if self.mode == CodeMode::Strict => {
                return Err(Stop::Throw(Throw::TypeError { span }));
            }
            Target::Undefined => return Ok(()), // Sloppy readonly write, even with a String RHS.
            Target::Missing if self.mode == CodeMode::Strict => {
                return Err(Stop::Throw(Throw::ReferenceError { span }));
            }
            Target::Missing => return Err(Stop::Engine(EngineFault::ImplicitGlobal { span })),
            Target::Lexical(slot) => {
                let cell = self
                    .realm
                    .globals
                    .get_mut(slot)
                    .ok_or(EvalError::InvalidScript)?;
                let destination = cell
                    .value
                    .as_mut()
                    .ok_or(Stop::Throw(Throw::ReferenceError { span }))?;
                if cell.kind == BindingKind::Const {
                    return Err(Stop::Throw(Throw::TypeError { span }));
                }
                destination
            }
            Target::Var(slot) => {
                &mut self
                    .realm
                    .object
                    .vars
                    .get_mut(slot)
                    .ok_or(EvalError::InvalidScript)?
                    .value
            }
        };
        *destination = escape(
            value,
            span,
            source,
            &mut self.realm.strings,
            &mut self.copied,
        )?;
        Ok(())
    }
}

fn escape(
    value: Operand,
    span: Span,
    source: TransientStrings<'_>,
    strings: &mut Arena,
    copied: &mut usize,
) -> Result<StoredValue, Stop> {
    let units = match value {
        Operand::Scalar(value) => return Ok(StoredValue::Scalar(value)),
        Operand::String(StringOperand::TypeName(name)) => return Ok(StoredValue::TypeName(name)),
        Operand::String(StringOperand::Empty) => return Ok(StoredValue::Empty),
        Operand::String(StringOperand::Realm(range)) => {
            range.get(strings.units()).ok_or(EvalError::InvalidScript)?;
            return Ok(StoredValue::Realm(range));
        }
        Operand::String(StringOperand::Literal(range)) => source.literal(range),
        Operand::String(StringOperand::Runtime(range)) => source.runtime(range),
    }
    .ok_or(EvalError::InvalidScript)?;
    strings.append(units, copied, span, Vec::try_reserve_exact)
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

impl Bindings for Execution<'_, '_> {
    type Error = Stop;

    fn retained_units(&self) -> &[u16] {
        self.realm.strings.units()
    }

    fn load(&self, index: usize) -> Result<Operand, Stop> {
        let (target, span, role) = self.site(index)?;
        match role {
            Role::Declaration(_) | Role::Var { .. } => Err(EvalError::InvalidScript.into()),
            // Compound assignments share a Write site's load and store.
            Role::Read | Role::Write => self.load_target(target, span, false),
            Role::Typeof => self.load_target(target, span, true),
        }
    }

    fn init(
        &mut self,
        index: usize,
        kind: BindingKind,
        _: usize,
        stack: &mut Vec<Operand>,
        source: TransientStrings<'_>,
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
        cell.value = Some(escape(
            init_operand(stack)?,
            span,
            source,
            &mut self.realm.strings,
            &mut self.copied,
        )?);
        Ok(())
    }

    fn store(
        &mut self,
        index: usize,
        keep: bool,
        stack: &mut Vec<Operand>,
        source: TransientStrings<'_>,
    ) -> Result<(), Stop> {
        let value = store_operand(stack, keep)?;
        let (target, span, role) = self.site(index)?;
        match (role, target) {
            (Role::Write, _) => self.put(target, span, value, source),
            (Role::Var { initialized: true }, Target::Var(_) | Target::Undefined) if !keep => {
                self.put(target, span, value, source)
            }
            _ => Err(EvalError::InvalidScript.into()),
        }
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
        let (target, span) = self.write_target(index)?;
        let old = self.load_target(target, span, false)?.to_number(
            numeric.source.with_retained(self.retained_units()),
            numeric.credits,
        )?;
        let new = update_number(op, old);
        update_result(stack, max, result, old, new)?;
        // The checked push must succeed before the cell changes. If PutValue
        // fails, remove the staged result; dispatch discards the frame on Stop.
        if let Err(stop) = self.put(
            target,
            span,
            Operand::Scalar(Value::Number(new)),
            numeric.source,
        ) {
            stack.pop();
            return Err(stop);
        }
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
        assert_eq!(
            realm.globals[0].value,
            Some(StoredValue::Scalar(Value::Undefined))
        );
    }

    #[test]
    fn late_mixed_preflight_failure_publishes_neither_record_nor_seed() {
        let mut realm = Realm::new();
        assert_eq!(
            run(&mut realm, "let l = 1; var x = 2;"),
            Ok(Completion::Normal)
        );
        let code = CompiledScript::compile("var undefined, y, y; let z; missing;").unwrap();
        // Lexical and var conflict passes (9 + 9), staging/linking declarations
        // (13 + 7 + 6 + 4), missing link (6), and one old row for each
        // record that must grow. Allocation may supply excess capacity.
        let old_capacities = (realm.globals.capacity(), realm.object.vars.capacity());
        let exact_work = 54 + usize::from(old_capacities.0 < 2) + usize::from(old_capacities.1 < 2);
        assert_eq!(
            realm
                .instantiate_with_work(&code, exact_work - 1)
                .unwrap_err(),
            InstantiateError::WorkLimitExceeded
        );
        assert_eq!(
            (realm.globals.capacity(), realm.object.vars.capacity()),
            old_capacities
        );
        assert_eq!((realm.globals.len(), realm.object.vars.len()), (1, 1));
        assert!(!realm.object.undefined_declared);
        assert_eq!(
            realm.globals[0].value,
            Some(StoredValue::Scalar(Value::Number(1.0)))
        );
        assert_eq!(
            realm.object.vars[0].value,
            StoredValue::Scalar(Value::Number(2.0))
        );
        drop(realm.instantiate_with_work(&code, exact_work).unwrap());
        assert!(realm.object.undefined_declared);
        assert_eq!((realm.globals.len(), realm.object.vars.len()), (2, 2));
        assert_eq!(realm.globals[1].value, None);
        assert_eq!(
            realm.object.vars[1].value,
            StoredValue::Scalar(Value::Undefined)
        );
        assert_eq!(
            realm.object.vars[0].value,
            StoredValue::Scalar(Value::Number(2.0))
        );
    }

    #[test]
    fn var_without_initializer_has_no_operations_or_stack() {
        let code = CompiledScript::compile("var a, a, undefined;").unwrap();
        assert!(code.instructions.is_empty());
        assert_eq!(code.max_stack_depth, 0);
        assert_eq!(code.declarations, 3);
        let mut realm = Realm::new();
        assert_eq!(
            realm.instantiate(&code).unwrap().execute(),
            Ok(Completion::Normal)
        );
        assert!(realm.object.undefined_declared);
        assert_eq!(realm.object.vars.len(), 1);
    }

    #[test]
    fn var_sites_authenticate_initializer_capability_and_target() {
        let code =
            CompiledScript::compile("var zero, written = 7, undefined = 1; let lexical;").unwrap();
        let mut realm = Realm::new();
        let mut ready = realm.instantiate(&code).unwrap();
        let scratch = EvalScratch::new();
        let mut execution = ready.execution();
        let invalid = |result| {
            assert!(matches!(
                result,
                Err(Stop::Engine(EngineFault::Evaluation(
                    EvalError::InvalidScript
                )))
            ))
        };
        let operand = Operand::Scalar(Value::Number(7.0));
        let mut stack = vec![operand];
        let mut credits = 0;
        for site in 0..3 {
            assert!(matches!(
                execution.load(site),
                Err(Stop::Engine(EngineFault::Evaluation(
                    EvalError::InvalidScript
                )))
            ));
            invalid(execution.update(
                site,
                Update::Increment,
                UpdateResult::New,
                &mut stack,
                2,
                UpdateNumeric {
                    source: scratch.transient(&[]),
                    credits: &mut credits,
                },
            ));
            invalid(execution.init(
                site,
                BindingKind::Let,
                0,
                &mut stack,
                scratch.transient(&[]),
            ));
        }
        invalid(execution.store(0, false, &mut vec![operand], scratch.transient(&[])));
        invalid(execution.store(1, true, &mut vec![operand], scratch.transient(&[])));
        for target in [Target::Lexical(0), Target::Missing, Target::Var(usize::MAX)] {
            let mut wrong = ready.links.clone();
            wrong[1] = target;
            let mut execution = ready.execution();
            execution.links = &wrong;
            invalid(execution.store(1, false, &mut vec![operand], scratch.transient(&[])));
        }
        assert_eq!(
            ready.realm.object.vars[0].value,
            StoredValue::Scalar(Value::Undefined)
        );
        assert_eq!(
            ready.realm.object.vars[1].value,
            StoredValue::Scalar(Value::Undefined)
        );
    }

    #[test]
    fn site_bound_is_checked_even_on_zero_op_code() {
        // The grammar's token cap is tighter than its site cap for present
        // source forms. Check the defensive linker ceiling directly.
        let mut code = CompiledScript::compile("var x;").unwrap();
        code.sites.resize_with(MAX_SITES, || Site {
            span: Span { start: 4, end: 5 },
            start: 0,
            end: 1,
            role: Role::Read,
        });
        let mut realm = Realm::new();
        let ready = realm.instantiate(&code).unwrap();
        assert_eq!(ready.links.len(), MAX_SITES);
        assert!(ready.links.iter().all(|t| matches!(t, Target::Var(0))));
        drop(ready);
        code.sites.push(Site {
            span: Span { start: 4, end: 5 },
            start: 0,
            end: 1,
            role: Role::Read,
        });
        assert_eq!(
            realm.instantiate(&code).unwrap_err(),
            InstantiateError::InvalidScript
        );
    }

    #[test]
    fn binding_operations_reject_wrong_roles_before_a_guest_read() {
        let code = CompiledScript::compile("let lexical = 7; missing;").unwrap();
        let mut realm = Realm::new();
        let mut ready = realm.instantiate(&code).unwrap();
        let mut execution = ready.execution();
        // A declaration site is not a read, even though its TDZ cell exists.
        assert!(matches!(
            execution.load(0),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::InvalidScript
            )))
        ));
        let mut stack = Vec::new();
        let scratch = EvalScratch::new();
        let mut credits = 0;
        // Using an ordinary missing Read site as an update is invalid, not
        // a guest ReferenceError from GetValue.
        assert!(matches!(
            execution.update(
                1,
                Update::Increment,
                UpdateResult::New,
                &mut stack,
                1,
                UpdateNumeric {
                    source: scratch.transient(&[]),
                    credits: &mut credits
                }
            ),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::InvalidScript
            )))
        ));
        assert!(stack.is_empty());
        assert_eq!(execution.realm.globals[0].value, None);
    }

    #[test]
    fn update_checks_stack_before_put_and_discards_result_on_throw() {
        let mut realm = Realm::new();
        let seed = CompiledScript::compile("let mutable = 7; const fixed = 3;").unwrap();
        assert_eq!(
            realm.instantiate(&seed).unwrap().execute(),
            Ok(Completion::Normal)
        );
        let code = CompiledScript::compile("(mutable++); (fixed++);").unwrap();
        let mut ready = realm.instantiate(&code).unwrap();
        let mut execution = ready.execution();
        assert!(matches!(
            execution.load(0),
            Ok(Operand::Scalar(Value::Number(7.0)))
        ));
        let mut stack = vec![Operand::Scalar(Value::Null)];
        let scratch = EvalScratch::new();
        let mut credits = 0;
        let mut update = |site, max| {
            execution.update(
                site,
                Update::Increment,
                UpdateResult::New,
                &mut stack,
                max,
                UpdateNumeric {
                    source: scratch.transient(&[]),
                    credits: &mut credits,
                },
            )
        };
        for site in [0, 1] {
            assert!(matches!(
                update(site, 1),
                Err(Stop::Engine(EngineFault::Evaluation(
                    EvalError::InvalidScript
                )))
            ));
        }
        assert!(matches!(
            update(1, 2),
            Err(Stop::Throw(Throw::TypeError { .. }))
        ));
        assert!(matches!(stack.as_slice(), [Operand::Scalar(Value::Null)]));
        assert_eq!(
            execution.realm.globals[0].value,
            Some(StoredValue::Scalar(Value::Number(7.0)))
        );
        assert_eq!(
            execution.realm.globals[1].value,
            Some(StoredValue::Scalar(Value::Number(3.0)))
        );
    }

    fn run(realm: &mut Realm, source: &str) -> Result<Completion, EngineFault> {
        let code = CompiledScript::compile(source).unwrap();
        realm.instantiate(&code).unwrap().execute()
    }

    #[test]
    fn only_accepted_nonempty_escapes_allocate_and_aliases_keep_their_range() {
        let mut realm = Realm::new();
        assert_eq!(realm.strings.chunks.capacity(), 0);
        for source in [
            "let scalar = 1, empty = '', runtime_empty = '' + '', static_name = typeof 0;",
            "'unretained' + 'scratch'; false && (scalar = 'skipped');",
            "undefined = 'ignored' + 'scratch'; const fixed = 1;",
        ] {
            assert_eq!(run(&mut realm, source), Ok(Completion::Normal));
        }
        for source in [
            "fixed = 'rejected' + 'const';",
            "tdz = `rejected${1}`; let tdz;",
        ] {
            assert!(matches!(run(&mut realm, source), Ok(Completion::Throw(_))));
        }
        assert!(matches!(
            run(&mut realm, "missing = 'rejected';"),
            Err(EngineFault::ImplicitGlobal { .. })
        ));
        assert_eq!(realm.strings.used, 0);
        assert_eq!(realm.strings.chunks.capacity(), 0);

        assert_eq!(
            run(&mut realm, r"let first = '\uD800\0'; let alias = first;"),
            Ok(Completion::Normal)
        );
        let first = realm
            .globals
            .iter()
            .find(|c| c.name == b"first")
            .unwrap()
            .value;
        let alias = realm
            .globals
            .iter()
            .find(|c| c.name == b"alias")
            .unwrap()
            .value;
        assert_eq!(first, alias);
        assert_eq!(realm.strings.units(), [0xD800, 0]);
        assert_eq!(realm.strings.chunks.len(), 1);
    }

    #[test]
    fn invalid_ranges_cannot_enter_or_leave_a_lexical_cell() {
        let mut realm = Realm::new();
        assert_eq!(run(&mut realm, "let x = 'x';"), Ok(Completion::Normal));
        assert_eq!(realm.strings.chunks.len(), 1);
        let original = realm.globals[0].value;
        let code = CompiledScript::compile("x; 'y';").unwrap();
        let mut ready = realm.instantiate(&code).unwrap();
        let mut execution = ready.execution();
        let span = Span { start: 0, end: 1 };
        let mut scratch = EvalScratch::new();
        let Instruction::String(literal) = code.instructions[2] else {
            panic!("expected literal");
        };
        let literal = StringOperand::Literal(literal);
        let stale_scratch = scratch
            .concatenate(
                super::super::ExternalStrings {
                    literals: &code.string_units,
                    retained: &[],
                },
                literal,
                literal,
            )
            .unwrap();
        scratch.reset();
        for invalid in [
            literal,
            stale_scratch,
            StringOperand::Realm(RealmRange { start: 0, end: 2 }),
        ] {
            let result = execution.put(
                Target::Lexical(0),
                span,
                Operand::String(invalid),
                scratch.transient(&[]),
            );
            assert!(matches!(
                result,
                Err(Stop::Engine(EngineFault::Evaluation(
                    EvalError::InvalidScript
                )))
            ));
            assert_eq!(execution.realm.globals[0].value, original);
            assert_eq!(execution.realm.strings.units(), [b'x' as u16]);
            assert_eq!(execution.copied, 0);
        }
        for range in [
            RealmRange { start: 0, end: 2 },
            RealmRange { start: 1, end: 0 },
            RealmRange { start: 1, end: 1 },
        ] {
            // Even a metadata-only use (such as truthiness) cannot get an
            // operand with corrupt offsets into padding, reversed, or empty.
            execution.realm.globals[0].value = Some(StoredValue::Realm(range));
            assert!(matches!(
                execution.load(0),
                Err(Stop::Engine(EngineFault::Evaluation(
                    EvalError::InvalidScript
                )))
            ));
        }
    }

    #[test]
    fn arena_reservation_failure_is_atomic_and_geometric_tiers_bound_old_moves() {
        let mut arena = Arena::default();
        let span = Span { start: 0, end: 1 };
        let mut copied = 0;
        let failed = |chunks: &mut Vec<Chunk>, _| chunks.try_reserve_exact(usize::MAX);
        assert!(matches!(
            arena.append(&[7], &mut copied, span, failed),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::AllocationFailed
            )))
        ));
        assert_eq!(
            (
                arena.used,
                arena.chunks.len(),
                arena.chunks.capacity(),
                copied
            ),
            (0, 0, 0, 0)
        );
        let mut tiers = Vec::new();
        let mut relocated = 0;
        for i in 0..MAX_RETAINED_UNITS {
            if i == MAX_COPIED_UNITS {
                copied = 0; // Model the second execute.
            }
            if i == CHUNK_UNITS {
                let capacity = arena.chunks.capacity();
                assert!(matches!(
                    arena.append(&[8], &mut copied, span, failed),
                    Err(Stop::Engine(EngineFault::Evaluation(
                        EvalError::AllocationFailed
                    )))
                ));
                assert_eq!(
                    (
                        arena.used,
                        arena.chunks.len(),
                        arena.chunks.capacity(),
                        copied
                    ),
                    (i, 1, capacity, i)
                );
                assert_eq!(arena.units(), [7; CHUNK_UNITS]);
            }
            assert!(
                arena
                    .append(&[7], &mut copied, span, |chunks, additional| {
                        tiers.push(chunks.len() + additional);
                        relocated += chunks.len() * CHUNK_UNITS;
                        chunks.try_reserve_exact(additional)
                    })
                    .is_ok()
            );
        }
        assert_eq!(tiers, [1, 2, 4, 8, 16, 32, 64, 128]);
        assert_eq!(relocated, 32_512);
        assert_eq!(arena.used, MAX_RETAINED_UNITS);
        assert_eq!(arena.units()[MAX_RETAINED_UNITS - 1], 7);
    }

    #[test]
    fn const_update_converts_a_retained_string_before_const_put_rejection() {
        let mut realm = Realm::new();
        assert_eq!(
            run(&mut realm, "const fixed = '42';"),
            Ok(Completion::Normal)
        );
        let code = CompiledScript::compile("fixed++; ").unwrap();
        let mut ready = realm.instantiate(&code).unwrap();
        let mut execution = ready.execution();
        let mut stack = Vec::new();
        let scratch = EvalScratch::new();
        let mut credits = 0;
        assert!(matches!(
            execution.update(
                0,
                Update::Increment,
                UpdateResult::New,
                &mut stack,
                1,
                UpdateNumeric {
                    source: scratch.transient(&[]),
                    credits: &mut credits
                }
            ),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::StringNumericConversionLimitExceeded
            )))
        ));
        assert!(stack.is_empty());
        assert_eq!(execution.copied, 0);
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
