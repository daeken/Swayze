//! Host-facing Swayze API. Only the documented language subsets are supported.

pub use swayze_wasm::{
    Error as WasmValidationError, ErrorKind as WasmErrorKind, Limit as WasmLimit,
    Module as WasmModule, validate_module,
};

use std::{convert::Infallible, fmt};

/// The requested frontend is not implemented. No input was validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unsupported {
    /// JavaScript frontend.
    JavaScript,
    /// WebAssembly frontend.
    WebAssembly,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::JavaScript => swayze_js::Unsupported.fmt(f),
            Self::WebAssembly => swayze_wasm::Unsupported.fmt(f),
        }
    }
}

impl std::error::Error for Unsupported {}

/// Returns [`Unsupported::JavaScript`] without reading or retaining the source.
pub fn prepare_js(source: &str) -> Result<Infallible, Unsupported> {
    swayze_js::prepare(source).map_err(|swayze_js::Unsupported| Unsupported::JavaScript)
}

/// Returns [`Unsupported::WebAssembly`] without reading or retaining the bytes.
pub fn prepare_wasm(bytes: &[u8]) -> Result<Infallible, Unsupported> {
    swayze_wasm::prepare(bytes).map_err(|swayze_wasm::Unsupported| Unsupported::WebAssembly)
}
