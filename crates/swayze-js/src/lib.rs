//! JavaScript frontend boundary. Parsing and execution are not implemented.

use std::{convert::Infallible, fmt};

/// The JavaScript frontend is not implemented; this says nothing about input validity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unsupported;

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JavaScript frontend is not implemented")
    }
}

impl std::error::Error for Unsupported {}

/// Always returns [`Unsupported`] without reading or retaining the source.
pub fn prepare(_source: &str) -> Result<Infallible, Unsupported> {
    Err(Unsupported)
}
