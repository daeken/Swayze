use crate::{ExecutionLimit, Export, InvokeError, Module, Trap};
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
/// indicates an `if` closed by `end` without an `else`. The depth in
/// `StackUnderflow` and `InvalidResultDepth` counts only operands above the
/// active frame's entry stack floor. The latter is reported at a delimiter
/// when an arm or function fails to produce exactly one result.
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
    DuplicateExportName,
    UnsupportedCount { field: IntegerField, count: u32 },
    InvalidIndex { field: IntegerField, index: u32 },
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
    LocalIndex,
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
    /// Decoded operations across the whole body, including `if` and `else` but not `end`.
    BodyInstructions,
    /// Peak number of live i32 operands in the code body.
    OperandStackDepth,
    /// Number of simultaneously open `if` frames, excluding the function frame.
    ControlDepth,
    /// Name bytes in one custom section, excluding its length field.
    CustomNameBytes,
    /// Name bytes in one export, excluding its length field.
    ExportNameBytes,
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
            DuplicateExportName => f.write_str("duplicate export name"),
            UnsupportedCount { field, count } => write!(f, "unsupported {field:?} of {count}"),
            InvalidIndex { field, index } => write!(f, "invalid {field:?} {index}"),
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
                    "expected one result above the frame's stack floor, found {depth}"
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
/// [`Limit::ExportNameBytes`].
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
    let mut parameter_count = None;
    let mut function_count = None;
    let mut functions = Vec::new();
    let mut exports = Vec::new();
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
            1 => parameter_count = Some(validate_type(&mut section)?),
            3 => {
                let count = section.count(IntegerField::FunctionCount, 1..=MAX_FUNCTIONS)?;
                for _ in 0..count {
                    section.index(IntegerField::TypeIndex, 1)?;
                }
                function_count = Some(count);
            }
            7 => exports = validate_exports(&mut section, function_count)?,
            10 => {
                // Decode even without the type section, so framing and ordering
                // errors still take precedence. The final presence check rejects it.
                functions =
                    validate_code(&mut section, function_count, parameter_count.unwrap_or(0))?;
            }
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
        // The presence of all three sections makes the count and index checks
        // above sufficient to associate each export and call with a validated body.
        for (id, present) in [
            (1, parameter_count.is_some()),
            (3, function_count.is_some()),
            (10, !functions.is_empty()),
        ] {
            if !present {
                return Err(module.error(ErrorKind::MissingSection { id }));
            }
        }
    }
    Ok(Module {
        parameter_count: parameter_count.unwrap_or(0) as usize,
        functions,
        exports,
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

const MAX_FUNCTIONS: u32 = 8;
const MAX_EXPORTS: u32 = 8;
const MAX_PARAMETERS: u32 = 4;
const MAX_DECLARED_LOCALS: u32 = 16;
const MAX_LOCAL_SLOTS: usize = (MAX_PARAMETERS + MAX_DECLARED_LOCALS) as usize;
const I32: u8 = 0x7f;

fn validate_type(section: &mut Reader<'_>) -> Result<u32, Error> {
    section.count(IntegerField::TypeCount, 1..=1)?;
    section.expect_byte(0x60)?; // functype
    let parameter_count = section.count(IntegerField::ParameterCount, 0..=MAX_PARAMETERS)?;
    for _ in 0..parameter_count {
        section.expect_byte(I32)?;
    }
    section.count(IntegerField::ResultCount, 1..=1)?;
    section.expect_byte(I32)?;
    Ok(parameter_count)
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

const IF: u8 = 0x04;
const ELSE: u8 = 0x05;
const END: u8 = 0x0b;

#[derive(Clone, Copy, Debug)]
enum Instruction {
    Const(i32),
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    Call(u32),
    Eqz,
    Binary(BinaryOp),
    JumpIfZero { target: usize },
    Jump { target: usize },
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
}

impl BinaryOp {
    fn apply(self, lhs: i32, rhs: i32) -> Result<i32, Trap> {
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
        }
    }
}

/// Only the decoder constructs this, after checking local and function indices,
/// every stack effect, and the complete body frame. Intra-body jump targets are
/// strictly forward decoded instruction indices, or the one-past-last index.
#[derive(Debug)]
pub(super) struct ValidatedCode {
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
    module: &Module,
    function_index: usize,
    args: &[i32],
) -> Result<i32, InvokeError> {
    let parameter_count = module.parameter_count;
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
            Instruction::Const(value) => frame.push(value),
            Instruction::LocalGet(index) => frame.push(frame.locals[index as usize]),
            Instruction::LocalSet(index) => frame.locals[index as usize] = frame.pop(),
            Instruction::LocalTee(index) => {
                frame.locals[index as usize] = frame.operands[frame.depth - 1]
            }
            Instruction::Call(index) => {
                if active + 1 == ExecutionLimit::CallDepth.maximum() {
                    return Err(InvokeError::LimitExceeded {
                        limit: ExecutionLimit::CallDepth,
                    });
                }
                // Keep the caller's prefix; the argument suffix is already in
                // parameter order. Reset reused frame storage before entering.
                let end = frame.depth;
                frame.depth -= parameter_count;
                let callee = CallFrame::new(index as usize, &frame.operands[frame.depth..end]);
                active += 1;
                frames[active] = callee;
            }
            Instruction::Eqz => {
                let top = &mut frame.operands[frame.depth - 1];
                *top = i32::from(*top == 0);
            }
            Instruction::Binary(op) => {
                let rhs = frame.pop();
                let lhs = &mut frame.operands[frame.depth - 1];
                *lhs = op.apply(*lhs, rhs).map_err(InvokeError::Trap)?;
            }
            Instruction::JumpIfZero { target } => {
                if frame.pop() == 0 {
                    frame.pc = target;
                }
            }
            Instruction::Jump { target } => frame.pc = target,
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
struct IfFrame {
    base: usize,
    arm: IfArm,
}

#[derive(Clone, Copy)]
enum IfArm {
    Then { if_pc: usize },
    Else { else_pc: usize },
}

fn check_result_depth(depth: usize, offset: usize) -> Result<(), Error> {
    if depth == 1 {
        Ok(())
    } else {
        Err(Error {
            offset,
            kind: ErrorKind::InvalidResultDepth { depth },
        })
    }
}

fn decode_plain(
    body: &mut Reader<'_>,
    opcode: u8,
    offset: usize,
    local_bound: u32,
    function_bound: u32,
) -> Result<Instruction, Error> {
    Ok(match opcode {
        0x10 => Instruction::Call(body.index(IntegerField::FunctionIndex, function_bound)?),
        0x20 => Instruction::LocalGet(body.index(IntegerField::LocalIndex, local_bound)?),
        0x21 => Instruction::LocalSet(body.index(IntegerField::LocalIndex, local_bound)?),
        0x22 => Instruction::LocalTee(body.index(IntegerField::LocalIndex, local_bound)?),
        0x41 => Instruction::Const(body.integer(IntegerField::I32Const)? as i32),
        0x45 => Instruction::Eqz,
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
        0x6a => Instruction::Binary(BinaryOp::Add),
        0x6b => Instruction::Binary(BinaryOp::Sub),
        0x6c => Instruction::Binary(BinaryOp::Mul),
        0x6d => Instruction::Binary(BinaryOp::DivS),
        0x6e => Instruction::Binary(BinaryOp::DivU),
        0x6f => Instruction::Binary(BinaryOp::RemS),
        0x70 => Instruction::Binary(BinaryOp::RemU),
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
    function_count: Option<u32>,
    parameter_count: u32,
) -> Result<Vec<ValidatedCode>, Error> {
    let allocation_error = section.error(ErrorKind::AllocationFailed);
    let allowed_counts = function_count.map_or(1..=MAX_FUNCTIONS, |count| count..=count);
    let count = section.count(IntegerField::CodeCount, allowed_counts)?;
    let mut functions = Vec::new();
    functions
        .try_reserve_exact(count as usize)
        .map_err(|_| allocation_error)?;
    for _ in 0..count {
        functions.push(validate_body(
            section,
            parameter_count,
            function_count.unwrap_or(0),
        )?);
    }
    Ok(functions)
}

fn validate_body(
    section: &mut Reader<'_>,
    parameter_count: u32,
    function_bound: u32,
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
    // An if/else reserves an instruction slot, then its matching delimiter
    // backpatches it. A final function end requires all frames to have closed.
    let mut decoded = [Instruction::Const(0); Limit::BodyInstructions.maximum()];
    let mut count = 0;
    let mut depth = 0usize;
    let mut frames = [IfFrame {
        base: 0,
        arm: IfArm::Then { if_pc: 0 },
    }; Limit::ControlDepth.maximum()];
    let mut nesting = 0;
    let end_offset = loop {
        let offset = body.position();
        let frame = frames[..nesting].last().copied();
        let base = frame.map_or(0, |frame| frame.base);
        let arm = frame.map(|frame| frame.arm);
        let available = depth - base;
        let opcode = body.byte().ok_or_else(|| {
            let expected = if matches!(arm, Some(IfArm::Then { .. })) {
                ELSE
            } else {
                END
            };
            body.error(ErrorKind::TruncatedByte { expected })
        })?;
        if opcode == END {
            match arm {
                Some(IfArm::Then { .. }) => {
                    return Err(Error {
                        offset,
                        kind: ErrorKind::MissingElse,
                    });
                }
                Some(IfArm::Else { else_pc }) => {
                    check_result_depth(available, offset)?;
                    // The source must be in the decoded prefix. Its target is
                    // the next slot, possibly the final one-past-last index.
                    decoded[..count][else_pc] = Instruction::Jump { target: count };
                    nesting -= 1;
                    continue;
                }
                None => {
                    check_result_depth(available, offset)?;
                    body.finish(ErrorKind::TrailingBodyBytes)?;
                    break offset;
                }
            }
        }
        let pc = count;
        if pc == decoded.len() {
            return Err(Error {
                offset,
                kind: ErrorKind::LimitExceeded {
                    limit: Limit::BodyInstructions,
                },
            });
        }
        count += 1;
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
            IF => {
                body.expect_byte(I32)?; // Only a single i32 result, no block parameters.
                let next = frames.get_mut(nesting).ok_or(Error {
                    offset,
                    kind: ErrorKind::LimitExceeded {
                        limit: Limit::ControlDepth,
                    },
                })?;
                depth = consume(1)?;
                *next = IfFrame {
                    base: depth,
                    arm: IfArm::Then { if_pc: pc },
                };
                nesting += 1;
            }
            ELSE => {
                let Some(IfArm::Then { if_pc }) = arm else {
                    return Err(Error {
                        offset,
                        kind: ErrorKind::UnexpectedElse,
                    });
                };
                check_result_depth(available, offset)?;
                // Skip the then arm and its else jump on a zero condition.
                decoded[..pc][if_pc] = Instruction::JumpIfZero { target: count };
                frames[nesting - 1].arm = IfArm::Else { else_pc: pc };
                depth = base;
            }
            _ => {
                let instruction =
                    decode_plain(&mut body, opcode, offset, local_bound, function_bound)?;
                depth = match instruction {
                    Instruction::Const(_) | Instruction::LocalGet(_) => depth + 1,
                    Instruction::Call(_) => consume(parameter_count as usize)? + 1,
                    Instruction::LocalSet(_) => consume(1)?,
                    Instruction::LocalTee(_) | Instruction::Eqz => consume(1)? + 1,
                    Instruction::Binary(_) => consume(2)? + 1,
                    Instruction::JumpIfZero { .. } | Instruction::Jump { .. } => {
                        unreachable!("only control delimiters create jumps")
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
    Ok(ValidatedCode { instructions })
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
