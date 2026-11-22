//! Bounded WebAssembly binary validation and straight-line i32 execution.
//!
//! # Supported slice
//!
//! [`validate_module`] accepts the [WebAssembly Core 1.0] header, any number of
//! custom sections within [`Limit`], and either no standard sections or exactly
//! one type, one function, and one code body in their respective sections. The
//! type must be `() -> i32`, the function must reference type zero, and its body
//! must have an empty locals vector and contain only `i32.const <signed LEB32>`,
//! `i32.add`, `i32.sub`, and `i32.mul`, followed by a single final `end`. There
//! are no calls, branches, or other control instructions in this slice.
//! An optional export section between function and code may contain zero or one
//! export, which must refer to function zero. Both kinds of names must be UTF-8.
//! Custom payloads after the name are opaque and discarded, even for well-known
//! names. All other standard sections (including import, memory, and start) are
//! rejected, even when empty.
//!
//! Validation starts with an empty operand stack and checks each stack effect:
//! `i32.const` pushes one i32, and the binary operators consume two and push
//! one. `end` must be the final byte of the body frame and leave exactly one
//! result. Each body is limited to 256 bytes including the locals vector and
//! `end`, 127 instructions excluding `end`, and 32 live i32 operands.
//!
//! Validation checks the entire input before returning an owned [`Module`] with
//! private decoded instructions and an optional bounded export name. A separate
//! [`Module::instantiate`] step creates an [`Instance`]. Its typed
//! [`Instance::invoke_i32`] method executes each stored instruction at most once
//! on a fixed stack without allocating. Arithmetic wraps modulo 2³²; subtraction
//! uses the top operand as the right-hand side. Empty, custom-only, and
//! unexported modules have no callable exports. [`prepare`] remains a stub.
//!
//! Public entry points and invocation errors are unchanged. [`Limit`] has new
//! variants for the body budgets (exhaustive matches must account for them).
//! A declared body size is checked against [`Limit::CodeBodyBytes`] rather than
//! [`Limit::SectionBytes`]. Unsupported body opcodes now report
//! [`ErrorKind::UnsupportedOpcode`] rather than [`ErrorKind::UnexpectedByte`].
//!
//! # Evidence and gaps
//!
//! Local tests exercise framing, UTF-8, size limits, section order and contents,
//! function/type/code and export relationships, bounded LEB32 decoding, stack
//! underflow and final depth, acceptance at and rejection beyond each body
//! budget, and ordered, wrapping arithmetic after discarding the input. Parsing
//! uses bounded borrowed frames and fallible reservations for decoded instructions
//! and the export name. There is no unsafe code, recursion, or external dependency.
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
        match &self.module.function {
            Some(function) if function.export.as_deref() == Some(name) => {
                Ok(function.code.execute())
            }
            _ => Err(InvokeError::ExportNotFound),
        }
    }
}

/// Why invoking a typed export failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvokeError {
    /// This module has no export with the requested name.
    ExportNotFound,
}

impl fmt::Display for InvokeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExportNotFound => f.write_str("WebAssembly export not found"),
        }
    }
}

impl std::error::Error for InvokeError {}

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
