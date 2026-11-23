use crate::{
    ExecutionLimit, Export, Instance, InvokeError, Module, Trap,
    memory::{AccessWidth, DataSegment, LoadKind, MEMORY_PAGE_BYTES},
};
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
///
/// `UnexpectedElse` indicates a stray or duplicate `else`; `MissingElse`
/// indicates an i32-result `if` closed by `end` without an `else`. The depth in
/// `StackUnderflow` and `InvalidResultDepth` counts only operands above the
/// active frame's entry stack floor. The latter is reported at a delimiter if
/// the depth differs from the frame's result arity (zero for void, one for i32).
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
    TruncatedData { declared: usize, available: usize },
    InvalidUtf8Name,
    DuplicateExportName,
    UnsupportedCount { field: IntegerField, count: u32 },
    InvalidIndex { field: IntegerField, index: u32 },
    TruncatedMemoryLimitsFlags,
    UnsupportedMemoryLimitsFlags { flags: u8 },
    InvalidMemoryLimits { minimum: u32, maximum: u32 },
    TruncatedGlobalMutability,
    InvalidGlobalMutability { byte: u8 },
    ImmutableGlobal { index: u32 },
    MissingMemory,
    InvalidMemoryAlignment { exponent: u32 },
    TruncatedByte { expected: u8 },
    UnexpectedByte { expected: u8, found: u8 },
    TrailingSectionBytes { id: u8 },
    TrailingBodyBytes,
    UnsupportedOpcode { opcode: u8 },
    UnexpectedElse,
    MissingElse,
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
    DataSize,
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
    MemoryCount,
    GlobalCount,
    DataCount,
    MemoryIndex,
    MemoryMinimum,
    MemoryMaximum,
    MemoryAlignment,
    MemoryOffset,
    CodeCount,
    LocalGroupCount,
    TypeIndex,
    FunctionIndex,
    LocalIndex,
    GlobalIndex,
    LabelIndex,
    I32Const,
    /// Number of declared locals in one group.
    LocalCount,
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
    /// Decoded operations across the whole body, including `if`, `else` and `br_if`,
    /// but not `block`, `loop`, or `end`.
    BodyInstructions,
    /// Peak number of live i32 operands in the code body.
    OperandStackDepth,
    /// Number of simultaneously open `block`, `loop`, and `if` frames, excluding the function frame.
    ControlDepth,
    /// Name bytes in one custom section, excluding its length field.
    CustomNameBytes,
    /// Name bytes in one export, excluding its length field.
    ExportNameBytes,
    /// Total retained payload bytes across all data segments, including overlaps.
    DataBytes,
}

