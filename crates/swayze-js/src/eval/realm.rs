//! Opt-in, bounded Script execution with persistent bindings and ordinary calls.
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
//! lexical declarations. `undefined` is the sole intrinsic fallback. Ordinary
//! functions have mutable simple formals and frame-local `var` bindings,
//! live Realm free names, and retain the raw receiver for a member call (there
//! is no `this` expression). Strictness comes from their definition, not the caller. The Realm retains immutable bodies for installed function identities
//! even if the original compiled Script is dropped. No Script results or
//! binding handles leave the Realm.
//!
//! An allowed binding or field write copies a nonempty Script-local or temporary String
//! into the Realm. Each execution can copy up to 16,384 UTF-16 units; the Realm
//! retains up to 32,768 copied units over its lifetime, even after overwrite.
//! New field keys share these limits. Existing Realm strings, checked code
//! names, static spellings and empty strings need no copy. These ceilings are independent of
//! the existing per-String and evaluation ceilings.

use std::{collections::TryReserveError, fmt};

use crate::{
    Error, Span,
    parser::{self, MAX_STRING_LITERAL_CODE_UNITS},
};

use super::{
    BindingKind, Bindings, CallTarget, Conversion, DotSite, EvalError, EvalScratch, ExitStack,
    Frame, Instruction, Operand, Properties, Stack, StaticText, StringOperand, TransientStrings,
    Update, UpdateNumeric, UpdateResult, Value, dispatch, init_operand, new_stack, store_operand,
    update_number, update_result, validate_body, validate_layout, validate_property_shape,
};

mod object_heap;
#[cfg(test)]
#[path = "realm/property_tests.rs"]
mod property_tests;
use object_heap::{
    Creation, Descriptor, FunctionRecord, HEAP_REQUEST_BYTES, Heap, Lookup, PROPERTY_WORK,
};
pub(super) use object_heap::{Intrinsic, ObjectId};

pub(crate) const MAX_NAME_BYTES: usize = 256;
pub(crate) const MAX_CODE_NAME_BYTES: usize = 16 * 1024;
pub(crate) const MAX_DECLARATIONS: usize = 256;
pub(crate) const MAX_SITES: usize = 4096;
pub(crate) const MAX_DOT_SITES: usize = 4096;
pub(crate) const MAX_DOT_KEY_UNITS: usize = 256;
pub(crate) const MAX_DOT_CODE_UNITS: usize = 16 * 1024;
pub(crate) const DOT_ROOT: u16 = u16::MAX;

// Shared with Realm lowering. A dot instruction's index selects exactly one
// occurrence, independently of the identifier/binding access table.
#[derive(Clone, Copy)]
pub(crate) struct DotOccurrence {
    pub(crate) owner: u16, // DOT_ROOT or the actual body index
    pub(crate) key_start: u16,
    pub(crate) key_end: u16,
    pub(crate) dot_start: u32,
    pub(crate) dot_end: u32,
}

const _: () = assert!(std::mem::size_of::<DotOccurrence>() == 16);

#[derive(Default)]
pub(crate) struct DotCode {
    pub(crate) source_len: u32,
    pub(crate) sites: Vec<DotOccurrence>,
    pub(crate) keys: Vec<u16>,
}

impl DotCode {
    fn occurrence(&self, owner: u16, index: u16) -> Option<(&[u16], Span)> {
        let site = self.sites.get(usize::from(index))?;
        if site.owner != owner
            || site.dot_end.checked_sub(site.dot_start) != Some(1)
            || site.dot_end > self.source_len
        {
            return None;
        }
        let key = self
            .keys
            .get(usize::from(site.key_start)..usize::from(site.key_end))?;
        if key.is_empty() || key.len() > MAX_DOT_KEY_UNITS {
            return None;
        }
        Some((
            key,
            Span {
                start: site.dot_start as usize,
                end: site.dot_end as usize,
            },
        ))
    }

    fn validate(&self, bodies: usize, work: &mut Work) -> Result<(), InstantiateError> {
        use InstantiateError::InvalidScript;
        if self.sites.len() > MAX_DOT_SITES || self.keys.len() > MAX_DOT_CODE_UNITS {
            return Err(InvalidScript);
        }
        work.charge(self.sites.len() + self.keys.len())?;
        let mut frontier = 0;
        for (index, site) in self.sites.iter().enumerate() {
            if site.owner != DOT_ROOT && usize::from(site.owner) >= bodies {
                return Err(InvalidScript);
            }
            let (key, _) = self
                .occurrence(site.owner, index as u16)
                .ok_or(InvalidScript)?;
            if usize::from(site.key_start) != frontier || !identifier_name(key) {
                return Err(InvalidScript);
            }
            frontier = usize::from(site.key_end);
        }
        if frontier != self.keys.len() {
            return Err(InvalidScript);
        }
        Ok(())
    }

    fn validate_ops(
        &self,
        owner: u16,
        ops: &[Instruction],
        peak: usize,
        exit: ExitStack,
        work: &mut Work,
    ) -> Result<(), InstantiateError> {
        let remaining = std::cell::Cell::new(*work);
        let charge = |n| {
            let mut ledger = remaining.get();
            ledger.charge(n)?;
            remaining.set(ledger);
            Ok(())
        };
        validate_property_shape(ops, peak, exit, charge, |index| {
            charge(1)?;
            self.occurrence(owner, index)
                .ok_or(InstantiateError::InvalidScript)?;
            Ok::<(), InstantiateError>(())
        })?;
        *work = remaining.get();
        Ok(())
    }
}

fn identifier_name(key: &[u16]) -> bool {
    let start =
        |u| u8::try_from(u).is_ok_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'$'));
    key.split_first().is_some_and(|(&head, tail)| {
        start(head) && tail.iter().all(|&u| start(u) || (48..=57).contains(&u))
    })
}

fn property_op(op: Instruction) -> bool {
    match op {
        Instruction::GetDot(_)
        | Instruction::FormDot(_)
        | Instruction::PutDot(_)
        | Instruction::PrepareDotCall(_) => true,
        Instruction::Call { member, .. } => member != super::BARE_CALL,
        _ => false,
    }
}

impl From<EvalError> for InstantiateError {
    fn from(error: EvalError) -> Self {
        if error == EvalError::AllocationFailed {
            Self::AllocationFailed
        } else {
            Self::InvalidScript
        }
    }
}

pub(crate) const MAX_PARAMETERS: u8 = 64;
pub(crate) const MAX_FRAME_LOCALS: u8 = 128;
const MAX_ARTIFACTS: usize = 16;
const MAX_ARTIFACT_BYTES: usize = 512 * 1024;
const MAX_FUNCTIONS: usize = 512;
const LOOKUP_WORK: usize = 128 * 1024;
const MAX_GLOBALS: usize = 512;
// One Script-instantiation budget: callable imports pay for name provenance
// and selected initial object/descriptor construction here, not on first call.
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
    Function { body: u16 },
    Local,
    Read,
    Typeof,
    Write,
}

#[derive(Clone, Copy)]
pub(crate) struct Site {
    pub(crate) span: Span,
    pub(crate) start: u16,
    pub(crate) end: u16,
    pub(crate) role: Role,
}

impl Site {
    fn access_name(self, names: &[u8]) -> Option<&[u8]> {
        if !matches!(self.role, Role::Read | Role::Typeof | Role::Write) {
            return None;
        }
        self.name_in(names)
    }

