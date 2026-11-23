use crate::{
    ExecutionLimit, Export, Instance, InvokeError, Module, ResultKind, Trap,
    memory::{
        AccessWidth, DataSegment, LoadKind, MAX_MEMORY_PAGES, MEMORY_PAGE_BYTES, MemoryLimits,
    },
    table::{ElementSegment, MAX_TABLE_ENTRIES},
};
use std::{cmp::Ordering, fmt, ops::RangeInclusive, str};

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
/// indicates a value-producing `if` closed by `end` without an `else`. The
/// depth in `StackUnderflow` and `InvalidResultDepth` counts only operands
/// above the active frame's entry stack floor. The latter is reported at a
/// delimiter if the depth differs from the frame's result arity (zero for
/// void, one for a numeric value), unless a missing result is supplied by an
/// unreachable frame's polymorphic floor. `TypeMismatch` is reported at the
/// consuming opcode or delimiter.
/// `BranchTableArityMismatch` and `BranchTableTypeMismatch` compare a
/// `br_table` target's result to that of the first target. The former is
/// reported for void versus a value, the latter for different numeric types.
/// `InvalidStartSignature` reports the parameter count and void or i32 result
/// of a start target that is not `() -> ()`; `InvalidStartI64Result` and
/// `InvalidStartFloatResult` report start targets with i64 or float results.
/// [`ErrorKind::UnsupportedExportSignature`] is retained for compatibility;
/// validation no longer emits it for exports with float parameters or results.
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
    TruncatedFloatConst { ty: ValType },
    TruncatedSection { declared: usize, available: usize },
    TruncatedBody { declared: usize, available: usize },
    TruncatedName { declared: usize, available: usize },
    TruncatedData { declared: usize, available: usize },
    InvalidUtf8Name,
    DuplicateExportName,
    UnsupportedCount { field: IntegerField, count: u32 },
    InvalidIndex { field: IntegerField, index: u32 },
    InvalidStartSignature { parameters: u32, result: ResultKind },
    InvalidStartI64Result { parameters: u32 },
    InvalidStartFloatResult { parameters: u32, result: ValType },
    UnsupportedExportSignature { function: u32 },
    TruncatedTableLimitsFlags,
    UnsupportedTableLimitsFlags { flags: u8 },
    InvalidTableLimits { minimum: u32, maximum: u32 },
    MissingTable,
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
    BranchTableArityMismatch { expected: usize, actual: usize },
    BranchTableTypeMismatch { expected: ValType, actual: ValType },
    StackUnderflow { depth: usize },
    TypeMismatch { expected: ValType, actual: ValType },
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

/// A count, index, or immediate encoded as a LEB32 integer (LEB64 for `I64Const`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum IntegerField {
    TypeCount,
    ParameterCount,
    ResultCount,
    FunctionCount,
    ExportCount,
    TableCount,
    TableMinimum,
    TableMaximum,
    TableIndex,
    ElementCount,
    /// Number of function indices in an active element segment.
    ElementFunctionCount,
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
    /// Number of explicit `br_table` labels, excluding the mandatory default.
    BranchTableCount,
    I32Const,
    I64Const,
    /// Number of declared locals in one group.
    LocalCount,
}

impl IntegerField {
    fn leb_width(self) -> u8 {
        if self == Self::I64Const { 64 } else { 32 }
    }
}

/// A concrete operand type. Only integers are supported in function
/// signatures and across the host boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
}

