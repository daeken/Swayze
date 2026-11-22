use std::{fmt, str};

/// Marker returned only after this crate's bounded validator accepts the input.
/// It contains no executable code or retained input.
///
/// ```compile_fail
/// use swayze_wasm::Module;
/// let forged = Module(());
/// ```
#[derive(Debug)]
pub struct Module(());

/// A validation failure with an absolute, zero-based byte offset in the input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    /// For truncation, the first missing byte (the end of the input or frame).
    /// Otherwise, the offending byte or start of the offending field.
    pub offset: usize,
    /// The reason validation stopped.
    pub kind: ErrorKind,
}

/// Why the input was not accepted. Unsupported does not imply valid Wasm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    TruncatedHeader,
    InvalidMagic,
    UnsupportedVersion { version: u32 },
    UnknownSectionId { id: u8 },
    UnsupportedSection { id: u8 },
    TruncatedLength { field: LengthField },
    LengthOverflow { field: LengthField },
    TruncatedSection { declared: usize, available: usize },
    TruncatedName { declared: usize, available: usize },
    InvalidUtf8Name,
    LimitExceeded { limit: Limit },
}

/// The field whose unsigned LEB128 length could not be decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LengthField {
    SectionSize,
    CustomNameSize,
}

/// Fixed resource bounds for [`validate_module`]. All maxima are inclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Limit {
    /// Complete input, including the header and all framing.
    ModuleBytes,
    /// Number of sections, including custom sections with empty names or bodies.
    SectionCount,
    /// Declared payload bytes in one section, excluding its ID and size field.
    SectionBytes,
    /// Name bytes in one custom section, excluding its length field.
    CustomNameBytes,
}