    fn name_in(self, names: &[u8]) -> Option<&[u8]> {
        let name = names.get(usize::from(self.start)..usize::from(self.end))?;
        (!name.is_empty() && name.len() <= MAX_NAME_BYTES).then_some(name)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeMode {
    Sloppy,
    Strict,
}

// The argument prefix and the full local region have different arities.
#[derive(Clone, Copy)]
pub(crate) struct FrameLayout {
    formals: u8,
    locals: u8, // Total slots, including the formal prefix.
}

impl FrameLayout {
    pub(crate) fn new(formals: usize, locals: usize) -> Option<Self> {
        let layout = Self {
            formals: formals.try_into().ok()?,
            locals: locals.try_into().ok()?,
        };
        layout.counts()?;
        Some(layout)
    }

    pub(super) fn counts(self) -> Option<(usize, usize)> {
        (self.formals <= MAX_PARAMETERS
            && self.formals <= self.locals
            && self.locals <= MAX_FRAME_LOCALS)
            .then_some((usize::from(self.formals), usize::from(self.locals)))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct NameRange {
    pub(crate) start: u16,
    pub(crate) end: u16,
}

impl NameRange {
    fn get(self, names: &[u8]) -> Option<&[u8]> {
        let name = names.get(usize::from(self.start)..usize::from(self.end))?;
        (!name.is_empty() && name.len() <= MAX_NAME_BYTES).then_some(name)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum BodyKind {
    Declaration,
    // Initial name belongs to this creation site. It is neither a property API
    // nor a lexical name; executing the same site again reuses the input only.
    Expression(Option<NameRange>),
}

pub(crate) struct FunctionBody {
    pub(crate) kind: BodyKind,
    pub(crate) instructions: Vec<Instruction>,
    pub(crate) max_stack_depth: usize,
    pub(crate) mode: CodeMode,
    pub(crate) layout: FrameLayout,
    pub(crate) accesses: std::ops::Range<usize>,
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
    pub(crate) functions: Vec<FunctionBody>,
    pub(crate) dot: DotCode,
}

impl CompiledScript {
    /// Compile under fixed ceilings, without checking or modifying any Realm.
    pub fn compile(source: &str) -> Result<Self, CompileError> {
        parser::compile_realm(source)
    }

    fn name(&self, site: &Site) -> Result<&[u8], InstantiateError> {
        site.name_in(&self.names)
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
    Arguments,
    Parameters,
    LocalSlots,
    NameBytes,
    PooledNameBytes,
    Declarations,
    Sites,
    DotSites,
    DotKeyUnits,
    PooledDotKeyUnits,
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
    TypeError {
        span: Span,
    },
    BindingLimitExceeded,
    ArtifactLimitExceeded,
    ArtifactBytesLimitExceeded,
    FunctionLimitExceeded,
    PropertyLimitExceeded,
    /// Instantiation would exceed 131,072 work credits, including callable
    /// metadata, initial heap construction, and any table moves.
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
    UnsupportedProperty {
        span: Span,
        cause: PropertyFault,
    },
    UnsupportedIntrinsicCall {
        span: Span,
        intrinsic: IntrinsicCall,
    },
    UnsupportedObjectConversion,
    PropertyWorkLimitExceeded,
    FunctionShapeLimitExceeded,
    PropertyLimitExceeded {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PropertyFault {
    PrimitiveReceiver,
    Builtin,
    RestrictedAccessor,
    AnnexB,
    IncompleteOwnShape,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum IntrinsicCall {
    Object,
    Function,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum StoredValue {
    Scalar(Value),
    StaticText(StaticText),
    Empty,
    Realm(RealmRange),
    Function(FunctionId),
    Object(ObjectId),
    CodeName(FunctionOrigin),
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
    ) -> Result<StoredValue, Stop> {
        if units.is_empty() {
            return Ok(StoredValue::Empty);
        }
        let [range, _] = self.plan(units, &[], copied, span)?.reserve()?.publish();
        Ok(range.map_or(StoredValue::Empty, StoredValue::Realm))
    }
}

mod arena_append {
    use super::{
        Arena, CHUNK_UNITS, Chunk, EngineFault, EvalError, MAX_COPIED_UNITS, MAX_RETAINED_UNITS,
        MAX_STRING_LITERAL_CODE_UNITS, RealmRange, Span, Stop, TryReserveError,
    };

    impl Arena {
        #[inline]
        pub(super) fn plan<'a, 's>(
            &'a mut self,
            first: &'s [u16],
            second: &'s [u16],
            copied: &'a mut usize,
            span: Span,
        ) -> Result<AppendPlan<'a, 's>, Stop> {
            if first.len() > MAX_STRING_LITERAL_CODE_UNITS
                || second.len() > MAX_STRING_LITERAL_CODE_UNITS
            {
                return Err(EvalError::InvalidScript.into());
            }
            let total = first.len() + second.len(); // Both lengths have been bounded.
            let (next_copied, end) = if total == 0 {
                (*copied, self.used)
            } else {
                let next_copied = copied
                    .checked_add(total)
                    .filter(|&total| total <= MAX_COPIED_UNITS)
                    .ok_or(Stop::Engine(EngineFault::StringCopyLimitExceeded { span }))?;
                let end = self
                    .used
                    .checked_add(total)
                    .filter(|&end| end <= MAX_RETAINED_UNITS)
                    .ok_or(Stop::Engine(EngineFault::StringRetentionLimitExceeded {
                        span,
                    }))?;
                (next_copied, end)
            };
            // used <= split <= end <= MAX_RETAINED_UNITS <= u16::MAX. Both
            // eventual ranges fit and have the validated lengths, including a
            // length of zero for an absent slice (which will not issue a range).
            Ok(AppendPlan {
                split: self.used + first.len(),
                arena: self,
                copied,
                first,
                second,
                end,
                next_copied,
            })
        }
    }

    // The exclusive borrow witnesses the checked prefix and copy balance until
    // publication. Shared source slices cannot borrow the arena being grown.
    pub(super) struct AppendPlan<'a, 's> {
        arena: &'a mut Arena,
        copied: &'a mut usize,
        first: &'s [u16],
        second: &'s [u16],
        split: usize,
        end: usize,
        next_copied: usize,
    }

    pub(super) struct ReservedAppend<'a, 's>(AppendPlan<'a, 's>);

    impl<'a, 's> AppendPlan<'a, 's> {
        #[inline]
        pub(super) fn reserve(self) -> Result<ReservedAppend<'a, 's>, Stop> {
            self.reserve_with(Vec::try_reserve_exact)
        }

        #[cfg(test)]
        pub(super) fn reserve_for_test(
            self,
            reserve: impl FnOnce(&mut Vec<Chunk>, usize) -> Result<(), TryReserveError>,
        ) -> Result<ReservedAppend<'a, 's>, Stop> {
            self.reserve_with(reserve)
        }

        #[inline]
        fn reserve_with(
            self,
            reserve: impl FnOnce(&mut Vec<Chunk>, usize) -> Result<(), TryReserveError>,
        ) -> Result<ReservedAppend<'a, 's>, Stop> {
            let needed = self.end.div_ceil(CHUNK_UNITS);
            let old = self.arena.chunks.len();
            if needed > 0 && (old == 0 || needed > old.next_power_of_two()) {
                // Follow logical power-of-two tiers even if the allocator supplies
                // extra capacity. At most 128 chunks (64 KiB) requested, at most
                // 64 old initialized chunks moved at once, 1+2+...+64 = 127
                // across the lifetime (thus <=32,512 old units in any execute).
                // Dropping a reserved plan can leave spare capacity, but only a
                // larger tier can relocate again. Requested old+new overlap is
                // <=96 KiB, or <=160 KiB with live scratch backing, excluding
                // allocator overhead/rounding.
                reserve(&mut self.arena.chunks, needed.next_power_of_two() - old)
                    .map_err(|_| EvalError::AllocationFailed)?;
            }
            assert!(self.arena.chunks.capacity() >= needed);
            Ok(ReservedAppend(self))
        }
    }

    impl ReservedAppend<'_, '_> {
        #[inline]
        pub(super) fn publish(self) -> [Option<RealmRange>; 2] {
            let AppendPlan {
                arena,
                copied,
                first,
                second,
                split,
                end,
                next_copied,
            } = self.0;
            // Only initialize reached chunks, at most 16 for one slice, 32
            // for two, and 128 over the Realm lifetime. Capacity is reserved.
            let needed = end.div_ceil(CHUNK_UNITS);
            while arena.chunks.len() < needed {
                arena.chunks.push([0; CHUNK_UNITS]);
            }
            let start = arena.used;
            let tail = &mut arena.chunks.as_flattened_mut()[start..end];
            let (left, right) = tail.split_at_mut(split - start);
            left.copy_from_slice(first);
            right.copy_from_slice(second);
            arena.used = end;
            *copied = next_copied;
            let range = |start: usize, end: usize| {
                (start < end).then_some(RealmRange {
                    start: start as u16,
                    end: end as u16,
                })
            };
            [range(start, split), range(split, end)]
        }
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

// Private append-only indices, meaningful only in the owning Realm. A cell
// points to an identity, not directly to a code address or binding name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FunctionId {
    User(u16),
    Intrinsic(Intrinsic),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FunctionOrigin {
    pub(super) artifact: u8,
    pub(super) body: u16,
}

pub(super) struct Artifact {
    bodies: Vec<FunctionBody>,
    string_units: Vec<u16>,
    literal_units: usize,
    names: Vec<u8>,
    sites: Vec<Site>,
    // Body-indexed ranges into the checked UTF-16 name tail, never literals.
    body_names: Vec<Option<NameRange>>,
    dot: DotCode,
}

impl Artifact {
    fn stage(
        code: &CompiledScript,
        retained: usize,
        work: &mut Work,
    ) -> Result<(Self, usize), InstantiateError> {
        // Exact fresh reservations: no realloc overlap within the immutable
        // backing. The budget includes measured capacities, including metadata.
        let mut bytes = retained;
        validate_body(&code.instructions, code.max_stack_depth, code.exit)
            .map_err(|_| InstantiateError::InvalidScript)?;
        let mut bodies = reserve_code(code.functions.len(), &mut bytes, work)?;
        let mut body_names = reserve_code(code.functions.len(), &mut bytes, work)?;
        let mut name_units = 0usize;
        let declarations = code
            .sites
            .get(..code.declarations)
            .ok_or(InstantiateError::InvalidScript)?;
        work.charge(declarations.len())?;
        let mut declaration_names = declarations.iter().filter_map(|site| match site.role {
            Role::Function { body } => Some((
                body,
                NameRange {
                    start: site.start,
                    end: site.end,
                },
            )),
            _ => None,
        });
        for (index, body) in code.functions.iter().enumerate() {
            let name = match body.kind {
                BodyKind::Declaration => {
                    let (site_body, name) = declaration_names
                        .next()
                        .ok_or(InstantiateError::InvalidScript)?;
                    if usize::from(site_body) != index {
                        return Err(InstantiateError::InvalidScript);
                    }
                    Some(name)
                }
                BodyKind::Expression(name) => name,
            };
            if let Some(range) = name {
                let name = range
                    .get(&code.names)
                    .ok_or(InstantiateError::InvalidScript)?;
                work.charge(name.len())?;
                name_units = name_units
                    .checked_add(name.len())
                    .filter(|&n| n <= MAX_CODE_NAME_BYTES)
                    .ok_or(InstantiateError::InvalidScript)?;
                let start = |b: u8| b.is_ascii_alphabetic() || matches!(b, b'_' | b'$');
                if !start(name[0]) || !name[1..].iter().all(|&b| start(b) || b.is_ascii_digit()) {
                    return Err(InstantiateError::InvalidScript);
                }
            }
            body_names.push(name);
            validate_body(&body.instructions, body.max_stack_depth, ExitStack::Empty)
                .map_err(|_| InstantiateError::InvalidScript)?;
            work.charge(body.instructions.len())?;
            if body.layout.counts().is_none() {
                return Err(InstantiateError::InvalidScript);
            }
            let mut has_dot = false;
            for &op in &body.instructions {
                if matches!(op, Instruction::CreateFunction(_)) {
                    return Err(InstantiateError::InvalidScript);
                }
                has_dot |= property_op(op);
            }
            if has_dot {
                code.dot.validate_ops(
                    index as u16,
                    &body.instructions,
                    body.max_stack_depth,
                    ExitStack::Empty,
                    work,
                )?;
            }
            bodies.push(FunctionBody {
                kind: body.kind,
                instructions: copy_code(&body.instructions, &mut bytes, work)?,
                max_stack_depth: body.max_stack_depth,
                mode: body.mode,
                layout: body.layout,
                accesses: body.accesses.clone(),
            });
        }
        if declaration_names.next().is_some() {
            return Err(InstantiateError::InvalidScript);
        }
        // A merge scan authenticates each root creator against a distinct
        // expression template in emission order. Charge both sides, including
        // declaration templates the filter visits but does not yield.
        work.charge(code.instructions.len())?;
        work.charge(bodies.len())?;
        let mut expected = bodies.iter().enumerate().filter_map(|(index, body)| {
            matches!(body.kind, BodyKind::Expression(_)).then_some(index)
        });
        let mut root_has_dot = false;
        for &op in &code.instructions {
            root_has_dot |= property_op(op);
            if let Instruction::CreateFunction(body) = op
                && expected.next() != Some(usize::from(body))
            {
                return Err(InstantiateError::InvalidScript);
            }
        }
        if root_has_dot {
            code.dot.validate_ops(
                DOT_ROOT,
                &code.instructions,
                code.max_stack_depth,
                code.exit,
                work,
            )?;
        }
        if expected.next().is_some() {
            return Err(InstantiateError::InvalidScript);
        }
        let literal_units = code.string_units.len();
        // Independent bounds for the original literal prefix and widened tail.
        const MAX_LITERAL_POOL_UNITS: usize = 16 * 1024;
        if literal_units > MAX_LITERAL_POOL_UNITS {
            return Err(InstantiateError::InvalidScript);
        }
        let mut string_units = reserve_code(literal_units + name_units, &mut bytes, work)?;
        string_units.extend_from_slice(&code.string_units);
        for range in body_names.iter_mut().flatten() {
            let name = range
                .get(&code.names)
                .ok_or(InstantiateError::InvalidScript)?;
            let start = string_units.len() as u16;
            string_units.extend(name.iter().copied().map(u16::from));
            *range = NameRange {
                start,
                end: string_units.len() as u16,
            };
        }
        let names = copy_code(&code.names, &mut bytes, work)?;
        let sites = copy_code(&code.sites, &mut bytes, work)?;
        Ok((
            Self {
                bodies,
                string_units,
                names,
                literal_units,
                sites,
                body_names,
                dot: DotCode {
                    source_len: code.dot.source_len,
                    sites: copy_code(&code.dot.sites, &mut bytes, work)?,
                    keys: copy_code(&code.dot.keys, &mut bytes, work)?,
                },
            },
            bytes,
        ))
    }

    pub(super) fn literal_units(&self) -> &[u16] {
        &self.string_units[..self.literal_units]
    }

    pub(super) fn initial_name(&self, body: u16) -> Option<&[u16]> {
        let index = usize::from(body);
        let template = self.bodies.get(index)?;
        template.layout.counts()?;
        let name = *self.body_names.get(index)?;
        match (template.kind, name) {
            (BodyKind::Expression(None), None) => Some(&[]),
            (BodyKind::Declaration | BodyKind::Expression(Some(_)), Some(range)) => {
                let (start, end) = (usize::from(range.start), usize::from(range.end));
                let name = self.string_units.get(start..end)?;
                (start >= self.literal_units && !name.is_empty() && name.len() <= MAX_NAME_BYTES)
                    .then_some(name)
            }
            _ => None,
        }
    }
}

fn reserve_code<T>(
    len: usize,
    bytes: &mut usize,
    work: &mut Work,
) -> Result<Vec<T>, InstantiateError> {
    use InstantiateError::ArtifactBytesLimitExceeded;
    let element = std::mem::size_of::<T>();
    let fits = |count: usize| {
        count
            .checked_mul(element)
            .and_then(|size| bytes.checked_add(size))
            .filter(|&total| total <= MAX_ARTIFACT_BYTES)
            .ok_or(ArtifactBytesLimitExceeded)
    };
    fits(len)?;
    work.charge(len)?;
    let mut result = Vec::new();
    result
        .try_reserve_exact(len)
        .map_err(|_| InstantiateError::AllocationFailed)?;
    *bytes = fits(result.capacity())?;
    Ok(result)
}

fn copy_code<T: Copy>(
    source: &[T],
    bytes: &mut usize,
    work: &mut Work,
) -> Result<Vec<T>, InstantiateError> {
    let mut result = reserve_code(source.len(), bytes, work)?;
    result.extend_from_slice(source);
    Ok(result)
}

#[derive(Clone, Copy, Default)]
pub(super) struct CallableCode<'a> {
    pub(super) artifacts: &'a [Artifact],
}

impl<'a> CallableCode<'a> {
    pub(super) fn body(self, origin: FunctionOrigin) -> Option<(&'a Artifact, &'a FunctionBody)> {
        let artifact = self.artifacts.get(usize::from(origin.artifact))?;
        Some((artifact, artifact.bodies.get(usize::from(origin.body))?))
    }
}

/// Owns two private binding records and immutable ordinary function bodies. No global object, handles, callbacks, or reentry.
#[derive(Default)]
pub struct Realm {
    state: RealmState,
    artifacts: Vec<Artifact>,
    artifact_bytes: usize,
    functions: Vec<FunctionRecord>,
    heap: Heap,
}

#[derive(Default)]
struct RealmState {
    globals: Vec<Cell>,
    object: ObjectBindings,
    strings: Arena,
}

impl Realm {
    pub fn new() -> Self {
        Self::default()
    }

    /// Preflight and atomically install new lexical cells in TDZ and new `var`
    /// cells initialized to `undefined`, and install or replace the last
    /// function declaration per name. A failed instantiation changes no
    /// bindings or retained code. A successful instantiation retains code for
    /// any callable body, even if the ready script is dropped or skips every
    /// expression. Function expressions acquire identities only when reached.
    /// Missing or uninitialized uses are not rejected by this check.
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
        self.instantiate_with_reserve(code, work, |_| Ok(()))
    }

    fn instantiate_with_reserve<'realm, 'code>(
        &'realm mut self,
        code: &'code CompiledScript,
        work: usize,
        reserve_heap: impl FnMut(usize) -> Result<(), InstantiateError>,
    ) -> Result<ReadyScript<'realm, 'code>, InstantiateError> {
        use InstantiateError::{
            AllocationFailed, BindingLimitExceeded, InvalidScript, SyntaxError,
        };
        let mut work = Work(work);
        let lexical_base = self.state.globals.len();
        let var_base = self.state.object.vars.len();
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
        let mut function_declarations = 0;
        for site in declarations {
            work.charge(1)?;
            match site.role {
                Role::Declaration(_) => lexical_count += 1,
                Role::Var { .. } => continue,
                Role::Function { .. } => {
                    function_declarations += 1;
                    continue;
                }
                _ => return Err(InvalidScript),
            }
            let name = code.name(site)?;
            let seed = self.state.object.undefined_declared;
            if (seed && work.equal(name, b"undefined")?)
                || work
                    .find(
                        name,
                        self.state.object.vars.iter().map(|c| c.name.as_slice()),
                    )?
                    .is_some()
                || work
                    .find(name, self.state.globals.iter().map(|c| c.name.as_slice()))?
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
                if matches!(site.role, Role::Var { .. } | Role::Function { .. })
                    && work
                        .find(
                            code.name(site)?,
                            self.state.globals.iter().map(|c| c.name.as_slice()),
                        )?
                        .is_some()
                {
                    return Err(SyntaxError { span: site.span });
                }
            }
        }
        // Check CanDeclareGlobalFunction in reverse last-occurrence order,
        // only after all lexical and VarDeclaredNames-vs-lexical checks.
        let mut selected = Vec::<usize>::new();
        if function_declarations != 0 {
            selected
                .try_reserve_exact(function_declarations)
                .map_err(|_| AllocationFailed)?;
            work.charge(code.functions.len())?;
            let mut next_body =
                code.functions
                    .iter()
                    .enumerate()
                    .rev()
                    .filter_map(|(index, body)| {
                        matches!(body.kind, BodyKind::Declaration).then_some(index)
                    });
            for (index, site) in declarations.iter().enumerate().rev() {
                work.charge(1)?;
                let Role::Function { body } = site.role else {
                    continue;
                };
                if next_body.next() != Some(usize::from(body)) {
                    return Err(InvalidScript);
                }
                let name = code.name(site)?;
                let mut duplicate = false;
                for &chosen in &selected {
                    if work.equal(name, code.name(&declarations[chosen])?)? {
                        duplicate = true;
                        break;
                    }
                }
                if duplicate {
                    continue;
                }
                // Existing declared rows are writable and enumerable. The
                // readonly, non-configurable undefined seed is not.
                if work.equal(name, b"undefined")? {
                    return Err(InstantiateError::TypeError { span: site.span });
                }
                selected.push(index);
            }
            if next_body.next().is_some() {
                return Err(InvalidScript);
            }
        }
        // Remaining CanDeclareGlobalVar checks all succeed in this Realm:
        // absent names are declarable and any existing object-side row or
        // readonly undefined can be declared without replacement.
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
        let artifact_id = self.artifacts.len();
        let mut installs = Vec::new();
        let mut identities = Vec::new();
        if !selected.is_empty() {
            if artifact_id == MAX_ARTIFACTS {
                return Err(InstantiateError::ArtifactLimitExceeded);
            }
            if self.functions.len() + selected.len() > MAX_FUNCTIONS {
                return Err(InstantiateError::FunctionLimitExceeded);
            }
            installs
                .try_reserve_exact(selected.len())
                .map_err(|_| AllocationFailed)?;
            identities
                .try_reserve_exact(selected.len())
                .map_err(|_| AllocationFailed)?;
            for index in selected.into_iter().rev() {
                let site = &declarations[index];
                let Role::Function { body } = site.role else {
                    return Err(InvalidScript);
                };
                let name = code.name(site)?;
                let slot = declare_var(
                    &mut work,
                    &self.state.object.vars,
                    &mut vars,
                    available - lexical_count,
                    name,
                )?;
                work.charge(4)?; // stage and publish identity + replacement
                let id = FunctionId::User((self.functions.len() + identities.len()) as u16);
                identities.push(FunctionOrigin {
                    artifact: artifact_id as u8,
                    body,
                });
                installs.push((slot, id));
            }
        }
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
                        if !self.state.object.undefined_declared && !declare_undefined {
                            work.charge(2)?; // Stage and publish membership.
                            declare_undefined = true;
                        }
                        Target::Undefined
                    } else {
                        Target::Var(declare_var(
                            &mut work,
                            &self.state.object.vars,
                            &mut vars,
                            available - lexical_count,
                            name,
                        )?)
                    }
                }
                Role::Function { .. } if !identities.is_empty() => {
                    work.charge(1)?; // Placeholder: declarations are not executable sites.
                    Target::Unlinked
                }
                _ => return Err(InvalidScript),
            };
            links.push(target);
        }
        // Linking below is a merge scan. Check the sorted, disjoint ranges
        // first, including empty bodies that the scan might never visit.
        let mut previous_end = code.declarations;
        let mut remaining_declarations = function_declarations;
        for body in &code.functions {
            work.charge(1)?;
            if matches!(body.kind, BodyKind::Declaration) {
                remaining_declarations =
                    remaining_declarations.checked_sub(1).ok_or(InvalidScript)?;
            }
            let range = &body.accesses;
            if range.start < previous_end || range.start > range.end || range.end > code.sites.len()
            {
                return Err(InvalidScript);
            }
            previous_end = range.end;
        }
        if remaining_declarations != 0 {
            return Err(InvalidScript);
        }
        let mut bodies = code.functions.iter().peekable();
        for (offset, site) in accesses.iter().enumerate() {
            work.charge(1)?;
            let index = code.declarations + offset;
            while bodies.peek().is_some_and(|body| body.accesses.end <= index) {
                work.charge(1)?;
                bodies.next();
            }
            if bodies
                .peek()
                .is_some_and(|body| body.accesses.contains(&index))
            {
                // Retained code must use current Realm lookups. Authenticate
                // the site's shape but do not make a semantic Ready link.
                match site.role {
                    Role::Local if (site.start, site.end) == (0, 0) => {}
                    _ => {
                        site.access_name(&code.names).ok_or(InvalidScript)?;
                    }
                }
                links.push(Target::Unlinked);
                continue;
            }
            let name = site.access_name(&code.names).ok_or(InvalidScript)?;
            let target = if let Some(slot) = work.find(
                name,
                self.state
                    .globals
                    .iter()
                    .chain(&lexicals)
                    .map(|c| c.name.as_slice()),
            )? {
                Target::Lexical(slot)
            } else if work.equal(name, b"undefined")? {
                Target::Undefined
            } else if let Some(slot) = work.find(
                name,
                self.state
                    .object
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
        let lexical_growth = prepare_growth(&self.state.globals, lexicals.len(), &mut work)?;
        let var_growth = prepare_growth(&self.state.object.vars, vars.len(), &mut work)?;
        code.dot.validate(code.functions.len(), &mut work)?;
        // Discover independently of table presence. Artifact staging uses its
        // existing charged instruction scans. For an artifact-free root, this
        // bounded discriminator replaces Ready's call probe with no added
        // preflight debit unless it finds a property op: then charge N visits
        // on the same ledger before metering the property shape checks.
        let mut root_validator = None;
        if code.functions.is_empty() {
            if code.instructions.len() > super::MAX_INSTRUCTIONS {
                return Err(InvalidScript);
            }
            let mut has_call = false;
            let mut has_dot = false;
            for &op in &code.instructions {
                has_call |= matches!(op, Instruction::Call { .. });
                has_dot |= property_op(op);
            }
            if has_dot {
                work.charge(code.instructions.len())?;
                code.dot.validate_ops(
                    DOT_ROOT,
                    &code.instructions,
                    code.max_stack_depth,
                    code.exit,
                    &mut work,
                )?;
            } else {
                root_validator = Some(if has_call {
                    RootLayout::Callable
                } else {
                    RootLayout::Ordinary
                });
            }
        }
        let artifact = if code.functions.is_empty() {
            None
        } else {
            if artifact_id >= MAX_ARTIFACTS {
                return Err(InstantiateError::ArtifactLimitExceeded);
            }
            Some(Artifact::stage(code, self.artifact_bytes, &mut work)?)
        };
        let root_artifact = artifact.as_ref().map(|_| artifact_id as u8);
        // The artifact table requests at most 24 simultaneous old/new slots.
        let artifact_growth =
            prepare_growth(&self.artifacts, usize::from(artifact.is_some()), &mut work)?;
        let creation = if identities.is_empty() {
            None
        } else {
            let (staged, _) = artifact.as_ref().ok_or(InvalidScript)?;
            let plan = Creation::plan(
                &mut self.heap,
                &mut self.functions,
                artifact_id as u8,
                staged,
                &identities,
            )?;
            work.charge(plan.function_moves())?;
            work.charge(plan.heap_work())?;
            if plan.request_bytes() > HEAP_REQUEST_BYTES {
                return Err(AllocationFailed);
            }
            Some(plan.reserve_with(reserve_heap)?)
        };
        // Publication starts only after all checks and detached reservations.
        // This plan borrows staged code, so publish the already-proven heap
        // and identities first; only this commit can observe future origins.
        if let Some(creation) = creation {
            creation.publish();
        }
        commit_rows(
            &mut self.state.globals,
            lexicals.into_iter(),
            lexical_growth,
        );
        commit_rows(&mut self.state.object.vars, vars.into_iter(), var_growth);
        if declare_undefined {
            self.state.object.undefined_declared = true;
        }
        if let Some((artifact, bytes)) = artifact {
            commit_rows(
                &mut self.artifacts,
                std::iter::once(artifact),
                artifact_growth,
            );
            self.artifact_bytes = bytes;
        }
        for (slot, function) in installs {
            self.state.object.vars[slot].value = StoredValue::Function(function);
        }
        // Ready's exclusive borrow keeps these indices and misses valid
        // until this execution finishes or is abandoned.
        Ok(ReadyScript {
            realm: self,
            code,
            links,
            root_artifact,
            root_validator,
        })
    }
}

fn declare_var(
    work: &mut Work,
    existing: &[VarCell],
    staged: &mut Vec<VarCell>,
    available: usize,
    name: &[u8],
) -> Result<usize, InstantiateError> {
    if let Some(slot) = work.find(
        name,
        existing
            .iter()
            .chain(staged.iter())
            .map(|c| c.name.as_slice()),
    )? {
        return Ok(slot);
    }
    if staged.len() == available {
        return Err(InstantiateError::BindingLimitExceeded);
    }
    let slot = existing.len() + staged.len();
    work.charge(name.len() + 2)?;
    staged.push(VarCell {
        name: owned_name(name)?,
        value: StoredValue::Scalar(Value::Undefined),
    });
    Ok(slot)
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

fn commit_rows<T>(
    rows: &mut Vec<T>,
    staged: impl ExactSizeIterator<Item = T>,
    growth: Option<Vec<T>>,
) {
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
            .field(
                "bindings",
                &(self.state.globals.len() + self.state.object.vars.len()),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
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
    Unlinked,
}

#[derive(Clone, Copy)]
enum RootLayout {
    Ordinary,
    Callable,
}

/// A one-shot execution guard. Dropping the guard abandons the body, but
/// leaves lexical bindings in TDZ, new `var` bindings `undefined`, and
/// function declarations installed.
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
    root_artifact: Option<u8>,
    root_validator: Option<RootLayout>,
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
        // Root layout was already checked for retained and property-bearing
        // code. Ordinary/body-free roots keep the original reached validators.
        if let Some(layout) = self.root_validator {
            let validate = match layout {
                RootLayout::Ordinary => validate_layout,
                RootLayout::Callable => validate_body,
            };
            validate(&code.instructions, code.max_stack_depth, code.exit)
                .map_err(EngineFault::Evaluation)?;
        }
        let stack = new_stack(code.max_stack_depth).map_err(EngineFault::Evaluation)?;
        let mut scratch = EvalScratch::new();
        let mut execution = self.execution();
        match dispatch(
            Frame::root(&code.instructions, code.max_stack_depth, code.exit),
            stack,
            &code.string_units,
            execution.callable,
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
            realm: &mut self.realm.state,
            callable: CallableCode {
                artifacts: &self.realm.artifacts,
            },
            functions: &mut self.realm.functions,
            heap: &mut self.realm.heap,
            property_work: Work(PROPERTY_WORK),
            heap_requests: HEAP_REQUEST_BYTES,
            root_artifact: self.root_artifact,
            active: None,
            lookup: Work(LOOKUP_WORK),
            sites: &self.code.sites,
            root_dot: &self.code.dot,
            links: &self.links,
            mode: self.code.mode,
            copied: 0,
        }
    }
}

struct Execution<'realm, 'code> {
    realm: &'realm mut RealmState,
    callable: CallableCode<'realm>,
    functions: &'realm mut Vec<FunctionRecord>,
    heap: &'realm mut Heap,
    property_work: Work,
    heap_requests: usize,
    root_artifact: Option<u8>,
    active: Option<FunctionOrigin>,
    lookup: Work,
    sites: &'code [Site],
    root_dot: &'code DotCode,
    links: &'code [Target],
    mode: CodeMode,
    copied: usize,
}

impl Execution<'_, '_> {
    fn site(&mut self, index: usize) -> Result<(Target, Span, Role), Stop> {
        if let Some(origin) = self.active {
            let (artifact, body) = self.callable.body(origin).ok_or(EvalError::InvalidScript)?;
            if !body.accesses.contains(&index) {
                return Err(EvalError::InvalidScript.into());
            }
            let site = artifact.sites.get(index).ok_or(EvalError::InvalidScript)?;
            let name = site
                .access_name(&artifact.names)
                .ok_or(EvalError::InvalidScript)?;
            let target = self
                .resolve_current(name)
                .map_err(|_| EvalError::GlobalLookupLimitExceeded)?;
            return Ok((target, site.span, site.role));
        }
        let site = self.sites.get(index).ok_or(EvalError::InvalidScript)?;
        let target = *self.links.get(index).ok_or(EvalError::InvalidScript)?;
        if matches!(target, Target::Unlinked) {
            return Err(EvalError::InvalidScript.into());
        }
        Ok((target, site.span, site.role))
    }

    fn resolve_current(&mut self, name: &[u8]) -> Result<Target, InstantiateError> {
        // A reached reference is resolved against the current (fixed-shape)
        // namespace; no closure stores Ready links, even if it never missed.
        self.lookup.charge(1)?;
        if let Some(slot) = self
            .lookup
            .find(name, self.realm.globals.iter().map(|c| c.name.as_slice()))?
        {
            Ok(Target::Lexical(slot))
        } else if self.lookup.equal(name, b"undefined")? {
            Ok(Target::Undefined)
        } else if let Some(slot) = self.lookup.find(
            name,
            self.realm.object.vars.iter().map(|c| c.name.as_slice()),
        )? {
            Ok(Target::Var(slot))
        } else {
            Ok(Target::Missing)
        }
    }

    fn mode(&self) -> Result<CodeMode, EvalError> {
        match self.active {
            None => Ok(self.mode),
            Some(origin) => self
                .callable
                .body(origin)
                .map(|(_, body)| body.mode)
                .ok_or(EvalError::InvalidScript),
        }
    }

    fn write_target(&mut self, index: usize) -> Result<(Target, Span), Stop> {
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
            Target::Unlinked => return Err(EvalError::InvalidScript.into()),
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
        self.load_value(value)
    }

    fn load_value(&self, value: StoredValue) -> Result<Operand, Stop> {
        Ok(match value {
            StoredValue::Scalar(value) => Operand::Scalar(value),
            StoredValue::Function(id) => {
                self.callable_object(id)?;
                Operand::Function(id)
            }
            StoredValue::Object(id) => {
                if !self.heap.is_noncallable(id) {
                    return Err(EvalError::InvalidScript.into());
                }
                Operand::Object(id)
            }
            StoredValue::CodeName(origin) => {
                let name = self
                    .callable
                    .body(origin)
                    .and_then(|(artifact, _)| artifact.initial_name(origin.body))
                    .ok_or(EvalError::InvalidScript)?;
                if name.is_empty() {
                    return Err(EvalError::InvalidScript.into());
                }
                Operand::String(StringOperand::CodeName(origin))
            }
            StoredValue::StaticText(name) => Operand::String(StringOperand::StaticText(name)),
            StoredValue::Empty => Operand::String(StringOperand::Empty),
            StoredValue::Realm(range) => {
                range
                    .get(self.retained_units())
                    .ok_or(EvalError::InvalidScript)?;
                Operand::String(StringOperand::Realm(range))
            }
        })
    }

    fn callable_object(&self, id: FunctionId) -> Result<ObjectId, Stop> {
        if let FunctionId::User(_) = id {
            let origin = self.function_origin(id)?;
            self.callable.body(origin).ok_or(EvalError::InvalidScript)?;
        }
        self.heap
            .callable_object(id, self.functions)
            .ok_or(EvalError::InvalidScript.into())
    }

    fn object_receiver(&self, value: Operand) -> Result<Option<ObjectId>, Stop> {
        match value {
            Operand::Function(id) => Ok(Some(self.callable_object(id)?)),
            Operand::Object(id) if self.heap.is_noncallable(id) => Ok(Some(id)),
            Operand::Object(_) => Err(EvalError::InvalidScript.into()),
            _ => Ok(None),
        }
    }

    fn put(
        &mut self,
        target: Target,
        span: Span,
        value: Operand,
        source: TransientStrings<'_>,
    ) -> Result<(), Stop> {
        let mode = self.mode()?;
        let destination = match target {
            // Only reached writes get here, after RHS evaluation/coercion.
            Target::Undefined if mode == CodeMode::Strict => {
                return Err(Stop::Throw(Throw::TypeError { span }));
            }
            Target::Undefined => return Ok(()), // Sloppy readonly write, even with a String RHS.
            Target::Missing if mode == CodeMode::Strict => {
                return Err(Stop::Throw(Throw::ReferenceError { span }));
            }
            Target::Missing => return Err(Stop::Engine(EngineFault::ImplicitGlobal { span })),
            Target::Unlinked => return Err(EvalError::InvalidScript.into()),
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

enum Escape<'a> {
    Value(StoredValue),
    Copy(&'a [u16]),
}

impl<'a> Escape<'a> {
    fn check(value: Operand, source: TransientStrings<'a>, retained: &[u16]) -> Result<Self, Stop> {
        let value = match value {
            Operand::Scalar(value) => StoredValue::Scalar(value),
            // All issued identity operands are checked by the producing load,
            // descriptor Get or creator and the heap never retires identities.
            Operand::Function(id) => StoredValue::Function(id),
            Operand::Object(id) => StoredValue::Object(id),
            Operand::String(StringOperand::StaticText(name)) => StoredValue::StaticText(name),
            Operand::String(StringOperand::Empty) => StoredValue::Empty,
            Operand::String(StringOperand::CodeName(origin)) => {
                source
                    .code_name(origin)
                    .filter(|s| !s.is_empty())
                    .ok_or(EvalError::InvalidScript)?;
                StoredValue::CodeName(origin)
            }
            Operand::String(StringOperand::Realm(range)) => {
                range.get(retained).ok_or(EvalError::InvalidScript)?;
                StoredValue::Realm(range)
            }
            Operand::String(string) => {
                let units = match string {
                    StringOperand::Literal(range) => source.literal(range),
                    StringOperand::ArtifactLiteral { artifact, range } => {
                        source.artifact_literal(artifact, range)
                    }
                    StringOperand::Runtime(range) => source.runtime(range),
                    _ => return Err(EvalError::InvalidScript.into()),
                }
                .ok_or(EvalError::InvalidScript)?;
                return Ok(if units.is_empty() {
                    Self::Value(StoredValue::Empty)
                } else {
                    Self::Copy(units)
                });
            }
        };
        Ok(Self::Value(value))
    }

    fn units(&self) -> &'a [u16] {
        match self {
            Self::Value(_) => &[],
            Self::Copy(units) => units,
        }
    }

    fn finish(self, range: Option<RealmRange>) -> StoredValue {
        match self {
            Self::Value(value) => value,
            Self::Copy(_) => StoredValue::Realm(range.expect("reserved nonempty value")),
        }
    }
}

fn escape(
    value: Operand,
    span: Span,
    source: TransientStrings<'_>,
    strings: &mut Arena,
    copied: &mut usize,
) -> Result<StoredValue, Stop> {
    match Escape::check(value, source, strings.units())? {
        Escape::Value(value) => Ok(value),
        Escape::Copy(units) => strings.append(units, copied, span),
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

impl From<Conversion> for Stop {
    fn from(error: Conversion) -> Self {
        match error {
            Conversion::Evaluation(error) => error.into(),
            Conversion::Object => Self::Engine(EngineFault::UnsupportedObjectConversion),
        }
    }
}

fn creation_stop(error: InstantiateError) -> Stop {
    match error {
        InstantiateError::PropertyLimitExceeded => {
            Stop::Engine(EngineFault::FunctionShapeLimitExceeded)
        }
        InstantiateError::FunctionLimitExceeded => EvalError::FunctionLimitExceeded.into(),
        InstantiateError::AllocationFailed => EvalError::AllocationFailed.into(),
        _ => EvalError::InvalidScript.into(),
    }
}

fn property_stop(error: InstantiateError, span: Span) -> Stop {
    match error {
        InstantiateError::PropertyLimitExceeded => {
            Stop::Engine(EngineFault::PropertyLimitExceeded { span })
        }
        InstantiateError::WorkLimitExceeded => Stop::Engine(EngineFault::PropertyWorkLimitExceeded),
        InstantiateError::FunctionLimitExceeded => EvalError::FunctionLimitExceeded.into(),
        InstantiateError::AllocationFailed => EvalError::AllocationFailed.into(),
        _ => EvalError::InvalidScript.into(),
    }
}

fn unsupported_property(span: Span, cause: PropertyFault) -> Stop {
    Stop::Engine(EngineFault::UnsupportedProperty { span, cause })
}

fn false_set(site: DotSite) -> Result<(), Stop> {
    if site.mode == CodeMode::Strict {
        Err(Stop::Throw(Throw::TypeError { span: site.span }))
    } else {
        Ok(())
    }
}

fn primitive_dot(receiver: Operand, key: &[u16], span: Span) -> Result<StringOperand, Stop> {
    match receiver {
        Operand::Scalar(Value::Null | Value::Undefined) => {
            Err(Stop::Throw(Throw::TypeError { span }))
        }
        Operand::String(string)
            if key
                .iter()
                .copied()
                .eq(b"length".iter().copied().map(u16::from)) =>
        {
            Ok(string)
        }
        Operand::Scalar(_)
            if key
                .iter()
                .copied()
                .eq(b"length".iter().copied().map(u16::from)) =>
        {
            Err(EvalError::UnsupportedPropertyReceiver.into())
        }
        _ => Err(unsupported_property(span, PropertyFault::PrimitiveReceiver)),
    }
}

fn dot_table<'a>(
    root: &'a DotCode,
    callable: CallableCode<'a>,
    active: Option<FunctionOrigin>,
    root_mode: CodeMode,
    owner: Option<FunctionOrigin>,
) -> Result<(&'a DotCode, u16, CodeMode), Stop> {
    if owner != active {
        return Err(EvalError::InvalidScript.into());
    }
    match owner {
        Some(origin) => {
            let (artifact, body) = callable.body(origin).ok_or(EvalError::InvalidScript)?;
            Ok((&artifact.dot, origin.body, body.mode))
        }
        None => Ok((root, DOT_ROOT, root_mode)),
    }
}

impl Properties for Execution<'_, '_> {
    type Error = Stop;

    fn dot_metadata(
        &self,
        owner: Option<FunctionOrigin>,
        index: u16,
    ) -> Result<(Span, CodeMode), Stop> {
        let (table, owner, mode) =
            dot_table(self.root_dot, self.callable, self.active, self.mode, owner)?;
        let (_, span) = table
            .occurrence(owner, index)
            .ok_or(EvalError::InvalidScript)?;
        Ok((span, mode))
    }

    fn charge_packets(&mut self, work: usize) -> Result<(), Stop> {
        self.property_work
            .charge(work)
            .map_err(|_| Stop::Engine(EngineFault::PropertyWorkLimitExceeded))
    }

    fn get_dot(
        &mut self,
        site: DotSite,
        receiver: Operand,
        source: TransientStrings<'_>,
    ) -> Result<Operand, Stop> {
        let object = self.object_receiver(receiver)?;
        let table = dot_table(
            self.root_dot,
            self.callable,
            self.active,
            self.mode,
            site.owner,
        )?;
        let (key, _) = table
            .0
            .occurrence(table.1, site.index)
            .ok_or(EvalError::InvalidScript)?;
        let Some(object) = object else {
            let string = primitive_dot(receiver, key, site.span)?;
            let units = string
                .get(source.with_retained(self.retained_units()))
                .ok_or(EvalError::InvalidScript)?;
            return Ok(Operand::Scalar(Value::Number(units.len() as f64)));
        };
        // `key` borrows immutable compiled/retained code, not the Realm heap.
        let mut work = self.property_work;
        let lookup = self
            .heap
            .lookup(object, key, self.retained_units(), &mut work)
            .map_err(|e| property_stop(e, site.span))?;
        self.property_work = work;
        match lookup {
            Lookup::Missing => Ok(Operand::Scalar(Value::Undefined)),
            Lookup::Found {
                descriptor: Descriptor::Data(value, _),
                ..
            } => self.load_value(value),
            Lookup::Found {
                descriptor: Descriptor::Builtin,
                ..
            } => Err(unsupported_property(site.span, PropertyFault::Builtin)),
            Lookup::Found {
                descriptor: Descriptor::Restricted,
                ..
            } => Err(unsupported_property(
                site.span,
                PropertyFault::RestrictedAccessor,
            )),
            Lookup::Found {
                descriptor: Descriptor::AnnexB,
                ..
            } => Err(unsupported_property(site.span, PropertyFault::AnnexB)),
            Lookup::Incomplete => Err(unsupported_property(
                site.span,
                PropertyFault::IncompleteOwnShape,
            )),
        }
    }

    fn put_dot(
        &mut self,
        site: DotSite,
        receiver: Operand,
        rhs: Operand,
        source: TransientStrings<'_>,
    ) -> Result<(), Stop> {
        self.put_dot_with(
            site,
            receiver,
            rhs,
            source,
            |edit| edit.reserve(),
            |plan| plan.reserve(),
        )
    }
}

impl Execution<'_, '_> {
    fn put_dot_with(
        &mut self,
        site: DotSite,
        receiver: Operand,
        rhs: Operand,
        source: TransientStrings<'_>,
        reserve_row: impl for<'a> FnOnce(
            object_heap::RowEdit<'a>,
        )
            -> Result<object_heap::ReservedEdit<'a>, InstantiateError>,
        reserve_append: impl for<'a, 's> FnOnce(
            arena_append::AppendPlan<'a, 's>,
        )
            -> Result<arena_append::ReservedAppend<'a, 's>, Stop>,
    ) -> Result<(), Stop> {
        let object = self.object_receiver(receiver)?;
        let (table, owner, _) = dot_table(
            self.root_dot,
            self.callable,
            self.active,
            self.mode,
            site.owner,
        )?;
        let (key, _) = table
            .occurrence(owner, site.index)
            .ok_or(EvalError::InvalidScript)?;
        let Some(object) = object else {
            primitive_dot(receiver, key, site.span)?;
            return false_set(site);
        };
        let mut work = self.property_work;
        let fail = |error| property_stop(error, site.span);
        let lookup = self
            .heap
            .lookup(object, key, self.realm.strings.units(), &mut work)
            .map_err(fail)?;
        match lookup {
            Lookup::Incomplete => {
                return Err(unsupported_property(
                    site.span,
                    PropertyFault::IncompleteOwnShape,
                ));
            }
            Lookup::Found {
                descriptor: Descriptor::Restricted,
                ..
            } => {
                return Err(unsupported_property(
                    site.span,
                    PropertyFault::RestrictedAccessor,
                ));
            }
            Lookup::Found {
                descriptor: Descriptor::AnnexB,
                ..
            } => return Err(unsupported_property(site.span, PropertyFault::AnnexB)),
            Lookup::Found { descriptor, .. } if !descriptor.writable() => {
                self.property_work = work;
                return false_set(site);
            }
            _ => {}
        }
        self.object_receiver(rhs)?;
        let escape = Escape::check(rhs, source, self.realm.strings.units())?;
        let edit = self.heap.edit(object, lookup).map_err(fail)?;
        let key = if edit.inserts() { key } else { &[] };
        work.charge(edit.work() + key.len() + escape.units().len())
            .map_err(fail)?;
        let requests = self
            .heap_requests
            .checked_sub(edit.request_bytes())
            .ok_or(EvalError::AllocationFailed)?;
        let append = self
            .realm
            .strings
            .plan(key, escape.units(), &mut self.copied, site.span)?;
        let row = reserve_row(edit).map_err(fail)?;
        let [key, value] = reserve_append(append)?.publish();
        row.publish(key, escape.finish(value));
        self.property_work = work;
        self.heap_requests = requests;
        Ok(())
    }
}

impl Bindings for Execution<'_, '_> {
    type Error = Stop;
    const CALLS: bool = true;

    fn retained_units(&self) -> &[u16] {
        self.realm.strings.units()
    }

    fn artifacts(&self) -> &[Artifact] {
        self.callable.artifacts
    }

    fn active_artifact(&self) -> Option<u8> {
        self.active
            .map(|origin| origin.artifact)
            .or(self.root_artifact)
    }

    fn select_body(&mut self, origin: Option<FunctionOrigin>) {
        self.active = origin;
    }

    fn non_callable(&self, span: Span) -> Stop {
        Stop::Throw(Throw::TypeError { span })
    }

    fn charge_call(&mut self, work: usize) -> Result<(), Stop> {
        self.lookup
            .charge(work)
            .map_err(|_| EvalError::GlobalLookupLimitExceeded.into())
    }

    fn function_origin(&self, id: FunctionId) -> Result<FunctionOrigin, Stop> {
        self.heap
            .origin(id, self.functions)
            .ok_or(EvalError::InvalidScript.into())
    }

    fn properties(&mut self) -> Option<&mut dyn Properties<Error = Stop>> {
        Some(self)
    }

    fn classify_callable(&self, id: FunctionId, span: Span) -> Result<CallTarget, Stop> {
        self.callable_object(id)?;
        match id {
            FunctionId::User(_) => self.function_origin(id).map(CallTarget::User),
            FunctionId::Intrinsic(Intrinsic::FunctionPrototype) => {
                Ok(CallTarget::FunctionPrototype)
            }
            FunctionId::Intrinsic(tag) => {
                Err(Stop::Engine(EngineFault::UnsupportedIntrinsicCall {
                    span,
                    intrinsic: match tag {
                        Intrinsic::ObjectConstructor => IntrinsicCall::Object,
                        Intrinsic::FunctionConstructor => IntrinsicCall::Function,
                        Intrinsic::FunctionPrototype => unreachable!(),
                    },
                }))
            }
        }
    }

    fn create_function(&mut self, body: u16) -> Result<FunctionId, Stop> {
        if self.active.is_some() {
            return Err(EvalError::InvalidScript.into());
        }
        let artifact = self.root_artifact.ok_or(EvalError::InvalidScript)?;
        let origin = FunctionOrigin { artifact, body };
        let (artifact, template) = self.callable.body(origin).ok_or(EvalError::InvalidScript)?;
        if !matches!(template.kind, BodyKind::Expression(_)) {
            return Err(EvalError::InvalidScript.into());
        }
        let plan = Creation::plan(
            self.heap,
            self.functions,
            origin.artifact,
            artifact,
            std::slice::from_ref(&origin),
        )
        .map_err(creation_stop)?;
        let mut lookup = self.lookup;
        let mut property_work = self.property_work;
        // Preserve the origin write/move debit in the existing call setup
        // budget. Initial shape also debits the ledger shared with property
        // packets, lookups and writes.
        lookup
            .charge(1 + plan.function_moves())
            .map_err(|_| EvalError::GlobalLookupLimitExceeded)?;
        property_work
            .charge(plan.heap_work())
            .map_err(|_| Stop::Engine(EngineFault::PropertyWorkLimitExceeded))?;
        let requests = self
            .heap_requests
            .checked_sub(plan.request_bytes())
            .ok_or(EvalError::AllocationFailed)?;
        let id = plan.reserve().map_err(creation_stop)?.publish();
        self.lookup = lookup;
        self.property_work = property_work;
        self.heap_requests = requests;
        Ok(id)
    }

    fn load(&mut self, index: usize) -> Result<Operand, Stop> {
        let (target, span, role) = self.site(index)?;
        match role {
            Role::Declaration(_) | Role::Var { .. } | Role::Function { .. } | Role::Local => {
                Err(EvalError::InvalidScript.into())
            }
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
        stack: &mut Stack<'_>,
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
        stack: &mut Stack<'_>,
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
        stack: &mut Stack<'_>,
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

    mod packet_checkpoint {
        use super::*;
        use crate::{
            eval::{
                bindings::{CallTarget, DotSite, Properties},
                validate_property_shape,
            },
            parser::{BARE_CALL, ShortCircuitCondition},
        };
        use Instruction::*;

        const CALL_SPAN: Span = Span { start: 10, end: 12 };
        const DOT_SPAN: Span = Span { start: 3, end: 5 };
        // These are scripted classifications at the adapter boundary, not
        // objects, descriptors, or claims about the retained identity table.
        const IMMEDIATE: FunctionId = FunctionId::Intrinsic(Intrinsic::FunctionPrototype);
        const OBJECT: FunctionId = FunctionId::Intrinsic(Intrinsic::ObjectConstructor);
        const FUNCTION: FunctionId = FunctionId::Intrinsic(Intrinsic::FunctionConstructor);

        #[derive(Debug, PartialEq)]
        enum Fault {
            Eval(EvalError),
            Get,
            Argument,
            PacketWork,
            NonCallable(Span),
            ObjectCall(Span),
            FunctionCall(Span),
        }

        impl From<EvalError> for Fault {
            fn from(error: EvalError) -> Self {
                Self::Eval(error)
            }
        }

        impl From<Conversion> for Fault {
            fn from(error: Conversion) -> Self {
                EvalError::from(error).into()
            }
        }

        #[derive(Debug, PartialEq)]
        enum Event {
            Debit(usize),
            Get(f64),
            Argument,
            Put(f64, f64),
            Setup(usize),
            Store,
        }

        // A transcript seam, not even a simulated property table. Genuine
        // user-call bytecode uses the existing FunctionOrigin/Artifact ABI.
        struct Trace<'a> {
            code: CallableCode<'a>,
            events: Vec<Event>,
            read: Operand,
            replace_read_on_arg: Option<Operand>,
            reject_get: bool,
            reject_arg: bool,
            first_visit: bool,
            credits: usize,
        }

        fn scalar(value: f64) -> Operand {
            Operand::Scalar(Value::Number(value))
        }

        fn number(value: Operand) -> f64 {
            let Operand::Scalar(Value::Number(number)) = value else {
                panic!("not a Number")
            };
            number
        }

        impl<'a> Trace<'a> {
            fn new(artifacts: &'a [Artifact]) -> Self {
                Self {
                    code: CallableCode { artifacts },
                    events: Vec::new(),
                    read: scalar(22.0),
                    replace_read_on_arg: None,
                    reject_get: false,
                    reject_arg: false,
                    first_visit: true,
                    credits: 128 * 1024,
                }
            }

            fn run(&mut self, ops: &[Instruction], peak: usize) -> Result<Operand, Fault> {
                dispatch(
                    Frame::root(ops, peak, ExitStack::Value),
                    new_stack(peak)?,
                    &[],
                    self.code,
                    &mut EvalScratch::new(),
                    self,
                )?
                .ok_or(EvalError::InvalidScript.into())
            }
        }

        impl Properties for Trace<'_> {
            type Error = Fault;

            fn dot_metadata(
                &self,
                owner: Option<FunctionOrigin>,
                index: u16,
            ) -> Result<(Span, CodeMode), Fault> {
                if index != owner.map_or(0, |origin| origin.body + 1) {
                    return Err(EvalError::InvalidScript.into());
                }
                Ok((
                    DOT_SPAN,
                    owner.map_or(CodeMode::Sloppy, |origin| {
                        self.code.body(origin).unwrap().1.mode
                    }),
                ))
            }

            fn charge_packets(&mut self, work: usize) -> Result<(), Fault> {
                self.events.push(Event::Debit(work));
                self.credits = self.credits.checked_sub(work).ok_or(Fault::PacketWork)?;
                Ok(())
            }

            fn get_dot(
                &mut self,
                site: DotSite,
                recv: Operand,
                _: TransientStrings<'_>,
            ) -> Result<Operand, Fault> {
                assert_eq!(site.span, DOT_SPAN);
                assert_eq!(site.index, site.owner.map_or(0, |origin| origin.body + 1));
                assert!(
                    site.mode
                        == if site.owner.is_some() {
                            CodeMode::Strict
                        } else {
                            CodeMode::Sloppy
                        }
                );
                self.events.push(Event::Get(number(recv)));
                if self.reject_get {
                    Err(Fault::Get)
                } else {
                    Ok(self.read)
                }
            }

            fn put_dot(
                &mut self,
                site: DotSite,
                recv: Operand,
                rhs: Operand,
                _: TransientStrings<'_>,
            ) -> Result<(), Fault> {
                assert_eq!(site.span, DOT_SPAN);
                assert_eq!(site.index, site.owner.map_or(0, |origin| origin.body + 1));
                assert!(
                    site.mode
                        == if site.owner.is_some() {
                            CodeMode::Strict
                        } else {
                            CodeMode::Sloppy
                        }
                );
                self.events.push(Event::Put(number(recv), number(rhs)));
                Ok(())
            }
        }

        impl Bindings for Trace<'_> {
            type Error = Fault;
            const CALLS: bool = true;

            fn properties(&mut self) -> Option<&mut dyn Properties<Error = Fault>> {
                Some(self)
            }
            fn non_callable(&self, span: Span) -> Fault {
                Fault::NonCallable(span)
            }
            fn classify_callable(&self, id: FunctionId, span: Span) -> Result<CallTarget, Fault> {
                match id {
                    IMMEDIATE => Ok(CallTarget::FunctionPrototype),
                    OBJECT => Err(Fault::ObjectCall(span)),
                    FUNCTION => Err(Fault::FunctionCall(span)),
                    _ => Ok(CallTarget::User(self.function_origin(id)?)),
                }
            }
            fn function_origin(&self, id: FunctionId) -> Result<FunctionOrigin, Fault> {
                let origin = FunctionOrigin {
                    artifact: 0,
                    body: match id {
                        FunctionId::User(index) => index,
                        _ => return Err(EvalError::InvalidScript.into()),
                    },
                };
                self.code.body(origin).ok_or(EvalError::InvalidScript)?;
                Ok(origin)
            }
            fn charge_call(&mut self, work: usize) -> Result<(), Fault> {
                self.events.push(Event::Setup(work));
                Ok(())
            }
            fn load(&mut self, slot: usize) -> Result<Operand, Fault> {
                match slot {
                    0 => Ok(Operand::Function(FunctionId::User(0))),
                    1 => {
                        self.events.push(Event::Argument);
                        if let Some(value) = self.replace_read_on_arg {
                            self.read = value;
                        }
                        if self.reject_arg {
                            Err(Fault::Argument)
                        } else {
                            Ok(scalar(9.0))
                        }
                    }
                    2 => Ok(Operand::Function(FunctionId::User(1))),
                    3 => Ok(Operand::Scalar(Value::Boolean(std::mem::take(
                        &mut self.first_visit,
                    )))),
                    _ => Err(EvalError::InvalidScript.into()),
                }
            }
            fn init(
                &mut self,
                _: usize,
                _: BindingKind,
                _: usize,
                _: &mut Stack<'_>,
                _: TransientStrings<'_>,
            ) -> Result<(), Fault> {
                self.events.push(Event::Store);
                Err(EvalError::InvalidScript.into())
            }
            fn store(
                &mut self,
                _: usize,
                keep: bool,
                stack: &mut Stack<'_>,
                _: TransientStrings<'_>,
            ) -> Result<(), Fault> {
                store_operand(stack, keep)?;
                self.events.push(Event::Store);
                Ok(())
            }
            fn update(
                &mut self,
                _: usize,
                _: Update,
                _: UpdateResult,
                _: &mut Stack<'_>,
                _: usize,
                _: UpdateNumeric<'_, '_>,
            ) -> Result<(), Fault> {
                Err(EvalError::InvalidScript.into())
            }
        }

        fn call(member: u16, argc: u8) -> Instruction {
            Call {
                start: CALL_SPAN.start as u32,
                end: CALL_SPAN.end as u32,
                member,
                argc,
            }
        }

        fn body(ops: Vec<Instruction>, peak: usize, formals: usize, locals: usize) -> FunctionBody {
            FunctionBody {
                kind: BodyKind::Declaration,
                instructions: ops,
                max_stack_depth: peak,
                mode: CodeMode::Strict,
                layout: FrameLayout::new(formals, locals).unwrap(),
                accesses: 0..0,
            }
        }

        fn artifact(bodies: Vec<FunctionBody>) -> [Artifact; 1] {
            [Artifact {
                bodies,
                body_names: vec![],
                dot: DotCode::default(),
                string_units: vec![],
                literal_units: 0,
                names: vec![],
                sites: vec![],
            }]
        }

        fn shape(ops: &[Instruction], peak: usize, credits: &mut usize) -> Result<(), Fault> {
            validate_property_shape(
                ops,
                peak,
                ExitStack::Value,
                |work| {
                    *credits = credits.checked_sub(work).ok_or(Fault::PacketWork)?;
                    Ok(())
                },
                |site| {
                    if site == 0 {
                        Ok(())
                    } else {
                        Err(EvalError::InvalidScript.into())
                    }
                },
            )
        }

        #[test]
        fn layout_and_requested_frame_backing_stay_compact() {
            use crate::eval::{MAX_GUEST_FRAMES, Packet};
            assert_eq!(size_of::<Instruction>(), 16);
            assert_eq!(size_of::<Operand>(), 16);
            assert_eq!(size_of::<StoredValue>(), 16);
            assert_eq!(size_of::<Packet>(), 24);
            assert_eq!(size_of::<Frame<'_>>(), 56);
            assert_eq!(MAX_GUEST_FRAMES * size_of::<Frame<'_>>(), 1792);
        }

        #[test]
        fn lexical_shape_checks_opener_identity_and_untaken_edges() {
            // The two packets have identical depth, site and kind, but the
            // branch lands inside the other opener. The true path runs fine
            // if a private test bypasses shape validation.
            let mut ops = [
                Number(4.0),
                FormDot(0),
                Boolean(true),
                JumpIfFalse(9),
                Number(1.0),
                PutDot(0),
                Discard,
                Number(2.0),
                FormDot(0),
                Number(9.0),
                PutDot(0),
            ];
            assert_eq!(
                shape(&ops, 1, &mut 100),
                Err(EvalError::InvalidScript.into())
            );
            assert_eq!(number(Trace::new(&[]).run(&ops, 1).unwrap()), 9.0);
            ops[2] = Boolean(false);
            // A reached-only check cannot distinguish openers at identical
            // height/kind/site; it will write to the original captured base.
            let mut trace = Trace::new(&[]);
            assert_eq!(number(trace.run(&ops, 1).unwrap()), 9.0);
            assert_eq!(trace.events, [Event::Debit(1), Event::Put(4.0, 9.0)]);

            let skip_opener = [Jump(3), Number(4.0), FormDot(0), Number(9.0), PutDot(0)];
            assert_eq!(
                shape(&skip_opener, 1, &mut 100),
                Err(EvalError::InvalidScript.into())
            );
            assert!(matches!(
                Trace::new(&[]).run(&skip_opener, 1),
                Err(Fault::Eval(EvalError::InvalidScript))
            ));

            for cond in [false, true] {
                let conditional = [
                    Number(4.0),
                    FormDot(0),
                    Boolean(cond),
                    JumpIfFalse(6),
                    Number(1.0),
                    Jump(7),
                    Number(2.0),
                    PutDot(0),
                ];
                assert_eq!(shape(&conditional, 1, &mut 26), Ok(())); // 9 + 8 + 9
                assert_eq!(shape(&conditional, 1, &mut 25), Err(Fault::PacketWork));
                let result = Trace::new(&[]).run(&conditional, 1).unwrap();
                assert_eq!(number(result), if cond { 1.0 } else { 2.0 });

                let short = [
                    Number(4.0),
                    FormDot(0),
                    Number(if cond { 1.0 } else { 0.0 }),
                    ShortCircuit {
                        target: 5,
                        condition: ShortCircuitCondition::Truthy,
                    },
                    LoadBinding(1),
                    PutDot(0),
                ];
                assert_eq!(shape(&short, 1, &mut 20), Ok(())); // 7 + 6 + 7
                let mut trace = Trace::new(&[]);
                assert_eq!(
                    number(trace.run(&short, 1).unwrap()),
                    if cond { 1.0 } else { 9.0 }
                );
                assert_eq!(trace.events.contains(&Event::Argument), !cond);
            }
            let bad_short = [
                Number(4.0),
                FormDot(0),
                Boolean(false),
                ShortCircuit {
                    target: 6,
                    condition: ShortCircuitCondition::Truthy,
                },
                Number(9.0),
                PutDot(0),
            ];
            assert_eq!(
                shape(&bad_short, 1, &mut 100),
                Err(EvalError::InvalidScript.into())
            );
            assert!(Trace::new(&[]).run(&bad_short, 1).is_ok());
        }

        #[test]
        fn property_layout_and_metadata_are_checked_even_without_artifact_or_call() {
            let ops = [
                Number(5.0),
                Number(6.0),
                FormDot(0),
                Add,
                Number(7.0),
                PutDot(0),
            ];
            // An invalid operand consumer isn't a structural/shape rejection.
            assert_eq!(shape(&ops, 2, &mut 100), Ok(()));
            assert!(matches!(
                Trace::new(&[]).run(&ops, 2),
                Err(Fault::Eval(EvalError::InvalidScript))
            ));
            let peak = [
                Number(0.0),
                Number(1.0),
                Number(2.0),
                Number(3.0),
                GetDot(0),
                Jump(6),
            ];
            assert_eq!(
                validate_layout(&peak, 4, ExitStack::Value),
                Err(EvalError::InvalidScript)
            );
            assert_eq!(shape(&peak, 4, &mut 100), Ok(()));
            let simple = [Number(4.0), FormDot(0), Number(9.0), PutDot(0)];
            assert_eq!(shape(&simple, 1, &mut 100), Ok(()));
            assert_eq!(number(Trace::new(&[]).run(&simple, 1).unwrap()), 9.0);
            let mut trace = Trace::new(&[]);
            for site in [1, crate::eval::MAX_DOT_SITES as u16, BARE_CALL] {
                let ops = [Number(7.0), GetDot(site)];
                assert_eq!(
                    shape(&ops, 1, &mut 100),
                    Err(EvalError::InvalidScript.into())
                );
                assert!(matches!(
                    trace.run(&ops, 1),
                    Err(Fault::Eval(EvalError::InvalidScript))
                ));
            }
            assert!(trace.events.is_empty());
        }

        #[test]
        fn prepared_callee_is_pinned_and_abrupt_outcomes_are_ordered() {
            let artifacts = artifact(vec![
                body(vec![LoadLocal(0), Return { value: true }], 1, 1, 1),
                body(vec![Number(76.0), Return { value: true }], 1, 0, 0),
            ]);
            let ops = [Number(4.0), PrepareDotCall(0), LoadBinding(1), call(0, 1)];
            let mut trace = Trace::new(&artifacts);
            trace.read = Operand::Function(FunctionId::User(0));
            trace.replace_read_on_arg = Some(Operand::Function(FunctionId::User(1)));
            assert_eq!(number(trace.run(&ops, 2).unwrap()), 9.0);
            assert_eq!(
                trace.events,
                [
                    Event::Debit(1),
                    Event::Get(4.0),
                    Event::Argument,
                    Event::Setup(2)
                ]
            );

            for (value, failure) in [
                (scalar(2.0), Some(Fault::NonCallable(CALL_SPAN))),
                (
                    Operand::Function(OBJECT),
                    Some(Fault::ObjectCall(CALL_SPAN)),
                ),
                (
                    Operand::Function(FUNCTION),
                    Some(Fault::FunctionCall(CALL_SPAN)),
                ),
                (Operand::Function(IMMEDIATE), None),
            ] {
                let mut trace = Trace::new(&[]);
                trace.read = value;
                match failure {
                    Some(failure) => assert_eq!(trace.run(&ops, 2).err(), Some(failure)),
                    None => assert!(matches!(
                        trace.run(&ops, 2),
                        Ok(Operand::Scalar(Value::Undefined))
                    )),
                }
                assert_eq!(
                    trace.events,
                    [Event::Debit(1), Event::Get(4.0), Event::Argument]
                );
                trace.events.clear();
                trace.reject_arg = true;
                assert_eq!(trace.run(&ops, 2).err(), Some(Fault::Argument));
                assert_eq!(
                    trace.events,
                    [Event::Debit(1), Event::Get(4.0), Event::Argument]
                );
                trace.events.clear();
                trace.reject_get = true;
                assert_eq!(trace.run(&ops, 2).err(), Some(Fault::Get));
                assert_eq!(trace.events, [Event::Debit(1), Event::Get(4.0)]);
            }
            let mut trace = Trace::new(&[]);
            trace.credits = 0;
            assert_eq!(trace.run(&ops, 2).err(), Some(Fault::PacketWork));
            assert_eq!(trace.events, [Event::Debit(1)]); // no early Get on work failure

            let put = [Number(4.0), FormDot(0), LoadBinding(1), PutDot(0)];
            let mut trace = Trace::new(&[]);
            trace.reject_arg = true;
            assert_eq!(trace.run(&put, 1).err(), Some(Fault::Argument));
            assert_eq!(trace.events, [Event::Debit(1), Event::Argument]);
            trace.events.clear();
            trace.reject_arg = false;
            assert_eq!(number(trace.run(&put, 1).unwrap()), 9.0);
            assert_eq!(
                trace.events,
                [Event::Debit(1), Event::Argument, Event::Put(4.0, 9.0)]
            );
        }

        #[test]
        fn ordinary_mutators_and_entire_frame_operations_respect_the_floor() {
            for (op, operand) in [
                (Discard, None),
                (Void, None),
                (Typeof, None),
                (StoreBindingKeep(7), None),
                (Add, Some(Number(1.0))),
                (call(BARE_CALL, 0), None),
                (call(BARE_CALL, 1), Some(Number(1.0))),
                (PutDot(0), Some(Number(1.0))),
                (call(1, 0), None),
                (call(0, 1), None),
                (call(0, 0), Some(Number(1.0))),
            ] {
                let mut ops = vec![Number(4.0), PrepareDotCall(0)];
                ops.extend(operand);
                ops.push(op);
                let mut trace = Trace::new(&[]);
                trace.read = Operand::Function(IMMEDIATE);
                assert_eq!(
                    trace.run(&ops, 2).err(),
                    Some(EvalError::InvalidScript.into()),
                    "{op:?}"
                );
                assert_eq!(trace.events, [Event::Debit(1), Event::Get(4.0)]);
            }
            for op in [
                JumpStatement(4),
                LoopBack(0),
                JumpIfFalseStatement(4),
                ShortCircuitStatement {
                    target: 4,
                    condition: ShortCircuitCondition::Truthy,
                },
                StoreBinding(7),
                InitBinding {
                    slot: 7,
                    kind: BindingKind::Let,
                },
                Return { value: true },
                StoreLocal {
                    slot: 0,
                    keep: false,
                },
            ] {
                let ops = [Number(4.0), FormDot(0), Number(9.0), op];
                let mut trace = Trace::new(&[]);
                assert_eq!(
                    shape(&ops, 1, &mut 100),
                    Err(EvalError::InvalidScript.into())
                );
                assert_eq!(
                    trace.run(&ops, 1).err(),
                    Some(EvalError::InvalidScript.into())
                );
                assert_eq!(trace.events, [Event::Debit(1)]);
            }
            let mut trace = Trace::new(&[]);
            for ops in [
                &[Number(4.0), FormDot(0), Number(9.0)][..],
                &[Number(4.0), PrepareDotCall(0)][..],
                &[Number(4.0), call(0, 0)][..],
            ] {
                assert_eq!(
                    trace.run(ops, 1).err(),
                    Some(EvalError::InvalidScript.into())
                );
            }
            // Discard as comma can consume an operand above the pinned callee.
            trace.read = Operand::Function(IMMEDIATE);
            let ops = [
                Number(4.0),
                PrepareDotCall(0),
                Number(9.0),
                Discard,
                Number(11.0),
                call(0, 1),
            ];
            assert!(matches!(
                trace.run(&ops, 2),
                Ok(Operand::Scalar(Value::Undefined))
            ));
        }

        #[test]
        fn recursive_callee_cannot_steal_same_body_site_and_absolute_anchor() {
            for steal in [false, true] {
                let mut ops = vec![
                    LoadBinding(3),
                    JumpIfFalse(8),
                    Number(4.0),
                    FormDot(1),
                    LoadBinding(0),
                    call(BARE_CALL, 0),
                    PutDot(1),
                    Return { value: true },
                    Number(9.0),
                ];
                if steal {
                    ops.push(PutDot(1));
                }
                ops.push(Return { value: true });
                let artifacts = artifact(vec![body(ops, 1, 0, 0)]);
                let mut trace = Trace::new(&artifacts);
                let result = trace.run(&[LoadBinding(0), call(BARE_CALL, 0)], 1);
                if steal {
                    assert_eq!(result.err(), Some(EvalError::InvalidScript.into()));
                    assert!(!trace.events.iter().any(|e| matches!(e, Event::Put(..))));
                } else {
                    assert_eq!(number(result.unwrap()), 9.0);
                    assert!(trace.events.contains(&Event::Put(4.0, 9.0)));
                }
            }
            for suffix in [vec![], vec![Return { value: false }]] {
                let mut ops = vec![Number(4.0), FormDot(1)];
                ops.extend(suffix);
                let artifacts = artifact(vec![body(ops, 1, 0, 0)]);
                assert_eq!(
                    Trace::new(&artifacts)
                        .run(&[LoadBinding(0), call(BARE_CALL, 0)], 1)
                        .err(),
                    Some(EvalError::InvalidScript.into())
                );
            }
        }

        #[test]
        fn slot_frontier_includes_both_packet_kinds_and_entered_receiver() {
            // 31 recursive frames * 128 locals + root/caller prefix + 3 peak.
            let wide = || {
                body(
                    vec![
                        LoadLocal(0),
                        JumpIfFalse(8),
                        LoadBinding(0),
                        LoadLocal(0),
                        Number(1.0),
                        Subtract,
                        call(BARE_CALL, 1),
                        Return { value: true },
                        Number(9.0),
                        Return { value: true },
                    ],
                    3,
                    1,
                    128,
                )
            };
            for padding in [124, 125] {
                let fit = padding == 124;
                for opener in [FormDot(0), PrepareDotCall(0)] {
                    let artifacts = artifact(vec![wide()]);
                    let mut trace = Trace::new(&artifacts);
                    trace.read = Operand::Function(IMMEDIATE);
                    let prepared = matches!(opener, PrepareDotCall(_));
                    let filler = padding - usize::from(prepared);
                    let mut ops = vec![Number(4.0), opener];
                    ops.extend(std::iter::repeat_n(Number(0.0), filler));
                    ops.extend([LoadBinding(0), Number(30.0), call(BARE_CALL, 1)]);
                    ops.extend(std::iter::repeat_n(Discard, filler + 1));
                    ops.extend(if prepared {
                        vec![call(0, 0)]
                    } else {
                        vec![Number(9.0), PutDot(0)]
                    });
                    assert_eq!(
                        trace.run(&ops, padding + 2).err(),
                        (!fit).then_some(EvalError::LiveSlotLimitExceeded.into())
                    );
                }
                let mut caller = vec![Number(0.0); padding];
                caller.extend([LoadBinding(0), Number(30.0), call(BARE_CALL, 1)]);
                caller.extend(std::iter::repeat_n(Discard, padding + 1));
                caller.push(Return { value: false });
                let artifacts = artifact(vec![wide(), body(caller, padding + 2, 0, 0)]);
                let mut trace = Trace::new(&artifacts);
                trace.read = Operand::Function(FunctionId::User(1));
                let ops = [Number(4.0), PrepareDotCall(0), call(0, 0)];
                assert_eq!(
                    trace.run(&ops, 1).err(),
                    (!fit).then_some(EvalError::LiveSlotLimitExceeded.into())
                );
                // Same activation and peak, with a bare call (no explicit receiver).
                let ops = [LoadBinding(2), call(BARE_CALL, 0)];
                assert!(trace.run(&ops, 1).is_ok());
            }
        }

        #[test]
        fn property_free_policies_do_not_execute_dots_or_treat_member_calls_as_bare() {
            for op in [
                GetDot(0),
                FormDot(0),
                PrepareDotCall(0),
                PutDot(0),
                call(0, 0),
            ] {
                let ops = [Number(4.0), op];
                let mut scratch = EvalScratch::new();
                assert!(matches!(
                    crate::eval::evaluate(&ops, 1, 0, &[], ExitStack::Value, &mut scratch),
                    Err(EvalError::InvalidScript)
                ));
                let mut code = CompiledScript::compile("1;").unwrap();
                code.instructions = ops.to_vec();
                assert_eq!(
                    Realm::new().instantiate(&code).unwrap_err(),
                    InstantiateError::InvalidScript
                );
            }
            let ops = [Jump(2), GetDot(0), Number(4.0)];
            let result =
                crate::eval::evaluate(&ops, 1, 0, &[], ExitStack::Value, &mut EvalScratch::new());
            assert!(matches!(
                result,
                Ok(Some(Operand::Scalar(Value::Number(4.0))))
            ));
        }

        #[test]
        fn prepare_slot_fit_is_checked_before_debit_or_early_get() {
            let wide = body(
                vec![
                    LoadLocal(0),
                    JumpIfFalse(8),
                    LoadBinding(0),
                    LoadLocal(0),
                    Number(1.0),
                    Subtract,
                    call(BARE_CALL, 1),
                    Return { value: true },
                    Number(4.0),
                    PrepareDotCall(1),
                    call(1, 0),
                    Return { value: true },
                ],
                3,
                1,
                128,
            );
            let artifacts = artifact(vec![wide]);
            for padding in [124, 125] {
                let mut trace = Trace::new(&artifacts);
                trace.read = Operand::Function(IMMEDIATE);
                let mut ops = vec![Number(0.0); padding];
                ops.extend([LoadBinding(0), Number(30.0), call(BARE_CALL, 1)]);
                ops.extend(std::iter::repeat_n(Discard, padding + 1));
                ops.push(Number(9.0));
                let fit = padding == 124;
                assert_eq!(
                    trace.run(&ops, padding + 2).err(),
                    (!fit).then_some(EvalError::LiveSlotLimitExceeded.into())
                );
                assert_eq!(trace.events.contains(&Event::Debit(1)), fit);
                assert_eq!(trace.events.contains(&Event::Get(4.0)), fit);
            }
        }

        #[test]
        fn receiver_is_released_on_return_and_fallthrough() {
            let wide = || {
                body(
                    vec![
                        LoadLocal(0),
                        JumpIfFalse(8),
                        LoadBinding(0),
                        LoadLocal(0),
                        Number(1.0),
                        Subtract,
                        call(BARE_CALL, 1),
                        Return { value: true },
                        Number(9.0),
                        Return { value: true },
                    ],
                    3,
                    1,
                    128,
                )
            };
            let mut ops = vec![Number(4.0), PrepareDotCall(0), call(0, 0), Discard];
            ops.extend(std::iter::repeat_n(Number(0.0), 125));
            ops.extend([LoadBinding(0), Number(30.0), call(BARE_CALL, 1)]);
            ops.extend(std::iter::repeat_n(Discard, 126));
            ops.push(Number(9.0));
            // The second call exactly fills the 4096 frontier, so a leaked
            // receiver from either completion form would make it fail.
            for member in [vec![], vec![Return { value: false }]] {
                let artifacts = artifact(vec![wide(), body(member, 0, 0, 0)]);
                let mut trace = Trace::new(&artifacts);
                trace.read = Operand::Function(FunctionId::User(1));
                assert_eq!(number(trace.run(&ops, 127).unwrap()), 9.0);
            }
        }

        #[test]
        fn immediate_intrinsic_does_not_use_a_user_frame_at_the_depth_limit() {
            let recursive = body(
                vec![
                    LoadLocal(0),
                    JumpIfFalse(8),
                    LoadBinding(0),
                    LoadLocal(0),
                    Number(1.0),
                    Subtract,
                    call(BARE_CALL, 1),
                    Return { value: true },
                    Number(4.0),
                    PrepareDotCall(1),
                    call(1, 0),
                    Return { value: true },
                ],
                3,
                1,
                1,
            );
            let artifacts = artifact(vec![recursive]);
            let ops = [LoadBinding(0), Number(31.0), call(BARE_CALL, 1)];
            let mut trace = Trace::new(&artifacts);
            trace.read = Operand::Function(IMMEDIATE);
            assert!(matches!(
                trace.run(&ops, 2),
                Ok(Operand::Scalar(Value::Undefined))
            ));
            assert_eq!(
                trace
                    .events
                    .iter()
                    .filter(|e| **e == Event::Setup(2))
                    .count(),
                32
            );
            trace.read = Operand::Function(FunctionId::User(0));
            assert_eq!(
                trace.run(&ops, 2).err(),
                Some(EvalError::CallDepthLimitExceeded.into())
            );
        }

        #[test]
        fn requested_packet_tiers_charge_moves_but_reuse_after_removal() {
            use crate::eval::{Packet, PacketKind, Packets};
            let mut trace = Trace::new(&[]);
            let mut packets = Packets::default();
            let mut largest_overlap = 0;
            for at in 0..4096 {
                let old = packets.tier;
                packets.reserve_push(0, 0, 0, &mut trace).unwrap();
                if old != packets.tier {
                    largest_overlap = largest_overlap.max(old + packets.tier);
                }
                packets.rows.push(Packet {
                    captured: scalar(4.0),
                    site: 0,
                    anchor: at,
                    kind: PacketKind::Form,
                });
            }
            assert_eq!((packets.tier, largest_overlap), (4096, 6144));
            assert_eq!(trace.credits, 128 * 1024 - (4096 + 4095));
            let events = trace.events.len();
            assert_eq!(
                packets.reserve_push(0, 0, 0, &mut trace),
                Err(EvalError::LiveSlotLimitExceeded.into())
            );
            assert_eq!(trace.events.len(), events);
            packets.rows.pop();
            packets.reserve_push(0, 0, 0, &mut trace).unwrap();
            assert_eq!(trace.events.last(), Some(&Event::Debit(1)));
            assert_eq!(packets.tier, 4096);

            // Reach both kinds on one LIFO stack, removing Prepare on the
            // immediate path then Form without spending more backing.
            let ops = [
                Number(4.0),
                FormDot(0),
                Number(6.0),
                PrepareDotCall(0),
                call(0, 0),
                Discard,
                Number(9.0),
                PutDot(0),
            ];
            let mut trace = Trace::new(&[]);
            trace.read = Operand::Function(IMMEDIATE);
            assert_eq!(number(trace.run(&ops, 1).unwrap()), 9.0);
            assert_eq!(
                trace.events,
                [
                    Event::Debit(1),
                    Event::Debit(2),
                    Event::Get(6.0),
                    Event::Put(4.0, 9.0)
                ]
            );
        }
    }

    #[test]
    fn initial_names_belong_to_creators_not_result_values_or_stores() {
        let source = "
            var a = function(){}, b = (function(){});
            let c = ((function(){})); const d = function(){};
            var x, y;
            x = function(){}; x &&= function(){}; x ||= function(){}; x ??= function(){};
            x = y = function(){}; var z = (y = (function(){}));
            x = (0, (function(){})); x = true ? function(){} : function(){};
            x = false || function(){}; x = function(){} || false;
            x += function(){}; x = void function(){};
            x = (function(){})(); x = y; (function(){}); x(function(){});
            for (var counter = function(){}; false; x = function(){}) {}
        ";
        let code = CompiledScript::compile(source).unwrap();
        let names = code
            .functions
            .iter()
            .map(|body| {
                let BodyKind::Expression(name) = body.kind else {
                    panic!("not an expression")
                };
                name.map(|range| std::str::from_utf8(range.get(&code.names).unwrap()).unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                Some("a"),
                Some("b"),
                Some("c"),
                Some("d"),
                Some("x"),
                Some("x"),
                Some("x"),
                Some("x"),
                Some("y"),
                Some("y"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some("counter"),
                Some("x")
            ]
        );
        assert!(CompiledScript::compile("var x; (x) = function(){};").is_err());
    }

    #[test]
    fn mostly_root_scripts_retain_body_data_but_not_ready_instructions() {
        let code =
            CompiledScript::compile(&format!("function f(){{}}{}", "1;".repeat(1800))).unwrap();
        assert_eq!(code.instructions.len(), 3599);
        assert_eq!(code.functions.len(), 1);
        assert!(code.functions[0].instructions.is_empty());
        // Scan root; reserve and visit one body; copy one site and name byte.
        // Initial name provenance adds a table entry, site walk and name check.
        let stage_cost = code.instructions.len() + 8;
        assert!(matches!(
            Artifact::stage(&code, 0, &mut Work(stage_cost - 1)),
            Err(InstantiateError::WorkLimitExceeded)
        ));
        let (_, bytes) = Artifact::stage(&code, 0, &mut Work(stage_cost)).unwrap();
        assert!(bytes * MAX_ARTIFACTS <= MAX_ARTIFACT_BYTES);
        let old_backing = bytes + code.instructions.len() * size_of::<Instruction>();
        assert!(old_backing * MAX_ARTIFACTS > MAX_ARTIFACT_BYTES);
        let mut realm = Realm::new();
        for admitted in 1..=MAX_ARTIFACTS {
            assert_eq!(
                realm.instantiate(&code).unwrap().execute(),
                Ok(Completion::Normal)
            );
            assert_eq!(
                (
                    realm.artifacts.len(),
                    realm.artifact_bytes,
                    realm.functions.len()
                ),
                (admitted, admitted * bytes, admitted)
            );
        }
        assert_eq!(
            realm.instantiate(&code).unwrap_err(),
            InstantiateError::ArtifactLimitExceeded
        );
        assert_eq!(
            (
                realm.artifacts.len(),
                realm.artifact_bytes,
                realm.functions.len()
            ),
            (MAX_ARTIFACTS, MAX_ARTIFACTS * bytes, MAX_ARTIFACTS)
        );
    }

    #[test]
    fn reached_forged_creator_cannot_borrow_an_unrelated_artifact() {
        let mut realm = Realm::new();
        assert_eq!(
            run(&mut realm, "false && function(){};"),
            Ok(Completion::Normal)
        );
        assert_eq!((realm.artifacts.len(), realm.functions.len()), (1, 0));
        let bytes = realm.artifact_bytes;
        let mut forged = CompiledScript::compile("1;").unwrap();
        assert!(forged.functions.is_empty());
        forged.instructions[0] = Instruction::CreateFunction(0);
        let ready = realm.instantiate(&forged).unwrap();
        assert!(ready.root_artifact.is_none());
        assert_eq!(
            ready.execute(),
            Err(EngineFault::Evaluation(EvalError::InvalidScript))
        );
        assert_eq!(
            (
                realm.artifacts.len(),
                realm.artifact_bytes,
                realm.functions.capacity()
            ),
            (1, bytes, 0)
        );
    }

    #[test]
    fn expression_retention_on_abandon_skip_and_atomic_stage_failure() {
        let code = CompiledScript::compile("var newVar; let x; false && function(){};").unwrap();
        let (_, payload) = Artifact::stage(&code, 0, &mut Work(PREFLIGHT_WORK)).unwrap();
        let mut realm = Realm::new();
        realm.artifact_bytes = MAX_ARTIFACT_BYTES - payload + 1;
        assert_eq!(
            realm.instantiate(&code).unwrap_err(),
            InstantiateError::ArtifactBytesLimitExceeded
        );
        assert!(realm.state.globals.is_empty() && realm.state.object.vars.is_empty());
        assert_eq!(
            (realm.artifacts.capacity(), realm.functions.capacity()),
            (0, 0)
        );
        realm.artifact_bytes -= 1;
        drop(realm.instantiate(&code).unwrap());
        assert_eq!(
            (
                realm.artifacts.len(),
                realm.artifact_bytes,
                realm.functions.len()
            ),
            (1, MAX_ARTIFACT_BYTES, 0)
        );
        let mut realm = Realm::new();
        let code = CompiledScript::compile("false && function(){};").unwrap();
        for _ in 0..MAX_ARTIFACTS {
            assert_eq!(
                realm.instantiate(&code).unwrap().execute(),
                Ok(Completion::Normal)
            );
        }
        assert_eq!(
            (realm.artifacts.len(), realm.functions.capacity()),
            (MAX_ARTIFACTS, 0)
        );
        assert_eq!(
            realm.instantiate(&code).unwrap_err(),
            InstantiateError::ArtifactLimitExceeded
        );
    }

    #[test]
    fn expression_create_growth_is_charged_before_append_and_shares_the_gdi_cap() {
        let mut realm = Realm::new();
        let code = CompiledScript::compile("function decl(){} (function(){});").unwrap();
        let mut ready = realm.instantiate(&code).unwrap();
        let mut execution = ready.execution();
        assert_eq!(
            (execution.functions.len(), execution.functions.capacity()),
            (1, 1)
        );
        execution.lookup = Work(1);
        assert!(matches!(
            execution.create_function(1),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::GlobalLookupLimitExceeded
            )))
        ));
        assert_eq!(
            (execution.functions.len(), execution.functions.capacity()),
            (1, 1)
        );
        execution.lookup = Work(2);
        assert!(matches!(
            execution.create_function(0),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::InvalidScript
            )))
        ));
        assert_eq!(execution.create_function(1).ok(), Some(FunctionId::User(1)));
        assert_eq!(
            (
                execution.functions.len(),
                execution.functions.capacity(),
                execution.lookup.0
            ),
            (2, 2, 0)
        );
        drop(ready);
        // Grow during dispatch while retaining an earlier identity in a var.
        assert_eq!(
            run(
                &mut realm,
                "var marker=0, f, keep=decl; for (; marker < 510; marker++) { f = function(){}; }"
            ),
            Ok(Completion::Normal)
        );
        assert_eq!(
            (realm.functions.len(), realm.functions.capacity()),
            (MAX_FUNCTIONS, MAX_FUNCTIONS)
        );
        assert_eq!(
            run(
                &mut realm,
                "if (keep !== decl || f === decl) bad; keep(); f(); false && function(){};"
            ),
            Ok(Completion::Normal)
        );
        assert_eq!(
            run(&mut realm, "f = (marker++, function(){});"),
            Err(EngineFault::Evaluation(EvalError::FunctionLimitExceeded))
        );
        assert_eq!(
            run(
                &mut realm,
                "if (marker !== 511 || typeof f !== 'function') bad;"
            ),
            Ok(Completion::Normal)
        );
        let code = CompiledScript::compile("function later(){} let notPublished;").unwrap();
        assert_eq!(
            realm.instantiate(&code).unwrap_err(),
            InstantiateError::FunctionLimitExceeded
        );
        assert!(realm.state.globals.is_empty());
    }

    #[test]
    fn private_expression_artifact_rejects_forged_creators_names_and_child_creation() {
        for corruption in 0..5 {
            let mut realm = Realm::new();
            let mut code =
                CompiledScript::compile("function d(){} let x = function(){return 1;};").unwrap();
            match corruption {
                0 => code.instructions[0] = Instruction::CreateFunction(0),
                1 => code.instructions[0] = Instruction::CreateFunction(u16::MAX),
                2 => code.instructions[0] = Instruction::CreateFunction(1),
                3 => {
                    code.functions[1].kind = BodyKind::Expression(Some(NameRange {
                        start: 0,
                        end: u16::MAX,
                    }))
                }
                4 => code.functions[1].instructions[0] = Instruction::CreateFunction(1),
                _ => unreachable!(),
            }
            if corruption == 2 {
                code.instructions.push(Instruction::CreateFunction(1));
            }
            assert_eq!(
                realm.instantiate(&code).unwrap_err(),
                InstantiateError::InvalidScript
            );
            assert!(
                (realm.state.globals.is_empty() && realm.state.object.vars.is_empty())
                    && realm.artifacts.is_empty()
                    && realm.functions.is_empty()
            );
        }
    }

    #[test]
    fn artifact_backing_exact_cap_and_late_gdi_failure_leave_replacement_unpublished() {
        let mut realm = Realm::new();
        let first = CompiledScript::compile("function f(){return 7;} var old = f;").unwrap();
        assert_eq!(
            realm.instantiate(&first).unwrap().execute(),
            Ok(Completion::Normal)
        );
        let replacement =
            CompiledScript::compile("function f(){return 'new';} let fresh; var extra;").unwrap();
        let (_, payload) = Artifact::stage(&replacement, 0, &mut Work(PREFLIGHT_WORK)).unwrap();
        let baseline = realm.artifact_bytes;
        let state = |realm: &Realm| {
            (
                realm.artifacts.len(),
                realm.artifacts.capacity(),
                realm.functions.len(),
                realm.functions.capacity(),
                realm.state.globals.len(),
                realm.state.globals.capacity(),
                realm.state.object.vars.len(),
                realm.state.object.vars.capacity(),
            )
        };
        let before = state(&realm);
        realm.artifact_bytes = MAX_ARTIFACT_BYTES - payload + 1;
        assert_eq!(
            realm.instantiate(&replacement).unwrap_err(),
            InstantiateError::ArtifactBytesLimitExceeded
        );
        assert_eq!(state(&realm), before);
        assert_eq!(realm.artifact_bytes, MAX_ARTIFACT_BYTES - payload + 1);
        let probe = CompiledScript::compile(
            "if (old !== f || f() !== 7 || typeof(extra) !== 'undefined') bad;",
        )
        .unwrap();
        assert_eq!(
            realm.instantiate(&probe).unwrap().execute(),
            Ok(Completion::Normal)
        );
        realm.artifact_bytes = MAX_ARTIFACT_BYTES - payload;
        drop(realm.instantiate(&replacement).unwrap());
        assert_eq!(realm.artifact_bytes, MAX_ARTIFACT_BYTES);
        let probe =
            CompiledScript::compile("if (old === f || old() !== 7 || f() !== 'new') bad;").unwrap();
        assert_eq!(
            realm.instantiate(&probe).unwrap().execute(),
            Ok(Completion::Normal)
        );
        // The test adjusted the measured total only, never the stored backing.
        realm.artifact_bytes = baseline + payload;
    }

    #[test]
    fn root_cannot_read_an_unlinked_body_site_even_with_typeof() {
        let mut code =
            CompiledScript::compile("function f(){return typeof missing;} typeof absent;").unwrap();
        code.instructions[0] = Instruction::LoadBinding(code.functions[0].accesses.start);
        let mut realm = Realm::new();
        assert_eq!(
            realm.instantiate(&code).unwrap().execute(),
            Err(EngineFault::Evaluation(EvalError::InvalidScript))
        );
        let probe = CompiledScript::compile("if (f() !== 'undefined') bad;").unwrap();
        assert_eq!(
            realm.instantiate(&probe).unwrap().execute(),
            Ok(Completion::Normal)
        );
    }

    #[test]
    fn preflight_rejects_forged_body_ranges_roles_and_names() {
        let source = "function f(x){return loc + global; var loc;} function g(){return other;} function h(){}";
        let invalid_layouts: [fn(&mut CompiledScript); 12] = [
            |code| code.functions[0].layout.formals = 3,
            |code| code.functions[0].layout.formals = MAX_PARAMETERS + 1,
            |code| code.functions[0].layout.locals = MAX_FRAME_LOCALS + 1,
            |code| code.functions[0].accesses.start = code.declarations - 1,
            |code| code.functions[1].accesses.start -= 1,
            |code| code.functions[2].accesses = code.declarations..code.declarations,
            |code| code.functions[2].accesses.end -= 1,
            |code| code.functions[2].accesses.end += 1,
            |code| code.sites[code.declarations].end = 1,
            |code| code.sites[code.declarations + 1].role = Role::Var { initialized: false },
            |code| {
                let site = &mut code.sites[code.declarations + 1];
                site.end = site.start;
            },
            |code| code.sites[code.declarations + 1].end = u16::MAX,
        ];
        let mut realm = Realm::new();
        let seed = CompiledScript::compile("function f(){return 17;} var old = f;").unwrap();
        assert_eq!(
            realm.instantiate(&seed).unwrap().execute(),
            Ok(Completion::Normal)
        );
        let mut check = |code: &CompiledScript| {
            assert_eq!(
                realm.instantiate(code).unwrap_err(),
                InstantiateError::InvalidScript
            );
            assert_eq!(
                (
                    realm.artifacts.len(),
                    realm.functions.len(),
                    realm.state.object.vars.len()
                ),
                (1, 1, 2)
            );
        };
        for forge in invalid_layouts {
            let mut code = CompiledScript::compile(source).unwrap();
            forge(&mut code);
            check(&code);
        }
        let mut code = CompiledScript::compile(source).unwrap();
        code.names.resize(MAX_NAME_BYTES + 1, b'a');
        let site = &mut code.sites[code.declarations + 1];
        site.start = 0;
        site.end = (MAX_NAME_BYTES + 1) as u16;
        check(&code);
        let probe = CompiledScript::compile("if (old !== f || f() !== 17) bad;").unwrap();
        assert_eq!(
            realm.instantiate(&probe).unwrap().execute(),
            Ok(Completion::Normal)
        );
    }

    #[test]
    fn callable_initial_heap_moves_the_production_preflight_boundary() {
        use std::fmt::Write as _;

        let mut source = String::new();
        for i in 0..157 {
            write!(source, "function f{i:03}(){{}}").unwrap();
        }
        source.push_str(&"1;".repeat(663));
        let at_limit = CompiledScript::compile(&format!("{source}+1;")).unwrap();
        let over_limit = CompiledScript::compile(&format!("{source}+ +1;")).unwrap();
        assert_eq!(
            (at_limit.instructions.len(), over_limit.instructions.len()),
            (1328, 1329)
        );
        assert!(
            at_limit
                .functions
                .iter()
                .all(|body| body.instructions.is_empty())
        );

        // A fresh Realm avoids allocator-dependent old-table moves. For n
        // distinct four-byte names, selection and binding lookup each compare
        // n(n−1)/2 candidates at 5 credits. Other old work is 24n plus the
        // root instruction scan. Name provenance adds 6n (range table, site
        // walk, and ASCII validation) plus 4n widened units. The heap adds
        // 12n + 62 (two writes per initial object or row). The resulting
        // pair is exactly 131,072/131,073 without changing the ceiling.
        let mut realm = Realm::new();
        assert_eq!(
            realm.instantiate(&over_limit).unwrap_err(),
            InstantiateError::WorkLimitExceeded
        );
        assert_eq!(
            (
                realm.artifact_bytes,
                realm.artifacts.len(),
                realm.functions.len(),
                realm.state.object.vars.len()
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(
            realm.instantiate(&at_limit).unwrap().execute(),
            Ok(Completion::Normal)
        );
        assert_eq!(
            (
                realm.artifacts.len(),
                realm.functions.len(),
                realm.state.object.vars.len()
            ),
            (1, 157, 157)
        );
    }

    #[test]
    fn callable_preflight_budget_is_atomic_even_at_final_table_growth() {
        fn fresh(code: &CompiledScript) -> Realm {
            let mut realm = Realm::new();
            drop(realm.instantiate(code).unwrap());
            realm
        }
        let first = CompiledScript::compile("function f(){return 1;}").unwrap();
        let replacement =
            CompiledScript::compile("function f(){return 2;} let pending; var extra;").unwrap();
        let (mut low, mut high) = (0, PREFLIGHT_WORK);
        // Capacity-sensitive accounting measures the allocator-visible growth
        // boundary portably. Each probe starts at the identical Realm shape.
        while high - low > 1 {
            let mut realm = fresh(&first);
            let middle = (low + high) / 2;
            if realm.instantiate_with_work(&replacement, middle).is_ok() {
                high = middle;
            } else {
                low = middle;
            }
        }
        let mut realm = fresh(&first);
        let caps = (
            realm.artifact_bytes,
            realm.artifacts.capacity(),
            realm.functions.capacity(),
            realm.state.globals.capacity(),
            realm.state.object.vars.capacity(),
        );
        assert_eq!(
            realm
                .instantiate_with_work(&replacement, high - 1)
                .unwrap_err(),
            InstantiateError::WorkLimitExceeded
        );
        assert_eq!(
            caps,
            (
                realm.artifact_bytes,
                realm.artifacts.capacity(),
                realm.functions.capacity(),
                realm.state.globals.capacity(),
                realm.state.object.vars.capacity()
            )
        );
        assert_eq!(
            (
                realm.artifacts.len(),
                realm.functions.len(),
                realm.state.globals.len(),
                realm.state.object.vars.len()
            ),
            (1, 1, 0, 1)
        );
        assert_eq!(
            realm.state.object.vars[0].value,
            StoredValue::Function(FunctionId::User(0))
        );
        drop(realm.instantiate_with_work(&replacement, high).unwrap());
        assert_eq!(
            realm.state.object.vars[0].value,
            StoredValue::Function(FunctionId::User(1))
        );
    }

    #[test]
    fn local_slots_do_not_add_declarations_or_sites_for_noop_occurrences() {
        let code = CompiledScript::compile(
            "function f(p){ var p, loc; p++; var loc = p; return loc; var loc, noOp; }",
        )
        .unwrap();
        let body = &code.functions[0];
        assert_eq!(body.layout.counts(), Some((1, 3)));
        assert_eq!(code.declarations, 1);
        assert_eq!(code.names, b"f");
        assert_eq!(body.accesses, 1..5);
        assert!(
            code.sites[body.accesses.clone()].iter().all(|site| {
                matches!(site.role, Role::Local) && (site.start, site.end) == (0, 0)
            })
        );
        assert!(body.instructions.iter().any(|op| matches!(
            op,
            Instruction::StoreLocal {
                slot: 1,
                keep: false
            }
        )));
        let mut realm = Realm::new();
        let ready = realm.instantiate(&code).unwrap();
        assert!(
            ready.links[body.accesses.clone()]
                .iter()
                .all(|link| matches!(link, Target::Unlinked))
        );
        drop(ready);
        assert_eq!(realm.state.object.vars.len(), 1);
        assert!(realm.state.globals.is_empty());
        let root = CompiledScript::compile("if (f(2) !== 3) bad;").unwrap();
        assert_eq!(
            realm.instantiate(&root).unwrap().execute(),
            Ok(Completion::Normal)
        );
        // Forged post-import layout cannot enter a frame with a formal
        // prefix longer than the whole local region.
        realm.artifacts[0].bodies[0].layout.locals = 0;
        assert_eq!(
            realm.instantiate(&root).unwrap().execute(),
            Err(EngineFault::Evaluation(EvalError::InvalidScript))
        );
    }

    #[test]
    fn malformed_private_code_cannot_pop_a_caller_temporary_or_jump_to_another_body() {
        let code = CompiledScript::compile(
            "if (10+f() !== 11) bad; function f(){return 1;} function other(){}",
        )
        .unwrap();
        let mut realm = Realm::new();
        let root = CompiledScript::compile("if (10+f() !== 11) bad;").unwrap();
        drop(realm.instantiate(&code).unwrap());
        realm.artifacts[0].bodies[0].instructions[0] = Instruction::Discard;
        let invalid = Err(EngineFault::Evaluation(EvalError::InvalidScript));
        assert_eq!(realm.instantiate(&root).unwrap().execute(), invalid);
        realm.artifacts[0].bodies[0].instructions[0] = Instruction::Jump(3);
        assert_eq!(realm.instantiate(&root).unwrap().execute(), invalid);
        realm.artifacts[0].bodies[0].instructions[0] = Instruction::LoadLocal(0);
        assert_eq!(realm.instantiate(&root).unwrap().execute(), invalid);
    }

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
        assert!(realm.state.globals.is_empty());
        let ready = realm.instantiate_with_work(&code, 22).unwrap();
        assert!(matches!(
            ready.execute(),
            Ok(Completion::Throw(Throw::ReferenceError { .. }))
        ));
        assert_eq!(
            realm.state.globals[0].value,
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
        let old_capacities = (
            realm.state.globals.capacity(),
            realm.state.object.vars.capacity(),
        );
        let exact_work = 54 + usize::from(old_capacities.0 < 2) + usize::from(old_capacities.1 < 2);
        assert_eq!(
            realm
                .instantiate_with_work(&code, exact_work - 1)
                .unwrap_err(),
            InstantiateError::WorkLimitExceeded
        );
        assert_eq!(
            (
                realm.state.globals.capacity(),
                realm.state.object.vars.capacity()
            ),
            old_capacities
        );
        assert_eq!(
            (realm.state.globals.len(), realm.state.object.vars.len()),
            (1, 1)
        );
        assert!(!realm.state.object.undefined_declared);
        assert_eq!(
            realm.state.globals[0].value,
            Some(StoredValue::Scalar(Value::Number(1.0)))
        );
        assert_eq!(
            realm.state.object.vars[0].value,
            StoredValue::Scalar(Value::Number(2.0))
        );
        drop(realm.instantiate_with_work(&code, exact_work).unwrap());
        assert!(realm.state.object.undefined_declared);
        assert_eq!(
            (realm.state.globals.len(), realm.state.object.vars.len()),
            (2, 2)
        );
        assert_eq!(realm.state.globals[1].value, None);
        assert_eq!(
            realm.state.object.vars[1].value,
            StoredValue::Scalar(Value::Undefined)
        );
        assert_eq!(
            realm.state.object.vars[0].value,
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
        assert!(realm.state.object.undefined_declared);
        assert_eq!(realm.state.object.vars.len(), 1);
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
                &mut Stack::root(&mut stack),
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
                &mut Stack::root(&mut stack),
                scratch.transient(&[]),
            ));
        }
        invalid(execution.store(
            0,
            false,
            &mut Stack::root(&mut vec![operand]),
            scratch.transient(&[]),
        ));
        invalid(execution.store(
            1,
            true,
            &mut Stack::root(&mut vec![operand]),
            scratch.transient(&[]),
        ));
        for target in [Target::Lexical(0), Target::Missing, Target::Var(usize::MAX)] {
            let mut wrong = ready.links.clone();
            wrong[1] = target;
            let mut execution = ready.execution();
            execution.links = &wrong;
            invalid(execution.store(
                1,
                false,
                &mut Stack::root(&mut vec![operand]),
                scratch.transient(&[]),
            ));
        }
        assert_eq!(
            ready.realm.state.object.vars[0].value,
            StoredValue::Scalar(Value::Undefined)
        );
        assert_eq!(
            ready.realm.state.object.vars[1].value,
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
                &mut Stack::root(&mut stack),
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
        let mut stack = new_stack(2).unwrap();
        stack.push(Operand::Scalar(Value::Null));
        let scratch = EvalScratch::new();
        let mut credits = 0;
        let mut update = |site, max| {
            execution.update(
                site,
                Update::Increment,
                UpdateResult::New,
                &mut Stack::root(&mut stack),
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
        assert_eq!(realm.state.strings.chunks.capacity(), 0);
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
        assert_eq!(realm.state.strings.used, 0);
        assert_eq!(realm.state.strings.chunks.capacity(), 0);

        assert_eq!(
            run(&mut realm, r"let first = '\uD800\0'; let alias = first;"),
            Ok(Completion::Normal)
        );
        let first = realm
            .state
            .globals
            .iter()
            .find(|c| c.name == b"first")
            .unwrap()
            .value;
        let alias = realm
            .state
            .globals
            .iter()
            .find(|c| c.name == b"alias")
            .unwrap()
            .value;
        assert_eq!(first, alias);
        assert_eq!(realm.state.strings.units(), [0xD800, 0]);
        assert_eq!(realm.state.strings.chunks.len(), 1);
    }

    #[test]
    fn invalid_ranges_cannot_enter_or_leave_a_lexical_cell() {
        let mut realm = Realm::new();
        assert_eq!(run(&mut realm, "let x = 'x';"), Ok(Completion::Normal));
        assert_eq!(realm.state.strings.chunks.len(), 1);
        let original = realm.state.globals[0].value;
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
                    artifacts: &[],
                    active_artifact: None,
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
            arena
                .plan(&[7], &[], &mut copied, span)
                .and_then(|plan| plan.reserve_for_test(failed)),
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
                    arena
                        .plan(&[8], &[], &mut copied, span)
                        .and_then(|plan| plan.reserve_for_test(failed)),
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
                    .plan(&[7], &[], &mut copied, span)
                    .ok()
                    .unwrap()
                    .reserve_for_test(|chunks, additional| {
                        tiers.push(chunks.len() + additional);
                        relocated += chunks.len() * CHUNK_UNITS;
                        chunks.try_reserve_exact(additional)
                    })
                    .ok()
                    .unwrap()
                    .publish()[0]
                    .is_some()
            );
        }
        assert_eq!(tiers, [1, 2, 4, 8, 16, 32, 64, 128]);
        assert_eq!(relocated, 32_512);
        assert_eq!(arena.used, MAX_RETAINED_UNITS);
        assert_eq!(arena.units()[MAX_RETAINED_UNITS - 1], 7);
    }

    fn arena_publish(
        arena: &mut Arena,
        first: &[u16],
        second: &[u16],
        copied: &mut usize,
        span: Span,
    ) -> [Option<RealmRange>; 2] {
        arena
            .plan(first, second, copied, span)
            .and_then(|plan| plan.reserve())
            .ok()
            .unwrap()
            .publish()
    }

    #[test]
    fn arena_pair_only_issues_ranges_on_publish() {
        let mut arena = Arena::default();
        let span = Span { start: 2, end: 5 };
        let mut copied = 0;
        let empty = arena
            .plan(&[], &[], &mut copied, span)
            .ok()
            .unwrap()
            .reserve_for_test(|_, _| panic!("an empty plan must not reserve"))
            .ok()
            .unwrap()
            .publish();
        assert_eq!(empty, [None, None]);
        assert_eq!(
            (
                arena.used,
                arena.chunks.len(),
                arena.chunks.capacity(),
                copied
            ),
            (0, 0, 0, 0)
        );
        let prefix = [7; CHUNK_UNITS];
        let first = RealmRange { start: 0, end: 1 };
        let second = RealmRange {
            start: first.end,
            end: CHUNK_UNITS as u16,
        };
        assert_eq!(
            arena_publish(&mut arena, &prefix[..1], &[], &mut copied, span),
            [Some(first), None]
        );
        assert_eq!(
            arena_publish(&mut arena, &[], &prefix[1..], &mut copied, span),
            [None, Some(second)]
        );

        let key = [0xD800, 0];
        let mut value = [0xDC00; CHUNK_UNITS - 1];
        // A surrogate pair straddles the chunk boundary; the other units
        // include lone surrogates and NUL and must survive verbatim too.
        value[CHUNK_UNITS - 3..].copy_from_slice(&[0xD83D, 0xDE00]);
        let future_key = RealmRange {
            start: second.end,
            end: second.end + 2,
        };
        let future_value = RealmRange {
            start: future_key.end,
            end: (2 * CHUNK_UNITS + 1) as u16,
        };
        let capacity = arena.chunks.capacity();
        {
            let _unreserved = arena.plan(&key, &value, &mut copied, span).ok().unwrap();
        }
        assert!(matches!(
            arena
                .plan(&key, &value, &mut copied, span)
                .ok()
                .unwrap()
                .reserve_for_test(|chunks, additional| {
                    assert_eq!((chunks.len(), additional), (1, 3));
                    chunks.try_reserve_exact(usize::MAX)
                }),
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
            (CHUNK_UNITS, 1, capacity, CHUNK_UNITS)
        );
        assert_eq!(arena.units(), prefix);
        {
            let _unpublished = arena
                .plan(&key, &value, &mut copied, span)
                .and_then(|plan| plan.reserve())
                .ok()
                .unwrap();
        }
        assert_eq!(
            (arena.used, arena.chunks.len(), copied),
            (CHUNK_UNITS, 1, CHUNK_UNITS)
        );
        assert_eq!(arena.units(), prefix);
        assert_eq!(future_key.get(arena.units()), None);
        assert_eq!(future_value.get(arena.units()), None);

        assert_eq!(
            arena_publish(&mut arena, &key, &value, &mut copied, span),
            [Some(future_key), Some(future_value)]
        );
        assert_eq!(first.get(arena.units()), Some(&prefix[..1]));
        assert_eq!(second.get(arena.units()), Some(&prefix[1..]));
        assert_eq!(future_key.get(arena.units()), Some(key.as_slice()));
        assert_eq!(future_value.get(arena.units()), Some(value.as_slice()));
        assert_eq!(&arena.units()[2 * CHUNK_UNITS - 1..], &[0xD83D, 0xDE00]);
        assert_eq!(
            (arena.used, arena.chunks.len(), copied),
            (2 * CHUNK_UNITS + 1, 3, 2 * CHUNK_UNITS + 1)
        );
    }

    #[test]
    fn arena_pair_validates_both_slices_then_copy_then_retention() {
        let mut arena = Arena::default();
        let span = Span { start: 2, end: 5 };
        let fill = [7; MAX_STRING_LITERAL_CODE_UNITS];
        let key = [1; MAX_STRING_LITERAL_CODE_UNITS];
        let value = [2; MAX_STRING_LITERAL_CODE_UNITS];
        let fits = key.len() + value.len() - 1;
        let prefix = MAX_RETAINED_UNITS - fits;
        while arena.used < prefix {
            let len = (prefix - arena.used).min(fill.len());
            let mut copied = 0; // Another execute.
            assert!(arena.append(&fill[..len], &mut copied, span).is_ok());
        }
        let old = (arena.used, arena.chunks.len(), arena.chunks.capacity());
        let mut copied = MAX_COPIED_UNITS - fits;
        // These synthetic private inputs would overflow both budgets if
        // length validation did not win. No such guest operand is needed.
        let invalid = [0; MAX_STRING_LITERAL_CODE_UNITS + 1];
        for (first, second) in [(&invalid[..], &key[..]), (&key[..], &invalid[..])] {
            assert!(matches!(
                arena.plan(first, second, &mut copied, span),
                Err(Stop::Engine(EngineFault::Evaluation(
                    EvalError::InvalidScript
                )))
            ));
        }
        assert!(matches!(arena.plan(&key, &value, &mut copied, span),
            Err(Stop::Engine(EngineFault::StringCopyLimitExceeded { span: actual })) if actual == span));
        assert_eq!(copied, MAX_COPIED_UNITS - fits);
        copied -= 1;
        assert!(matches!(arena.plan(&key, &value, &mut copied, span),
            Err(Stop::Engine(EngineFault::StringRetentionLimitExceeded { span: actual })) if actual == span));
        assert_eq!(copied, MAX_COPIED_UNITS - fits - 1);
        assert_eq!(
            (arena.used, arena.chunks.len(), arena.chunks.capacity()),
            old
        );
        assert!(arena.units().iter().all(|&unit| unit == 7));

        copied += 1;
        let key = &key[..key.len() - 1];
        let [Some(k), Some(v)] = arena_publish(&mut arena, key, &value, &mut copied, span) else {
            panic!("both limits are inclusive")
        };
        assert_eq!(copied, MAX_COPIED_UNITS);
        assert_eq!(arena.used, MAX_RETAINED_UNITS);
        assert_eq!(usize::from(k.start), prefix);
        assert_eq!(usize::from(v.end), MAX_RETAINED_UNITS);
        assert_eq!(k.get(arena.units()), Some(key));
        assert_eq!(v.get(arena.units()), Some(value.as_slice()));
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
                &mut Stack::root(&mut stack),
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
