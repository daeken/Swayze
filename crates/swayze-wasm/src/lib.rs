//! Bounded WebAssembly binary validation and i32 execution.
//!
//! # Supported slice
//!
//! [`validate_module`] accepts the [WebAssembly Core 1.0] header, any number of
//! custom sections within [`Limit`], and optional memory, global, and data
//! sections. Modules with functions must have one to eight types, one to eight
//! function declarations, and exactly as many code bodies as function
//! declarations, in that section order. Each type must have zero to four `i32`
//! parameters and exactly one `i32` result, even if unused.
//! Every function declares its own zero-based index into the type table; types
//! may be shared. Each body may declare up to 16 `i32` locals in total. The
//! number of groups and each group's local count are unsigned LEB32; zero-count
//! groups are allowed but must still specify `i32`.
//! Supported instructions are `local.get`, `local.set`, `local.tee`, `global.get`,
//! `global.set` (each with an unsigned LEB32 index), `i32.const <signed LEB32>`,
//! `i32.eqz`, `i32.eq`, `i32.ne`, `i32.lt_s`, `i32.lt_u`, `i32.gt_s`, `i32.gt_u`, `i32.le_s`,
//! `i32.le_u`, `i32.ge_s`, `i32.ge_u`, `i32.clz`, `i32.ctz`, `i32.popcnt`,
//! `i32.add`, `i32.sub`, `i32.mul`, `i32.div_s`, `i32.div_u`, `i32.rem_s`, `i32.rem_u`,
//! `i32.and`, `i32.or`, `i32.xor`, `i32.shl`, `i32.shr_s`, `i32.shr_u`, `i32.rotl`,
//! `i32.rotr`, `i32.load`, `i32.load8_s`, `i32.load8_u`, `i32.load16_s`,
//! `i32.load16_u`, `i32.store`, `i32.store8`, `i32.store16`, `memory.size`,
//! `nop`, `drop`, untyped `select`, `call`, `if (result i32) ... else ... end`,
//! `block (result i32) ... end`, `loop ... end`, and `br_if <depth>`. Local indices
//! start at zero and address parameters first, then locals in declaration order.
//! Both can be assigned.
//! `call` takes an unsigned LEB32 index into the exact function declarations,
//! including forward and self references. The control constructs may nest
//! together. `if` and `block` accept only block type byte `0x7f` (one i32 result,
//! no block parameters), and `if` requires both arms. `loop` accepts only `0x40`
//! (no block parameters or results). `br_if` takes an unsigned LEB32 label
//! depth: zero names the innermost frame, and each enclosing frame, including
//! `if`, adds one to the depth. A branch may cross any of these frames but may
//! target only a `block` or `loop`. Branches to `if` or the implicit function
//! label, other block types, typed `select`, `br`, `return`, `unreachable`, and
//! indirect calls are unsupported.
//!
//! If present, the memory section follows any function section and precedes
//! any global, export, and code sections. It may be empty (a zero-length vector) or
//! declare one private, unshared memory32. The limits use flag byte `0x00`
//! (minimum only) or `0x01` (minimum and maximum), followed by unsigned LEB32
//! page counts. The minimum and any explicit maximum must be zero or one
//! 64-KiB page, and the maximum must not be less than the minimum. The minimum
//! is the fixed instance size. Loads, stores, and `memory.size` require a
//! defined memory, which may have zero pages. Loads and stores take an unsigned
//! LEB32 alignment exponent and an unsigned LEB32 byte offset. The exponent
//! must be 0 for byte accesses, at most 1 for 16-bit accesses, and at most 2 for
//! 32-bit accesses.
//! `memory.size` takes a literal reserved byte `0x00`, not a general LEB32 index.
//! Other memory instructions, including `memory.grow`, are unsupported.
//!
//! The optional global section (ID 6) follows any type, function, and memory
//! sections and precedes any export and code sections. It may contain zero to
//! eight defined `i32` globals, indexed from zero in declaration order. Each
//! declaration uses value type byte `0x7f`, literal mutability byte `0x00`
//! (immutable) or `0x01` (mutable), and exactly `i32.const <signed LEB32> end`.
//! Every body can read either kind and can set only mutable globals, including
//! globals at later indices. Global imports, exports, and initializer reads
//! are unsupported.
//!
//! The optional data section (ID 11) follows any memory, global, export, and
//! code sections. It may contain zero to eight active segments with at most 64 KiB of
//! payload bytes total, counting overlaps. Each segment requires a defined
//! memory and encodes the Core 1.0 memory index zero as unsigned LEB32, exactly
//! `i32.const <signed LEB32> end`, and a length-prefixed vector of raw bytes.
//! An empty segment vector does not require memory. Passive segments, bulk
//! memory, and global offsets are unsupported.
//!
//! An optional export section between function and code (after memory and
//! globals, if present) may contain zero to eight function exports. Memory and
//! globals cannot be exported. Name matching is exact (no Unicode normalization),
//! and duplicate export names are rejected. Each export refers to a defined
//! function by its zero-based declaration index. Distinct names may refer to
//! the same function.
//! Both export and custom names must be UTF-8.
//! Custom payloads after the name are opaque and discarded, even for well-known
//! names. All other standard sections (including import, start, and data count)
//! are rejected, even when empty.
//!
//! Parameters do not start on the operand stack. Validation starts with an empty
//! stack and checks each stack effect: `local.get`, `global.get`, and `i32.const`
//! push one i32; `local.set` and `global.set` pop one, and `local.tee` requires
//! and leaves the top operand. `nop` has no stack effect; `drop` discards the top
//! i32. Untyped `select` consumes three i32 operands pushed in first-value,
//! second-value, condition order. It pushes the first value for any nonzero
//! condition and the second for zero. Unlike `if`, `select` does not skip
//! either value's computation; a producer may trap even if its result would be
//! unselected or dropped.
//! `i32.eqz`, `i32.clz`, `i32.ctz`, and `i32.popcnt` each consume one and push
//! one; the binary operators consume two and push one. `call` consumes as many
//! i32s as the target function's type has parameters (the top operand is the
//! last parameter) and pushes one result.
//! Loads consume an i32 base address and push an i32. Stores consume an i32
//! address and an i32 value (value on top) and produce no result. `memory.size`
//! takes no operands and pushes the instance's actual page count (0 or 1).
//! `if` consumes an i32 condition from above the enclosing frame's operand floor
//! and saves the remaining depth as the floor for both arms. `else` checks for
//! exactly one new result and resets validation to the saved depth, and the
//! matching `end` checks that the else arm also leaves exactly one new result.
//! `block` and `loop` save the current depth as their floor without consuming
//! any operands. A `block` must leave exactly one new i32 at its normal exit;
//! a `loop` must leave none. Every operand access must stay above the active
//! floor. `br_if` requires an i32 condition and, for a block target, an i32
//! result immediately below it. Both must be above the active innermost frame's
//! floor, even when targeting an outer frame. With a zero condition, execution
//! pops only the condition and continues. With a nonzero condition, a block
//! branch keeps the result, truncates to the target block's floor, pushes the
//! result back, and resumes after that block's `end`. A taken loop branch
//! truncates to the target loop's floor and resumes at its body start. Both
//! taken edges preserve the target's operand prefix and locals. Both `if` arms
//! and the entire `br_if` fallthrough path are validated regardless of the
//! condition; no unreachable typing is used.
//! The function has a floor of zero; its own `end` must be the final byte of
//! the body frame and leave exactly one result. The inclusive limits are 256
//! body bytes (including the locals vector and all delimiters), 127 decoded
//! instructions (including `nop`, `drop`, `select`, `if`, `else`, and `br_if`,
//! but not `block`, `loop`, or `end`), 32 live i32 operands including those below
//! floors, and 16 simultaneously open `block`, `loop`, and `if` frames combined,
//! excluding the function frame.
//!
//! Validation resolves every function's type index and validates all global
//! declarations before decoding any body. It checks the entire input, even
//! unexported bodies and untaken arms. It returns an immutable, owned [`Module`]
//! with private decoded bodies, their arities, named export entries, the memory
//! size (if defined), global initial values, and the active data segments. It
//! does not allocate linear memory. A separate fallible [`Module::instantiate`]
//! step checks all segment ranges before allocating and zeroing fresh private
//! linear memory. It then applies the segments in source order; the last
//! segment to write a byte wins. Data offsets are interpreted as `u32` bits
//! and widened before adding the payload length. An empty segment may
//! start at the end of memory, but not beyond it. An out-of-bounds segment is an
//! [`InstantiationError::DataSegmentOutOfBounds`], not a validation error or an
//! invocation trap. Global initial values are copied into fixed private slots
//! with no allocation. Failed instantiation exposes no instance. The typed
//! [`Instance::invoke_i32_with_args`] method
//! scans at most eight export names for an exact match, then checks the selected
//! function's arity and begins execution without allocating or recursing
//! in Rust. Zero selects the else arm; every nonzero i32 selects the then arm.
//! Validation uses a fixed stack of control frames. Each target block keeps
//! a linked chain of incoming branch slots in fixed scratch storage. Validation
//! backpatches `if` and `block` targets at matching delimiters to strictly later
//! decoded instruction indices or the one-past-last index in the same body.
//! Loop continues have a separate target at or before the branch, resolved
//! from the target loop's saved body start. Each decoded `br_if` also stores
//! its target's operand floor.
//! Execution does not dispatch instructions skipped by a control transfer.
//! Each active call frame has its own fixed operand stack and local storage.
//! Parameters are copied from the supplied arguments or caller's operands, and
//! declared locals start at zero for every frame. A callee starts with no
//! operands. Its single result is pushed onto the caller's remaining operands
//! before execution resumes. [`Instance::invoke_i32`] is equivalent to passing an
//! empty argument slice. The top operand is the right-hand side of each binary
//! operation. `i32.eqz` tests for zero, and `i32.eq` and `i32.ne` compare the
//! operand bits for equality or inequality. Ordering comparisons interpret the
//! bits as signed `i32` for `_s` and unsigned `u32` for `_u`. These tests and
//! comparisons do not trap and return exactly `1` if true or `0` if false.
//! Addition, subtraction, and multiplication wrap modulo 2³². Signed
//! division truncates toward zero, and a nonzero signed remainder has the sign
//! of the dividend. Unsigned division and remainder interpret the operand bits
//! as `u32` and preserve the result bits in the returned `i32` value.
//! The bit operations do not trap. `i32.clz` and `i32.ctz` count leading and
//! trailing zero bits in a 32-bit value (both return 32 for zero); `i32.popcnt`
//! counts set bits. `i32.and`, `i32.or`, and `i32.xor` operate independently on
//! each pair of bits. Shifts and rotates use only the low five bits of the
//! right operand, even for negative counts. `i32.shl` shifts left, drops bits
//! above bit 31, and fills with zeros. `i32.shr_s` fills with the sign bit and
//! `i32.shr_u` fills with zeros. `i32.rotl` and `i32.rotr` wrap the shifted-out
//! bits to the opposite end in the indicated direction.
//! Loads and stores add the base bits interpreted as `u32` and the `u32` offset
//! in `u64`, without wrapping at 32 bits. The entire 1-, 2-, or 4-byte range
//! must be in bounds, or the instruction returns [`Trap::MemoryOutOfBounds`]
//! without reading or writing any bytes. Accesses are little-endian and may
//! be unaligned; validated alignment annotations do not constrain addresses.
//! Narrow loads sign-extend (`_s`) or zero-extend (`_u`) to i32, and narrow
//! stores write only the low 8 or 16 bits of the value.
//!
//! A zero right operand traps for all four division and remainder instructions.
//! `i32::MIN / -1` traps for signed division; signed remainder for those operands
//! returns zero. These are runtime outcomes, not validation errors.
//! [`InvokeError::Trap`] reports the first guest trap and skips all remaining
//! instructions, including in callers. Host resource limits are separate:
//! [`InvokeError::LimitExceeded`] stops the entire invocation before it would
//! exceed either 16 live frames (including the exported function) or 1024 total
//! instruction dispatches. Every executed decoded instruction costs one dispatch,
//! including `nop`, `drop`, `select`, memory and global instructions, a `call`,
//! an `if` test, an executed `else` jump, or a `br_if` with either outcome.
//! Skipped instructions, `block`, `loop`, and `end` delimiters, and returning
//! from frames cost nothing. A continue does not reset the budget: all loop
//! iterations and calls share it, including recursive guest calls.
//! An invocation can return just after its last allowed dispatch. If the next
//! instruction would exceed the dispatch budget, that limit takes precedence
//! over executing it.
//! The same instance can be invoked again with fresh call-local state and budgets.
//! All functions in an instance share its memory and globals. Completed memory
//! and global stores persist across calls, exports, traps, and host limits.
//! Separate instances have independent memory and globals. Empty, custom-only,
//! memory-only, globals-only, and other unexported modules have no callable exports.
//! [`prepare`] remains a stub.
//!
//! # Evidence and gaps
//!
//! Local tests exercise framing, UTF-8, size limits, section order and contents,
//! function/type/code and export relationships, parameter types and counts,
//! exact invocation arity, bounded LEB32 decoding (including local counts and
//! indices), local declaration types and limits, each stack effect and body
//! budget, reads and writes in the combined local index space, fresh locals on
//! successive calls, wrapping arithmetic after discarding the input, signed and
//! unsigned division and remainder edge cases, name and arity checks before
//! execution, and stopping at the first guest trap and reusing the instance.
//! Multi-function tests check dispatch by declaration index, exact UTF-8 names
//! and aliases, shared and mixed arities, traps and fresh locals across dispatches,
//! duplicate names, exact type and function index bounds, independent count caps,
//! malformed unused types, and later unexported bodies and their frame boundaries.
//! Direct-call tests cover nested and repeated calls, forward and self targets,
//! calls between different signatures, argument order, frame isolation, bounded
//! index decoding, underflow at both arm floors, zero-argument calls at the
//! operand cap, and propagation and reuse after traps or execution limits.
//! Budget tests exercise the last allowed frame and dispatch, the next
//! attempted frame and dispatch, and budget sharing across completed callees.
//! Comparison tests cover exact 0/1 results, equal operands and signed versus
//! unsigned boundary values, unary and binary underflows at the function and
//! arm floors, conditional selection, and comparisons at the instruction and
//! operand limits.
//! Bit-operation tests distinguish each opcode using exact 32-bit results.
//! They cover zero, the high bit, all bits set, masked counts including negative
//! values and `i32::MIN`, signed and unsigned right shifts, preserved operand
//! prefixes, underflow at control floors (including both untaken arms), and
//! exact decoded-instruction and dispatch budget boundaries.
//! Stack-operation tests check selection order and zero, positive, and negative
//! conditions (including signed extrema), discarded values and eager producer
//! traps, preserved operands across mixed control, exact underflow offsets
//! including in skipped code, and exact body byte, instruction, operand, and
//! dispatch limits with `nop`, including limit precedence over a guest trap.
//! Conditional tests exercise zero and nonzero conditions, nested joins,
//! preserved outer operands and selected-arm effects, suppression of untaken
//! traps, rejection of untaken invalid code, arm result counts and floor
//! violations (even when depth would later recover), delimiter errors, and
//! control, instruction and operand bounds.
//! Block and branch tests cover taken exits across mixed frame kinds, separate
//! fixup chains for inner and outer blocks, stack truncation versus fallthrough,
//! skipped traps and rejected skipped syntax, padded depths, unsupported labels,
//! delimiter errors, and exact body and execution budgets.
//! Loop tests cover outer continues across mixed frame kinds, preserved locals
//! and stack prefixes, different taken and fallthrough stack effects, full body
//! validation, shared caps, and the exact dispatch boundary in a called nested
//! loop and instance reuse after hitting the limit. Both target kinds check
//! operand underflow against the innermost floor even on an untaken path.
//! Memory tests cover exact limits, width-specific alignment and framed memarg
//! decoding, actual page counts and the reserved `memory.size` byte, absent and
//! zero-page memory, ordering and unsupported features, operand floors and
//! validation in untaken arms and unexported bodies, and zero initialization.
//! They exercise narrow-load sign boundaries, truncating stores, overlapping
//! little-endian and unaligned accesses, end-of-page accesses and straddles,
//! partial-store prevention, widened address arithmetic, independent instances,
//! and sharing across calls and exports. They also check store persistence
//! and suppression of later stores after guest traps or dispatch exhaustion.
//! Data tests cover section order and framing, index and offset encodings, the
//! count and aggregate byte limits, absent and zero-page memories, full-range
//! preflight (including wrapping offsets and empty segments at the end), overlap
//! order, and reinitialization from the same module into independent instances.
//! Global tests cover section order, declaration counts and encodings, exact
//! initializer expressions, index decoding and bounds, mutability and operand
//! floors in all bodies, and unsupported imports, exports, and global offsets.
//! They exercise shared state across exports and calls, fresh instances, and
//! suppression of later writes after a trap or exhausted dispatch budget.
//! Parsing uses fixed scratch tables for up to eight type arities, eight resolved
//! function arities, and eight global declarations. It checks local group counts
//! against the remaining local budget without expanding groups, uses bounded
//! borrowed frames, and decodes instructions into a fixed-size scratch array. After checking `end`
//! and the complete body frame, it calls `Vec::try_reserve_exact` for the count and
//! copies those instructions. The body, export, and data segment vectors are
//! reserved fallibly after checking their counts. Each name and segment payload
//! is reserved fallibly after checking its length, with an aggregate limit for
//! data payloads. The count caps bound aggregate decoding to eight body budgets and
//! duplicate checks to at most 28 comparisons of bounded names. There is no
//! unsafe code, Rust recursion, or external dependency.
//! This is not full Core 1.0 validation or a conformance or security proof.
//! No upstream Wasm spec suite or fuzzing has been run.
//!
//! [WebAssembly Core 1.0]: https://www.w3.org/TR/2019/REC-wasm-core-1-20191205/

