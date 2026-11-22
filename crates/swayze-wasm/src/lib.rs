//! Bounded WebAssembly binary validation and i32 execution.
//!
//! # Supported slice
//!
//! [`validate_module`] accepts the [WebAssembly Core 1.0] header, any number of
//! custom sections within [`Limit`], and either no standard sections or exactly
//! one type, one function, and one code body in their respective sections. The
//! type must have zero to four `i32` parameters and exactly one `i32` result.
//! The function must reference type zero. Its body may declare up to 16 `i32`
//! locals in total. The number of groups and each group's local count are
//! unsigned LEB32; zero-count groups are allowed but must still specify `i32`.
//! Supported instructions are `local.get`, `local.set`, `local.tee` (each with
//! an unsigned LEB32 index), `i32.const <signed LEB32>`, `i32.eqz`, `i32.eq`,
//! `i32.ne`, `i32.lt_s`, `i32.lt_u`, `i32.gt_s`, `i32.gt_u`, `i32.le_s`,
//! `i32.le_u`, `i32.ge_s`, `i32.ge_u`, `i32.add`, `i32.sub`, `i32.mul`,
//! `i32.div_s`, `i32.div_u`, `i32.rem_s`, `i32.rem_u`, and
//! `if (result i32) ... else ... end` (block type byte `0x7f`). Local indices
//! start at zero and address parameters first, then locals in declaration order.
//! Both can be assigned. `if` can nest, takes no block parameters, and requires
//! both arms. There are no calls, `block`, `loop`, `br`, `return`, `unreachable`,
//! or other control instructions.
//! An optional export section between function and code may contain zero or one
//! export, which must refer to function zero. Both kinds of names must be UTF-8.
//! Custom payloads after the name are opaque and discarded, even for well-known
//! names. All other standard sections (including import, memory, and start) are
//! rejected, even when empty.
//!
//! Parameters do not start on the operand stack. Validation starts with an empty
//! stack and checks each stack effect: `local.get` and `i32.const` push one i32,
//! `local.set` pops one, `local.tee` requires and leaves the top operand,
//! `i32.eqz` consumes one and pushes one, and the binary operators consume two
//! and push one. `if` consumes an i32 condition from above the enclosing
//! frame's operand floor and saves the remaining depth as the floor for both
//! arms. Every operand access in an arm must stay above that floor. `else`
//! checks for exactly one new result and resets validation to the saved depth,
//! and the matching `end` checks that the else arm also leaves exactly one new
//! result. Both arms are checked regardless of the condition.
//! The function has a floor of zero; its own `end` must be the final byte of
//! the body frame and leave exactly one result. The inclusive limits are 256
//! body bytes (including the locals vector and all `end` bytes), 127 decoded
//! instructions (including `if` and `else` across both arms, excluding `end`),
//! 32 live i32 operands including those below arm floors, and 16 nested `if`s.
//!
//! Validation checks the entire input before returning an owned [`Module`] with
//! private decoded instructions and an optional bounded export name. A separate
//! [`Module::instantiate`] step creates an [`Instance`]. Its typed
//! [`Instance::invoke_i32_with_args`] method looks up the exact export name,
//! checks the argument count, and executes without allocating or recursing.
//! Zero selects the else arm; every nonzero i32 selects the then arm. Validation
//! uses a fixed stack of control frames and backpatches private jump targets at
//! matching delimiters. Each target is a strictly later decoded instruction
//! index or the one-past-last index. Execution visits each decoded instruction
//! at most once, and never executes an untaken arm.
//! Each invocation has a fixed operand stack and fixed local storage:
//! parameters are copied from the supplied arguments, and declared locals start
//! at zero on every call. [`Instance::invoke_i32`] is equivalent to passing an
//! empty argument slice. The top operand is the right-hand side of each binary
//! operation. `i32.eqz` tests for zero, and `i32.eq` and `i32.ne` compare the
//! operand bits for equality or inequality. Ordering comparisons interpret the
//! bits as signed `i32` for `_s` and unsigned `u32` for `_u`. These tests and
//! comparisons do not trap and return exactly `1` if true or `0` if false.
//! Addition, subtraction, and multiplication wrap modulo 2³². Signed
//! division truncates toward zero, and a nonzero signed remainder has the sign
//! of the dividend. Unsigned division and remainder interpret the operand bits
//! as `u32` and preserve the result bits in the returned `i32` value.
//!
//! A zero right operand traps for all four division and remainder instructions.
//! `i32::MIN / -1` traps for signed division; signed remainder for those operands
//! returns zero. These are runtime outcomes, not validation errors.
//! [`InvokeError::Trap`] reports the first guest trap and skips all remaining
//! instructions. The same instance can be invoked again with fresh call-local
//! state. Empty, custom-only, and unexported modules have no callable exports.
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
//! Comparison tests cover exact 0/1 results, equal operands and signed versus
//! unsigned boundary values, unary and binary underflows at the function and
//! arm floors, conditional selection, and comparisons at the instruction and
//! operand limits.
//! Conditional tests exercise zero and nonzero conditions, nested joins,
//! preserved outer operands and selected-arm effects, suppression of untaken
//! traps, rejection of untaken invalid code, arm result counts and floor
//! violations (even when depth would later recover), delimiter errors, and
//! control, instruction and operand bounds.
//! Parsing checks declaration counts against the remaining local budget without
//! expanding groups. It uses bounded borrowed frames and decodes instructions
//! into a fixed-size scratch array. After checking `end` and the complete body
//! frame, it calls `Vec::try_reserve_exact` for the decoded instruction count and
//! copies those instructions. The export name is also reserved fallibly. There
//! is no unsafe code, recursion, or external dependency.
//! This is not full Core 1.0 validation or a conformance or security proof.
//! No upstream Wasm spec suite or fuzzing has been run.
//!
//! [WebAssembly Core 1.0]: https://www.w3.org/TR/2019/REC-wasm-core-1-20191205/