impl Limit {
    /// The inclusive maximum for this resource.
    pub const fn maximum(self) -> usize {
        match self {
            Self::ModuleBytes => 1024 * 1024,
            Self::SectionCount => 1024,
            Self::SectionBytes => 256 * 1024,
            Self::CodeBodyBytes => 256,
            Self::BodyInstructions => 127,
            Self::OperandStackDepth => 32,
            Self::ControlDepth => 16,
            Self::CustomNameBytes | Self::ExportNameBytes => 4 * 1024,
            Self::DataBytes => MEMORY_PAGE_BYTES,
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
            TruncatedData {
                declared,
                available,
            } => write!(
                f,
                "data segment declares {declared} bytes but only {available} remain in its section"
            ),
            InvalidUtf8Name => f.write_str("invalid UTF-8 in name"),
            DuplicateExportName => f.write_str("duplicate export name"),
            UnsupportedCount { field, count } => write!(f, "unsupported {field:?} of {count}"),
            InvalidIndex { field, index } => write!(f, "invalid {field:?} {index}"),
            TruncatedMemoryLimitsFlags => f.write_str("missing memory limits flags"),
            UnsupportedMemoryLimitsFlags { flags } => {
                write!(f, "unsupported memory limits flags {flags:#04x}")
            }
            InvalidMemoryLimits { minimum, maximum } => {
                write!(f, "memory minimum {minimum} exceeds maximum {maximum}")
            }
            TruncatedGlobalMutability => f.write_str("missing global mutability byte"),
            InvalidGlobalMutability { byte } => {
                write!(f, "invalid global mutability byte {byte:#04x}")
            }
            ImmutableGlobal { index } => write!(f, "cannot set immutable global {index}"),
            MissingMemory => {
                f.write_str("memory instruction or data segment requires a defined memory")
            }
            InvalidMemoryAlignment { exponent } => {
                write!(
                    f,
                    "i32 memory alignment exponent {exponent} exceeds natural alignment"
                )
            }
            TruncatedByte { expected } => write!(f, "missing expected byte {expected:#04x}"),
            UnexpectedByte { expected, found } => {
                write!(f, "expected byte {expected:#04x}, found {found:#04x}")
            }
            TrailingSectionBytes { id } => write!(f, "trailing bytes in section ID {id}"),
            TrailingBodyBytes => f.write_str("trailing bytes after code body end"),
            UnsupportedOpcode { opcode } => write!(f, "unsupported opcode {opcode:#04x}"),
            UnexpectedElse => f.write_str("else without a matching then arm"),
            MissingElse => f.write_str("i32-result if requires an else arm"),
            StackUnderflow { depth } => {
                write!(
                    f,
                    "not enough operands above the frame's stack floor, found {depth}"
                )
            }
            InvalidResultDepth { depth } => {
                write!(
                    f,
                    "invalid result depth above the frame's stack floor, found {depth}"
                )
            }
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
/// executing it. Work is at most linear in the input length under the fixed
/// section and vector caps. Allocations retain up to eight bodies bounded by
/// [`Limit::BodyInstructions`] and eight export entries with names bounded by
/// [`Limit::ExportNameBytes`], plus up to eight active data segments bounded in
/// total by [`Limit::DataBytes`]. Validation neither allocates linear memory
/// nor checks that the data ranges fit; both belong to instantiation.
///
/// The module-byte limit is checked before decoding, and the section-count
/// limit before reading the next section. Unsupported known IDs are rejected
/// after checking the section frame but not its contents; unknown IDs are
/// rejected before reading the size. Decodable sizes are checked against
/// their limit before checking availability. Bad UTF-8 is reported at the start
/// of the invalid or incomplete sequence.
/// A local group's count that exceeds the remaining declared-local budget is
/// reported as [`ErrorKind::UnsupportedCount`] with [`IntegerField::LocalCount`].
/// A code count differing from the function count is reported as
/// [`ErrorKind::UnsupportedCount`] with [`IntegerField::CodeCount`].
/// A repeated export name is reported at the start of its length field.
/// A `br_if` depth that does not name an enclosing block or loop is reported as
/// [`ErrorKind::InvalidIndex`] with [`IntegerField::LabelIndex`] at the start of
/// the immediate. This includes depths naming an `if` or the function label.
/// Oversized memory counts or page bounds are reported as
/// [`ErrorKind::UnsupportedCount`], and a reversed pair as
/// [`ErrorKind::InvalidMemoryLimits`] at the start of the maximum field.
/// [`ErrorKind::InvalidMemoryAlignment`] points to the memarg's exponent.
/// For `memory.size`, a malformed reserved byte is reported as
/// [`ErrorKind::TruncatedByte`] or [`ErrorKind::UnexpectedByte`].
/// [`ErrorKind::MissingMemory`] points to the instruction opcode or the data
/// segment's memory index. A data length above the remaining aggregate budget
/// is reported as [`ErrorKind::LimitExceeded`] at the start of the length field.
/// Oversized global counts are reported as [`ErrorKind::UnsupportedCount`]
/// with [`IntegerField::GlobalCount`]. Invalid or missing global mutability
/// bytes are reported as [`ErrorKind::InvalidGlobalMutability`] or
/// [`ErrorKind::TruncatedGlobalMutability`]. [`ErrorKind::ImmutableGlobal`]
/// points to the start of the `global.set` index field.
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
    let mut type_arities = [0; MAX_TYPES as usize];
    let mut type_count = None;
    let mut function_arities = [0; MAX_FUNCTIONS as usize];
    let mut function_count = None;
    let mut functions = Vec::new();
    let mut exports = Vec::new();
    let mut memory_bytes = None;
    let mut globals = [Global::default(); MAX_GLOBALS as usize];
    let mut global_count = 0;
    let mut data_segments = Vec::new();
    let mut has_function_sections = false;
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
        if id <= last_standard && matches!(id, 1 | 3 | 5 | 6 | 7 | 10 | 11) {
            return Err(Error {
                offset: id_offset,
                kind: ErrorKind::SectionOutOfOrder { id },
            });
        }
        match id {
            1 => type_count = Some(validate_types(&mut section, &mut type_arities)?),
            3 => {
                let count = section.count(IntegerField::FunctionCount, 1..=MAX_FUNCTIONS)?;
                // Without a type section, use a placeholder zero-arg type at
                // index zero while checking framing and order. Reject its absence
                // in the final required-section check.
                let bound = type_count.unwrap_or(1);
                for arity in &mut function_arities[..count as usize] {
                    let index = section.index(IntegerField::TypeIndex, bound)?;
                    *arity = type_arities[index as usize];
                }
                function_count = Some(count);
            }
            5 => memory_bytes = validate_memory(&mut section)?,
            6 => global_count = validate_globals(&mut section, &mut globals)?,
            7 => exports = validate_exports(&mut section, function_count)?,
            10 => {
                let declarations = function_count.map(|count| &function_arities[..count as usize]);
                functions = validate_code(
                    &mut section,
                    declarations,
                    type_arities[0],
                    memory_bytes.is_some(),
                    &globals[..global_count],
                )?;
            }
            11 => data_segments = validate_data(&mut section, memory_bytes.is_some())?,
            _ => {
                return Err(Error {
                    offset: id_offset,
                    kind: ErrorKind::UnsupportedSection { id },
                });
            }
        }
        section.finish(ErrorKind::TrailingSectionBytes { id })?;
        last_standard = id;
        has_function_sections |= matches!(id, 1 | 3 | 7 | 10);
    }
    if !module.remaining.is_empty() {
        return Err(module.error(ErrorKind::LimitExceeded { limit }));
    }
    if has_function_sections {
        // The presence of all three sections makes the count and index checks
        // above sufficient to associate each export and call with a validated body.
        for (id, present) in [
            (1, type_count.is_some()),
            (3, function_count.is_some()),
            (10, !functions.is_empty()),
        ] {
            if !present {
                return Err(module.error(ErrorKind::MissingSection { id }));
            }
        }
    }
    Ok(Module {
        functions,
        exports,
        memory_bytes,
        global_initial_values: globals.map(|global| global.initial_value),
        data_segments,
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

const MAX_TYPES: u32 = 8;
const MAX_FUNCTIONS: u32 = 8;
const MAX_EXPORTS: u32 = 8;
pub(super) const MAX_GLOBALS: u32 = 8;
const MAX_DATA_SEGMENTS: u32 = 8;
const MAX_MEMORY_PAGES: u32 = 1;
const MAX_PARAMETERS: u32 = 4;
const MAX_DECLARED_LOCALS: u32 = 16;
const MAX_LOCAL_SLOTS: usize = (MAX_PARAMETERS + MAX_DECLARED_LOCALS) as usize;
const I32: u8 = 0x7f;

fn validate_types(
    section: &mut Reader<'_>,
    arities: &mut [u32; MAX_TYPES as usize],
) -> Result<u32, Error> {
    let count = section.count(IntegerField::TypeCount, 1..=MAX_TYPES)?;
    for arity in &mut arities[..count as usize] {
        section.expect_byte(0x60)?; // functype
        let parameter_count = section.count(IntegerField::ParameterCount, 0..=MAX_PARAMETERS)?;
        for _ in 0..parameter_count {
            section.expect_byte(I32)?;
        }
        section.count(IntegerField::ResultCount, 1..=1)?;
        section.expect_byte(I32)?;
        *arity = parameter_count;
    }
    Ok(count)
}

fn validate_memory(section: &mut Reader<'_>) -> Result<Option<usize>, Error> {
    if section.count(IntegerField::MemoryCount, 0..=1)? == 0 {
        return Ok(None);
    }
    // Core 1.0 limits use a literal discriminant byte followed by uLEB32
    // page counts. Other flags, including shared and memory64, are unsupported.
    let offset = section.position();
    let flags = section
        .byte()
        .ok_or_else(|| section.error(ErrorKind::TruncatedMemoryLimitsFlags))?;
    if !matches!(flags, 0x00 | 0x01) {
        return Err(Error {
            offset,
            kind: ErrorKind::UnsupportedMemoryLimitsFlags { flags },
        });
    }
    let minimum = section.count(IntegerField::MemoryMinimum, 0..=MAX_MEMORY_PAGES)?;
    if flags == 0x01 {
        let offset = section.position();
        let maximum = section.count(IntegerField::MemoryMaximum, 0..=MAX_MEMORY_PAGES)?;
        if maximum < minimum {
            return Err(Error {
                offset,
                kind: ErrorKind::InvalidMemoryLimits { minimum, maximum },
            });
        }
    }
    // No growth: the maximum is validated but only the initial size matters.
    Ok(Some(minimum as usize * MEMORY_PAGE_BYTES))
}

#[derive(Clone, Copy, Default)]
struct Global {
    initial_value: i32,
    mutable: bool,
}

fn validate_globals(
    section: &mut Reader<'_>,
    globals: &mut [Global; MAX_GLOBALS as usize],
) -> Result<usize, Error> {
    let count = section.count(IntegerField::GlobalCount, 0..=MAX_GLOBALS)? as usize;
    for global in &mut globals[..count] {
        section.expect_byte(I32)?;
        let offset = section.position();
        let mutable = match section.byte() {
            Some(0) => false,
            Some(1) => true,
            Some(byte) => {
                return Err(Error {
                    offset,
                    kind: ErrorKind::InvalidGlobalMutability { byte },
                });
            }
            None => return Err(section.error(ErrorKind::TruncatedGlobalMutability)),
        };
        *global = Global {
            initial_value: section.i32_const_expr()?,
            mutable,
        };
    }
    Ok(count)
}

fn validate_exports(
    section: &mut Reader<'_>,
    function_count: Option<u32>,
) -> Result<Vec<Export>, Error> {
    let allocation_error = section.error(ErrorKind::AllocationFailed);
    let count = section.count(IntegerField::ExportCount, 0..=MAX_EXPORTS)?;
    let mut exports: Vec<Export> = Vec::new();
    exports
        .try_reserve_exact(count as usize)
        .map_err(|_| allocation_error)?;
    for _ in 0..count {
        let offset = section.position();
        let name = section.name(LengthField::ExportNameSize)?;
        if exports.iter().any(|export| export.name == name) {
            return Err(Error {
                offset,
                kind: ErrorKind::DuplicateExportName,
            });
        }
        section.expect_byte(0x00)?; // function export descriptor
        let index = if let Some(bound) = function_count {
            section.index(IntegerField::FunctionIndex, bound)?
        } else {
            // No valid target without a function section. Preserve framing and
            // order checks before the final required-section check rejects it.
            section.integer(IntegerField::FunctionIndex)?
        };
        let mut owned = String::new();
        owned.try_reserve_exact(name.len()).map_err(|_| Error {
            offset,
            kind: ErrorKind::AllocationFailed,
        })?;
        owned.push_str(name);
        exports.push(Export {
            name: owned,
            function_index: index as usize,
        });
    }
    Ok(exports)
}

fn validate_data(section: &mut Reader<'_>, has_memory: bool) -> Result<Vec<DataSegment>, Error> {
    let allocation_error = section.error(ErrorKind::AllocationFailed);
    let count = section.count(IntegerField::DataCount, 0..=MAX_DATA_SEGMENTS)?;
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(count as usize)
        .map_err(|_| allocation_error)?;
    let limit = Limit::DataBytes;
    let mut budget = limit.maximum();
    for _ in 0..count {
        // Core 1.0 encodes a memory index, not a bulk-memory flags byte.
        let index_offset = section.position();
        section.index(IntegerField::MemoryIndex, 1)?;
        if !has_memory {
            return Err(Error {
                offset: index_offset,
                kind: ErrorKind::MissingMemory,
            });
        }
        let offset = section.i32_const_expr()? as u32;

        let length_offset = section.position();
        let declared = section.length(LengthField::DataSize)?;
        if declared > budget {
            return Err(Error {
                offset: length_offset,
                kind: ErrorKind::LimitExceeded { limit },
            });
        }
        budget -= declared;
        let available = section.remaining.len();
        let payload = section.take(declared).ok_or(Error {
            offset: section.end,
            kind: ErrorKind::TruncatedData {
                declared,
                available,
            },
        })?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(declared).map_err(|_| Error {
            offset: length_offset,
            kind: ErrorKind::AllocationFailed,
        })?;
        bytes.extend_from_slice(payload);
        segments.push(DataSegment { offset, bytes });
    }
    Ok(segments)
}

const BLOCK: u8 = 0x02;
const LOOP: u8 = 0x03;
const EMPTY_BLOCK_TYPE: u8 = 0x40;
const IF: u8 = 0x04;
const ELSE: u8 = 0x05;
const END: u8 = 0x0b;
const BR_IF: u8 = 0x0d;
const I32_CONST: u8 = 0x41;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultArity {
    Void,
    I32,
}

impl ResultArity {
    fn count(self) -> usize {
        match self {
            Self::Void => 0,
            Self::I32 => 1,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Instruction {
    Nop,
    Drop,
    Select,
    Const(i32),
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    Call(u32),
    Unary(UnaryOp),
    Binary(BinaryOp),
    LoadI32 {
        kind: LoadKind,
        offset: u32,
    },
    StoreI32 {
        width: AccessWidth,
        offset: u32,
    },
    MemorySize,
    JumpIfZero {
        target: usize,
    },
    Jump {
        target: usize,
    },
    BranchIfNonzero {
        target: usize,
        floor: usize,
        result: ResultArity,
    },
    ContinueIfNonzero {
        target: LoopTarget,
        floor: usize,
    },
}

/// The first decoded slot of the target loop body, no later than the
/// continue instruction itself. Forward block and if edges do not use this.
#[derive(Clone, Copy, Debug)]
struct LoopTarget(usize);

impl LoopTarget {
    fn at_branch(body_pc: usize, branch_pc: usize) -> Self {
        // The decoder recorded body_pc at loop entry, and the count only grows.
        assert!(body_pc <= branch_pc);
        Self(body_pc)
    }
}

#[derive(Clone, Copy, Debug)]
enum UnaryOp {
    Eqz,
    Clz,
    Ctz,
    Popcnt,
}

impl UnaryOp {
    fn apply(self, operand: i32) -> i32 {
        let bits = operand as u32;
        match self {
            Self::Eqz => i32::from(operand == 0),
            Self::Clz => bits.leading_zeros() as i32,
            Self::Ctz => bits.trailing_zeros() as i32,
            Self::Popcnt => bits.count_ones() as i32,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum BinaryOp {
    Eq,
    Ne,
    LtS,
    LtU,
    GtS,
    GtU,
    LeS,
    LeU,
    GeS,
    GeU,
    Add,
    Sub,
    Mul,
    DivS,
    DivU,
    RemS,
    RemU,
    And,
    Or,
    Xor,
    Shl,
    ShrS,
    ShrU,
    Rotl,
    Rotr,
}

impl BinaryOp {
    fn apply(self, lhs: i32, rhs: i32) -> Result<i32, Trap> {
        // Shifts and rotates use the low five bits even for negative counts.
        let shift = (rhs as u32) & 31;
        match self {
            Self::Eq => Ok(i32::from(lhs == rhs)),
            Self::Ne => Ok(i32::from(lhs != rhs)),
            Self::LtS => Ok(i32::from(lhs < rhs)),
            Self::LtU => Ok(i32::from((lhs as u32) < (rhs as u32))),
            Self::GtS => Ok(i32::from(lhs > rhs)),
            Self::GtU => Ok(i32::from((lhs as u32) > (rhs as u32))),
            Self::LeS => Ok(i32::from(lhs <= rhs)),
            Self::LeU => Ok(i32::from((lhs as u32) <= (rhs as u32))),
            Self::GeS => Ok(i32::from(lhs >= rhs)),
            Self::GeU => Ok(i32::from((lhs as u32) >= (rhs as u32))),
            Self::Add => Ok(lhs.wrapping_add(rhs)),
            Self::Sub => Ok(lhs.wrapping_sub(rhs)),
            Self::Mul => Ok(lhs.wrapping_mul(rhs)),
            Self::DivS if rhs == 0 => Err(Trap::IntegerDivideByZero),
            Self::DivS => lhs.checked_div(rhs).ok_or(Trap::IntegerOverflow),
            // Same-width casts reinterpret the operands and preserve the result bits.
            Self::DivU => (lhs as u32)
                .checked_div(rhs as u32)
                .map(|value| value as i32)
                .ok_or(Trap::IntegerDivideByZero),
            // Wasm remainder by -1 is zero even for i32::MIN, unlike checked_rem.
            Self::RemS if rhs == -1 => Ok(0),
            Self::RemS => lhs.checked_rem(rhs).ok_or(Trap::IntegerDivideByZero),
            Self::RemU => (lhs as u32)
                .checked_rem(rhs as u32)
                .map(|value| value as i32)
                .ok_or(Trap::IntegerDivideByZero),
            Self::And => Ok(lhs & rhs),
            Self::Or => Ok(lhs | rhs),
            Self::Xor => Ok(lhs ^ rhs),
            Self::Shl => Ok(((lhs as u32) << shift) as i32),
            Self::ShrS => Ok(lhs >> shift),
            Self::ShrU => Ok(((lhs as u32) >> shift) as i32),
            Self::Rotl => Ok((lhs as u32).rotate_left(shift) as i32),
            Self::Rotr => Ok((lhs as u32).rotate_right(shift) as i32),
        }
    }
}

/// Only the decoder constructs this, after checking all indices, global set
/// mutability, every stack effect, and the complete body frame. Block and if
/// jump targets are strictly later decoded instruction indices or the
/// one-past-last index. Block exits carry their target floor and result arity.
/// Loop continues target an in-body index at or before the branch.
#[derive(Debug)]
pub(super) struct ValidatedCode {
    parameter_count: usize,
    instructions: Vec<Instruction>,
}

struct CallFrame {
    function_index: usize,
    pc: usize,
    locals: [i32; MAX_LOCAL_SLOTS],
    operands: [i32; Limit::OperandStackDepth.maximum()],
    depth: usize,
}

impl CallFrame {
    const EMPTY: Self = Self {
        function_index: 0,
        pc: 0,
        locals: [0; MAX_LOCAL_SLOTS],
        operands: [0; Limit::OperandStackDepth.maximum()],
        depth: 0,
    };

    fn new(function_index: usize, args: &[i32]) -> Self {
        let mut frame = Self {
            function_index,
            ..Self::EMPTY
        };
        frame.locals[..args.len()].copy_from_slice(args);
        frame
    }

    fn push(&mut self, value: i32) {
        self.operands[self.depth] = value;
        self.depth += 1;
    }

    fn pop(&mut self) -> i32 {
        self.depth -= 1;
        self.operands[self.depth]
    }
}

pub(super) fn execute(
    instance: &Instance<'_>,
    function_index: usize,
    args: &[i32],
) -> Result<i32, InvokeError> {
    let module = instance.module;
    let parameter_count = module.functions[function_index].parameter_count;
    if args.len() != parameter_count {
        return Err(InvokeError::ArityMismatch {
            expected: parameter_count,
            actual: args.len(),
        });
    }
    let mut frames = [CallFrame::EMPTY; ExecutionLimit::CallDepth.maximum()];
    frames[0] = CallFrame::new(function_index, args);
    let mut active = 0;
    let mut remaining_dispatches = ExecutionLimit::InstructionDispatches.maximum();
    loop {
        let frame = &mut frames[active];
        // Export and call indices are bounded by the exact decoded body count.
        let code = &module.functions[frame.function_index];
        let Some(&instruction) = code.instructions.get(frame.pc) else {
            // At the one-past-last PC validation guarantees one result. Returning
            // does not spend fuel, even when the final dispatch used it up.
            debug_assert_eq!(frame.pc, code.instructions.len());
            debug_assert_eq!(frame.depth, 1);
            let value = frame.operands[0];
            if active == 0 {
                return Ok(value);
            }
            active -= 1;
            frames[active].push(value);
            continue;
        };
        if remaining_dispatches == 0 {
            return Err(InvokeError::LimitExceeded {
                limit: ExecutionLimit::InstructionDispatches,
            });
        }
        remaining_dispatches -= 1;
        frame.pc += 1;
        match instruction {
            Instruction::Nop => {}
            Instruction::Drop => {
                frame.pop();
            }
            Instruction::Select => {
                let condition = frame.pop();
                let second = frame.pop();
                let first = frame.pop();
                frame.push(if condition != 0 { first } else { second });
            }
            Instruction::Const(value) => frame.push(value),
            Instruction::LocalGet(index) => frame.push(frame.locals[index as usize]),
            Instruction::LocalSet(index) => frame.locals[index as usize] = frame.pop(),
            Instruction::LocalTee(index) => {
                frame.locals[index as usize] = frame.operands[frame.depth - 1]
            }
            Instruction::GlobalGet(index) => frame.push(instance.globals[index as usize].get()),
            Instruction::GlobalSet(index) => instance.globals[index as usize].set(frame.pop()),
            Instruction::Call(index) => {
                if active + 1 == ExecutionLimit::CallDepth.maximum() {
                    return Err(InvokeError::LimitExceeded {
                        limit: ExecutionLimit::CallDepth,
                    });
                }
                // Keep the caller's prefix; the argument suffix is already in
                // parameter order. Reset reused frame storage before entering.
                let index = index as usize;
                let end = frame.depth;
                frame.depth -= module.functions[index].parameter_count;
                let callee = CallFrame::new(index, &frame.operands[frame.depth..end]);
                active += 1;
                frames[active] = callee;
            }
            Instruction::Unary(op) => {
                let top = &mut frame.operands[frame.depth - 1];
                *top = op.apply(*top);
            }
            Instruction::Binary(op) => {
                let rhs = frame.pop();
                let lhs = &mut frame.operands[frame.depth - 1];
                *lhs = op.apply(*lhs, rhs).map_err(InvokeError::Trap)?;
            }
            Instruction::LoadI32 { kind, offset } => {
                let address_and_result = &mut frame.operands[frame.depth - 1];
                *address_and_result = instance
                    .memory
                    .load_i32(*address_and_result, offset, kind)
                    .map_err(InvokeError::Trap)?;
            }
            Instruction::StoreI32 { width, offset } => {
                let value = frame.pop();
                let address = frame.pop();
                instance
                    .memory
                    .store_i32(address, offset, width, value)
                    .map_err(InvokeError::Trap)?;
            }
            Instruction::MemorySize => frame.push(instance.memory.size_in_pages()),
            Instruction::JumpIfZero { target } => {
                if frame.pop() == 0 {
                    frame.pc = target;
                }
            }
            Instruction::Jump { target } => frame.pc = target,
            Instruction::BranchIfNonzero {
                target,
                floor,
                result,
            } => {
                if frame.pop() != 0 {
                    let value = match result {
                        ResultArity::Void => None,
                        ResultArity::I32 => Some(frame.pop()),
                    };
                    // Match the depth of a normal block exit, preserving its prefix.
                    frame.depth = floor;
                    if let Some(value) = value {
                        frame.push(value);
                    }
                    frame.pc = target;
                }
            }
            Instruction::ContinueIfNonzero { target, floor } => {
                if frame.pop() != 0 {
                    // Restart with the loop's entry stack and unchanged locals.
                    frame.depth = floor;
                    frame.pc = target.0;
                }
            }
        }
    }
}

fn validate_locals(body: &mut Reader<'_>) -> Result<u32, Error> {
    let groups = body.integer(IntegerField::LocalGroupCount)?;
    let mut remaining = MAX_DECLARED_LOCALS;
    // Every group consumes at least two bytes of the bounded body, even if its
    // count is zero. No declarations are expanded or allocated here.
    for _ in 0..groups {
        let count = body.count(IntegerField::LocalCount, 0..=remaining)?;
        body.expect_byte(I32)?;
        // Count was checked against the remaining budget before subtraction.
        remaining -= count;
    }
    Ok(MAX_DECLARED_LOCALS - remaining)
}

#[derive(Clone, Copy)]
struct ControlFrame {
    base: usize,
    result: ResultArity,
    kind: ControlKind,
}

#[derive(Clone, Copy)]
enum ControlKind {
    Block { last_branch: Option<usize> },
    Loop { body_pc: usize },
    Then { if_pc: usize },
    Else { else_pc: usize },
}

fn check_result_depth(depth: usize, expected: usize, offset: usize) -> Result<(), Error> {
    if depth == expected {
        Ok(())
    } else {
        Err(Error {
            offset,
            kind: ErrorKind::InvalidResultDepth { depth },
        })
    }
}

fn require_memory(has_memory: bool, offset: usize) -> Result<(), Error> {
    if has_memory {
        Ok(())
    } else {
        Err(Error {
            offset,
            kind: ErrorKind::MissingMemory,
        })
    }
}

fn decode_memarg(
    body: &mut Reader<'_>,
    opcode_offset: usize,
    width: AccessWidth,
    has_memory: bool,
) -> Result<u32, Error> {
    let alignment_offset = body.position();
    let exponent = body.integer(IntegerField::MemoryAlignment)?;
    let memory_offset = body.integer(IntegerField::MemoryOffset)?;
    if exponent > width.alignment_exponent() {
        return Err(Error {
            offset: alignment_offset,
            kind: ErrorKind::InvalidMemoryAlignment { exponent },
        });
    }
    require_memory(has_memory, opcode_offset)?;
    Ok(memory_offset)
}

fn decode_plain(
    body: &mut Reader<'_>,
    opcode: u8,
    offset: usize,
    local_bound: u32,
    function_bound: u32,
    has_memory: bool,
    globals: &[Global],
) -> Result<Instruction, Error> {
    let load = |body: &mut Reader<'_>, kind: LoadKind| {
        let offset = decode_memarg(body, offset, kind.width(), has_memory)?;
        Ok(Instruction::LoadI32 { kind, offset })
    };
    let store = |body: &mut Reader<'_>, width: AccessWidth| {
        let offset = decode_memarg(body, offset, width, has_memory)?;
        Ok(Instruction::StoreI32 { width, offset })
    };
    Ok(match opcode {
        0x01 => Instruction::Nop,
        0x10 => Instruction::Call(body.index(IntegerField::FunctionIndex, function_bound)?),
        0x1a => Instruction::Drop,
        0x1b => Instruction::Select,
        0x20 => Instruction::LocalGet(body.index(IntegerField::LocalIndex, local_bound)?),
        0x21 => Instruction::LocalSet(body.index(IntegerField::LocalIndex, local_bound)?),
        0x22 => Instruction::LocalTee(body.index(IntegerField::LocalIndex, local_bound)?),
        0x23 => {
            Instruction::GlobalGet(body.index(IntegerField::GlobalIndex, globals.len() as u32)?)
        }
        0x24 => {
            let offset = body.position();
            let index = body.index(IntegerField::GlobalIndex, globals.len() as u32)?;
            if !globals[index as usize].mutable {
                return Err(Error {
                    offset,
                    kind: ErrorKind::ImmutableGlobal { index },
                });
            }
            Instruction::GlobalSet(index)
        }
        0x28 => load(body, LoadKind::Full)?,
        0x2c => load(body, LoadKind::ByteSigned)?,
        0x2d => load(body, LoadKind::ByteUnsigned)?,
        0x2e => load(body, LoadKind::HalfwordSigned)?,
        0x2f => load(body, LoadKind::HalfwordUnsigned)?,
        0x36 => store(body, AccessWidth::Word)?,
        0x3a => store(body, AccessWidth::Byte)?,
        0x3b => store(body, AccessWidth::Halfword)?,
        0x3f => {
            // Core 1.0 reserves a literal zero byte here, not a LEB32 index.
            body.expect_byte(0)?;
            require_memory(has_memory, offset)?;
            Instruction::MemorySize
        }
        I32_CONST => Instruction::Const(body.integer(IntegerField::I32Const)? as i32),
        0x45 => Instruction::Unary(UnaryOp::Eqz),
        0x46 => Instruction::Binary(BinaryOp::Eq),
        0x47 => Instruction::Binary(BinaryOp::Ne),
        0x48 => Instruction::Binary(BinaryOp::LtS),
        0x49 => Instruction::Binary(BinaryOp::LtU),
        0x4a => Instruction::Binary(BinaryOp::GtS),
        0x4b => Instruction::Binary(BinaryOp::GtU),
        0x4c => Instruction::Binary(BinaryOp::LeS),
        0x4d => Instruction::Binary(BinaryOp::LeU),
        0x4e => Instruction::Binary(BinaryOp::GeS),
        0x4f => Instruction::Binary(BinaryOp::GeU),
        0x67 => Instruction::Unary(UnaryOp::Clz),
        0x68 => Instruction::Unary(UnaryOp::Ctz),
        0x69 => Instruction::Unary(UnaryOp::Popcnt),
        0x6a => Instruction::Binary(BinaryOp::Add),
        0x6b => Instruction::Binary(BinaryOp::Sub),
        0x6c => Instruction::Binary(BinaryOp::Mul),
        0x6d => Instruction::Binary(BinaryOp::DivS),
        0x6e => Instruction::Binary(BinaryOp::DivU),
        0x6f => Instruction::Binary(BinaryOp::RemS),
        0x70 => Instruction::Binary(BinaryOp::RemU),
        0x71 => Instruction::Binary(BinaryOp::And),
        0x72 => Instruction::Binary(BinaryOp::Or),
        0x73 => Instruction::Binary(BinaryOp::Xor),
        0x74 => Instruction::Binary(BinaryOp::Shl),
        0x75 => Instruction::Binary(BinaryOp::ShrS),
        0x76 => Instruction::Binary(BinaryOp::ShrU),
        0x77 => Instruction::Binary(BinaryOp::Rotl),
        0x78 => Instruction::Binary(BinaryOp::Rotr),
        _ => {
            return Err(Error {
                offset,
                kind: ErrorKind::UnsupportedOpcode { opcode },
            });
        }
    })
}

fn validate_code(
    section: &mut Reader<'_>,
    declarations: Option<&[u32]>,
    first_type_arity: u32,
    has_memory: bool,
    globals: &[Global],
) -> Result<Vec<ValidatedCode>, Error> {
    let allocation_error = section.error(ErrorKind::AllocationFailed);
    let allowed_counts = declarations.map_or(1..=MAX_FUNCTIONS, |arities| {
        let count = arities.len() as u32;
        count..=count
    });
    let count = section.count(IntegerField::CodeCount, allowed_counts)?;
    let mut functions = Vec::new();
    functions
        .try_reserve_exact(count as usize)
        .map_err(|_| allocation_error)?;
    for index in 0..count {
        let (parameter_count, function_arities) = match declarations {
            Some(arities) => (arities[index as usize], arities),
            // Preserve framing and ordering diagnostics for incomplete modules:
            // without declarations there are no call targets, and the first type
            // gives the placeholder arity until the final required-section check.
            None => (first_type_arity, &[][..]),
        };
        functions.push(validate_body(
            section,
            parameter_count,
            function_arities,
            has_memory,
            globals,
        )?);
    }
    Ok(functions)
}

fn validate_body(
    section: &mut Reader<'_>,
    parameter_count: u32,
    function_arities: &[u32],
    has_memory: bool,
    globals: &[Global],
) -> Result<ValidatedCode, Error> {
    let declared = section.length(LengthField::CodeBodySize)?;
    let available = section.remaining.len();
    let mut body = section.frame(declared).ok_or(Error {
        offset: section.end,
        kind: ErrorKind::TruncatedBody {
            declared,
            available,
        },
    })?;
    let local_bound = parameter_count + validate_locals(&mut body)?;
    // Forward transfers reserve slots that their matching delimiters backpatch.
    // Each target block chains its incoming branches by PC, even from nested
    // frames; each link is visited once.
    // A loop branch is resolved immediately to the PC saved at entry.
    let mut decoded = [Instruction::Const(0); Limit::BodyInstructions.maximum()];
    let mut previous_branch = [None; Limit::BodyInstructions.maximum()];
    let mut count = 0;
    let mut depth = 0usize;
    let mut frames = [ControlFrame {
        base: 0,
        result: ResultArity::Void,
        kind: ControlKind::Block { last_branch: None },
    }; Limit::ControlDepth.maximum()];
    let mut nesting = 0;
    let end_offset = loop {
        let offset = body.position();
        let frame = frames[..nesting].last().copied();
        let base = frame.map_or(0, |frame| frame.base);
        let result = frame.map_or(ResultArity::I32, |frame| frame.result);
        let kind = frame.map(|frame| frame.kind);
        let available = depth - base;
        let requires_else =
            result == ResultArity::I32 && matches!(kind, Some(ControlKind::Then { .. }));
        let opcode = body.byte().ok_or_else(|| {
            let expected = if requires_else { ELSE } else { END };
            body.error(ErrorKind::TruncatedByte { expected })
        })?;
        if opcode == END {
            if requires_else {
                return Err(Error {
                    offset,
                    kind: ErrorKind::MissingElse,
                });
            }
            check_result_depth(available, result.count(), offset)?;
            // Forward edges target the next decoded slot, possibly the final
            // one-past-last index. All sources are in the decoded prefix.
            match kind {
                Some(ControlKind::Loop { .. }) => {}
                Some(ControlKind::Block { mut last_branch }) => {
                    while let Some(pc) = last_branch {
                        decoded[..count][pc] = Instruction::BranchIfNonzero {
                            target: count,
                            floor: base,
                            result,
                        };
                        last_branch = previous_branch[pc];
                    }
                }
                Some(ControlKind::Else { else_pc }) => {
                    decoded[..count][else_pc] = Instruction::Jump { target: count };
                }
                None => {
                    body.finish(ErrorKind::TrailingBodyBytes)?;
                    break offset;
                }
                Some(ControlKind::Then { if_pc }) => {
                    // The implicit empty else skips straight to the join.
                    decoded[..count][if_pc] = Instruction::JumpIfZero { target: count };
                }
            }
            nesting -= 1;
            continue;
        }
        let pc = count;
        if !matches!(opcode, BLOCK | LOOP) {
            if pc == decoded.len() {
                return Err(Error {
                    offset,
                    kind: ErrorKind::LimitExceeded {
                        limit: Limit::BodyInstructions,
                    },
                });
            }
            count += 1;
        }
        let consume = |operands| {
            available
                .checked_sub(operands)
                .map(|left| base + left)
                .ok_or(Error {
                    offset,
                    kind: ErrorKind::StackUnderflow { depth: available },
                })
        };
        match opcode {
            BLOCK | LOOP | IF => {
                let result = if opcode == LOOP {
                    body.expect_byte(EMPTY_BLOCK_TYPE)?;
                    ResultArity::Void
                } else {
                    body.block_result()?
                };
                let next = frames.get_mut(nesting).ok_or(Error {
                    offset,
                    kind: ErrorKind::LimitExceeded {
                        limit: Limit::ControlDepth,
                    },
                })?;
                let kind = match opcode {
                    BLOCK => ControlKind::Block { last_branch: None },
                    LOOP => ControlKind::Loop { body_pc: pc },
                    IF => {
                        depth = consume(1)?;
                        ControlKind::Then { if_pc: pc }
                    }
                    _ => unreachable!("only control openers"),
                };
                *next = ControlFrame {
                    base: depth,
                    result,
                    kind,
                };
                nesting += 1;
            }
            ELSE => {
                let Some(ControlKind::Then { if_pc }) = kind else {
                    return Err(Error {
                        offset,
                        kind: ErrorKind::UnexpectedElse,
                    });
                };
                check_result_depth(available, result.count(), offset)?;
                // Skip the then arm and its else jump on a zero condition.
                decoded[..pc][if_pc] = Instruction::JumpIfZero { target: count };
                frames[nesting - 1].kind = ControlKind::Else { else_pc: pc };
                depth = base;
            }
            BR_IF => {
                // Count all explicit frames. An if label or the implicit
                // function label is not a supported destination.
                let label_offset = body.position();
                let label = body.index(IntegerField::LabelIndex, nesting as u32)?;
                let target = &mut frames[nesting - 1 - label as usize];
                // `consume` checks the active floor; only the taken edge uses
                // the target floor. Fallthrough pops just the condition.
                match &mut target.kind {
                    ControlKind::Block { last_branch } => {
                        let results = target.result.count();
                        depth = consume(1 + results)? + results;
                        previous_branch[pc] = *last_branch;
                        *last_branch = Some(pc);
                    }
                    ControlKind::Loop { body_pc } => {
                        depth = consume(1)?;
                        decoded[pc] = Instruction::ContinueIfNonzero {
                            target: LoopTarget::at_branch(*body_pc, pc),
                            floor: target.base,
                        };
                    }
                    ControlKind::Then { .. } | ControlKind::Else { .. } => {
                        return Err(Error {
                            offset: label_offset,
                            kind: ErrorKind::InvalidIndex {
                                field: IntegerField::LabelIndex,
                                index: label,
                            },
                        });
                    }
                }
            }
            _ => {
                let instruction = decode_plain(
                    &mut body,
                    opcode,
                    offset,
                    local_bound,
                    function_arities.len() as u32,
                    has_memory,
                    globals,
                )?;
                depth = match instruction {
                    Instruction::Nop => depth,
                    Instruction::Select => consume(3)? + 1,
                    Instruction::Const(_)
                    | Instruction::LocalGet(_)
                    | Instruction::GlobalGet(_)
                    | Instruction::MemorySize => depth + 1,
                    Instruction::Call(index) => {
                        consume(function_arities[index as usize] as usize)? + 1
                    }
                    Instruction::Drop | Instruction::LocalSet(_) | Instruction::GlobalSet(_) => {
                        consume(1)?
                    }
                    Instruction::LocalTee(_)
                    | Instruction::Unary(_)
                    | Instruction::LoadI32 { .. } => consume(1)? + 1,
                    Instruction::StoreI32 { .. } => consume(2)?,
                    Instruction::Binary(_) => consume(2)? + 1,
                    Instruction::JumpIfZero { .. }
                    | Instruction::Jump { .. }
                    | Instruction::BranchIfNonzero { .. }
                    | Instruction::ContinueIfNonzero { .. } => {
                        unreachable!("plain instructions do not create jumps")
                    }
                };
                if depth > Limit::OperandStackDepth.maximum() {
                    return Err(Error {
                        offset,
                        kind: ErrorKind::LimitExceeded {
                            limit: Limit::OperandStackDepth,
                        },
                    });
                }
                decoded[pc] = instruction;
            }
        }
    };
    let mut instructions = Vec::new();
    instructions.try_reserve_exact(count).map_err(|_| Error {
        offset: end_offset,
        kind: ErrorKind::AllocationFailed,
    })?;
    instructions.extend_from_slice(&decoded[..count]);
    Ok(ValidatedCode {
        parameter_count: parameter_count as usize,
        instructions,
    })
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

    fn block_result(&mut self) -> Result<ResultArity, Error> {
        let offset = self.position();
        // Preserve the original expected byte in errors for block and if.
        match self.byte() {
            Some(EMPTY_BLOCK_TYPE) => Ok(ResultArity::Void),
            Some(I32) => Ok(ResultArity::I32),
            Some(found) => Err(Error {
                offset,
                kind: ErrorKind::UnexpectedByte {
                    expected: I32,
                    found,
                },
            }),
            None => Err(self.error(ErrorKind::TruncatedByte { expected: I32 })),
        }
    }

    fn i32_const_expr(&mut self) -> Result<i32, Error> {
        self.expect_byte(I32_CONST)?;
        let value = self.integer(IntegerField::I32Const)? as i32;
        self.expect_byte(END)?;
        Ok(value)
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
            LengthField::DataSize => Limit::DataBytes,
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

    fn index(&mut self, field: IntegerField, bound: u32) -> Result<u32, Error> {
        let offset = self.position();
        let index = self.integer(field)?;
        if index < bound {
            Ok(index)
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