mod binary;
mod memory;

pub use binary::{Error, ErrorKind, IntegerField, LengthField, Limit, validate_module};

use memory::{DataSegment, LinearMemory};
use std::{cell::Cell, convert::Infallible, fmt};

/// An owned, opaque module produced only by successful [`validate_module`].
///
/// ```compile_fail
/// use swayze_wasm::Module;
/// let forged = Module {
///     functions: Vec::new(), exports: Vec::new(), memory_bytes: None, data_segments: Vec::new(),
///     global_initial_values: [0; 8],
/// };
/// ```
#[derive(Debug)]
pub struct Module {
    functions: Vec<binary::ValidatedCode>,
    exports: Vec<Export>,
    // None means absent; Some is a validated byte length (zero or one page).
    memory_bytes: Option<usize>,
    global_initial_values: [i32; binary::MAX_GLOBALS as usize],
    data_segments: Vec<DataSegment>,
}

#[derive(Debug)]
struct Export {
    name: String,
    function_index: usize,
}

impl Module {
    /// Create an independent instance without executing guest code.
    ///
    /// Active data segments are checked against the fixed memory size before
    /// allocating or writing any linear memory. A one-page memory is allocated
    /// fallibly, zeroed, then initialized by the segments in source order.
    /// No linear memory is allocated for an absent or zero-page memory.
    /// Global initial values are copied into fixed, independent instance slots.
    pub fn instantiate(&self) -> Result<Instance<'_>, InstantiationError> {
        let memory = LinearMemory::new(self.memory_bytes.unwrap_or(0), &self.data_segments)?;
        Ok(Instance {
            module: self,
            memory,
            globals: self.global_initial_values.map(Cell::new),
        })
    }
}