impl Limit {
    /// The inclusive maximum in bytes or sections, depending on the limit.
    pub const fn maximum(self) -> usize {
        match self {
            Self::ModuleBytes => 1024 * 1024,
            Self::SectionCount => 1024,
            Self::SectionBytes => 256 * 1024,
            Self::CustomNameBytes => 4 * 1024,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ErrorKind::*;
        write!(f, "WebAssembly at byte {}: ", self.offset)?;
        match self.kind {
            TruncatedHeader => f.write_str("truncated header"),
            InvalidMagic => f.write_str("invalid magic"),
            UnsupportedVersion { version } => write!(f, "unsupported binary version {version}"),
            UnknownSectionId { id } => write!(f, "unknown section ID {id}"),
            UnsupportedSection { id } => write!(f, "unsupported section ID {id}"),
            TruncatedLength { field } => write!(f, "truncated LEB32 for {field:?}"),
            LengthOverflow { field } => write!(f, "LEB32 overflow for {field:?}"),
            TruncatedSection {
                declared,
                available,
            } => write!(
                f,
                "section declares {declared} bytes but only {available} remain"
            ),
            TruncatedName {
                declared,
                available,
            } => write!(
                f,
                "custom name declares {declared} bytes but only {available} remain in its section"
            ),
            InvalidUtf8Name => f.write_str("invalid UTF-8 in custom section name"),
            LimitExceeded { limit } => {
                write!(f, "{limit:?} limit exceeded (maximum {})", limit.maximum())
            }
        }
    }
}

impl std::error::Error for Error {}

/// Validate a WebAssembly Core 1.0 module containing only custom sections.
///
/// This checks the complete input on success. Work is linear in input length,
/// uses constant additional space, and is bounded by [`Limit`]. The validator
/// does not instantiate anything or interpret the contents of custom sections
/// beyond the names.
///
/// The module-byte limit is checked before decoding, and section count before
/// reading the next section. Section IDs 1 through 11 (Core 1.0), 12 (data count),
/// and 13 (tag) are recognized but unsupported. They are rejected after checking
/// the section size and that its payload fits, without validating the payload
/// or any later sections. Unknown IDs are rejected before reading the size.
/// Decodable sizes are checked against their limit before checking availability.
/// Bad UTF-8 is reported at the start of the invalid or incomplete sequence.
pub fn validate_module(bytes: &[u8]) -> Result<Module, Error> {
    let limit = Limit::ModuleBytes;
    if bytes.len() > limit.maximum() {
        return Err(Error {
            offset: limit.maximum(),
            kind: ErrorKind::LimitExceeded { limit },
        });
    }
    let mut module = Reader {
        remaining: bytes,
        end: bytes.len(),
    };
    read_header(&mut module)?;

    let limit = Limit::SectionCount;
    for _ in 0..limit.maximum() {
        let id_offset = module.position();
        let Some(id) = module.byte() else {
            break;
        };
        if id > 13 {
            return Err(Error {
                offset: id_offset,
                kind: ErrorKind::UnknownSectionId { id },
            });
        }
        let declared = module.length(LengthField::SectionSize)?;
        let available = module.remaining.len();
        let payload = module.take(declared).ok_or(Error {
            offset: module.end,
            kind: ErrorKind::TruncatedSection {
                declared,
                available,
            },
        })?;
        if id != 0 {
            return Err(Error {
                offset: id_offset,
                kind: ErrorKind::UnsupportedSection { id },
            });
        }
        validate_custom(Reader {
            remaining: payload,
            end: module.position(),
        })?;
    }
    if !module.remaining.is_empty() {
        return Err(module.error(ErrorKind::LimitExceeded { limit }));
    }
    Ok(Module(()))
}

fn read_header(module: &mut Reader<'_>) -> Result<(), Error> {
    for expected in *b"\0asm" {
        let offset = module.position();
        match module.byte() {
            Some(byte) if byte == expected => (),
            Some(_) => {
                return Err(Error {
                    offset,
                    kind: ErrorKind::InvalidMagic,
                });
            }
            None => return Err(module.error(ErrorKind::TruncatedHeader)),
        }
    }
    let offset = module.position();
    let mut version = [0; 4];
    for byte in &mut version {
        *byte = module
            .byte()
            .ok_or_else(|| module.error(ErrorKind::TruncatedHeader))?;
    }
    let version = u32::from_le_bytes(version);
    if version != 1 {
        return Err(Error {
            offset,
            kind: ErrorKind::UnsupportedVersion { version },
        });
    }
    Ok(())
}

fn validate_custom(mut custom: Reader<'_>) -> Result<(), Error> {
    let declared = custom.length(LengthField::CustomNameSize)?;
    let name_offset = custom.position();
    let available = custom.remaining.len();
    let name = custom.take(declared).ok_or(Error {
        offset: custom.end,
        kind: ErrorKind::TruncatedName {
            declared,
            available,
        },
    })?;
    str::from_utf8(name).map_err(|err| Error {
        // valid_up_to() is bounded by name.len(), and the whole name fits.
        offset: name_offset + err.valid_up_to(),
        kind: ErrorKind::InvalidUtf8Name,
    })?;
    Ok(())
}

struct Reader<'a> {
    // The unconsumed suffix of this frame. end is its (exclusive) absolute
    // end, so remaining.len() <= end even for a nested frame.
    remaining: &'a [u8],
    end: usize,
}

impl<'a> Reader<'a> {
    fn position(&self) -> usize {
        self.end - self.remaining.len()
    }

    fn error(&self, kind: ErrorKind) -> Error {
        Error {
            offset: self.position(),
            kind,
        }
    }

    fn byte(&mut self) -> Option<u8> {
        let (&byte, rest) = self.remaining.split_first()?;
        self.remaining = rest;
        Some(byte)
    }

    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let (bytes, rest) = self.remaining.split_at_checked(length)?;
        self.remaining = rest;
        Some(bytes)
    }

    fn length(&mut self, field: LengthField) -> Result<usize, Error> {
        let start = self.position();
        let mut value = 0u32;
        let mut shift = 0;
        loop {
            let offset = self.position();
            let byte = self
                .byte()
                .ok_or_else(|| self.error(ErrorKind::TruncatedLength { field }))?;
            // At most five bytes; the fifth can carry only four value bits
            // and must terminate. Non-minimal encodings within this width are legal.
            if shift == 28 && byte > 0x0f {
                return Err(Error {
                    offset,
                    kind: ErrorKind::LengthOverflow { field },
                });
            }
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }

        let limit = match field {
            LengthField::SectionSize => Limit::SectionBytes,
            LengthField::CustomNameSize => Limit::CustomNameBytes,
        };
        match usize::try_from(value) {
            Ok(length) if length <= limit.maximum() => Ok(length),
            _ => Err(Error {
                offset: start,
                kind: ErrorKind::LimitExceeded { limit },
            }),
        }
    }
}
