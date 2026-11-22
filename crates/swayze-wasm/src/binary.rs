use crate::{Function, Module};
use std::{fmt, ops::RangeInclusive, str};

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
    SectionOutOfOrder { id: u8 },
    MissingSection { id: u8 },
    TruncatedLength { field: LengthField },
    LengthOverflow { field: LengthField },
    TruncatedInteger { field: IntegerField },
    IntegerOverflow { field: IntegerField },
    TruncatedSection { declared: usize, available: usize },
    TruncatedBody { declared: usize, available: usize },
    TruncatedName { declared: usize, available: usize },
    InvalidUtf8Name,
    UnsupportedCount { field: IntegerField, count: u32 },
    InvalidIndex { field: IntegerField, index: u32 },
    TruncatedByte { expected: u8 },
    UnexpectedByte { expected: u8, found: u8 },
    TrailingSectionBytes { id: u8 },
    TrailingBodyBytes,
    UnsupportedOpcode { opcode: u8 },
    StackUnderflow { depth: usize },
    InvalidResultDepth { depth: usize },
    AllocationFailed,
    LimitExceeded { limit: Limit },
}

/// The field whose unsigned LEB128 length could not be decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LengthField {
    SectionSize,
    CustomNameSize,
    ExportNameSize,
    CodeBodySize,
}

/// A count, index, or immediate encoded as a LEB32 integer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum IntegerField {
    TypeCount,
    ParameterCount,
    ResultCount,
    FunctionCount,
    ExportCount,
    CodeCount,
    LocalGroupCount,
    TypeIndex,
    FunctionIndex,
    I32Const,
}

/// Fixed resource bounds for [`validate_module`]. All maxima are inclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Limit {
    /// Complete input, including the header and all framing.
    ModuleBytes,
    /// Number of sections, including custom sections with empty names or bodies.
    SectionCount,
    /// Declared payload bytes in one section, excluding framing.
    SectionBytes,
    /// Declared bytes in the code body, including the locals vector and `end`.
    CodeBodyBytes,
    /// Decoded `i32.const`, `i32.add`, `i32.sub`, and `i32.mul` operations; excludes `end`.
    BodyInstructions,
    /// Peak number of live i32 operands in the code body.
    OperandStackDepth,
    /// Name bytes in one custom section, excluding its length field.
    CustomNameBytes,
    /// Name bytes in one export, excluding its length field.
    ExportNameBytes,
}

impl Limit {
    /// The inclusive maximum in bytes, sections, instructions, or operands.
    pub const fn maximum(self) -> usize {
        match self {
            Self::ModuleBytes => 1024 * 1024,
            Self::SectionCount => 1024,
            Self::SectionBytes => 256 * 1024,
            Self::CodeBodyBytes => 256,
            Self::BodyInstructions => 127,
            Self::OperandStackDepth => 32,
            Self::CustomNameBytes | Self::ExportNameBytes => 4 * 1024,
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
            SectionOutOfOrder { id } => write!(f, "duplicate or out-of-order section ID {id}"),
            MissingSection { id } => write!(f, "missing required section ID {id}"),
            TruncatedLength { field } => write!(f, "truncated LEB32 for {field:?}"),
            LengthOverflow { field } => write!(f, "LEB32 overflow for {field:?}"),
            TruncatedInteger { field } => write!(f, "truncated LEB32 for {field:?}"),
            IntegerOverflow { field } => write!(f, "LEB32 overflow for {field:?}"),
            TruncatedSection {
                declared,
                available,
            } => write!(
                f,
                "section declares {declared} bytes but only {available} remain"
            ),
            TruncatedBody {
                declared,
                available,
            } => write!(
                f,
                "code body declares {declared} bytes but only {available} remain in its section"
            ),
            TruncatedName {
                declared,
                available,
            } => write!(
                f,
                "name declares {declared} bytes but only {available} remain in its section"
            ),
            InvalidUtf8Name => f.write_str("invalid UTF-8 in name"),
            UnsupportedCount { field, count } => write!(f, "unsupported {field:?} of {count}"),
            InvalidIndex { field, index } => write!(f, "invalid {field:?} {index}"),
            TruncatedByte { expected } => write!(f, "missing expected byte {expected:#04x}"),
            UnexpectedByte { expected, found } => {
                write!(f, "expected byte {expected:#04x}, found {found:#04x}")
            }
            TrailingSectionBytes { id } => write!(f, "trailing bytes in section ID {id}"),
            TrailingBodyBytes => f.write_str("trailing bytes after code body end"),
            UnsupportedOpcode { opcode } => write!(f, "unsupported opcode {opcode:#04x}"),
            StackUnderflow { depth } => {
                write!(f, "binary operator needs two operands, found {depth}")
            }
            InvalidResultDepth { depth } => write!(f, "expected one result at end, found {depth}"),
            AllocationFailed => f.write_str("could not reserve validated module storage"),
            LimitExceeded { limit } => {
                write!(f, "{limit:?} limit exceeded (maximum {})", limit.maximum())
            }
        }
    }
}