/// Why creating an instance of a validated module failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstantiationError {
    /// The instance's linear memory could not be reserved.
    MemoryAllocationFailed,
    /// An active data segment does not fit in the initial linear memory.
    DataSegmentOutOfBounds {
        /// Zero-based index of the first out-of-bounds segment in source order.
        segment_index: usize,
    },
}

impl fmt::Display for InstantiationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryAllocationFailed => {
                f.write_str("could not reserve WebAssembly linear memory")
            }
            Self::DataSegmentOutOfBounds { segment_index } => {
                write!(
                    f,
                    "WebAssembly data segment {segment_index} is out of bounds"
                )
            }
        }
    }
}

impl std::error::Error for InstantiationError {}

/// An instance of a validated module, with private, persistent memory and globals.
///
/// Invocation through `&self` can change this state. An instance is `Send`
/// (it can move to another thread when its module borrow permits), but not
/// `Sync`: sharing one instance between threads requires synchronization.
/// The immutable module can be shared to create independent instances.
#[derive(Debug)]
pub struct Instance<'module> {
    module: &'module Module,
    memory: LinearMemory,
    globals: [Cell<i32>; binary::MAX_GLOBALS as usize],
}

impl Instance<'_> {
    /// Invoke the exact named export as a `() -> i32` function.
    /// A matching export with parameters returns [`InvokeError::ArityMismatch`].
    /// A guest trap returns [`InvokeError::Trap`]; exceeding a host execution
    /// limit returns [`InvokeError::LimitExceeded`] instead.
    ///
    /// ```
    /// use swayze_wasm::validate_module;
    /// let bytes = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7f\x03\x02\x01\0\
    ///               \x07\x0a\x01\x06answer\0\0\x0a\x09\x01\x07\0\x41\x28\x41\x02\x6a\x0b";
    /// let module = validate_module(bytes)?;
    /// assert_eq!(module.instantiate()?.invoke_i32("answer")?, 42);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn invoke_i32(&self, name: &str) -> Result<i32, InvokeError> {
        self.invoke_i32_with_args(name, &[])
    }

    /// Invoke the exact named export, passing i32 arguments in parameter order.
    ///
    /// An unknown name returns [`InvokeError::ExportNotFound`]. If the name
    /// matches, the number of arguments must equal the declared parameter count
    /// or this returns [`InvokeError::ArityMismatch`] without executing code.
    /// Assignments to parameters do not affect the supplied slice, and declared
    /// locals are initialized to zero on each invocation. A guest trap stops
    /// execution and returns [`InvokeError::Trap`]; exceeding a host execution
    /// limit returns [`InvokeError::LimitExceeded`]. Later calls start with
    /// fresh frames and execution budgets but observe any completed memory and
    /// global stores, including those made before a trap or limit.
    pub fn invoke_i32_with_args(&self, name: &str, args: &[i32]) -> Result<i32, InvokeError> {
        let export = self
            .module
            .exports
            .iter()
            .find(|export| export.name == name)
            .ok_or(InvokeError::ExportNotFound)?;
        binary::execute(self, export.function_index, args)
    }
}

