//! Bounded WebAssembly binary validation and constant-function execution.
//!
//! # Supported slice
//!
//! [`validate_module`] accepts the [WebAssembly Core 1.0] header, any number of
//! custom sections within [`Limit`], and either no standard sections or exactly
//! one type, one function, and one code body in their respective sections. The
//! type must be `() -> i32`, the function must reference type zero, and its body
//! must have an empty locals vector and exactly `i32.const <signed LEB32>; end`.
//! An optional export section between function and code may contain zero or one
//! export, which must refer to function zero. Both kinds of names must be UTF-8.
//! Custom payloads after the name are opaque and discarded, even for well-known
//! names. All other standard sections (including import, memory, and start) are
//! rejected, even when empty.
//!
//! Validation checks the entire input before returning an owned [`Module`]. It
//! retains only the decoded constant and an optional, bounded export name. A
//! separate [`Module::instantiate`] step creates an [`Instance`]; calling its
//! typed [`Instance::invoke_i32`] method by exact export name returns the
//! prevalidated constant. Empty, custom-only, and unexported modules have no
//! callable exports. The existing [`prepare`] entry point remains a stub.
//!
//! # Evidence and gaps
//!
//! Local tests exercise framing, UTF-8, size limits, section order and contents,
//! function/type/code and export relationships, bounded LEB32 decoding, and
//! invocation after discarding the input. Parsing uses bounded borrowed frames;
//! it allocates only to retain an export name after successful validation. No
//! unsafe code, recursion, generic interpreter, or external dependencies are
//! used. This is not full Core 1.0 validation or a conformance or security proof.
//! No upstream Wasm spec suite, fuzzing, or facade integration is provided here.
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
    function: Option<ConstantFunction>,
}

#[derive(Debug)]
struct ConstantFunction {
    value: i32,
    export: Option<Box<str>>,
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
    ///               \x07\x0a\x01\x06answer\0\0\x0a\x06\x01\x04\0\x41\x2a\x0b";
    /// let module = validate_module(bytes)?;
    /// assert_eq!(module.instantiate().invoke_i32("answer")?, 42);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn invoke_i32(&self, name: &str) -> Result<i32, InvokeError> {
        match &self.module.function {
            Some(function) if function.export.as_deref() == Some(name) => Ok(function.value),
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