impl fmt::Display for ValType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
        })
    }
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
    /// Decoded operations across the whole body, including `if`, `else`, `br`, `br_if`,
    /// and `br_table`, but not `block`, `loop`, or `end`.
    BodyInstructions,
    /// Peak number of explicitly stacked operands of any type during body validation.
    OperandStackDepth,
    /// Number of simultaneously open `block`, `loop`, and `if` frames, excluding the function frame.
    ControlDepth,
    /// Name bytes in one custom section, excluding its length field.
    CustomNameBytes,
    /// Name bytes in one export, excluding its length field.
    ExportNameBytes,
    /// Total retained payload bytes across all data segments, including overlaps.
    DataBytes,
    /// Total retained function indices across all active element segments, including overlaps.
    ElementFunctionIndices,
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
            Self::ElementFunctionIndices => 64,
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
            TruncatedInteger { field } => {
                write!(f, "truncated LEB{} for {field:?}", field.leb_width())
            }
            IntegerOverflow { field } => {
                write!(f, "LEB{} overflow for {field:?}", field.leb_width())
            }
            TruncatedFloatConst { ty } => write!(f, "truncated fixed-width {ty}.const immediate"),
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
            InvalidStartSignature { parameters, result } => write!(
                f,
                "start function must take no parameters and return void, found {parameters} parameters and {result} result"
            ),
            InvalidStartI64Result { parameters } => write!(
                f,
                "start function must take no parameters and return void, found {parameters} parameters and i64 result"
            ),
            InvalidStartFloatResult { parameters, result } => write!(
                f,
                "start function must take no parameters and return void, found {parameters} parameters and {result} result"
            ),
            UnsupportedExportSignature { function } => write!(
                f,
                "cannot export function {function}: float parameters and results are not supported in exports"
            ),
            TruncatedTableLimitsFlags => f.write_str("missing table limits flags"),
            UnsupportedTableLimitsFlags { flags } => {
                write!(f, "unsupported table limits flags {flags:#04x}")
            }
            InvalidTableLimits { minimum, maximum } => {
                write!(f, "table minimum {minimum} exceeds maximum {maximum}")
            }
            MissingTable => f.write_str("a defined table is required"),
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
            MissingElse => f.write_str("value-producing if requires an else arm"),
            BranchTableArityMismatch { expected, actual } => {
                write!(
                    f,
                    "br_table label has result arity {actual}, expected {expected}"
                )
            }
            BranchTableTypeMismatch { expected, actual } => {
                write!(
                    f,
                    "br_table label has result type {actual}, expected {expected}"
                )
            }
            StackUnderflow { depth } => {
                write!(
                    f,
                    "not enough operands above the frame's stack floor, found {depth}"
                )
            }
            TypeMismatch { expected, actual } => {
                write!(f, "expected {expected} operand, found {actual}")
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
/// [`Limit::BodyInstructions`], with a separate table-target array per body (at
/// most one target per [`Limit::CodeBodyBytes`] byte). There are up to eight
/// export entries with names bounded by [`Limit::ExportNameBytes`], up to eight
/// active data segments bounded in total by [`Limit::DataBytes`], and up to eight
/// active element segments bounded in total by [`Limit::ElementFunctionIndices`].
/// Validation does not allocate linear memory or check segment ranges; both
/// belong to instantiation.
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
/// The start signature check runs only after all other module checks pass,
/// including section framing, index bounds and code validation.
/// [`ErrorKind::InvalidStartSignature`],
/// [`ErrorKind::InvalidStartI64Result`] and [`ErrorKind::InvalidStartFloatResult`]
/// point to the start of the start function index.
/// A `br`, `br_if`, or `br_table` depth that does not name an enclosing label
/// (including the implicit function label) is reported as
/// [`ErrorKind::InvalidIndex`] with [`IntegerField::LabelIndex`] at the start of
/// the immediate. A `br_table` with more than 16 explicit entries is reported
/// as [`ErrorKind::UnsupportedCount`] with [`IntegerField::BranchTableCount`].
/// [`ErrorKind::BranchTableArityMismatch`] and [`ErrorKind::BranchTableTypeMismatch`]
/// point to the first table label immediate (including the default) whose
/// result differs from the first label's.
/// Oversized table counts, entry bounds, or element segment counts are reported
/// as [`ErrorKind::UnsupportedCount`], and a reversed table bounds pair as
/// [`ErrorKind::InvalidTableLimits`] at the start of the maximum field.
/// [`ErrorKind::MissingTable`] points to the `call_indirect` opcode or the start
/// of an active segment's table index. An element function-index vector above
/// the remaining aggregate budget reports [`ErrorKind::LimitExceeded`] at the
/// start of its count field.
/// Oversized memory counts or page bounds are reported as
/// [`ErrorKind::UnsupportedCount`], and a reversed pair as
/// [`ErrorKind::InvalidMemoryLimits`] at the start of the maximum field.
/// [`ErrorKind::InvalidMemoryAlignment`] points to the memarg's exponent.
/// For `memory.size`, `memory.grow`, or `call_indirect`, a malformed reserved
/// byte is reported as [`ErrorKind::TruncatedByte`] or [`ErrorKind::UnexpectedByte`].
/// [`ErrorKind::MissingMemory`] points to the instruction opcode or the data
/// segment's memory index. A data length above the remaining aggregate budget
/// is reported as [`ErrorKind::LimitExceeded`] at the start of the length field.
/// Oversized global counts are reported as [`ErrorKind::UnsupportedCount`]
/// with [`IntegerField::GlobalCount`]. Invalid or missing global mutability
/// bytes are reported as [`ErrorKind::InvalidGlobalMutability`] or
/// [`ErrorKind::TruncatedGlobalMutability`] before checking the initializer.
/// An initializer opcode that does not match the declared type reports
/// [`ErrorKind::UnexpectedByte`] at that opcode. A short float initializer
/// immediate reports [`ErrorKind::TruncatedFloatConst`] at the section end.
/// After index validation,
/// [`ErrorKind::ImmutableGlobal`] points to the start of the `global.set`
/// index field and takes precedence over operand errors.
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
    let mut types = [FunctionType::PLACEHOLDER; MAX_TYPES as usize];
    let mut type_count = None;
    let mut function_types = [FunctionType::PLACEHOLDER; MAX_FUNCTIONS as usize];
    let mut function_count = None;
    let mut functions = Vec::new();
    let mut exports = Vec::new();
    let mut start = None;
    let mut table_entries = None;
    let mut element_segments = Vec::new();
    let mut memory_limits = None;
    let mut globals = [Global::EMPTY; MAX_GLOBALS as usize];
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
        if id <= last_standard && matches!(id, 1 | 3..=11) {
            return Err(Error {
                offset: id_offset,
                kind: ErrorKind::SectionOutOfOrder { id },
            });
        }
        match id {
            1 => type_count = Some(validate_types(&mut section, &mut types)?),
            3 => {
                let count = section.count(IntegerField::FunctionCount, 1..=MAX_FUNCTIONS)?;
                // Without a type section, use a placeholder zero-arg type at
                // index zero while checking framing and order. Reject its absence
                // in the final required-section check.
                let bound = type_count.unwrap_or(1);
                for signature in &mut function_types[..count as usize] {
                    let index = section.index(IntegerField::TypeIndex, bound)?;
                    *signature = types[index as usize];
                }
                function_count = Some(count);
            }
            4 => table_entries = validate_table(&mut section)?,
            5 => memory_limits = validate_memory(&mut section)?,
            6 => global_count = validate_globals(&mut section, &mut globals)?,
            7 => exports = validate_exports(&mut section, function_count)?,
            8 => {
                let offset = section.position();
                let index =
                    section.index(IntegerField::FunctionIndex, function_count.unwrap_or(0))?;
                start = Some((index as usize, offset));
            }
            9 => {
                element_segments = validate_elements(
                    &mut section,
                    table_entries.is_some(),
                    function_count.unwrap_or(0),
                )?;
            }
            10 => {
                let environment = BodyEnvironment {
                    types: &types[..type_count.unwrap_or(0) as usize],
                    functions: &function_types[..function_count.unwrap_or(0) as usize],
                    has_table: table_entries.is_some(),
                    has_memory: memory_limits.is_some(),
                    globals: &globals[..global_count],
                };
                functions = validate_code(&mut section, &environment)?;
            }
            11 => data_segments = validate_data(&mut section, memory_limits.is_some())?,
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
        // above sufficient to associate exports, calls, and start with validated bodies.
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
    // Do not report a start signature error for a placeholder type or mask malformed code.
    let start_function = if let Some((index, offset)) = start {
        let signature = function_types[index];
        let parameters = u32::from(signature.parameter_count);
        if parameters != 0 || signature.result.is_some() {
            let kind = match signature.result {
                None => ErrorKind::InvalidStartSignature {
                    parameters,
                    result: ResultKind::Void,
                },
                Some(ValType::I32) => ErrorKind::InvalidStartSignature {
                    parameters,
                    result: ResultKind::I32,
                },
                Some(ValType::I64) => ErrorKind::InvalidStartI64Result { parameters },
                Some(result @ (ValType::F32 | ValType::F64)) => {
                    ErrorKind::InvalidStartFloatResult { parameters, result }
                }
            };
            return Err(Error { offset, kind });
        }
        Some(index)
    } else {
        None
    };
    Ok(Module {
        functions,
        exports,
        start_function,
        memory_limits,
        table_entries,
        element_segments,
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
const MAX_ELEMENT_SEGMENTS: u32 = 8;
const MAX_PARAMETERS: u32 = 4;
const MAX_DECLARED_LOCALS: u32 = 16;
const MAX_LOCAL_SLOTS: usize = (MAX_PARAMETERS + MAX_DECLARED_LOCALS) as usize;
const I32: u8 = 0x7f;
const I64: u8 = 0x7e;

// Unused parameter slots are always i32, so derived equality is structural
// (arity, every positional parameter type, and result), independent of type indices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FunctionType {
    parameters: [ValType; MAX_PARAMETERS as usize],
    parameter_count: u8,
    result: Option<ValType>,
}

impl FunctionType {
    // Keep the existing framing checks for a module missing its type section.
    const PLACEHOLDER: Self = Self {
        parameters: [ValType::I32; MAX_PARAMETERS as usize],
        parameter_count: 0,
        result: Some(ValType::I32),
    };

    fn parameters(&self) -> &[ValType] {
        &self.parameters[..self.parameter_count as usize]
    }

    fn result_kind(self) -> ResultKind {
        match self.result {
            None => ResultKind::Void,
            Some(ValType::I32) => ResultKind::I32,
            Some(ValType::I64) => ResultKind::I64,
            Some(ValType::F32) => ResultKind::F32,
            Some(ValType::F64) => ResultKind::F64,
        }
    }
}

fn validate_types(
    section: &mut Reader<'_>,
    types: &mut [FunctionType; MAX_TYPES as usize],
) -> Result<u32, Error> {
    let count = section.count(IntegerField::TypeCount, 1..=MAX_TYPES)?;
    for signature in &mut types[..count as usize] {
        section.expect_byte(0x60)?; // functype
        let parameter_count = section.count(IntegerField::ParameterCount, 0..=MAX_PARAMETERS)?;
        let mut parameters = [ValType::I32; MAX_PARAMETERS as usize];
        for ty in &mut parameters[..parameter_count as usize] {
            *ty = section.numeric_type()?;
        }
        let result = if section.count(IntegerField::ResultCount, 0..=1)? == 0 {
            None
        } else {
            Some(section.numeric_type()?)
        };
        *signature = FunctionType {
            parameters,
            parameter_count: parameter_count as u8,
            result,
        };
    }
    Ok(count)
}

fn validate_table(section: &mut Reader<'_>) -> Result<Option<usize>, Error> {
    if section.count(IntegerField::TableCount, 0..=1)? == 0 {
        return Ok(None);
    }
    section.expect_byte(0x70)?; // Core 1.0 funcref (anyfunc)
    let offset = section.position();
    let flags = section
        .byte()
        .ok_or_else(|| section.error(ErrorKind::TruncatedTableLimitsFlags))?;
    if !matches!(flags, 0x00 | 0x01) {
        return Err(Error {
            offset,
            kind: ErrorKind::UnsupportedTableLimitsFlags { flags },
        });
    }
    let minimum = section.count(IntegerField::TableMinimum, 0..=MAX_TABLE_ENTRIES)?;
    if flags == 0x01 {
        let offset = section.position();
        let maximum = section.count(IntegerField::TableMaximum, 0..=MAX_TABLE_ENTRIES)?;
        if maximum < minimum {
            return Err(Error {
                offset,
                kind: ErrorKind::InvalidTableLimits { minimum, maximum },
            });
        }
    }
    // No growth: the maximum is validated but only the initial size matters.
    Ok(Some(minimum as usize))
}

fn validate_memory(section: &mut Reader<'_>) -> Result<Option<MemoryLimits>, Error> {
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
    let maximum = if flags == 0x01 {
        let offset = section.position();
        let maximum = section.count(IntegerField::MemoryMaximum, 0..=MAX_MEMORY_PAGES)?;
        if maximum < minimum {
            return Err(Error {
                offset,
                kind: ErrorKind::InvalidMemoryLimits { minimum, maximum },
            });
        }
        Some(maximum)
    } else {
        None
    };
    Ok(Some(MemoryLimits {
        minimum_pages: minimum,
        maximum_pages: maximum,
    }))
}

#[derive(Clone, Copy)]
struct Global {
    initial_value: Operand,
    mutable: bool,
}

impl Global {
    const EMPTY: Self = Self {
        initial_value: Operand::I32(0),
        mutable: false,
    };

    fn operand_type(self) -> ValType {
        self.initial_value.operand_type()
    }
}

fn validate_globals(
    section: &mut Reader<'_>,
    globals: &mut [Global; MAX_GLOBALS as usize],
) -> Result<usize, Error> {
    let count = section.count(IntegerField::GlobalCount, 0..=MAX_GLOBALS)? as usize;
    for global in &mut globals[..count] {
        let ty = section.numeric_type()?;
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
            initial_value: section.global_initializer(ty)?,
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
        let index = if let Some(count) = function_count {
            section.index(IntegerField::FunctionIndex, count)?
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

fn validate_elements(
    section: &mut Reader<'_>,
    has_table: bool,
    function_count: u32,
) -> Result<Vec<ElementSegment>, Error> {
    let allocation_error = section.error(ErrorKind::AllocationFailed);
    let count = section.count(IntegerField::ElementCount, 0..=MAX_ELEMENT_SEGMENTS)?;
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(count as usize)
        .map_err(|_| allocation_error)?;
    let limit = Limit::ElementFunctionIndices;
    let mut budget = limit.maximum() as u32;
    for _ in 0..count {
        // Core 1.0 encodes a table index, not a post-MVP segment flags byte.
        let index_offset = section.position();
        section.index(IntegerField::TableIndex, 1)?;
        if !has_table {
            return Err(Error {
                offset: index_offset,
                kind: ErrorKind::MissingTable,
            });
        }
        let offset = section.i32_const_expr()? as u32;
        let count_offset = section.position();
        let count = section.integer(IntegerField::ElementFunctionCount)?;
        if count > budget {
            return Err(Error {
                offset: count_offset,
                kind: ErrorKind::LimitExceeded { limit },
            });
        }
        budget -= count;
        let mut function_indices = Vec::new();
        function_indices
            .try_reserve_exact(count as usize)
            .map_err(|_| Error {
                offset: count_offset,
                kind: ErrorKind::AllocationFailed,
            })?;
        for _ in 0..count {
            function_indices
                .push(section.index(IntegerField::FunctionIndex, function_count)? as usize);
        }
        segments.push(ElementSegment {
            offset,
            function_indices,
        });
    }
    Ok(segments)
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
const BR: u8 = 0x0c;
const BR_IF: u8 = 0x0d;
const BR_TABLE: u8 = 0x0e;
const RETURN: u8 = 0x0f;
const I32_CONST: u8 = 0x41;
const I64_CONST: u8 = 0x42;
const F32_CONST: u8 = 0x43;
const F64_CONST: u8 = 0x44;

const MAX_BR_TABLE_ENTRIES: u32 = 16;
// Separate per-body pool, including defaults. Each target uses at least one
// byte of its body's frame, independent of the decoded instruction count.
const MAX_BODY_TABLE_TARGETS: usize = Limit::CodeBodyBytes.maximum();

#[derive(Clone, Copy, Debug)]
enum Instruction {
    Unreachable,
    Nop,
    Drop,
    Select,
    Const(Operand),
    Convert(Conversion),
    FloatUnary(FloatWidth, FloatUnaryOp),
    FloatBinary(FloatWidth, FloatBinaryOp),
    FloatCompare(FloatWidth, FloatComparison),
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    Call(CallTarget),
    Unary(UnaryOp),
    Binary(BinaryOp),
    I64Eqz,
    I64Unary(I64UnaryOp),
    I64Binary(I64BinaryOp),
    Load {
        kind: LoadKind,
        offset: u32,
        ty: ValType,
    },
    Store {
        width: AccessWidth,
        offset: u32,
        ty: ValType,
    },
    MemorySize,
    MemoryGrow,
    JumpIfZero {
        target: usize,
    },
    Jump {
        target: usize,
    },
    Branch {
        edge: BranchEdge,
        conditional: bool,
    },
    BranchTable {
        first: usize,
        explicit_count: u32,
    },
}

/// A WebAssembly numeric value tagged with its exact type.
///
/// Float variants carry raw IEEE 754 bits, not native Rust floats. All bit
/// patterns are valid, and equality compares the tag and payload exactly,
/// distinguishing positive zero from negative zero and different NaN encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Value {
    /// A 32-bit WebAssembly integer.
    I32(i32),
    /// A 64-bit WebAssembly integer.
    I64(i64),
    /// The exact IEEE 754 binary32 encoding of a WebAssembly `f32`.
    F32(u32),
    /// The exact IEEE 754 binary64 encoding of a WebAssembly `f64`.
    F64(u64),
}

impl Value {
    fn operand_type(self) -> ValType {
        match self {
            Self::I32(_) => ValType::I32,
            Self::I64(_) => ValType::I64,
            Self::F32(_) => ValType::F32,
            Self::F64(_) => ValType::F64,
        }
    }
}

// Private stack, local, and global slots keep IEEE 754 encodings in integer
// storage so even signaling NaNs pass through without a native float operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Operand {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
}

impl From<Value> for Operand {
    fn from(value: Value) -> Self {
        match value {
            Value::I32(value) => Self::I32(value),
            Value::I64(value) => Self::I64(value),
            Value::F32(bits) => Self::F32(bits),
            Value::F64(bits) => Self::F64(bits),
        }
    }
}

impl From<Operand> for Value {
    fn from(operand: Operand) -> Self {
        match operand {
            Operand::I32(value) => Self::I32(value),
            Operand::I64(value) => Self::I64(value),
            Operand::F32(bits) => Self::F32(bits),
            Operand::F64(bits) => Self::F64(bits),
        }
    }
}

impl Operand {
    fn zero(ty: ValType) -> Self {
        match ty {
            ValType::I32 => Self::I32(0),
            ValType::I64 => Self::I64(0),
            ValType::F32 => Self::F32(0),
            ValType::F64 => Self::F64(0),
        }
    }

    fn operand_type(self) -> ValType {
        match self {
            Self::I32(_) => ValType::I32,
            Self::I64(_) => ValType::I64,
            Self::F32(_) => ValType::F32,
            Self::F64(_) => ValType::F64,
        }
    }

    // Only validated code reaches execution. These checks guard the internal
    // invariant; no operand can be silently read as a different type.
    fn as_i32(self) -> i32 {
        match self {
            Self::I32(value) => value,
            _ => unreachable!("validated i32 operand"),
        }
    }

    fn as_i64(self) -> i64 {
        match self {
            Self::I64(value) => value,
            _ => unreachable!("validated i64 operand"),
        }
    }

    fn f32_bits(self) -> u32 {
        match self {
            Self::F32(bits) => bits,
            _ => unreachable!("validated f32 operand"),
        }
    }

    fn f64_bits(self) -> u64 {
        match self {
            Self::F64(bits) => bits,
            _ => unreachable!("validated f64 operand"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum IntegerType {
    I32S,
    I32U,
    I64S,
    I64U,
}

impl IntegerType {
    fn operand_type(self) -> ValType {
        match self {
            Self::I32S | Self::I32U => ValType::I32,
            Self::I64S | Self::I64U => ValType::I64,
        }
    }

    fn magnitude_and_sign(self, value: Operand) -> (u64, bool) {
        let signed = match self {
            Self::I32S => i64::from(value.as_i32()),
            Self::I32U => return (u64::from(value.as_i32() as u32), false),
            Self::I64S => value.as_i64(),
            Self::I64U => return (value.as_i64() as u64, false),
        };
        // unsigned_abs also gives the exact magnitude of i64::MIN.
        (signed.unsigned_abs(), signed < 0)
    }

    fn encode_truncated_magnitude(self, magnitude: u64, negative: bool) -> Result<Operand, Trap> {
        let limit = match (self, negative) {
            (Self::I32S, true) => 1 << 31,
            (Self::I32S, false) => (1 << 31) - 1,
            (Self::I64S, true) => 1 << 63,
            (Self::I64S, false) => (1 << 63) - 1,
            (Self::I32U | Self::I64U, true) => 0,
            (Self::I32U, false) => u64::from(u32::MAX),
            (Self::I64U, false) => u64::MAX,
        };
        // Check after discarding the fractional bits: unsigned accepts negative
        // zero magnitudes, and signed accepts the extra magnitude of MIN.
        if magnitude > limit {
            return Err(Trap::IntegerOverflow);
        }
        let bits = if negative {
            magnitude.wrapping_neg()
        } else {
            magnitude
        };
        Ok(match self {
            Self::I32S | Self::I32U => Operand::I32(bits as i32),
            Self::I64S | Self::I64U => Operand::I64(bits as i64),
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Conversion {
    WrapI64,
    ExtendI32S,
    ExtendI32U,
    TruncFloat(FloatWidth, IntegerType),
    IntToFloat(IntegerType, FloatWidth),
    DemoteF64,
    PromoteF32,
    I32ReinterpretF32,
    I64ReinterpretF64,
    F32ReinterpretI32,
    F64ReinterpretI64,
}

impl Conversion {
    fn types(self) -> (ValType, ValType) {
        use ValType::{F32, F64, I32, I64};
        match self {
            Self::WrapI64 => (I64, I32),
            Self::ExtendI32S | Self::ExtendI32U => (I32, I64),
            Self::TruncFloat(width, target) => (width.operand_type(), target.operand_type()),
            Self::IntToFloat(source, width) => (source.operand_type(), width.operand_type()),
            Self::DemoteF64 => (F64, F32),
            Self::PromoteF32 => (F32, F64),
            Self::I32ReinterpretF32 => (F32, I32),
            Self::I64ReinterpretF64 => (F64, I64),
            Self::F32ReinterpretI32 => (I32, F32),
            Self::F64ReinterpretI64 => (I64, F64),
        }
    }

    fn apply(self, value: Operand) -> Result<Operand, Trap> {
        Ok(match self {
            Self::WrapI64 => Operand::I32(value.as_i64() as i32),
            Self::ExtendI32S => Operand::I64(i64::from(value.as_i32())),
            Self::ExtendI32U => Operand::I64(i64::from(value.as_i32() as u32)),
            Self::TruncFloat(width, target) => {
                let (magnitude, negative) = truncated_float_magnitude(value, width)?;
                target.encode_truncated_magnitude(magnitude, negative)?
            }
            Self::IntToFloat(source, width) => {
                let (magnitude, negative) = source.magnitude_and_sign(value);
                let bits = integer_to_float_bits(magnitude, negative, width);
                match width {
                    FloatWidth::F32 => Operand::F32(bits as u32),
                    FloatWidth::F64 => Operand::F64(bits),
                }
            }
            Self::DemoteF64 => Operand::F32(demote_f64_bits(value.f64_bits())),
            Self::PromoteF32 => Operand::F64(promote_f32_bits(value.f32_bits())),
            Self::I32ReinterpretF32 => Operand::I32(value.f32_bits() as i32),
            Self::I64ReinterpretF64 => Operand::I64(value.f64_bits() as i64),
            Self::F32ReinterpretI32 => Operand::F32(value.as_i32() as u32),
            Self::F64ReinterpretI64 => Operand::F64(value.as_i64() as u64),
        })
    }
}

const F32_SIGN: u32 = 1 << 31;
const F64_SIGN: u64 = 1 << 63;
const F32_INFINITY: u32 = 0x7f80_0000;
const F64_INFINITY: u64 = 0x7ff0_0000_0000_0000;
const F32_CANONICAL_NAN: u32 = 0x7fc0_0000;
const F64_CANONICAL_NAN: u64 = 0x7ff8_0000_0000_0000;

fn truncated_float_magnitude(value: Operand, width: FloatWidth) -> Result<(u64, bool), Trap> {
    let (bits, sign, infinity, fraction_bits, bias) = match width {
        FloatWidth::F32 => (
            u64::from(value.f32_bits()),
            u64::from(F32_SIGN),
            u64::from(F32_INFINITY),
            23,
            127,
        ),
        FloatWidth::F64 => (value.f64_bits(), F64_SIGN, F64_INFINITY, 52, 1023),
    };
    let negative = bits & sign != 0;
    let magnitude = bits & !sign;
    if magnitude > infinity {
        return Err(Trap::InvalidConversionToInteger);
    }
    let exponent = (magnitude >> fraction_bits) as i32 - bias;
    if exponent < 0 {
        return Ok((0, negative)); // Includes zeros, subnormals, and all other |x| < 1.
    }
    if exponent >= 64 {
        // Infinities and finite |x| >= 2^64 cannot fit even the widest target.
        return Err(Trap::IntegerOverflow);
    }

    // Normal and finite, with its leading bit at an exponent in 0..=63.
    // Moving that bit into place cannot overflow u64; right shifts discard
    // exactly the fractional bits (both directions shift by at most 52).
    let implicit_bit = 1 << fraction_bits;
    let significand = implicit_bit | (magnitude & (implicit_bit - 1));
    let integral = if exponent < fraction_bits {
        significand >> (fraction_bits - exponent)
    } else {
        significand << (exponent - fraction_bits)
    };
    Ok((integral, negative))
}

fn integer_to_float_bits(magnitude: u64, negative: bool, width: FloatWidth) -> u64 {
    if magnitude == 0 {
        return 0; // Integer zero always produces +0.
    }
    let (fraction_bits, bias, sign) = match width {
        FloatWidth::F32 => (23, 127, u64::from(F32_SIGN)),
        FloatWidth::F64 => (52, 1023, F64_SIGN),
    };
    let exponent = magnitude.ilog2(); // 0..=63: all nonzero results are normal.
    let significand = if exponent <= fraction_bits {
        // Exact: shift left at most 52 places, to put the leading one at fraction_bits.
        magnitude << (fraction_bits - exponent)
    } else {
        // Round once, dropping 1..=40 bits for f32 or 1..=11 for f64.
        round_shift_ties_even(magnitude, exponent - fraction_bits)
    };
    // Remove the leading one. Addition propagates a rounding carry into the
    // exponent field. The resulting exponent is at most 64, so never infinity.
    let magnitude =
        (u64::from(exponent + bias) << fraction_bits) + (significand - (1 << fraction_bits));
    magnitude | if negative { sign } else { 0 }
}

// Round value / 2^shift to nearest, ties to even. Callers require 0 < shift < 64.
fn round_shift_ties_even(value: u64, shift: u32) -> u64 {
    let truncated = value >> shift;
    let discarded = value & ((1 << shift) - 1);
    let half = 1 << (shift - 1);
    let round_up = discarded > half || (discarded == half && truncated & 1 != 0);
    truncated + u64::from(round_up)
}

fn promote_f32_bits(bits: u32) -> u64 {
    let sign = u64::from(bits & F32_SIGN) << 32;
    let magnitude = bits & !F32_SIGN;
    match magnitude {
        0 => return sign,
        F32_INFINITY => return sign | F64_INFINITY,
        n if n > F32_INFINITY => return F64_CANONICAL_NAN,
        _ => {}
    }

    let field = u64::from(magnitude >> 23);
    let fraction = u64::from(magnitude & ((1 << 23) - 1));
    let (field, fraction) = if field == 0 {
        // A subnormal is fraction * 2^-149. Its leading one is at 0..=22,
        // so the unbiased exponent -149..=-127 is normal in f64. Remove
        // that leading one and place the rest with a u64 shift of 30..=52.
        let leading = fraction.ilog2();
        (
            u64::from(leading) + (1023 - 149),
            (fraction ^ (1 << leading)) << (52 - leading),
        )
    } else {
        // Normal f32 exponents -126..=127 are all normal in f64 as well.
        (field + (1023 - 127), fraction << (52 - 23))
    };
    sign | (field << 52) | fraction
}

fn demote_f64_bits(bits: u64) -> u32 {
    let sign = ((bits & F64_SIGN) >> 32) as u32;
    let magnitude = bits & !F64_SIGN;
    if magnitude > F64_INFINITY {
        return F32_CANONICAL_NAN;
    }
    let exponent = (magnitude >> 52) as i32 - 1023;
    if exponent < -150 {
        // Strictly below half the smallest f32 subnormal, including all
        // f64 subnormals and zeros (field zero).
        return sign;
    }
    if exponent > 127 {
        // At least 2^128, including infinities. The overflow midpoint
        // 2^128 - 2^103 is handled by rounding at exponent 127.
        return sign | F32_INFINITY;
    }

    // The surviving inputs are normal f64: significand * 2^(exponent-52).
    // Keep 24 significand bits for a normal f32 (shift 29). For exponents
    // -150..=-127 round in units of 2^-149 instead (shift 30..=53).
    // All shifts (including midpoint and mask) are below the u64 width.
    let significand = (1 << 52) | (magnitude & ((1 << 52) - 1));
    let shift = 29 + (-126 - exponent).max(0) as u32;
    let rounded = round_shift_ties_even(significand, shift) as u32;
    let magnitude = if exponent < -126 {
        // At most 2^23: a carry out of the fraction encodes the least normal.
        rounded
    } else {
        // Strip the leading one (rounded is in 2^23..=2^24). Addition
        // propagates any carry into the exponent, including to infinity
        // at the midpoint above max finite (odd retained significand).
        (((exponent + 127) as u32) << 23) + (rounded - (1 << 23))
    };
    sign | magnitude
}

#[derive(Clone, Copy, Debug)]
enum FloatWidth {
    F32,
    F64,
}

impl FloatWidth {
    fn operand_type(self) -> ValType {
        match self {
            Self::F32 => ValType::F32,
            Self::F64 => ValType::F64,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FloatUnaryOp {
    Abs,
    Neg,
    Round(FloatRounding),
    Sqrt,
}

impl FloatUnaryOp {
    fn apply(self, width: FloatWidth, value: Operand) -> Operand {
        match (width, self) {
            (FloatWidth::F32, Self::Abs) => Operand::F32(value.f32_bits() & !F32_SIGN),
            (FloatWidth::F32, Self::Neg) => Operand::F32(value.f32_bits() ^ F32_SIGN),
            (FloatWidth::F64, Self::Abs) => Operand::F64(value.f64_bits() & !F64_SIGN),
            (FloatWidth::F64, Self::Neg) => Operand::F64(value.f64_bits() ^ F64_SIGN),
            (FloatWidth::F32, Self::Round(mode)) => Operand::F32(mode.round_bits(
                u64::from(value.f32_bits()),
                u64::from(F32_SIGN),
                u64::from(F32_INFINITY),
                u64::from(F32_CANONICAL_NAN),
                23,
                127,
            ) as u32),
            (FloatWidth::F64, Self::Round(mode)) => Operand::F64(mode.round_bits(
                value.f64_bits(),
                F64_SIGN,
                F64_INFINITY,
                F64_CANONICAL_NAN,
                52,
                1023,
            )),
            (FloatWidth::F32, Self::Sqrt) => Operand::F32(sqrt_bits(
                u64::from(value.f32_bits()),
                u64::from(F32_SIGN),
                u64::from(F32_INFINITY),
                u64::from(F32_CANONICAL_NAN),
                23,
                127,
            ) as u32),
            (FloatWidth::F64, Self::Sqrt) => Operand::F64(sqrt_bits(
                value.f64_bits(),
                F64_SIGN,
                F64_INFINITY,
                F64_CANONICAL_NAN,
                52,
                1023,
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FloatRounding {
    Ceil,
    Floor,
    Trunc,
    Nearest,
}

impl FloatRounding {
    fn round_bits(
        self,
        bits: u64,
        sign_mask: u64,
        infinity: u64,
        canonical_nan: u64,
        fraction_bits: u32,
        bias: u32,
    ) -> u64 {
        let sign = bits & sign_mask;
        let magnitude = bits & !sign_mask;
        let implicit_bit = 1 << fraction_bits;
        if magnitude > infinity {
            // All NaNs map to the positive canonical quiet NaN, not a payload-preserving result.
            return canonical_nan;
        }
        let exponent = (magnitude >> fraction_bits) as u32;
        if magnitude == 0 || exponent >= bias + fraction_bits {
            // Both zero signs, infinities, and finite values with no fractional bits.
            return bits;
        }
        let outward = matches!((self, sign == 0), (Self::Ceil, true) | (Self::Floor, false));
        if exponent < bias {
            // Below one, nearest rounds away from zero only strictly above half;
            // a tie at half rounds to the even integer zero, with the original sign.
            let half = u64::from(bias - 1) << fraction_bits;
            let one = u64::from(bias) << fraction_bits;
            let up = outward || (matches!(self, Self::Nearest) && magnitude > half);
            return sign | if up { one } else { 0 };
        }

        // The exponent locates the units bit. The implicit leading one is
        // needed to test parity when the magnitude is in [1, 2).
        let unit = 1 << (fraction_bits - (exponent - bias));
        let fraction = magnitude & (unit - 1);
        if fraction == 0 {
            return bits;
        }
        let whole = magnitude - fraction;
        let half = unit >> 1;
        let significand = implicit_bit | (magnitude & (implicit_bit - 1));
        let odd = significand & unit != 0;
        let up = outward
            || (matches!(self, Self::Nearest) && (fraction > half || (fraction == half && odd)));
        // Adding one unit to the truncated encoding can carry into the exponent.
        sign | (whole + if up { unit } else { 0 })
    }
}

fn sqrt_bits(
    bits: u64,
    sign_mask: u64,
    infinity: u64,
    canonical_nan: u64,
    fraction_bits: u32,
    bias: i32,
) -> u64 {
    if bits & !sign_mask == 0 || bits == infinity {
        return bits;
    }
    if bits & sign_mask != 0 || bits > infinity {
        return canonical_nan;
    }

    // Write the input as m * 2^(e-h), with h fraction bits and m in
    // [2^h, 2^(h+1)). For a subnormal, shift its leading one into place.
    let implicit_bit = 1 << fraction_bits;
    let fraction = bits & (implicit_bit - 1);
    let field = (bits >> fraction_bits) as i32;
    let (m, e) = if field == 0 {
        let shift = fraction_bits - fraction.ilog2();
        (fraction << shift, 1 - bias - shift as i32)
    } else {
        (implicit_bit | fraction, field - bias)
    };
    // Split e = 2k + p with k = e.div_euclid(2) and p = e & 1 in {0, 1}.
    // This gives k rounded down and p = 1 even for negative odd e (e.g. -3 = -4 + 1).
    // The exact root is sqrt(N) * 2^(k-h) for N = m << (h+p). N has at most
    // 2(h+1) bits (48 or 106), so u128 covers the radicand and all intermediates.
    let n = u128::from(m) << (fraction_bits + (e & 1) as u32);
    let mut root = 0;
    let mut remainder = 0;
    // One guest dispatch, with exactly h+1 (24 or 53) restoring steps here.
    // Invariant: N_prefix = q^2 + r and 0 <= r < 2q+1, where q = root and
    // r = remainder, so q = floor(sqrt(N_prefix)). This starts at q = r = 0.
    // Appending two bits d makes the prefix 4*N_prefix+d, with floor root 2q
    // or 2q+1. Compare 4r+d to (2q+1)^2 - (2q)^2 = 4q+1 to pick the root
    // and subtract only when the trial square fits, preserving the invariant.
    for pair in (0..=fraction_bits).rev() {
        remainder = (remainder << 2) | ((n >> (2 * pair)) & 3);
        let trial = (root << 2) | 1;
        root <<= 1;
        if remainder >= trial {
            remainder -= trial;
            root |= 1;
        }
    }
    // (root + 1/2)^2 = root^2 + root + 1/4. Integral N cannot tie,
    // and is above the midpoint exactly when remainder > root.
    root += u128::from(remainder > root);

    // N >= 2^(2h) so root >= 2^h, and removing the implicit bit cannot
    // underflow. The output exponent k stays normal: -75..=63 for f32 and
    // -537..=511 for f64. At the upper end of the significand range,
    // N <= (2^(h+1)-1)*2^(h+1), which is 1/4 below (2^(h+1)-1/2)^2.
    // Rounding therefore cannot carry the significand into the next binade.
    let exponent = (e.div_euclid(2) + bias) as u64;
    (exponent << fraction_bits) | (root as u64 - implicit_bit)
}

#[derive(Clone, Copy, Debug)]
enum FloatBinaryOp {
    Min,
    Max,
    Copysign,
    Add,
    Sub,
    Mul,
    Div,
}

impl FloatBinaryOp {
    fn apply(self, width: FloatWidth, lhs: Operand, rhs: Operand) -> Operand {
        match width {
            FloatWidth::F32 => Operand::F32(self.apply_bits(
                width,
                u64::from(lhs.f32_bits()),
                u64::from(rhs.f32_bits()),
            ) as u32),
            FloatWidth::F64 => Operand::F64(self.apply_bits(width, lhs.f64_bits(), rhs.f64_bits())),
        }
    }

    fn apply_bits(self, width: FloatWidth, lhs: u64, rhs: u64) -> u64 {
        let (sign, infinity, canonical_nan, fraction_bits, bias) = match width {
            FloatWidth::F32 => (
                u64::from(F32_SIGN),
                u64::from(F32_INFINITY),
                u64::from(F32_CANONICAL_NAN),
                23,
                127,
            ),
            FloatWidth::F64 => (F64_SIGN, F64_INFINITY, F64_CANONICAL_NAN, 52, 1023),
        };
        let min = match self {
            Self::Add | Self::Sub => {
                let rhs = if matches!(self, Self::Sub) {
                    rhs ^ sign
                } else {
                    rhs
                };
                return add_bits(lhs, rhs, sign, infinity, canonical_nan, fraction_bits, bias);
            }
            Self::Mul => {
                return mul_bits(lhs, rhs, sign, infinity, canonical_nan, fraction_bits, bias);
            }
            Self::Div => {
                return div_bits(lhs, rhs, sign, infinity, canonical_nan, fraction_bits, bias);
            }
            Self::Min => true,
            Self::Max => false,
            Self::Copysign => return (lhs & !sign) | (rhs & sign),
        };
        use Ordering::{Equal, Greater, Less};
        match (compare_float_bits(lhs, rhs, sign, infinity), min) {
            (None, _) => canonical_nan,
            (Some(Less), true) | (Some(Greater), false) => lhs,
            (Some(Greater), true) | (Some(Less), false) => rhs,
            // Equal encodings are unchanged by either operation. For mixed
            // zeros, OR selects -0 and AND selects +0, regardless of order.
            (Some(Equal), true) => lhs | rhs,
            (Some(Equal), false) => lhs & rhs,
        }
    }
}

// Subtraction has already flipped the right operand's sign bit.
fn add_bits(
    lhs: u64,
    rhs: u64,
    sign_mask: u64,
    infinity: u64,
    canonical_nan: u64,
    fraction_bits: u32,
    bias: i32,
) -> u64 {
    let lhs_magnitude = lhs & !sign_mask;
    let rhs_magnitude = rhs & !sign_mask;
    if lhs_magnitude > infinity || rhs_magnitude > infinity {
        return canonical_nan;
    }

    // The unsigned encodings order magnitudes, so only A can determine the
    // sign of a nonzero result with opposite effective operand signs.
    let (a, b, sign) = if lhs_magnitude >= rhs_magnitude {
        (lhs_magnitude, rhs_magnitude, lhs & sign_mask)
    } else {
        (rhs_magnitude, lhs_magnitude, rhs & sign_mask)
    };
    let opposite = (lhs ^ rhs) & sign_mask != 0;
    if a == infinity {
        return if opposite && b == infinity {
            canonical_nan
        } else {
            sign | infinity
        };
    }
    if opposite && a == b {
        return 0; // Mixed effective zeros and exact cancellation give +0.
    }
    if b == 0 {
        return sign | a; // Includes two same-sign effective zeros.
    }

    // Decode M * 2^(e-bias-h), where h is the fraction width and e is
    // the exponent field clamped to 1. Subnormals have no implicit bit.
    let implicit = 1 << fraction_bits;
    let a_field = (a >> fraction_bits) as i32;
    let b_field = (b >> fraction_bits) as i32;
    let a_e = a_field.max(1);
    let b_e = b_field.max(1);
    let gap = a_e - b_e;
    let h = fraction_bits as i32;
    if gap >= h + 3 {
        // In units U = 2^(a_e-bias-h) of A's significand, B is strictly
        // below 2^(h+1-gap) <= 1/4. A is normal here. Outward spacing is
        // U (counting the hypothetical next binade above max finite);
        // inward spacing is U except at a power of two above the least
        // normal, where it is U/2. Either direction is strictly short of
        // its midpoint. Finish here: no jammed bits enter the exact path.
        return sign | a;
    }

    let significand =
        |magnitude, field| (magnitude & (implicit - 1)) | if field == 0 { 0 } else { implicit };
    let aligned_a = u128::from(significand(a, a_field)) << gap;
    let aligned_b = u128::from(significand(b, b_field));
    // gap <= h+2, so even a sum needs at most 2h+4 <= 108 bits.
    // Alignment loses nothing, even if subtracting cancels leading bits.
    let exact = if opposite {
        aligned_a - aligned_b
    } else {
        aligned_a + aligned_b
    };
    let top = exact.ilog2() as i32; // Nonzero: opposite equal magnitudes exited.
    let exponent = b_e - bias + top - h;
    if exponent > bias {
        return sign | infinity;
    }

    // Target units are 2^(max(exponent, 1-bias)-h). The clamped shift is
    // in -h..=h+3. Left shifts are exact; all subnormal results are exact
    // too, since the inputs are integer multiples of the least subnormal.
    // The retained magnitude has at most h+1 bits, so it and any rounding
    // carry fit in u64.
    let shift = (top - h).max(1 - b_e);
    let rounded = if shift <= 0 {
        (exact << -shift) as u64
    } else {
        let midpoint = 1_u128 << (shift - 1);
        let remainder = exact & ((midpoint << 1) - 1);
        let retained = exact >> shift;
        let up = remainder > midpoint || (remainder == midpoint && retained & 1 != 0);
        retained as u64 + u64::from(up)
    };
    // The packer propagates a rounding carry, including from the odd max
    // finite significand at the tie with the hypothetical even endpoint.
    sign | pack_rounded_finite(rounded, exponent, fraction_bits, bias)
}

fn mul_bits(
    lhs: u64,
    rhs: u64,
    sign_mask: u64,
    infinity: u64,
    canonical_nan: u64,
    fraction_bits: u32,
    bias: i32,
) -> u64 {
    let sign = (lhs ^ rhs) & sign_mask;
    let lhs = lhs & !sign_mask;
    let rhs = rhs & !sign_mask;
    if lhs > infinity || rhs > infinity {
        return canonical_nan;
    }
    if lhs == infinity || rhs == infinity {
        return if lhs == 0 || rhs == 0 {
            canonical_nan
        } else {
            sign | infinity
        };
    }
    if lhs == 0 || rhs == 0 {
        return sign;
    }

    let min_exponent = 1 - bias;
    let (lhs_m, lhs_e) = normalized_finite(lhs, fraction_bits, bias);
    let (rhs_m, rhs_e) = normalized_finite(rhs, fraction_bits, bias);
    let product = u128::from(lhs_m) * u128::from(rhs_m);
    // The full product occupies 48 or 106 bits at most, with its top bit
    // at 2h or 2h+1. Its top exponent is in -298..=255 or -2148..=2047.
    let top = product.ilog2() as i32;
    let h = fraction_bits as i32;
    let exponent = lhs_e + rhs_e + top - 2 * h;
    if exponent > bias {
        return sign | infinity;
    }

    // Round once, in units of 2^(max(exponent, min_exponent)-h). In
    // particular, do not round to normal precision before shifting into
    // the subnormal range. For normal results S = top - h (h or h+1).
    // Below the normal range, substituting exponent cancels top:
    // S = h + min_exponent - lhs_e - rhs_e, at most 195 (f32) or 1178 (f64).
    // At S >= 128 the product is strictly below half a destination unit;
    // exit before forming the shift mask.
    let shift = top - h + (min_exponent - exponent).max(0);
    if shift >= 128 {
        return sign;
    }
    let midpoint = 1_u128 << (shift - 1);
    let remainder = product & ((midpoint << 1) - 1);
    let retained = product >> shift;
    let up = remainder > midpoint || (remainder == midpoint && retained & 1 != 0);
    let rounded = retained as u64 + u64::from(up);

    sign | pack_rounded_finite(rounded, exponent, fraction_bits, bias)
}

fn div_bits(
    lhs: u64,
    rhs: u64,
    sign_mask: u64,
    infinity: u64,
    canonical_nan: u64,
    fraction_bits: u32,
    bias: i32,
) -> u64 {
    let sign = (lhs ^ rhs) & sign_mask;
    let lhs = lhs & !sign_mask;
    let rhs = rhs & !sign_mask;
    if lhs > infinity || rhs > infinity || (lhs == rhs && (lhs == 0 || lhs == infinity)) {
        return canonical_nan;
    }
    if lhs == infinity || rhs == 0 {
        return sign | infinity;
    }
    if lhs == 0 || rhs == infinity {
        return sign;
    }

    // Both inputs are finite and nonzero; in particular rhs_m cannot be zero.
    let (lhs_m, lhs_e) = normalized_finite(lhs, fraction_bits, bias);
    let (rhs_m, rhs_e) = normalized_finite(rhs, fraction_bits, bias);
    // The exponent difference is at most 276 (f32) or 2097 (f64) in magnitude.
    let difference = lhs_e - rhs_e;
    // The exact significand ratio is strictly between 1/2 and 2. Comparing
    // the significands decides which binade it lies in, without rounding.
    let exponent = difference - i32::from(lhs_m < rhs_m);
    let h = fraction_bits as i32;
    let min_exponent = 1 - bias;
    if exponent > bias {
        return sign | infinity;
    }
    if exponent < min_exponent - h - 1 {
        return sign; // Strictly less than half the least subnormal.
    }

    // Express the exact quotient directly in units of 2^(max(exponent, emin)-h).
    // shift = h + [lhs_m < rhs_m] - max(emin - exponent, 0), so the underflow
    // guard bounds it to -1..=h+1. The numerator has at most 2h+2 bits, the
    // denominator at most h+2, and doubling the remainder needs at most h+3.
    let shift = difference - exponent.max(min_exponent) + h;
    let lhs_m = u128::from(lhs_m);
    let rhs_m = u128::from(rhs_m);
    let (numerator, denominator) = if shift < 0 {
        (lhs_m, rhs_m << 1) // The only negative shift is -1.
    } else {
        (lhs_m << shift, rhs_m)
    };
    let retained = numerator / denominator;
    let twice_remainder = 2 * (numerator % denominator);
    let up = twice_remainder > denominator || (twice_remainder == denominator && retained & 1 != 0);
    // Rounding can reach at most 2^(h+1) for a normal binade or 2^h below it.
    let rounded = retained as u64 + u64::from(up);
    sign | pack_rounded_finite(rounded, exponent, fraction_bits, bias)
}

// Decode a positive finite nonzero magnitude as m * 2^(e-h), with h the
// fraction width and m in [2^h, 2^(h+1)). A subnormal's nonzero fraction shifts
// left by 1..=h, giving e in -149..=127 (f32) or -1074..=1023 (f64).
fn normalized_finite(magnitude: u64, fraction_bits: u32, bias: i32) -> (u64, i32) {
    let implicit_bit = 1 << fraction_bits;
    let fraction = magnitude & (implicit_bit - 1);
    let field = (magnitude >> fraction_bits) as i32;
    if field == 0 {
        let shift = fraction_bits - fraction.ilog2();
        (fraction << shift, 1 - bias - shift as i32)
    } else {
        (implicit_bit | fraction, field - bias)
    }
}

// Pack a magnitude already rounded in units of 2^(max(exponent, 1-bias)-h),
// with exponent <= bias. No further rounding is performed here.
fn pack_rounded_finite(rounded: u64, exponent: i32, fraction_bits: u32, bias: i32) -> u64 {
    if exponent < 1 - bias {
        // 0..=2^h: a carry out of the fraction gives the least normal.
        rounded
    } else {
        // 2^h..=2^(h+1): remove the implicit bit, then let addition carry
        // into the exponent, including to infinity at the overflow tie.
        let implicit_bit = 1 << fraction_bits;
        (((exponent + bias) as u64) << fraction_bits) + (rounded - implicit_bit)
    }
}

#[derive(Clone, Copy, Debug)]
enum FloatComparison {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl FloatComparison {
    fn apply(self, width: FloatWidth, lhs: Operand, rhs: Operand) -> i32 {
        use Ordering::{Equal, Greater, Less};
        let order = match width {
            FloatWidth::F32 => compare_float_bits(
                u64::from(lhs.f32_bits()),
                u64::from(rhs.f32_bits()),
                u64::from(F32_SIGN),
                u64::from(F32_INFINITY),
            ),
            FloatWidth::F64 => {
                compare_float_bits(lhs.f64_bits(), rhs.f64_bits(), F64_SIGN, F64_INFINITY)
            }
        };
        let Some(order) = order else {
            return i32::from(matches!(self, Self::Ne)); // NaNs are unordered.
        };
        i32::from(match self {
            Self::Eq => order == Equal,
            Self::Ne => order != Equal,
            Self::Lt => order == Less,
            Self::Gt => order == Greater,
            Self::Le => order != Greater,
            Self::Ge => order != Less,
        })
    }
}

fn compare_float_bits(lhs: u64, rhs: u64, sign: u64, infinity: u64) -> Option<Ordering> {
    let lhs_magnitude = lhs & !sign;
    let rhs_magnitude = rhs & !sign;
    // An all-ones exponent and nonzero fraction exceeds infinity's magnitude,
    // whether the NaN is quiet or signaling.
    if lhs_magnitude > infinity || rhs_magnitude > infinity {
        return None;
    }
    // Ignore the sign of zero for comparison only. For equal signs, exponent
    // and fraction bits order magnitudes; negative numbers reverse that order.
    let lhs_negative = lhs & sign != 0 && lhs_magnitude != 0;
    let rhs_negative = rhs & sign != 0 && rhs_magnitude != 0;
    Some(match (lhs_negative, rhs_negative) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (true, true) => rhs_magnitude.cmp(&lhs_magnitude),
        (false, false) => lhs_magnitude.cmp(&rhs_magnitude),
    })
}

#[derive(Clone, Copy, Debug)]
enum CallTarget {
    Direct(u32),
    Indirect(FunctionType),
}

#[derive(Clone, Copy, Debug)]
struct BranchEdge {
    target: BranchTarget,
    floor: usize,
}

#[derive(Clone, Copy, Debug)]
enum BranchTarget {
    End { pc: usize, result: Option<ValType> },
    Loop(LoopTarget),
    Function,
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

#[derive(Clone, Copy, Debug)]
enum I64UnaryOp {
    Clz,
    Ctz,
    Popcnt,
}

impl I64UnaryOp {
    fn apply(self, operand: i64) -> i64 {
        let bits = operand as u64;
        i64::from(match self {
            Self::Clz => bits.leading_zeros(),
            Self::Ctz => bits.trailing_zeros(),
            Self::Popcnt => bits.count_ones(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum I64BinaryOp {
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

impl I64BinaryOp {
    fn result_type(self) -> ValType {
        match self {
            Self::Add
            | Self::Sub
            | Self::Mul
            | Self::DivS
            | Self::DivU
            | Self::RemS
            | Self::RemU
            | Self::And
            | Self::Or
            | Self::Xor
            | Self::Shl
            | Self::ShrS
            | Self::ShrU
            | Self::Rotl
            | Self::Rotr => ValType::I64,
            Self::Eq
            | Self::Ne
            | Self::LtS
            | Self::LtU
            | Self::GtS
            | Self::GtU
            | Self::LeS
            | Self::LeU
            | Self::GeS
            | Self::GeU => ValType::I32,
        }
    }

    fn apply(self, lhs: i64, rhs: i64) -> Result<Operand, Trap> {
        // Mask to six bits before shifting, including for negative counts.
        let shift = (rhs & 63) as u32;
        let comparison = match self {
            Self::Eq => lhs == rhs,
            Self::Ne => lhs != rhs,
            Self::LtS => lhs < rhs,
            Self::LtU => (lhs as u64) < (rhs as u64),
            Self::GtS => lhs > rhs,
            Self::GtU => (lhs as u64) > (rhs as u64),
            Self::LeS => lhs <= rhs,
            Self::LeU => (lhs as u64) <= (rhs as u64),
            Self::GeS => lhs >= rhs,
            Self::GeU => (lhs as u64) >= (rhs as u64),
            Self::Add => return Ok(Operand::I64(lhs.wrapping_add(rhs))),
            Self::Sub => return Ok(Operand::I64(lhs.wrapping_sub(rhs))),
            Self::Mul => return Ok(Operand::I64(lhs.wrapping_mul(rhs))),
            Self::DivS if rhs == 0 => return Err(Trap::IntegerDivideByZero),
            Self::DivS => {
                return lhs
                    .checked_div(rhs)
                    .map(Operand::I64)
                    .ok_or(Trap::IntegerOverflow);
            }
            // Same-width casts reinterpret the operands and preserve the result bits.
            Self::DivU => {
                return (lhs as u64)
                    .checked_div(rhs as u64)
                    .map(|value| Operand::I64(value as i64))
                    .ok_or(Trap::IntegerDivideByZero);
            }
            // Wasm remainder by -1 is zero even for i64::MIN, unlike checked_rem.
            Self::RemS if rhs == -1 => return Ok(Operand::I64(0)),
            Self::RemS => {
                return lhs
                    .checked_rem(rhs)
                    .map(Operand::I64)
                    .ok_or(Trap::IntegerDivideByZero);
            }
            Self::RemU => {
                return (lhs as u64)
                    .checked_rem(rhs as u64)
                    .map(|value| Operand::I64(value as i64))
                    .ok_or(Trap::IntegerDivideByZero);
            }
            Self::And => return Ok(Operand::I64(lhs & rhs)),
            Self::Or => return Ok(Operand::I64(lhs | rhs)),
            Self::Xor => return Ok(Operand::I64(lhs ^ rhs)),
            Self::Shl => return Ok(Operand::I64(((lhs as u64) << shift) as i64)),
            Self::ShrS => return Ok(Operand::I64(lhs >> shift)),
            Self::ShrU => return Ok(Operand::I64(((lhs as u64) >> shift) as i64)),
            Self::Rotl => return Ok(Operand::I64(lhs.rotate_left(shift))),
            Self::Rotr => return Ok(Operand::I64(lhs.rotate_right(shift))),
        };
        Ok(Operand::I32(i32::from(comparison)))
    }
}

/// Only the decoder constructs this, after checking all indices, global set
/// mutability, every stack effect, and the complete body frame. Each local
/// index is below the exact parameter-plus-declaration bound, and writes
/// preserve the slot's declared tag. Block and if jump targets are strictly
/// later decoded instruction indices or the one-past-last index. Block and if
/// exits carry their target floor and result type, if any. Loop continues
/// target an in-body index at or before the branch. Function exits target the
/// one-past-last index with the signature's result type, if any. A table refers
/// to consecutive edges of the same label type, with its default at the end.
/// Implicit operands are validation-only: no execution falls through
/// `unreachable`, `br`, `br_table`, or `return`, and transfers establish the
/// target's concrete stack depth.
#[derive(Debug)]
pub(super) struct ValidatedCode {
    signature: FunctionType,
    // The tags provide local types during validation; each call copies these
    // typed zeros before loading tagged arguments into the parameter prefix.
    local_zeros: [Operand; MAX_LOCAL_SLOTS],
    instructions: Vec<Instruction>,
    table_targets: Vec<BranchEdge>,
}

struct CallFrame {
    function_index: usize,
    pc: usize,
    locals: [Operand; MAX_LOCAL_SLOTS],
    operands: [Operand; Limit::OperandStackDepth.maximum()],
    depth: usize,
}

impl CallFrame {
    const EMPTY: Self = Self {
        function_index: 0,
        pc: 0,
        locals: [Operand::I32(0); MAX_LOCAL_SLOTS],
        operands: [Operand::I32(0); Limit::OperandStackDepth.maximum()],
        depth: 0,
    };

    fn new(
        function_index: usize,
        code: &ValidatedCode,
        args: impl ExactSizeIterator<Item = Operand>,
    ) -> Self {
        let mut frame = Self {
            function_index,
            locals: code.local_zeros,
            ..Self::EMPTY
        };
        debug_assert_eq!(args.len(), code.signature.parameter_count as usize);
        for (local, arg) in frame.locals[..args.len()].iter_mut().zip(args) {
            debug_assert_eq!(local.operand_type(), arg.operand_type());
            *local = arg;
        }
        frame
    }

    fn push(&mut self, value: Operand) {
        self.operands[self.depth] = value;
        self.depth += 1;
    }

    fn push_i32(&mut self, value: i32) {
        self.push(Operand::I32(value));
    }

    fn pop(&mut self) -> Operand {
        self.depth -= 1;
        self.operands[self.depth]
    }

    fn pop_i32(&mut self) -> i32 {
        self.pop().as_i32()
    }

    fn take_branch(&mut self, edge: BranchEdge, code: &ValidatedCode) {
        let (pc, result) = match edge.target {
            BranchTarget::End { pc, result } => (pc, result),
            BranchTarget::Loop(LoopTarget(pc)) => (pc, None),
            BranchTarget::Function => (code.instructions.len(), code.signature.result),
        };
        let value = result.map(|ty| {
            let value = self.pop();
            debug_assert_eq!(value.operand_type(), ty);
            value
        });
        // Restore the target's prefix, not the active frame's.
        self.depth = edge.floor;
        if let Some(value) = value {
            self.push(value);
        }
        self.pc = pc;
    }
}

pub(super) fn invoke_index(
    instance: &Instance<'_>,
    function_index: usize,
    args: impl ExactSizeIterator<Item = Value> + Clone,
    expected: ResultKind,
) -> Result<Option<Value>, InvokeError> {
    let signature = instance.module.functions[function_index].signature;
    let parameters = signature.parameters();
    if args.len() != parameters.len() {
        return Err(InvokeError::ArityMismatch {
            expected: parameters.len(),
            actual: args.len(),
        });
    }
    let actual = signature.result_kind();
    if actual != expected {
        return Err(InvokeError::ResultMismatch { expected, actual });
    }
    // Arity bounds both passes over the borrowed arguments to four items.
    for (index, (&expected, arg)) in parameters.iter().zip(args.clone()).enumerate() {
        let actual = arg.operand_type();
        if actual != expected {
            return Err(InvokeError::ArgumentTypeMismatch {
                index,
                expected,
                actual,
            });
        }
    }
    let result =
        execute(instance, function_index, args.map(Operand::from)).map_err(
            |error| match error {
                ExecutionError::Trap(trap) => InvokeError::Trap(trap),
                ExecutionError::LimitExceeded { limit } => InvokeError::LimitExceeded { limit },
            },
        )?;
    Ok(result.map(Value::from))
}

pub(super) enum ExecutionError {
    Trap(Trap),
    LimitExceeded { limit: ExecutionLimit },
}

pub(super) fn execute_start(
    instance: &Instance<'_>,
    function_index: usize,
) -> Result<(), ExecutionError> {
    let result = execute(instance, function_index, std::iter::empty())?;
    debug_assert!(result.is_none(), "validated start signature");
    Ok(())
}

// The caller supplies the checked entry arguments. Execution keeps all
// arguments and results tagged as operands, including across private calls.
fn execute(
    instance: &Instance<'_>,
    function_index: usize,
    args: impl ExactSizeIterator<Item = Operand>,
) -> Result<Option<Operand>, ExecutionError> {
    let module = instance.module;
    let mut frames = [CallFrame::EMPTY; ExecutionLimit::CallDepth.maximum()];
    frames[0] = CallFrame::new(function_index, &module.functions[function_index], args);
    let mut active = 0;
    let mut remaining_dispatches = ExecutionLimit::InstructionDispatches.maximum();
    loop {
        let frame = &mut frames[active];
        // Start, export and call indices are bounded by the exact decoded body count.
        let code = &module.functions[frame.function_index];
        let Some(&instruction) = code.instructions.get(frame.pc) else {
            // Fallthrough and function exits leave exactly the declared results
            // in this frame. Completing the call requires no dispatch.
            debug_assert_eq!(frame.pc, code.instructions.len());
            let result = code.signature.result;
            debug_assert_eq!(frame.depth, usize::from(result.is_some()));
            let value = result.map(|ty| {
                let value = frame.operands[0];
                debug_assert_eq!(value.operand_type(), ty);
                value
            });
            if active == 0 {
                return Ok(value);
            }
            active -= 1;
            if let Some(value) = value {
                frames[active].push(value);
            }
            continue;
        };
        if remaining_dispatches == 0 {
            return Err(ExecutionError::LimitExceeded {
                limit: ExecutionLimit::InstructionDispatches,
            });
        }
        remaining_dispatches -= 1;
        frame.pc += 1;
        match instruction {
            Instruction::Unreachable => return Err(ExecutionError::Trap(Trap::Unreachable)),
            Instruction::Nop => {}
            Instruction::Drop => {
                frame.pop();
            }
            Instruction::Select => {
                let condition = frame.pop_i32();
                let second = frame.pop();
                let first = frame.pop();
                frame.push(if condition != 0 { first } else { second });
            }
            Instruction::Const(value) => frame.push(value),
            Instruction::Convert(op) => {
                let value = op.apply(frame.pop()).map_err(ExecutionError::Trap)?;
                frame.push(value);
            }
            Instruction::FloatUnary(width, op) => {
                let value = op.apply(width, frame.pop());
                frame.push(value);
            }
            Instruction::FloatBinary(width, op) => {
                let rhs = frame.pop();
                let lhs = frame.pop();
                frame.push(op.apply(width, lhs, rhs));
            }
            Instruction::FloatCompare(width, op) => {
                let rhs = frame.pop();
                let lhs = frame.pop();
                frame.push(Operand::I32(op.apply(width, lhs, rhs)));
            }
            Instruction::LocalGet(index) => frame.push(frame.locals[index as usize]),
            Instruction::LocalSet(index) => frame.locals[index as usize] = frame.pop(),
            Instruction::LocalTee(index) => {
                frame.locals[index as usize] = frame.operands[frame.depth - 1];
            }
            Instruction::GlobalGet(index) => frame.push(instance.globals[index as usize].get()),
            Instruction::GlobalSet(index) => instance.globals[index as usize].set(frame.pop()),
            Instruction::Call(target) => {
                let index = match target {
                    CallTarget::Direct(index) => index as usize,
                    CallTarget::Indirect(expected) => {
                        // Dispatch was charged above. Trap on an invalid target
                        // before checking whether another frame can be entered.
                        let table = instance.table.as_ref().expect("validated table presence");
                        let index = table
                            .resolve(frame.pop_i32() as u32)
                            .map_err(ExecutionError::Trap)?;
                        if module.functions[index].signature != expected {
                            return Err(ExecutionError::Trap(Trap::IndirectCallTypeMismatch));
                        }
                        index
                    }
                };
                if active + 1 == ExecutionLimit::CallDepth.maximum() {
                    return Err(ExecutionError::LimitExceeded {
                        limit: ExecutionLimit::CallDepth,
                    });
                }
                // Keep the caller's prefix; the argument suffix is already in
                // parameter order. Reset reused frame storage before entering.
                let callee_code = &module.functions[index];
                let end = frame.depth;
                frame.depth -= callee_code.signature.parameter_count as usize;
                let args = frame.operands[frame.depth..end].iter().copied();
                let callee = CallFrame::new(index, callee_code, args);
                active += 1;
                frames[active] = callee;
            }
            Instruction::Unary(op) => {
                let result = op.apply(frame.pop_i32());
                frame.push_i32(result);
            }
            Instruction::Binary(op) => {
                let rhs = frame.pop_i32();
                let lhs = frame.pop_i32();
                frame.push_i32(op.apply(lhs, rhs).map_err(ExecutionError::Trap)?);
            }
            Instruction::I64Eqz => {
                let operand = frame.pop().as_i64();
                frame.push_i32(i32::from(operand == 0));
            }
            Instruction::I64Unary(op) => {
                let result = op.apply(frame.pop().as_i64());
                frame.push(Operand::I64(result));
            }
            Instruction::I64Binary(op) => {
                let rhs = frame.pop().as_i64();
                let lhs = frame.pop().as_i64();
                frame.push(op.apply(lhs, rhs).map_err(ExecutionError::Trap)?);
            }
            Instruction::Load { kind, offset, ty } => {
                let address = frame.pop_i32();
                let value = instance
                    .memory
                    .load(address, offset, kind)
                    .map_err(ExecutionError::Trap)?;
                // Float loads tag the raw bits; they never create a native float.
                frame.push(match ty {
                    ValType::I32 => Operand::I32(value as i32),
                    ValType::I64 => Operand::I64(value),
                    ValType::F32 => Operand::F32(value as u32),
                    ValType::F64 => Operand::F64(value as u64),
                });
            }
            Instruction::Store { width, offset, ty } => {
                let value = frame.pop();
                let bits = match ty {
                    ValType::I32 => i64::from(value.as_i32()),
                    ValType::I64 => value.as_i64(),
                    ValType::F32 => i64::from(value.f32_bits()),
                    ValType::F64 => value.f64_bits() as i64,
                };
                let address = frame.pop_i32();
                instance
                    .memory
                    .store(address, offset, width, bits)
                    .map_err(ExecutionError::Trap)?;
            }
            Instruction::MemorySize => frame.push_i32(instance.memory.size_in_pages()),
            Instruction::MemoryGrow => {
                let delta = frame.pop_i32() as u32;
                frame.push_i32(instance.memory.grow(delta));
            }
            Instruction::JumpIfZero { target } => {
                if frame.pop_i32() == 0 {
                    frame.pc = target;
                }
            }
            Instruction::Jump { target } => frame.pc = target,
            Instruction::Branch { edge, conditional } => {
                // A conditional fallthrough pops only the test; any tagged
                // payload is already in the right slot.
                if !conditional || frame.pop_i32() != 0 {
                    frame.take_branch(edge, code);
                }
            }
            Instruction::BranchTable {
                first,
                explicit_count,
            } => {
                // Clamp in u32 before casting: index is at most explicit_count
                // (16), where the default lives. The decoder appended all
                // explicit_count + 1 edges starting at first, so first + index
                // is below table_targets.len() (at most MAX_BODY_TABLE_TARGETS).
                let index = (frame.pop_i32() as u32).min(explicit_count) as usize;
                frame.take_branch(code.table_targets[first + index], code);
            }
        }
    }
}

fn decode_locals(
    body: &mut Reader<'_>,
    signature: FunctionType,
) -> Result<([Operand; MAX_LOCAL_SLOTS], u32), Error> {
    let groups = body.integer(IntegerField::LocalGroupCount)?;
    let mut zeros = [Operand::I32(0); MAX_LOCAL_SLOTS];
    let mut bound = u32::from(signature.parameter_count);
    let limit = bound + MAX_DECLARED_LOCALS;
    for (zero, &ty) in zeros.iter_mut().zip(signature.parameters()) {
        *zero = Operand::zero(ty);
    }
    // Every group consumes at least two bytes of the bounded body, even if its
    // count is zero. Expand only checked counts into the fixed slot array.
    for _ in 0..groups {
        let count = body.count(IntegerField::LocalCount, 0..=limit - bound)?;
        let zero = Operand::zero(body.numeric_type()?);
        let end = bound + count;
        zeros[bound as usize..end as usize].fill(zero);
        bound = end;
    }
    Ok((zeros, bound))
}

#[derive(Clone, Copy)]
struct ControlFrame {
    base: usize,
    result: Option<ValType>,
    kind: ControlKind,
    last_branch: Option<BranchSlot>,
    unreachable: bool,
}

impl ControlFrame {
    fn function(result: Option<ValType>) -> Self {
        Self {
            base: 0,
            result,
            kind: ControlKind::Function,
            last_branch: None,
            unreachable: false,
        }
    }

    fn mark_unreachable(&mut self) -> usize {
        self.unreachable = true;
        self.base
    }

    fn label_result(&self) -> Option<ValType> {
        // A continue supplies loop inputs, not the fallthrough result. Core
        // 1.0 loops have no inputs; the other labels use their end result.
        match self.kind {
            ControlKind::Loop { .. } => None,
            _ => self.result,
        }
    }

    fn pop_result(self, stack: &mut OperandStack, offset: usize) -> Result<(), Error> {
        let depth = stack.depth - self.base;
        let expected = usize::from(self.result.is_some());
        if depth == expected || (self.unreachable && depth < expected) {
            stack.pop(self, expected, self.result, offset)
        } else {
            Err(Error {
                offset,
                kind: ErrorKind::InvalidResultDepth { depth },
            })
        }
    }
}

#[derive(Clone, Copy)]
enum ControlKind {
    Function,
    Block,
    Loop { body_pc: usize },
    Then { if_pc: usize },
    Else { else_pc: usize },
}

// Ordinary branches have one inline edge; a table can have several incoming
// edges to the same or different labels. Link by edge slot rather than source PC.
#[derive(Clone, Copy)]
enum BranchSlot {
    Inline(usize),
    Table(usize),
}

impl BranchSlot {
    fn link_index(self) -> usize {
        match self {
            Self::Inline(pc) => pc,
            Self::Table(index) => Limit::BodyInstructions.maximum() + index,
        }
    }
}

fn decode_branch_edge(
    target: &mut ControlFrame,
    pc: usize,
    slot: BranchSlot,
    previous_branch: &mut [Option<BranchSlot>],
) -> BranchEdge {
    let destination = match target.kind {
        ControlKind::Block | ControlKind::Then { .. } | ControlKind::Else { .. } => {
            previous_branch[slot.link_index()] = target.last_branch.replace(slot);
            // The target's end backpatches the forward PC.
            BranchTarget::End {
                pc: 0,
                result: target.result,
            }
        }
        ControlKind::Loop { body_pc } => BranchTarget::Loop(LoopTarget::at_branch(body_pc, pc)),
        ControlKind::Function => BranchTarget::Function,
    };
    BranchEdge {
        target: destination,
        floor: target.base,
    }
}

struct OperandStack {
    // None is a type-polymorphic value in a counted slot, distinct from
    // implicit operands at an unreachable frame's floor.
    types: [Option<ValType>; Limit::OperandStackDepth.maximum()],
    depth: usize,
}

impl OperandStack {
    fn new() -> Self {
        Self {
            types: [None; Limit::OperandStackDepth.maximum()],
            depth: 0,
        }
    }

    fn push(&mut self, ty: ValType, offset: usize) -> Result<(), Error> {
        self.push_slot(Some(ty), offset)
    }

    fn push_slot(&mut self, ty: Option<ValType>, offset: usize) -> Result<(), Error> {
        let slot = self.types.get_mut(self.depth).ok_or(Error {
            offset,
            kind: ErrorKind::LimitExceeded {
                limit: Limit::OperandStackDepth,
            },
        })?;
        *slot = ty;
        self.depth += 1;
        Ok(())
    }

    fn depth_after_pop(
        &self,
        frame: ControlFrame,
        count: usize,
        offset: usize,
    ) -> Result<usize, Error> {
        let available = self.depth - frame.base;
        if count > available && !frame.unreachable {
            return Err(Error {
                offset,
                kind: ErrorKind::StackUnderflow { depth: available },
            });
        }
        // Missing operands in a dead tail are implicit and consume no slots.
        Ok(self.depth - count.min(available))
    }

    // All typed inputs here share an expected type. Drop accepts any type.
    fn pop(
        &mut self,
        frame: ControlFrame,
        count: usize,
        expected: Option<ValType>,
        offset: usize,
    ) -> Result<(), Error> {
        let remaining = self.depth_after_pop(frame, count, offset)?;
        if let Some(expected) = expected {
            for &actual in self.types[remaining..self.depth].iter().rev().flatten() {
                if actual != expected {
                    return Err(Error {
                        offset,
                        kind: ErrorKind::TypeMismatch { expected, actual },
                    });
                }
            }
        }
        self.depth = remaining;
        Ok(())
    }

    fn call(
        &mut self,
        frame: ControlFrame,
        signature: FunctionType,
        indirect: bool,
        offset: usize,
    ) -> Result<(), Error> {
        let parameters = signature.parameters();
        // Preserve underflow-before-type precedence across all inputs. Pop
        // from the right so explicit values in a dead tail still match their
        // positions; the polymorphic floor supplies any missing prefix.
        self.depth_after_pop(frame, parameters.len() + usize::from(indirect), offset)?;
        if indirect {
            self.pop(frame, 1, Some(ValType::I32), offset)?;
        }
        for &ty in parameters.iter().rev() {
            self.pop(frame, 1, Some(ty), offset)?;
        }
        if let Some(result) = signature.result {
            self.push(result, offset)?;
        }
        Ok(())
    }

    fn pop_transfer(
        &mut self,
        frame: ControlFrame,
        has_selector: bool,
        result: Option<ValType>,
        offset: usize,
    ) -> Result<(), Error> {
        let count = usize::from(result.is_some());
        if has_selector {
            // Preserve underflow-before-type diagnostics across both operands.
            self.depth_after_pop(frame, 1 + count, offset)?;
            self.pop(frame, 1, Some(ValType::I32), offset)?;
        }
        self.pop(frame, count, result, offset)
    }

    fn select(&mut self, frame: ControlFrame, offset: usize) -> Result<(), Error> {
        // As with other consumers, report underflow before checking types.
        let remaining = self.depth_after_pop(frame, 3, offset)?;
        self.pop(frame, 1, Some(ValType::I32), offset)?;
        // Either known candidate sets the result type, and if both are known
        // they must agree. Implicit or unknown candidates defer.
        let mut candidates = self.types[remaining..self.depth].iter().flatten().copied();
        let first = candidates.next();
        if let (Some(first), Some(second)) = (first, candidates.next())
            && first != second
        {
            // The check is symmetric; preserve the existing diagnostic for
            // an i32/i64 pair in either order. Otherwise name both candidates.
            let (expected, actual) = match (first, second) {
                (ValType::I64, ValType::I32) => (ValType::I32, ValType::I64),
                pair => pair,
            };
            return Err(Error {
                offset,
                kind: ErrorKind::TypeMismatch { expected, actual },
            });
        }
        self.depth = remaining;
        self.push_slot(first, offset)
    }
}

fn validate_plain_stack(
    instruction: Instruction,
    operands: &mut OperandStack,
    frame: ControlFrame,
    offset: usize,
    environment: &BodyEnvironment<'_>,
    local_zeros: &[Operand; MAX_LOCAL_SLOTS],
) -> Result<(), Error> {
    use ValType::{I32, I64};
    let i32_type = Some(I32);
    let (count, expected, result) = match instruction {
        Instruction::Nop => return Ok(()),
        Instruction::Drop => (1, None, None),
        Instruction::Select => return operands.select(frame, offset),
        Instruction::Const(value) => (0, None, Some(value.operand_type())),
        Instruction::Convert(op) => {
            let (from, to) = op.types();
            (1, Some(from), Some(to))
        }
        Instruction::FloatUnary(width, _) => {
            let ty = Some(width.operand_type());
            (1, ty, ty)
        }
        Instruction::FloatBinary(width, _) => {
            let ty = Some(width.operand_type());
            (2, ty, ty)
        }
        Instruction::FloatCompare(width, _) => (2, Some(width.operand_type()), i32_type),
        Instruction::LocalGet(index) => (0, None, Some(local_zeros[index as usize].operand_type())),
        Instruction::GlobalGet(index) => (
            0,
            None,
            Some(environment.globals[index as usize].operand_type()),
        ),
        Instruction::MemorySize => (0, None, i32_type),
        Instruction::Call(target) => {
            let (signature, indirect) = match target {
                CallTarget::Direct(index) => (environment.functions[index as usize], false),
                CallTarget::Indirect(expected) => (expected, true),
            };
            return operands.call(frame, signature, indirect, offset);
        }
        Instruction::LocalSet(index) => (1, Some(local_zeros[index as usize].operand_type()), None),
        Instruction::LocalTee(index) => {
            let ty = Some(local_zeros[index as usize].operand_type());
            (1, ty, ty)
        }
        Instruction::GlobalSet(index) => (
            1,
            Some(environment.globals[index as usize].operand_type()),
            None,
        ),
        Instruction::Unary(_) | Instruction::MemoryGrow => (1, i32_type, i32_type),
        Instruction::Load { ty, .. } => (1, i32_type, Some(ty)),
        Instruction::Store { ty, .. } => {
            // Report underflow before checking either type, then check the
            // value before the address (even for a concrete value in dead code).
            operands.depth_after_pop(frame, 2, offset)?;
            operands.pop(frame, 1, Some(ty), offset)?;
            (1, i32_type, None)
        }
        Instruction::Binary(_) => (2, i32_type, i32_type),
        Instruction::I64Eqz => (1, Some(I64), i32_type),
        Instruction::I64Unary(_) => (1, Some(I64), Some(I64)),
        Instruction::I64Binary(op) => (2, Some(I64), Some(op.result_type())),
        Instruction::Unreachable
        | Instruction::JumpIfZero { .. }
        | Instruction::Jump { .. }
        | Instruction::Branch { .. }
        | Instruction::BranchTable { .. } => unreachable!("handled during control validation"),
    };
    operands.pop(frame, count, expected, offset)?;
    if let Some(result) = result {
        operands.push(result, offset)?;
    }
    Ok(())
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

struct BodyEnvironment<'a> {
    types: &'a [FunctionType],
    functions: &'a [FunctionType],
    has_table: bool,
    has_memory: bool,
    globals: &'a [Global],
}

fn decode_plain(
    body: &mut Reader<'_>,
    opcode: u8,
    offset: usize,
    local_bound: u32,
    environment: &BodyEnvironment<'_>,
) -> Result<Instruction, Error> {
    let globals = environment.globals;
    let has_memory = environment.has_memory;
    let load = |body: &mut Reader<'_>, kind: LoadKind, ty| {
        let offset = decode_memarg(body, offset, kind.width(), has_memory)?;
        Ok(Instruction::Load { kind, offset, ty })
    };
    let store = |body: &mut Reader<'_>, width: AccessWidth, ty| {
        let offset = decode_memarg(body, offset, width, has_memory)?;
        Ok(Instruction::Store { width, offset, ty })
    };
    Ok(match opcode {
        0x00 => Instruction::Unreachable,
        0x01 => Instruction::Nop,
        0x10 => Instruction::Call(CallTarget::Direct(body.index(
            IntegerField::FunctionIndex,
            environment.functions.len() as u32,
        )?)),
        0x11 => {
            let index = body.index(IntegerField::TypeIndex, environment.types.len() as u32)?;
            // Core 1.0 reserves a literal zero byte here, not a LEB32 index.
            body.expect_byte(0)?;
            if !environment.has_table {
                return Err(Error {
                    offset,
                    kind: ErrorKind::MissingTable,
                });
            }
            Instruction::Call(CallTarget::Indirect(environment.types[index as usize]))
        }
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
        0x28 => load(body, LoadKind::WordUnsigned, ValType::I32)?,
        0x29 => load(body, LoadKind::Doubleword, ValType::I64)?,
        0x2a => load(body, LoadKind::WordUnsigned, ValType::F32)?,
        0x2b => load(body, LoadKind::Doubleword, ValType::F64)?,
        0x2c => load(body, LoadKind::ByteSigned, ValType::I32)?,
        0x2d => load(body, LoadKind::ByteUnsigned, ValType::I32)?,
        0x2e => load(body, LoadKind::HalfwordSigned, ValType::I32)?,
        0x2f => load(body, LoadKind::HalfwordUnsigned, ValType::I32)?,
        0x30 => load(body, LoadKind::ByteSigned, ValType::I64)?,
        0x31 => load(body, LoadKind::ByteUnsigned, ValType::I64)?,
        0x32 => load(body, LoadKind::HalfwordSigned, ValType::I64)?,
        0x33 => load(body, LoadKind::HalfwordUnsigned, ValType::I64)?,
        0x34 => load(body, LoadKind::WordSigned, ValType::I64)?,
        0x35 => load(body, LoadKind::WordUnsigned, ValType::I64)?,
        0x36 => store(body, AccessWidth::Word, ValType::I32)?,
        0x37 => store(body, AccessWidth::Doubleword, ValType::I64)?,
        0x38 => store(body, AccessWidth::Word, ValType::F32)?,
        0x39 => store(body, AccessWidth::Doubleword, ValType::F64)?,
        0x3a => store(body, AccessWidth::Byte, ValType::I32)?,
        0x3b => store(body, AccessWidth::Halfword, ValType::I32)?,
        0x3c => store(body, AccessWidth::Byte, ValType::I64)?,
        0x3d => store(body, AccessWidth::Halfword, ValType::I64)?,
        0x3e => store(body, AccessWidth::Word, ValType::I64)?,
        0x3f | 0x40 => {
            // Core 1.0 reserves a literal zero byte here, not a LEB32 index.
            body.expect_byte(0)?;
            require_memory(has_memory, offset)?;
            if opcode == 0x3f {
                Instruction::MemorySize
            } else {
                Instruction::MemoryGrow
            }
        }
        I32_CONST => Instruction::Const(body.const_operand(ValType::I32)?),
        I64_CONST => Instruction::Const(body.const_operand(ValType::I64)?),
        F32_CONST => Instruction::Const(body.const_operand(ValType::F32)?),
        F64_CONST => Instruction::Const(body.const_operand(ValType::F64)?),
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
        0x50 => Instruction::I64Eqz,
        0x51 => Instruction::I64Binary(I64BinaryOp::Eq),
        0x52 => Instruction::I64Binary(I64BinaryOp::Ne),
        0x53 => Instruction::I64Binary(I64BinaryOp::LtS),
        0x54 => Instruction::I64Binary(I64BinaryOp::LtU),
        0x55 => Instruction::I64Binary(I64BinaryOp::GtS),
        0x56 => Instruction::I64Binary(I64BinaryOp::GtU),
        0x57 => Instruction::I64Binary(I64BinaryOp::LeS),
        0x58 => Instruction::I64Binary(I64BinaryOp::LeU),
        0x59 => Instruction::I64Binary(I64BinaryOp::GeS),
        0x5a => Instruction::I64Binary(I64BinaryOp::GeU),
        0x5b => Instruction::FloatCompare(FloatWidth::F32, FloatComparison::Eq),
        0x5c => Instruction::FloatCompare(FloatWidth::F32, FloatComparison::Ne),
        0x5d => Instruction::FloatCompare(FloatWidth::F32, FloatComparison::Lt),
        0x5e => Instruction::FloatCompare(FloatWidth::F32, FloatComparison::Gt),
        0x5f => Instruction::FloatCompare(FloatWidth::F32, FloatComparison::Le),
        0x60 => Instruction::FloatCompare(FloatWidth::F32, FloatComparison::Ge),
        0x61 => Instruction::FloatCompare(FloatWidth::F64, FloatComparison::Eq),
        0x62 => Instruction::FloatCompare(FloatWidth::F64, FloatComparison::Ne),
        0x63 => Instruction::FloatCompare(FloatWidth::F64, FloatComparison::Lt),
        0x64 => Instruction::FloatCompare(FloatWidth::F64, FloatComparison::Gt),
        0x65 => Instruction::FloatCompare(FloatWidth::F64, FloatComparison::Le),
        0x66 => Instruction::FloatCompare(FloatWidth::F64, FloatComparison::Ge),
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
        0x79 => Instruction::I64Unary(I64UnaryOp::Clz),
        0x7a => Instruction::I64Unary(I64UnaryOp::Ctz),
        0x7b => Instruction::I64Unary(I64UnaryOp::Popcnt),
        0x7c => Instruction::I64Binary(I64BinaryOp::Add),
        0x7d => Instruction::I64Binary(I64BinaryOp::Sub),
        0x7e => Instruction::I64Binary(I64BinaryOp::Mul),
        0x7f => Instruction::I64Binary(I64BinaryOp::DivS),
        0x80 => Instruction::I64Binary(I64BinaryOp::DivU),
        0x81 => Instruction::I64Binary(I64BinaryOp::RemS),
        0x82 => Instruction::I64Binary(I64BinaryOp::RemU),
        0x83 => Instruction::I64Binary(I64BinaryOp::And),
        0x84 => Instruction::I64Binary(I64BinaryOp::Or),
        0x85 => Instruction::I64Binary(I64BinaryOp::Xor),
        0x86 => Instruction::I64Binary(I64BinaryOp::Shl),
        0x87 => Instruction::I64Binary(I64BinaryOp::ShrS),
        0x88 => Instruction::I64Binary(I64BinaryOp::ShrU),
        0x89 => Instruction::I64Binary(I64BinaryOp::Rotl),
        0x8a => Instruction::I64Binary(I64BinaryOp::Rotr),
        0x8b => Instruction::FloatUnary(FloatWidth::F32, FloatUnaryOp::Abs),
        0x8c => Instruction::FloatUnary(FloatWidth::F32, FloatUnaryOp::Neg),
        0x8d => Instruction::FloatUnary(FloatWidth::F32, FloatUnaryOp::Round(FloatRounding::Ceil)),
        0x8e => Instruction::FloatUnary(FloatWidth::F32, FloatUnaryOp::Round(FloatRounding::Floor)),
        0x8f => Instruction::FloatUnary(FloatWidth::F32, FloatUnaryOp::Round(FloatRounding::Trunc)),
        0x90 => {
            Instruction::FloatUnary(FloatWidth::F32, FloatUnaryOp::Round(FloatRounding::Nearest))
        }
        0x91 => Instruction::FloatUnary(FloatWidth::F32, FloatUnaryOp::Sqrt),
        0x92 => Instruction::FloatBinary(FloatWidth::F32, FloatBinaryOp::Add),
        0x93 => Instruction::FloatBinary(FloatWidth::F32, FloatBinaryOp::Sub),
        0x94 => Instruction::FloatBinary(FloatWidth::F32, FloatBinaryOp::Mul),
        0x95 => Instruction::FloatBinary(FloatWidth::F32, FloatBinaryOp::Div),
        0x96 => Instruction::FloatBinary(FloatWidth::F32, FloatBinaryOp::Min),
        0x97 => Instruction::FloatBinary(FloatWidth::F32, FloatBinaryOp::Max),
        0x98 => Instruction::FloatBinary(FloatWidth::F32, FloatBinaryOp::Copysign),
        0x99 => Instruction::FloatUnary(FloatWidth::F64, FloatUnaryOp::Abs),
        0x9a => Instruction::FloatUnary(FloatWidth::F64, FloatUnaryOp::Neg),
        0x9b => Instruction::FloatUnary(FloatWidth::F64, FloatUnaryOp::Round(FloatRounding::Ceil)),
        0x9c => Instruction::FloatUnary(FloatWidth::F64, FloatUnaryOp::Round(FloatRounding::Floor)),
        0x9d => Instruction::FloatUnary(FloatWidth::F64, FloatUnaryOp::Round(FloatRounding::Trunc)),
        0x9e => {
            Instruction::FloatUnary(FloatWidth::F64, FloatUnaryOp::Round(FloatRounding::Nearest))
        }
        0x9f => Instruction::FloatUnary(FloatWidth::F64, FloatUnaryOp::Sqrt),
        0xa0 => Instruction::FloatBinary(FloatWidth::F64, FloatBinaryOp::Add),
        0xa1 => Instruction::FloatBinary(FloatWidth::F64, FloatBinaryOp::Sub),
        0xa2 => Instruction::FloatBinary(FloatWidth::F64, FloatBinaryOp::Mul),
        0xa3 => Instruction::FloatBinary(FloatWidth::F64, FloatBinaryOp::Div),
        0xa4 => Instruction::FloatBinary(FloatWidth::F64, FloatBinaryOp::Min),
        0xa5 => Instruction::FloatBinary(FloatWidth::F64, FloatBinaryOp::Max),
        0xa6 => Instruction::FloatBinary(FloatWidth::F64, FloatBinaryOp::Copysign),
        0xa7 => Instruction::Convert(Conversion::WrapI64),
        0xa8 => Instruction::Convert(Conversion::TruncFloat(FloatWidth::F32, IntegerType::I32S)),
        0xa9 => Instruction::Convert(Conversion::TruncFloat(FloatWidth::F32, IntegerType::I32U)),
        0xaa => Instruction::Convert(Conversion::TruncFloat(FloatWidth::F64, IntegerType::I32S)),
        0xab => Instruction::Convert(Conversion::TruncFloat(FloatWidth::F64, IntegerType::I32U)),
        0xac => Instruction::Convert(Conversion::ExtendI32S),
        0xad => Instruction::Convert(Conversion::ExtendI32U),
        0xae => Instruction::Convert(Conversion::TruncFloat(FloatWidth::F32, IntegerType::I64S)),
        0xaf => Instruction::Convert(Conversion::TruncFloat(FloatWidth::F32, IntegerType::I64U)),
        0xb0 => Instruction::Convert(Conversion::TruncFloat(FloatWidth::F64, IntegerType::I64S)),
        0xb1 => Instruction::Convert(Conversion::TruncFloat(FloatWidth::F64, IntegerType::I64U)),
        0xb2 => Instruction::Convert(Conversion::IntToFloat(IntegerType::I32S, FloatWidth::F32)),
        0xb3 => Instruction::Convert(Conversion::IntToFloat(IntegerType::I32U, FloatWidth::F32)),
        0xb4 => Instruction::Convert(Conversion::IntToFloat(IntegerType::I64S, FloatWidth::F32)),
        0xb5 => Instruction::Convert(Conversion::IntToFloat(IntegerType::I64U, FloatWidth::F32)),
        0xb6 => Instruction::Convert(Conversion::DemoteF64),
        0xb7 => Instruction::Convert(Conversion::IntToFloat(IntegerType::I32S, FloatWidth::F64)),
        0xb8 => Instruction::Convert(Conversion::IntToFloat(IntegerType::I32U, FloatWidth::F64)),
        0xb9 => Instruction::Convert(Conversion::IntToFloat(IntegerType::I64S, FloatWidth::F64)),
        0xba => Instruction::Convert(Conversion::IntToFloat(IntegerType::I64U, FloatWidth::F64)),
        0xbb => Instruction::Convert(Conversion::PromoteF32),
        0xbc => Instruction::Convert(Conversion::I32ReinterpretF32),
        0xbd => Instruction::Convert(Conversion::I64ReinterpretF64),
        0xbe => Instruction::Convert(Conversion::F32ReinterpretI32),
        0xbf => Instruction::Convert(Conversion::F64ReinterpretI64),
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
    environment: &BodyEnvironment<'_>,
) -> Result<Vec<ValidatedCode>, Error> {
    let allocation_error = section.error(ErrorKind::AllocationFailed);
    let declaration_count = environment.functions.len() as u32;
    let allowed_counts = if declaration_count == 0 {
        1..=MAX_FUNCTIONS
    } else {
        declaration_count..=declaration_count
    };
    let count = section.count(IntegerField::CodeCount, allowed_counts)?;
    let mut functions = Vec::new();
    functions
        .try_reserve_exact(count as usize)
        .map_err(|_| allocation_error)?;
    for index in 0..count {
        // Preserve framing and ordering diagnostics for incomplete modules:
        // without declarations there are no direct call targets, and the first
        // type (if any) stands in until the final required-section check.
        let signature = environment
            .functions
            .get(index as usize)
            .or_else(|| environment.types.first())
            .copied()
            .unwrap_or(FunctionType::PLACEHOLDER);
        functions.push(validate_body(section, signature, environment)?);
    }
    Ok(functions)
}

fn validate_body(
    section: &mut Reader<'_>,
    signature: FunctionType,
    environment: &BodyEnvironment<'_>,
) -> Result<ValidatedCode, Error> {
    use ValType::I32;
    let declared = section.length(LengthField::CodeBodySize)?;
    let available = section.remaining.len();
    let mut body = section.frame(declared).ok_or(Error {
        offset: section.end,
        kind: ErrorKind::TruncatedBody {
            declared,
            available,
        },
    })?;
    let (local_zeros, local_bound) = decode_locals(&mut body, signature)?;
    // Forward transfers reserve slots that their matching delimiters backpatch.
    // Each block or if chains its incoming edges, each visited once at the
    // target's end. Loop edges resolve immediately to the PC saved at entry.
    let mut decoded = [Instruction::Nop; Limit::BodyInstructions.maximum()];
    let mut table_targets = [BranchEdge {
        target: BranchTarget::Function,
        floor: 0,
    }; MAX_BODY_TABLE_TARGETS];
    let mut target_count = 0;
    let mut previous_branch = [None; Limit::BodyInstructions.maximum() + MAX_BODY_TABLE_TARGETS];
    let mut count = 0;
    let mut operands = OperandStack::new();
    // Slot zero is the function frame, which does not count against the cap.
    let mut frames = [ControlFrame::function(signature.result); Limit::ControlDepth.maximum() + 1];
    let mut nesting = 0;
    let end_offset = loop {
        let offset = body.position();
        let frame = frames[nesting];
        let ControlFrame {
            result,
            kind,
            mut last_branch,
            ..
        } = frame;
        let requires_else = result.is_some() && matches!(kind, ControlKind::Then { .. });
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
            frame.pop_result(&mut operands, offset)?;
            // Push the declared result onto the parent's stack as a concrete
            // value, even if the child supplied it only by polymorphism.
            if let Some(ty) = result {
                operands.push(ty, offset)?;
            }
            // Forward edges target the next decoded slot, possibly the final
            // one-past-last index. All sources are in the decoded prefix.
            match kind {
                ControlKind::Block | ControlKind::Loop { .. } => {}
                ControlKind::Else { else_pc } => {
                    decoded[..count][else_pc] = Instruction::Jump { target: count };
                }
                ControlKind::Function => {
                    body.finish(ErrorKind::TrailingBodyBytes)?;
                    break offset;
                }
                ControlKind::Then { if_pc } => {
                    // The implicit empty else skips straight to the join.
                    decoded[..count][if_pc] = Instruction::JumpIfZero { target: count };
                }
            }
            while let Some(slot) = last_branch {
                let edge = match slot {
                    BranchSlot::Inline(pc) => {
                        let Instruction::Branch { edge, .. } = &mut decoded[..count][pc] else {
                            unreachable!("inline fixups link only branches");
                        };
                        edge
                    }
                    BranchSlot::Table(index) => &mut table_targets[..target_count][index],
                };
                let BranchTarget::End { pc: target_pc, .. } = &mut edge.target else {
                    unreachable!("end fixups link only forward edges");
                };
                *target_pc = count;
                last_branch = previous_branch[slot.link_index()];
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
        match opcode {
            BLOCK | LOOP | IF => {
                let result = body.block_result()?;
                let next = frames.get_mut(nesting + 1).ok_or(Error {
                    offset,
                    kind: ErrorKind::LimitExceeded {
                        limit: Limit::ControlDepth,
                    },
                })?;
                let kind = match opcode {
                    BLOCK => ControlKind::Block,
                    LOOP => ControlKind::Loop { body_pc: pc },
                    IF => {
                        operands.pop(frame, 1, Some(I32), offset)?;
                        ControlKind::Then { if_pc: pc }
                    }
                    _ => unreachable!("only control openers"),
                };
                *next = ControlFrame {
                    base: operands.depth,
                    result,
                    kind,
                    last_branch: None,
                    unreachable: false,
                };
                nesting += 1;
            }
            ELSE => {
                let ControlKind::Then { if_pc } = kind else {
                    return Err(Error {
                        offset,
                        kind: ErrorKind::UnexpectedElse,
                    });
                };
                frame.pop_result(&mut operands, offset)?;
                // Skip the then arm and its else jump on a zero condition.
                decoded[..pc][if_pc] = Instruction::JumpIfZero { target: count };
                // The arms share a label: keep its branch chain until end.
                frames[nesting].kind = ControlKind::Else { else_pc: pc };
                frames[nesting].unreachable = false;
            }
            BR | BR_IF | RETURN => {
                // The function is at depth `nesting`, outside every explicit
                // frame. Only branches have a label immediate.
                let label = if opcode == RETURN {
                    nesting as u32
                } else {
                    body.index(IntegerField::LabelIndex, (nesting + 1) as u32)?
                };
                let target = &mut frames[nesting - label as usize];
                let edge =
                    decode_branch_edge(target, pc, BranchSlot::Inline(pc), &mut previous_branch);
                // Pop against the active floor. Only execution of the taken
                // branch uses the target floor.
                let conditional = opcode == BR_IF;
                let label_result = target.label_result();
                operands.pop_transfer(frame, conditional, label_result, offset)?;
                decoded[pc] = Instruction::Branch { edge, conditional };
                if !conditional {
                    operands.depth = frames[nesting].mark_unreachable();
                } else if let Some(ty) = label_result {
                    // Fallthrough restores the payload, even if it was implicit.
                    operands.push(ty, offset)?;
                }
            }
            BR_TABLE => {
                let explicit_count =
                    body.count(IntegerField::BranchTableCount, 0..=MAX_BR_TABLE_ENTRIES)?;
                let first = target_count;
                let mut common = None;
                // Decode the declared vector and then exactly one mandatory
                // default, even when the vector is empty or the frame is dead.
                for _ in 0..=explicit_count {
                    let label_offset = body.position();
                    let label = body.index(IntegerField::LabelIndex, (nesting + 1) as u32)?;
                    let target = &mut frames[nesting - label as usize];
                    let actual = target.label_result();
                    let expected = *common.get_or_insert(actual);
                    if actual != expected {
                        return Err(Error {
                            offset: label_offset,
                            kind: match (expected, actual) {
                                (Some(expected), Some(actual)) => {
                                    ErrorKind::BranchTableTypeMismatch { expected, actual }
                                }
                                _ => ErrorKind::BranchTableArityMismatch {
                                    expected: usize::from(expected.is_some()),
                                    actual: usize::from(actual.is_some()),
                                },
                            },
                        });
                    }
                    // A successful label immediate used at least one byte, so
                    // the body byte bound also bounds the cumulative index.
                    table_targets[target_count] = decode_branch_edge(
                        target,
                        pc,
                        BranchSlot::Table(target_count),
                        &mut previous_branch,
                    );
                    target_count += 1;
                }
                let label_result = common.expect("every table has a default");
                operands.pop_transfer(frame, true, label_result, offset)?;
                decoded[pc] = Instruction::BranchTable {
                    first,
                    explicit_count,
                };
                operands.depth = frames[nesting].mark_unreachable();
            }
            _ => {
                let instruction =
                    decode_plain(&mut body, opcode, offset, local_bound, environment)?;
                if matches!(instruction, Instruction::Unreachable) {
                    operands.depth = frames[nesting].mark_unreachable();
                } else {
                    validate_plain_stack(
                        instruction,
                        &mut operands,
                        frame,
                        offset,
                        environment,
                        &local_zeros,
                    )?;
                }
                decoded[pc] = instruction;
            }
        }
    };
    // The successful function end closes the exact body frame. Every inner
    // frame has now closed and patched its incoming edges; unvalidated and
    // partially fixed instructions and targets never leave the scratch arrays.
    let mut instructions = Vec::new();
    let mut retained_targets = Vec::new();
    let allocation_error = Error {
        offset: end_offset,
        kind: ErrorKind::AllocationFailed,
    };
    instructions
        .try_reserve_exact(count)
        .map_err(|_| allocation_error)?;
    retained_targets
        .try_reserve_exact(target_count)
        .map_err(|_| allocation_error)?;
    instructions.extend_from_slice(&decoded[..count]);
    retained_targets.extend_from_slice(&table_targets[..target_count]);
    Ok(ValidatedCode {
        signature,
        local_zeros,
        instructions,
        table_targets: retained_targets,
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

    fn numeric_type(&mut self) -> Result<ValType, Error> {
        let offset = self.position();
        // Keep the existing error byte for unsupported and missing types.
        match self.byte() {
            Some(I32) => Ok(ValType::I32),
            Some(I64) => Ok(ValType::I64),
            Some(0x7d) => Ok(ValType::F32),
            Some(0x7c) => Ok(ValType::F64),
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

    fn block_result(&mut self) -> Result<Option<ValType>, Error> {
        let offset = self.position();
        // Preserve the original expected byte in errors for block and if.
        match self.byte() {
            Some(EMPTY_BLOCK_TYPE) => Ok(None),
            Some(I32) => Ok(Some(ValType::I32)),
            Some(I64) => Ok(Some(ValType::I64)),
            Some(0x7d) => Ok(Some(ValType::F32)),
            Some(0x7c) => Ok(Some(ValType::F64)),
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

    fn global_initializer(&mut self, ty: ValType) -> Result<Operand, Error> {
        self.expect_byte(match ty {
            ValType::I32 => I32_CONST,
            ValType::I64 => I64_CONST,
            ValType::F32 => F32_CONST,
            ValType::F64 => F64_CONST,
        })?;
        let value = self.const_operand(ty)?;
        self.expect_byte(END)?;
        Ok(value)
    }

    fn const_operand(&mut self, ty: ValType) -> Result<Operand, Error> {
        Ok(match ty {
            ValType::I32 => Operand::I32(self.integer(IntegerField::I32Const)? as i32),
            ValType::I64 => Operand::I64(self.i64_const()?),
            ValType::F32 => Operand::F32(u32::from_le_bytes(self.float_const_bytes(ty)?)),
            ValType::F64 => Operand::F64(u64::from_le_bytes(self.float_const_bytes(ty)?)),
        })
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

    fn float_const_bytes<const N: usize>(&mut self, ty: ValType) -> Result<[u8; N], Error> {
        let (bytes, rest) = self.remaining.split_first_chunk().ok_or(Error {
            offset: self.end,
            kind: ErrorKind::TruncatedFloatConst { ty },
        })?;
        self.remaining = rest;
        Ok(*bytes)
    }

    fn i64_const(&mut self) -> Result<i64, Error> {
        let field = IntegerField::I64Const;
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let offset = self.position();
            let byte = self
                .byte()
                .ok_or_else(|| self.error(ErrorKind::TruncatedInteger { field }))?;
            if shift == 63 {
                // Only the sign bit remains. A tenth byte must terminate, with
                // all six unused payload bits equal to the actual sign bit.
                return match byte {
                    0x00 => Ok(value as i64),
                    0x7f => Ok((value | (1 << 63)) as i64),
                    _ => Err(Error {
                        offset,
                        kind: ErrorKind::IntegerOverflow { field },
                    }),
                };
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                if byte & 0x40 != 0 {
                    value |= u64::MAX << (shift + 7);
                }
                return Ok(value as i64);
            }
            shift += 7;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_bits_do_not_enlarge_operand_slots() {
        assert!(std::mem::size_of::<Operand>() <= std::mem::size_of::<Value>());
        assert!(std::mem::align_of::<Operand>() <= std::mem::align_of::<Value>());
    }

    #[test]
    fn host_operand_conversions_preserve_tags_and_raw_bits() {
        for (value, operand, ty) in [
            (Value::I32(i32::MIN), Operand::I32(i32::MIN), ValType::I32),
            (Value::I64(i64::MIN), Operand::I64(i64::MIN), ValType::I64),
            (
                Value::F32(0x8000_0000),
                Operand::F32(0x8000_0000),
                ValType::F32,
            ),
            (
                Value::F32(0x7f80_0b12),
                Operand::F32(0x7f80_0b12),
                ValType::F32,
            ),
            (
                Value::F64(0xfff0_000b_89ab_cdef),
                Operand::F64(0xfff0_000b_89ab_cdef),
                ValType::F64,
            ),
        ] {
            assert_eq!(value.operand_type(), ty);
            assert_eq!(Operand::from(value), operand);
            assert_eq!(Value::from(operand), value);
        }
    }

    #[test]
    fn signed_and_unsigned_extension_differ_in_the_high_bits() {
        // The public i32 result cannot observe those high bits; test the
        // conversions used by dispatch directly as well as wrap round trips.
        for (input, signed, unsigned) in [
            (0, 0, 0),
            (i32::MAX, 0x7fff_ffff, 0x7fff_ffff),
            (i32::MIN, -0x8000_0000, 0x8000_0000),
            (-1, -1, 0xffff_ffff),
        ] {
            let input = Operand::I32(input);
            let extended_s = Conversion::ExtendI32S.apply(input).unwrap();
            let extended_u = Conversion::ExtendI32U.apply(input).unwrap();
            assert_eq!(extended_s, Operand::I64(signed));
            assert_eq!(extended_u, Operand::I64(unsigned));
            assert_eq!(Conversion::WrapI64.apply(extended_s), Ok(input));
            assert_eq!(Conversion::WrapI64.apply(extended_u), Ok(input));
        }
    }

    #[test]
    fn decodes_full_i64_signed_values_not_just_the_low_word() {
        for (immediate, expected) in [
            (&[0x40][..], -64),
            (&[0xc0, 0x00], 64),
            (&[0x80, 0x80, 0x80, 0x80, 0x10], 0x1_0000_0000),
            (&[0xff, 0xff, 0xff, 0xff, 0x6f], -0x1_0000_0001),
            (
                &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00],
                i64::MAX,
            ),
            (
                &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x7f],
                i64::MIN,
            ),
        ] {
            let mut reader = Reader {
                remaining: immediate,
                end: immediate.len(),
            };
            assert_eq!(reader.i64_const(), Ok(expected));
            assert_eq!(reader.position(), immediate.len());
        }
    }
}