/// Why invoking a typed export failed: host-side lookup, arity or resource
/// limit, or a guest trap.
/// Module validation failures are reported separately as [`Error`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvokeError {
    /// This module has no export with the requested name.
    ExportNotFound,
    /// The export exists but the supplied argument count does not match its type.
    ArityMismatch { expected: usize, actual: usize },
    /// Guest execution stopped before returning a value.
    Trap(Trap),
    /// The invocation would exceed a host resource bound, not a Core trap.
    LimitExceeded { limit: ExecutionLimit },
}

impl fmt::Display for InvokeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExportNotFound => f.write_str("WebAssembly export not found"),
            Self::ArityMismatch { expected, actual } => {
                write!(
                    f,
                    "WebAssembly export expects {expected} arguments, got {actual}"
                )
            }
            Self::Trap(trap) => write!(f, "WebAssembly trapped: {trap}"),
            Self::LimitExceeded { limit } => write!(
                f,
                "WebAssembly execution {limit:?} limit exceeded (maximum {})",
                limit.maximum()
            ),
        }
    }
}

impl std::error::Error for InvokeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Trap(trap) => Some(trap),
            Self::ExportNotFound | Self::ArityMismatch { .. } | Self::LimitExceeded { .. } => None,
        }
    }
}

/// Fixed host resource bounds for one invocation. These are not Wasm traps.
///
/// Dispatch is budgeted before executing an instruction. If no dispatches
/// remain when a call would also exceed the frame limit, the dispatch limit
/// is reported instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionLimit {
    /// Simultaneously live function frames, including the exported function.
    CallDepth,
    /// Total executed decoded instructions across all frames in the invocation.
    InstructionDispatches,
}

impl ExecutionLimit {
    /// The inclusive maximum for this resource.
    pub const fn maximum(self) -> usize {
        match self {
            Self::CallDepth => 16,
            Self::InstructionDispatches => 1024,
        }
    }
}

/// The reason a validated guest instruction trapped at runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trap {
    /// An integer division or remainder instruction had a zero divisor.
    IntegerDivideByZero,
    /// Signed integer division could not represent its quotient.
    IntegerOverflow,
    /// The full range of a load or store was not within the instance's memory.
    MemoryOutOfBounds,
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IntegerDivideByZero => f.write_str("integer divide by zero"),
            Self::IntegerOverflow => f.write_str("integer overflow"),
            Self::MemoryOutOfBounds => f.write_str("out of bounds memory access"),
        }
    }
}

impl std::error::Error for Trap {}

/// The WebAssembly frontend is not implemented; this says nothing about input validity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unsupported;

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebAssembly frontend is not implemented")
    }
}

impl std::error::Error for Unsupported {}

/// Always returns [`Unsupported`] without reading or retaining the bytes.
pub fn prepare(_bytes: &[u8]) -> Result<Infallible, Unsupported> {
    Err(Unsupported)
}
