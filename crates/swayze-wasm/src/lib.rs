//! Bounded WebAssembly binary validation and i32 execution.
//!
//! # Supported slice
//!
//! [`validate_module`] accepts the [WebAssembly Core 1.0] header, any number of
//! custom sections within [`Limit`], and an optional memory section. Modules
//! with functions must have one to eight types, one to eight function
//! declarations, and exactly as many code bodies as function declarations,
//! in that section order. Each type must have zero to four `i32` parameters
//! and exactly one `i32` result, even if unused.
//! Every function declares its own zero-based index into the type table; types
//! may be shared. Each body may declare up to 16 `i32` locals in total. The
//! number of groups and each group's local count are unsigned LEB32; zero-count
//! groups are allowed but must still specify `i32`.
//! Supported instructions are `local.get`, `local.set`, `local.tee` (each with
//! an unsigned LEB32 index), `i32.const <signed LEB32>`, `i32.eqz`, `i32.eq`,
//! `i32.ne`, `i32.lt_s`, `i32.lt_u`, `i32.gt_s`, `i32.gt_u`, `i32.le_s`,
//! `i32.le_u`, `i32.ge_s`, `i32.ge_u`, `i32.add`, `i32.sub`, `i32.mul`,
//! `i32.div_s`, `i32.div_u`, `i32.rem_s`, `i32.rem_u`, `i32.load`, `i32.store`, `call`,
//! `if (result i32) ... else ... end`, `block (result i32) ... end`,
//! `loop ... end`, and `br_if 0`. Local indices start at zero and address
//! parameters first, then locals in declaration order. Both can be assigned.
//! `call` takes an unsigned LEB32 index into the exact function declarations,
//! including forward and self references. The control constructs may nest
//! together. `if` and `block` accept only block type byte `0x7f` (one i32 result,
//! no block parameters), and `if` requires both arms. `loop` accepts only `0x40`
//! (no block parameters or results). `br_if` takes an unsigned LEB32 label
//! index, which must be zero with a `block` or `loop` as the innermost control
//! frame. Other label indices, `if` and function label targets, other block
//! types, `br`, `return`, `unreachable`, and indirect calls are unsupported.
//!
//! If present, the memory section follows any function section and precedes
//! any export and code sections. It may be empty (a zero-length vector) or
//! declare one private, unshared memory32. The limits use flag byte `0x00`
//! (minimum only) or `0x01` (minimum and maximum), followed by unsigned LEB32
//! page counts. The minimum and any explicit maximum must be zero or one
//! 64-KiB page, and the maximum must not be less than the minimum. The minimum
//! is the fixed instance size; there is no growth or `memory.size` instruction.
//! `i32.load` and `i32.store` require a defined memory (even zero pages) and
//! take an unsigned LEB32 alignment exponent of 0, 1, or 2 and an unsigned
//! LEB32 byte offset. Other memory instructions are unsupported.
//!
//! An optional export section between function and code (after memory, if
//! present) may contain zero to eight function exports. Memory cannot be
//! exported. Name matching is exact (no Unicode normalization), and duplicate
//! export names are rejected. Each export refers to a defined function by its
//! zero-based declaration index. Distinct names may refer to the same function.
//! Both export and custom names must be UTF-8.
//! Custom payloads after the name are opaque and discarded, even for well-known
//! names. All other standard sections (including import, start, and data) are
//! rejected, even when empty.
//!
//! Parameters do not start on the operand stack. Validation starts with an empty
//! stack and checks each stack effect: `local.get` and `i32.const` push one i32,
//! `local.set` pops one, `local.tee` requires and leaves the top operand,
//! `i32.eqz` consumes one and pushes one, and the binary operators consume two
//! and push one. `call` consumes as many i32s as the target function's type has
//! parameters (the top operand is the last parameter) and pushes one result.
//! `i32.load` consumes an i32 base address and pushes the loaded i32.
//! `i32.store` consumes an i32 address followed by an i32 value from the stack
//! (value on top) and produces no result.
//! `if` consumes an i32 condition from above the enclosing frame's operand floor
//! and saves the remaining depth as the floor for both arms. `else` checks for
//! exactly one new result and resets validation to the saved depth, and the
//! matching `end` checks that the else arm also leaves exactly one new result.
//! `block` and `loop` save the current depth as their floor without consuming
//! any operands. A `block` must leave exactly one new i32 at its normal exit;
//! a `loop` must leave none. Every operand access must stay above the active
//! floor. `br_if` pops an i32 condition. With a zero condition, execution
//! continues with all other operands unchanged. For a block target, it also
//! requires an i32 result beneath the condition and above the floor; with a
//! nonzero condition, it keeps that result and the prefix below the floor,
//! discards all other operands, and resumes after the matching block `end`.
//! A loop target requires only the condition. If nonzero, `br_if` discards all
//! operands above the loop floor and resumes at the start of the loop body
//! with the prefix and locals intact. Both `if` arms and the entire `br_if`
//! fallthrough path are validated regardless of the condition.
//! The function has a floor of zero; its own `end` must be the final byte of
//! the body frame and leave exactly one result. The inclusive limits are 256
//! body bytes (including the locals vector and all delimiters), 127 decoded
//! instructions (including `if`, `else`, and `br_if`, but not `block`, `loop`,
//! or `end`), 32 live i32 operands including those below floors, and 16
//! simultaneously open `block`, `loop`, and `if` frames combined, excluding
//! the function frame.
//!
//! Validation resolves every function's type index before decoding any body,
//! and checks the entire input, including every body even if unexported. It
//! returns an immutable, owned [`Module`] with private decoded bodies, their
//! arities, named export entries, and the memory size, if defined. A separate
//! fallible [`Module::instantiate`] step creates an [`Instance`] and reserves
//! and zeroes its private linear memory; validation retains only the size, not
//! its contents. The typed [`Instance::invoke_i32_with_args`] method
//! scans at most eight export names for an exact match, then checks the selected
//! function's arity and begins execution without allocating or recursing
//! in Rust. Zero selects the else arm; every nonzero i32 selects the then arm.
//! Validation uses a fixed stack of control frames. It backpatches `if` and
//! `block` targets at matching delimiters to strictly later decoded instruction
//! indices or the one-past-last index in the same body. Loop continues have a
//! separate target at or before the branch, resolved from the loop's saved
//! body start. Each decoded `br_if` also stores its target's operand floor.
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
//! Memory instructions add the base bits interpreted as `u32` and the `u32`
//! offset in `u64`, without wrapping at 32 bits. The entire four-byte range
//! must be in bounds, or the instruction returns [`Trap::MemoryOutOfBounds`]
//! without reading or writing any bytes. Accesses are little-endian and may
//! be unaligned; validated alignment annotations do not constrain addresses.
//!
//! A zero right operand traps for all four division and remainder instructions.
//! `i32::MIN / -1` traps for signed division; signed remainder for those operands
//! returns zero. These are runtime outcomes, not validation errors.
//! [`InvokeError::Trap`] reports the first guest trap and skips all remaining
//! instructions, including in callers. Host resource limits are separate:
//! [`InvokeError::LimitExceeded`] stops the entire invocation before it would
//! exceed either 16 live frames (including the exported function) or 1024 total
//! instruction dispatches. Every executed decoded instruction costs one dispatch,
//! including a memory access, a `call`, an `if` test, an executed `else` jump,
//! or a `br_if` with either outcome. Skipped instructions, `block`, `loop`, and
//! `end` delimiters, and returning from frames cost nothing. A continue does
//! not reset the budget:
//! all loop iterations and calls share it, including recursive guest calls.
//! An invocation can return just after its last allowed dispatch. If the next
//! instruction would exceed the dispatch budget, that limit takes precedence
//! over executing it.
//! The same instance can be invoked again with fresh call-local state and budgets.
//! All functions in an instance share its memory. Stores persist across calls,
//! exports, traps, and host limits: completed stores are not rolled back. Separate
//! instances have independent memory. Empty, custom-only, memory-only, and
//! unexported modules have no callable exports.
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
//! Conditional tests exercise zero and nonzero conditions, nested joins,
//! preserved outer operands and selected-arm effects, suppression of untaken
//! traps, rejection of untaken invalid code, arm result counts and floor
//! violations (even when depth would later recover), delimiter errors, and
//! control, instruction and operand bounds.
//! Block and branch tests cover mixed nesting and floors, taken truncation vs.
//! fallthrough, multiple exits, skipped traps and rejected skipped syntax,
//! label immediates and delimiter errors, and exact body and execution budgets.
//! Loop tests cover mixed nesting with forward edges, preserved locals and
//! stack prefix, different taken and fallthrough stack effects, full body
//! validation, shared caps, and the exact dispatch boundary in a called loop
//! and instance reuse after hitting the limit.
//! Memory tests cover exact limits and memarg decoding within section and body
//! frames, ordering and unsupported features, operand floors (including untaken
//! arms), unexported bodies, zero pages and zero initialization, overlapping
//! little-endian and unaligned accesses, the last complete word, partial-store
//! prevention, widened address arithmetic, independent instances, and sharing
//! across calls and exports. They also check store persistence and suppression
//! of later stores after guest traps or dispatch exhaustion.
//! Parsing uses fixed scratch tables for up to eight type arities and eight
//! resolved function arities. It checks local group counts against the remaining
//! local budget without expanding groups, uses bounded borrowed frames, and
//! decodes instructions into a fixed-size scratch array. After checking `end`
//! and the complete body frame, it calls `Vec::try_reserve_exact` for the count and
//! copies those instructions. The body and export vectors are reserved fallibly
//! after checking their counts, and each name is reserved fallibly after checking
//! its length. The count caps bound aggregate decoding to eight body budgets and
//! duplicate checks to at most 28 comparisons of bounded names. There is no
//! unsafe code, Rust recursion, or external dependency.
//! This is not full Core 1.0 validation or a conformance or security proof.
//! No upstream Wasm spec suite or fuzzing has been run.
//!
//! [WebAssembly Core 1.0]: https://www.w3.org/TR/2019/REC-wasm-core-1-20191205/