impl std::error::Error for Error {}

/// Validate the crate's [supported slice](crate#supported-slice) of WebAssembly Core 1.0.
///
/// Success checks the complete input and returns an owned module without
/// executing it. Work is at most linear in the input length. Allocations retain
/// only validated instructions and an optional export name, with sizes bounded by
/// [`Limit::BodyInstructions`] and [`Limit::ExportNameBytes`].
///
/// The module-byte limit is checked before decoding, and the section-count
/// limit before reading the next section. Unsupported known IDs are rejected
/// after checking the section frame but not its contents; unknown IDs are
/// rejected before reading the size. Decodable sizes are checked against
/// their limit before checking availability. Bad UTF-8 is reported at the start
/// of the invalid or incomplete sequence.
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

    let mut last_standard = 0;
    let mut has_type = false;
    let mut has_function = false;
    let mut export = None;
    let mut code = None;
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
        let mut section = module.frame(declared).ok_or(Error {
            offset: module.end,
            kind: ErrorKind::TruncatedSection {
                declared,
                available,
            },
        })?;
        if id == 0 {
            section.name(LengthField::CustomNameSize)?;
            continue; // The rest of a custom section is opaque.
        }
        if id <= last_standard && matches!(id, 1 | 3 | 7 | 10) {
            return Err(Error {
                offset: id_offset,
                kind: ErrorKind::SectionOutOfOrder { id },
            });
        }
        match id {
            1 => {
                validate_type(&mut section)?;
                has_type = true;
            }
            3 => {
                section.count(IntegerField::FunctionCount, 1..=1)?;
                section.zero_index(IntegerField::TypeIndex)?;
                has_function = true;
            }
            7 => export = validate_export(&mut section)?,
            10 => code = Some(validate_code(&mut section)?),
            _ => {
                return Err(Error {
                    offset: id_offset,
                    kind: ErrorKind::UnsupportedSection { id },
                });
            }
        }
        section.finish(ErrorKind::TrailingSectionBytes { id })?;
        last_standard = id;
    }
    if !module.remaining.is_empty() {
        return Err(module.error(ErrorKind::LimitExceeded { limit }));
    }
    if last_standard != 0 {
        // The exact counts, zero indices, and presence of all three sections
        // together establish the type/function/code and export relationships.
        for (id, present) in [(1, has_type), (3, has_function), (10, code.is_some())] {
            if !present {
                return Err(module.error(ErrorKind::MissingSection { id }));
            }
        }
    }
    let export = if let Some(name) = export {
        let mut owned = String::new();
        owned
            .try_reserve_exact(name.len())
            .map_err(|_| module.error(ErrorKind::AllocationFailed))?;
        owned.push_str(name);
        Some(owned)
    } else {
        None
    };
    Ok(Module {
        function: code.map(|code| Function { code, export }),
    })
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

fn validate_type(section: &mut Reader<'_>) -> Result<(), Error> {
    section.count(IntegerField::TypeCount, 1..=1)?;
    section.expect_byte(0x60)?; // functype
    section.count(IntegerField::ParameterCount, 0..=0)?;
    section.count(IntegerField::ResultCount, 1..=1)?;
    section.expect_byte(0x7f) // i32
}

