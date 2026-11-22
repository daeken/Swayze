//! Bounded WebAssembly binary validation. Nothing is instantiated or executed.
//!
//! # Supported slice
//!
//! [`validate_module`] accepts the [WebAssembly Core 1.0] header followed by zero
//! or more custom sections. Custom-section names must be UTF-8; their remaining
//! payload is opaque and discarded, including payloads with standardized names.
//! Other sections are rejected, even when empty. A successful [`Module`] neither
//! borrows nor retains the original bytes and grants no authority to execute.
//! The existing [`prepare`] entry point remains a stub.
//!
//! # Evidence and gaps
//!
//! Local tests cover the header, custom framing and UTF-8, padded and overflowing
//! lengths, unsupported versus unknown IDs, truncation, and each quota at and
//! over its boundary. The validator uses no allocation, recursion, or dependency
//! beyond Rust's standard library. It is a standalone slice, not a conformance
//! or security proof; upstream spec suites, fuzzing, validation of standard
//! sections and instructions, and facade integration are not yet provided.
//!
//! [WebAssembly Core 1.0]: https://www.w3.org/TR/2019/REC-wasm-core-1-20191205/

mod binary;

pub use binary::{Error, ErrorKind, LengthField, Limit, Module, validate_module};

use std::{convert::Infallible, fmt};

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