mod binary;
mod memory;

pub use binary::{Error, ErrorKind, IntegerField, LengthField, Limit, validate_module};

use memory::LinearMemory;
use std::{convert::Infallible, fmt};

/// An owned, opaque module produced only by successful [`validate_module`].
///
/// ```compile_fail
/// use swayze_wasm::Module;
/// let forged = Module { functions: Vec::new(), exports: Vec::new(), memory_bytes: None };
/// ```
#[derive(Debug)]
pub struct Module {
    functions: Vec<binary::ValidatedCode>,
    exports: Vec<Export>,
    // None means absent; Some is a validated byte length (zero or one page).
    memory_bytes: Option<usize>,
}

#[derive(Debug)]
struct Export {
    name: String,
    function_index: usize,
}

impl Module {
    /// Create an independent instance without executing guest code.
    ///
    /// A one-page memory is allocated fallibly and initialized to zero here.
    /// No linear memory is allocated for an absent or zero-page memory.
    pub fn instantiate(&self) -> Result<Instance<'_>, InstantiationError> {
        let memory = LinearMemory::new(self.memory_bytes.unwrap_or(0))?;
        Ok(Instance {
            module: self,
            memory,
        })
    }
}

/// Why creating an instance of a validated module failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstantiationError {
    /// The instance's linear memory could not be reserved.
    MemoryAllocationFailed,
}

impl fmt::Display for InstantiationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryAllocationFailed => {
                f.write_str("could not reserve WebAssembly linear memory")
            }
        }
    }
}

impl std::error::Error for InstantiationError {}

/// An instance of a validated module, with private, persistent linear memory.
///
/// Invocation through `&self` can change this memory. An instance is `Send`
/// (it can move to another thread when its module borrow permits), but not
/// `Sync`: sharing one instance between threads requires synchronization.
/// The immutable module can be shared to create independent instances.
#[derive(Debug)]
pub struct Instance<'module> {
    module: &'module Module,
    memory: LinearMemory,
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
    /// fresh frames and execution budgets but observe any completed stores,
    /// including those made before a trap or limit.
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