fn validate_export<'a>(section: &mut Reader<'a>) -> Result<Option<&'a str>, Error> {
    if section.count(IntegerField::ExportCount, 0..=1)? == 0 {
        return Ok(None);
    }
    let name = section.name(LengthField::ExportNameSize)?;
    section.expect_byte(0x00)?; // function export descriptor
    section.zero_index(IntegerField::FunctionIndex)?;
    Ok(Some(name))
}

const END: u8 = 0x0b;

#[derive(Clone, Copy, Debug)]
enum Instruction {
    Const(i32),
    Binary(BinaryOp),
}

#[derive(Clone, Copy, Debug)]
enum BinaryOp {
    Add,
    Sub,
    Mul,
}

/// Only the decoder below constructs this, after checking every stack effect
/// and the complete body frame. There is no way to mutate it after validation.
#[derive(Debug)]
pub(super) struct ValidatedCode {
    instructions: Vec<Instruction>,
}

impl ValidatedCode {
    pub(super) fn execute(&self) -> i32 {
        let mut operands = [0i32; Limit::OperandStackDepth.maximum()];
        let mut depth = 0;
        // Each stored instruction runs once, for at most BodyInstructions steps.
        // Validation starts at the same depth, proves every index fits, and
        // establishes that depth is one on completion.
        for &instruction in &self.instructions {
            match instruction {
                Instruction::Const(value) => {
                    operands[depth] = value;
                    depth += 1;
                }
                Instruction::Binary(op) => {
                    let rhs = operands[depth - 1];
                    let lhs = &mut operands[depth - 2];
                    *lhs = match op {
                        BinaryOp::Add => lhs.wrapping_add(rhs),
                        BinaryOp::Sub => lhs.wrapping_sub(rhs),
                        BinaryOp::Mul => lhs.wrapping_mul(rhs),
                    };
                    depth -= 1;
                }
            }
        }
        operands[0]
    }
}

