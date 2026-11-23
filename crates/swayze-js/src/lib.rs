//! Bounded parser and evaluator for a small JavaScript Script subset. See the [slice notes](#slice-notes).
//!
//! # Slice notes
#![doc = include_str!("../README.md")]

mod eval;
mod parser;
mod radix_integer;

pub use eval::{EvalError, EvalScratch, PrimitiveValue, Value};
pub use parser::{
    Error, ErrorKind, Limit, Limits, Script, Span, parse_script, parse_script_with_limits,
};

#[cfg(test)]
mod number_format;

#[cfg(test)]
mod tests;

use std::{convert::Infallible, fmt};

/// The compatibility path cannot prepare JavaScript for execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unsupported;

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JavaScript execution preparation is not implemented")
    }
}

impl std::error::Error for Unsupported {}

/// Compatibility shim: always returns [`Unsupported`] without reading the source.
/// Use [`parse_script`] and [`Script::evaluate_primitive`] for the supported subset.
pub fn prepare(_source: &str) -> Result<Infallible, Unsupported> {
    Err(Unsupported)
}