mod binary;

pub use binary::{Error, ErrorKind, IntegerField, LengthField, Limit, validate_module};

use std::{convert::Infallible, fmt};

/// An owned, opaque module produced only by successful [`validate_module`].
///
/// ```compile_fail
/// use swayze_wasm::Module;
/// let forged = Module { function: None };
/// ```
#[derive(Debug)]
pub struct Module {
    function: Option<Function>,
}

#[derive(Debug)]
struct Function {
    code: binary::ValidatedCode,
    export: Option<String>,
}

impl Module {
    /// Create an instance without allocating or executing guest code.
    /// This slice has no imports or per-instance mutable state.
    pub fn instantiate(&self) -> Instance<'_> {
        Instance { module: self }
    }
}

/// An instance of a validated module.
#[derive(Debug)]
pub struct Instance<'module> {
    module: &'module Module,
}

impl Instance<'_> {
    /// Invoke the exact named export as a `() -> i32` function.
    /// A matching export with parameters returns [`InvokeError::ArityMismatch`].
    /// A guest trap returns [`InvokeError::Trap`].
    ///
    /// ```
    /// use swayze_wasm::validate_module;
    /// let bytes = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7f\x03\x02\x01\0\
    ///               \x07\x0a\x01\x06answer\0\0\x0a\x09\x01\x07\0\x41\x28\x41\x02\x6a\x0b";
    /// let module = validate_module(bytes)?;
    /// assert_eq!(module.instantiate().invoke_i32("answer")?, 42);
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
    /// execution and returns [`InvokeError::Trap`] without affecting later calls.
    pub fn invoke_i32_with_args(&self, name: &str, args: &[i32]) -> Result<i32, InvokeError> {
        match &self.module.function {
            Some(function) if function.export.as_deref() == Some(name) => {
                function.code.execute(args)
            }
            _ => Err(InvokeError::ExportNotFound),
        }
    }
}

/// Why invoking a typed export failed: host-side lookup or arity, or a guest trap.
/// Module validation failures are reported separately as [`Error`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvokeError {
    /// This module has no export with the requested name.
    ExportNotFound,
    /// The export exists but the supplied argument count does not match its type.
    ArityMismatch { expected: usize, actual: usize },
    /// Guest execution stopped before returning a value.
    Trap(Trap),
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
        }
    }
}

impl std::error::Error for InvokeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Trap(trap) => Some(trap),
            Self::ExportNotFound | Self::ArityMismatch { .. } => None,
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
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IntegerDivideByZero => f.write_str("integer divide by zero"),
            Self::IntegerOverflow => f.write_str("integer overflow"),
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