fn validate_code(section: &mut Reader<'_>) -> Result<ValidatedCode, Error> {
    section.count(IntegerField::CodeCount, 1..=1)?;
    let declared = section.length(LengthField::CodeBodySize)?;
    let available = section.remaining.len();
    let mut body = section.frame(declared).ok_or(Error {
        offset: section.end,
        kind: ErrorKind::TruncatedBody {
            declared,
            available,
        },
    })?;
    body.count(IntegerField::LocalGroupCount, 0..=0)?;
    let mut decoded = [Instruction::Const(0); Limit::BodyInstructions.maximum()];
    let mut count = 0;
    let mut depth = 0usize;
    loop {
        let offset = body.position();
        let opcode = body
            .byte()
            .ok_or_else(|| body.error(ErrorKind::TruncatedByte { expected: END }))?;
        if opcode == END {
            if depth != 1 {
                return Err(Error {
                    offset,
                    kind: ErrorKind::InvalidResultDepth { depth },
                });
            }
            body.finish(ErrorKind::TrailingBodyBytes)?;
            let mut instructions = Vec::new();
            instructions.try_reserve_exact(count).map_err(|_| Error {
                offset,
                kind: ErrorKind::AllocationFailed,
            })?;
            instructions.extend_from_slice(&decoded[..count]);
            return Ok(ValidatedCode { instructions });
        }
        let slot = decoded.get_mut(count).ok_or(Error {
            offset,
            kind: ErrorKind::LimitExceeded {
                limit: Limit::BodyInstructions,
            },
        })?;
        let instruction = match opcode {
            0x41 => Instruction::Const(body.integer(IntegerField::I32Const)? as i32),
            0x6a => Instruction::Binary(BinaryOp::Add),
            0x6b => Instruction::Binary(BinaryOp::Sub),
            0x6c => Instruction::Binary(BinaryOp::Mul),
            _ => {
                return Err(Error {
                    offset,
                    kind: ErrorKind::UnsupportedOpcode { opcode },
                });
            }
        };
        depth = match instruction {
            Instruction::Const(_) => depth
                .checked_add(1)
                .filter(|&next| next <= Limit::OperandStackDepth.maximum())
                .ok_or(Error {
                    offset,
                    kind: ErrorKind::LimitExceeded {
                        limit: Limit::OperandStackDepth,
                    },
                })?,
            Instruction::Binary(_) => {
                // Consume both operands before accounting for the single result.
                let remaining = depth.checked_sub(2).ok_or(Error {
                    offset,
                    kind: ErrorKind::StackUnderflow { depth },
                })?;
                remaining + 1
            }
        };
        *slot = instruction;
        count += 1;
    }
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

    fn frame(&mut self, length: usize) -> Option<Reader<'a>> {
        let remaining = self.take(length)?;
        Some(Reader {
            remaining,
            end: self.position(),
        })
    }

    fn finish(&self, trailing: ErrorKind) -> Result<(), Error> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(self.error(trailing))
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), Error> {
        let offset = self.position();
        match self.byte() {
            Some(found) if found == expected => Ok(()),
            Some(found) => Err(Error {
                offset,
                kind: ErrorKind::UnexpectedByte { expected, found },
            }),
            None => Err(self.error(ErrorKind::TruncatedByte { expected })),
        }
    }

    fn name(&mut self, field: LengthField) -> Result<&'a str, Error> {
        let declared = self.length(field)?;
        let name_offset = self.position();
        let available = self.remaining.len();
        let name = self.take(declared).ok_or(Error {
            offset: self.end,
            kind: ErrorKind::TruncatedName {
                declared,
                available,
            },
        })?;
        str::from_utf8(name).map_err(|err| Error {
            offset: name_offset + err.valid_up_to(),
            kind: ErrorKind::InvalidUtf8Name,
        })
    }

    fn length(&mut self, field: LengthField) -> Result<usize, Error> {
        let start = self.position();
        let value = self.leb32(
            false,
            ErrorKind::TruncatedLength { field },
            ErrorKind::LengthOverflow { field },
        )?;
        let limit = match field {
            LengthField::SectionSize => Limit::SectionBytes,
            LengthField::CodeBodySize => Limit::CodeBodyBytes,
            LengthField::CustomNameSize => Limit::CustomNameBytes,
            LengthField::ExportNameSize => Limit::ExportNameBytes,
        };
        match usize::try_from(value) {
            Ok(length) if length <= limit.maximum() => Ok(length),
            _ => Err(Error {
                offset: start,
                kind: ErrorKind::LimitExceeded { limit },
            }),
        }
    }

    fn count(&mut self, field: IntegerField, allowed: RangeInclusive<u32>) -> Result<u32, Error> {
        let offset = self.position();
        let count = self.integer(field)?;
        if allowed.contains(&count) {
            Ok(count)
        } else {
            Err(Error {
                offset,
                kind: ErrorKind::UnsupportedCount { field, count },
            })
        }
    }

    fn zero_index(&mut self, field: IntegerField) -> Result<(), Error> {
        let offset = self.position();
        let index = self.integer(field)?;
        if index == 0 {
            Ok(())
        } else {
            Err(Error {
                offset,
                kind: ErrorKind::InvalidIndex { field, index },
            })
        }
    }

    fn integer(&mut self, field: IntegerField) -> Result<u32, Error> {
        self.leb32(
            field == IntegerField::I32Const,
            ErrorKind::TruncatedInteger { field },
            ErrorKind::IntegerOverflow { field },
        )
    }

    // For signed input the returned bits represent the two's-complement i32.
    fn leb32(
        &mut self,
        signed: bool,
        truncated: ErrorKind,
        overflow: ErrorKind,
    ) -> Result<u32, Error> {
        let mut value = 0u32;
        let mut shift = 0;
        loop {
            let offset = self.position();
            let byte = self.byte().ok_or_else(|| self.error(truncated))?;
            if shift == 28 {
                // The fifth byte must terminate; only four value bits remain.
                // Signed unused bits must agree with the true sign (bit 3).
                let fits = if signed {
                    byte <= 0x07 || (0x78..=0x7f).contains(&byte)
                } else {
                    byte <= 0x0f
                };
                if !fits {
                    return Err(Error {
                        offset,
                        kind: overflow,
                    });
                }
            }
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                if signed && shift < 28 && byte & 0x40 != 0 {
                    value |= u32::MAX << (shift + 7);
                }
                return Ok(value);
            }
            shift += 7;
        }
    }
}
