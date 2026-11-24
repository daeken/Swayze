use std::{cmp::Ordering, fmt};

use crate::{
    CompletionScript, Script,
    parser::{
        BindingKind, ExitStack, Instruction, PreparedScript, ShortCircuitCondition, Update,
        UpdateResult,
    },
};

// Fixed evaluation ceilings, independent of custom parse limits.
pub(crate) const MAX_INSTRUCTIONS: usize = 4096;
const MAX_DISPATCHES: usize = 16_384;
const MAX_STRING_COMPARISON_PAIRS: usize = 65_536;

mod bindings;
pub mod realm;
use bindings::{
    Bindings, LegacyBindings, UpdateNumeric, init_operand, store_operand, update_result,
};

mod string_concat;
mod string_number;

pub use string_concat::EvalScratch;
use string_concat::{ExternalStrings, StringOperand, StringPools, TransientStrings, TypeName};

/// A scalar value in the supported JavaScript subset.
///
/// The derived [`PartialEq`] compares Rust enum variants and their payloads.
/// Number equality treats signed zeros as equal and NaN as unequal to itself.
/// This is a host-side convenience, not a definition of JavaScript equality.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    /// A JavaScript Number (binary64), including signed zeros, infinities, and NaN.
    Number(f64),
    /// A JavaScript Boolean.
    Boolean(bool),
    /// The JavaScript `null` value.
    Null,
    /// The JavaScript `undefined` value.
    Undefined,
}

/// A supported primitive result. Strings borrow immutable UTF-16 units from
/// the prepared Script, the caller's evaluation scratch, or private static
/// storage. The return type still ties the borrow to the inputs of the API.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrimitiveValue<'a> {
    /// Number, Boolean, `null`, or `undefined`.
    Scalar(Value),
    /// JavaScript String code units, without normalization or replacement.
    String(&'a [u16]),
}

#[derive(Clone, Copy)]
enum Operand {
    Scalar(Value),
    // Producers check logical bounds before creating operands. Realm GetValue
    // also checks offsets before returning them to metadata-only consumers.
    String(StringOperand),
    Function(realm::FunctionId),
}

impl Operand {
    fn to_number(
        self,
        strings: StringPools<'_>,
        credits_left: &mut usize,
    ) -> Result<f64, EvalError> {
        match self {
            Self::Scalar(value) => Ok(value.to_number()),
            Self::Function(_) => Err(EvalError::UnsupportedCallableConversion),
            Self::String(range) => {
                let units = range.get(strings).ok_or(EvalError::InvalidScript)?;
                string_number::convert(units, credits_left)
            }
        }
    }

    fn to_boolean(self) -> bool {
        match self {
            Self::Scalar(value) => value.to_boolean(),
            Self::Function(_) => true,
            Self::String(range) => !range.is_empty(),
        }
    }
}

impl Value {
    fn to_number(self) -> f64 {
        match self {
            Self::Number(value) => value,
            Self::Boolean(true) => 1.0,
            Self::Boolean(false) | Self::Null => 0.0,
            Self::Undefined => f64::NAN,
        }
    }

    fn to_boolean(self) -> bool {
        match self {
            Self::Number(value) => value != 0.0 && !value.is_nan(),
            Self::Boolean(value) => value,
            Self::Null | Self::Undefined => false,
        }
    }

    fn loosely_equals(self, other: Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Undefined) | (Self::Undefined, Self::Null) => true,
            (Self::Number(_), Self::Boolean(_)) | (Self::Boolean(_), Self::Number(_)) => {
                self.to_number() == other.to_number()
            }
            // Same tags use strict equality; all other mixed tags are unequal.
            _ => self.strictly_equals(other),
        }
    }

    fn strictly_equals(self, other: Self) -> bool {
        match (self, other) {
            // Binary64 equality equates signed zeros and never equates NaN.
            (Self::Number(lhs), Self::Number(rhs)) => lhs == rhs,
            (Self::Boolean(lhs), Self::Boolean(rhs)) => lhs == rhs,
            (Self::Null, Self::Null) | (Self::Undefined, Self::Undefined) => true,
            _ => false,
        }
    }
}

fn number_to_uint32(number: f64) -> u32 {
    const MODULUS: f64 = 4_294_967_296.0; // 2^32
    if !number.is_finite() {
        return 0;
    }
    // Reduce the represented Number, not its source text. The truncated
    // integer's remainder is exact: the modulus is a power of two, and
    // both the remainder and its nonnegative residue fit exactly in f64.
    let remainder = number.trunc() % MODULUS;
    (if remainder < 0.0 {
        remainder + MODULUS
    } else {
        remainder
    }) as u32
}

fn number_to_int32(number: f64) -> i32 {
    number_to_uint32(number) as i32 // Interpret the same bits as a signed integer.
}

impl ShortCircuitCondition {
    fn skips_rhs(self, lhs: Operand) -> bool {
        match self {
            Self::Falsy => !lhs.to_boolean(),
            Self::Truthy => lhs.to_boolean(),
            Self::NonNullish => !matches!(lhs, Operand::Scalar(Value::Null | Value::Undefined)),
        }
    }
}

/// Failure to evaluate a prepared script or obtain the requested result type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EvalError {
    /// The prepared instruction sequence exceeds the 4,096-instruction ceiling.
    LimitExceeded,
    /// Execution would dispatch more than 16,384 instructions in this evaluation.
    DispatchLimitExceeded,
    /// The instruction sequence has invalid control flow, operands, or binding-slot usage.
    InvalidScript,
    /// The evaluator could not allocate bounded operand, binding, or runtime String storage.
    AllocationFailed,
    /// The numeric convenience API received a non-Number result.
    NonNumericResult,
    /// The scalar convenience API received a String result.
    NonScalarResult,
    /// An operator's String behavior is outside the subset (retained for compatibility).
    UnsupportedStringOperation,
    /// A String comparison would exceed the evaluation's 65,536-pair budget.
    StringComparisonLimitExceeded,
    /// An operator's String-to-Number conversion would exceed the 262,144-credit evaluation budget.
    StringNumericConversionLimitExceeded,
    /// A concatenation or template append would produce more than 4,094 UTF-16 units.
    StringConcatenationLengthLimitExceeded,
    /// A concatenation or template append would exceed the evaluation's 32,768 appended-unit budget.
    StringConcatenationBudgetExceeded,
    /// The primitive convenience API received a runtime String result.
    RuntimeStringResultRequiresScratch,
    /// A reached `.length` receiver is not a primitive String.
    /// This is a subset error, not JavaScript `undefined` or a `TypeError`.
    UnsupportedPropertyReceiver,
    /// Converting a callable to a primitive requires unsupported object machinery.
    UnsupportedCallableConversion,
    CallDepthLimitExceeded,
    LiveSlotLimitExceeded,
    GlobalLookupLimitExceeded,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsupportedCallableConversion => {
                "callable-to-primitive conversion is outside the subset"
            }
            Self::CallDepthLimitExceeded => "JavaScript guest frame limit exceeded",
            Self::LiveSlotLimitExceeded => "JavaScript live frame slot limit exceeded",
            Self::GlobalLookupLimitExceeded => {
                "JavaScript call setup and global lookup budget exceeded"
            }
            Self::LimitExceeded => "prepared JavaScript instruction limit exceeded",
            Self::DispatchLimitExceeded => "JavaScript evaluation dispatch limit exceeded",
            Self::InvalidScript => "invalid prepared JavaScript instruction sequence",
            Self::AllocationFailed => "could not allocate JavaScript evaluation storage",
            Self::NonNumericResult => "script result is not a Number",
            Self::NonScalarResult => "script result is not a scalar",
            Self::UnsupportedStringOperation => {
                "operator on a JavaScript String is outside the subset"
            }
            Self::StringComparisonLimitExceeded => "JavaScript string comparison limit exceeded",
            Self::StringNumericConversionLimitExceeded => {
                "JavaScript string numeric conversion limit exceeded"
            }
            Self::StringConcatenationLengthLimitExceeded => {
                "JavaScript string concatenation length limit exceeded"
            }
            Self::StringConcatenationBudgetExceeded => {
                "JavaScript string concatenation append budget exceeded"
            }
            Self::RuntimeStringResultRequiresScratch => {
                "runtime JavaScript string result requires evaluation scratch"
            }
            Self::UnsupportedPropertyReceiver => {
                "JavaScript .length receiver is outside the primitive String subset"
            }
        })
    }
}

impl std::error::Error for EvalError {}

impl Script {
    /// Evaluate the Script and return its Number result without coercing it.
    ///
    /// Use [`Script::evaluate_primitive_with_scratch`] to accept any supported value.
    ///
    /// # Errors
    ///
    /// Returns execution errors, or [`EvalError::NonNumericResult`] if the final
    /// value is not a Number.
    pub fn evaluate(&self) -> Result<f64, EvalError> {
        match self.evaluate_value() {
            Ok(Value::Number(value)) => Ok(value),
            Ok(_) | Err(EvalError::NonScalarResult) => Err(EvalError::NonNumericResult),
            Err(error) => Err(error),
        }
    }

    /// Evaluate the Script and return a scalar result without coercing it.
    ///
    /// # Errors
    ///
    /// Returns execution errors, or [`EvalError::NonScalarResult`] if the final
    /// value is a String (literal, `typeof` spelling, or runtime result).
    pub fn evaluate_value(&self) -> Result<Value, EvalError> {
        match self.run(&mut EvalScratch::new())? {
            Operand::Scalar(value) => Ok(value),
            Operand::String(_) => Err(EvalError::NonScalarResult),
            Operand::Function(_) => Err(EvalError::InvalidScript),
        }
    }

    /// Evaluate the Script and return the final expression's tagged primitive value.
    ///
    /// For scalar operands, arithmetic and relational operators leave Numbers
    /// unchanged and convert `true` to `1`, `false` and `null` to `+0`, and
    /// `undefined` to NaN. Arithmetic returns a Number. Relational operators
    /// return a Boolean: two Strings are ordered lexicographically by UTF-16
    /// code units, with a proper prefix sorting before the longer String.
    /// Otherwise, both operands are converted to Numbers, and the relation is
    /// always `false` if either converted Number is NaN.
    /// Unary `+`, `-`, and `~` and binary `-`, `*`, `/`, `%`, `**`, `&`, `^`,
    /// `|`, `<<`, `>>`, and `>>>` (including their eager compound forms), mixed
    /// relational comparisons, and loose equality between a String and a Number
    /// or Boolean convert Strings to Numbers using ECMAScript's whitespace,
    /// signed decimal and `Infinity` forms, and unsigned `0x`, `0o`, and `0b`
    /// integer forms. Empty or whitespace-only Strings become `+0`; invalid
    /// numeric text becomes NaN.
    /// Unary `+` returns the Number and unary `-` negates it, including signed
    /// zero. Unary `~` applies bitwise NOT after converting the rounded Number
    /// to a signed 32-bit integer. Binary operators that coerce Strings convert
    /// the left operand and then the right, once each, after evaluating both
    /// operand expressions. Eager compounds use the value loaded before the RHS
    /// runs and store the result only after the operation succeeds.
    /// `**` and `**=` handle the ECMAScript Number special cases explicitly;
    /// ordinary finite powers use a platform-dependent approximation.
    /// Bitwise operators truncate and wrap the coerced Number to 32 bits
    /// (nonfinite values and signed zeros become zero). Shifts use the
    /// low five bits of the converted count. Bitwise results are Numbers
    /// representing signed 32-bit integers, except for zero-fill right shift
    /// (`>>>`), which returns an unsigned 32-bit result.
    /// Unary `void` evaluates its operand once, then returns [`Value::Undefined`]
    /// without coercing the operand's value.
    /// Equality and inequality return Booleans. Strict equality (`===`) never
    /// coerces. Loose equality (`==`) uses strict equality for matching tags,
    /// equates `null` with `undefined`, and compares mixed pairs of Numbers,
    /// Booleans, and Strings numerically after conversion. Strings compare code
    /// units with other Strings; mixed strict equality and String/nullish loose
    /// equality are false without conversion. `!==` and `!=` negate their
    /// corresponding equality results. Binary `+` and `+=` concatenate if
    /// either operand is a String, converting a scalar operand to its ECMAScript
    /// String spelling. This appends UTF-16 units left to right without normalization.
    /// If neither operand is a String, both are converted to Numbers and added.
    ///
    /// Logical negation (`!`), short-circuit operators (`&&`, `||`), and the
    /// conditional operator (`?:`) share truthiness: `false`, `null`, `undefined`,
    /// either signed zero, NaN, and the empty String are falsy; all other
    /// supported values are truthy. Every String is non-nullish.
    /// `!` returns a Boolean. `&&` returns a falsy left operand, `||` returns a
    /// truthy left operand, and `??` returns a left operand that is neither `null`
    /// nor `undefined`. Otherwise they evaluate and return the right operand.
    /// Logical assignments (`&&=`, `||=`, `??=`) use the same tests to skip
    /// both RHS evaluation and the store on an initialized `let` target.
    /// They leave no result, and their RHS is still checked during parsing.
    /// Consuming `++` and `--` updates (alone or followed by a comma suffix
    /// in a statement or `for` clause) load one initialized `let`, convert
    /// its value (including Strings) to Number once, and store the result of
    /// adding or subtracting `1`. Neither form leaves a result for the update itself.
    /// An update used as an expression operand, such as in `x++ + 1`,
    /// performs the same write and yields a Number: the new Number for
    /// prefix, or the old coerced Number (including signed zero or NaN)
    /// for postfix.
    /// A conditional expression evaluates its test and only the selected arm,
    /// preserving the selected value and type. A comma sequence evaluates its operands
    /// from left to right, discarding each but the last without coercion. A simple
    /// assignment chain in an operand stores the RHS value before the sequence
    /// advances or returns that value as its result.
    /// A standalone expression statement also discards its result without coercion.
    /// Only the required final root expression supplies the Script's result.
    /// Statement tests check the final operand's truthiness after any stores.
    /// An `if` statement executes only the selected branch, leaving no operand
    /// stack result.
    /// A `while` statement evaluates its test before each iteration and runs its
    /// body only when the test is truthy. A `for` statement first runs its
    /// optional initializer and then tests (an omitted test is truthy). Each
    /// normal iteration ends with the optional update, then another test.
    /// A `do { ... } while (test);` statement runs its body before its first
    /// test and repeats while the test is truthy. The test is outside the
    /// body scope. `break;` exits the innermost loop without running its
    /// update or test. `continue;` skips the rest of that loop's body and
    /// runs its next test (after the update in a `for`, if present).
    /// None of these statements leaves a result.
    ///
    /// This runs no host callbacks and does not read source text. Initializations
    /// and assignments use fresh immutable `const` and mutable `let` slots for
    /// each call. A block declaration initializes its slot each time it executes,
    /// including on repeated loop iterations. Independent of parse limits, a
    /// script can have at most 4,096 prepared instructions, including jumps and
    /// skipped paths. Each call can dispatch at most 16,384 instructions; each
    /// execution of an instruction counts once, including tests, jumps, and
    /// repeated loop instructions.
    /// String equality and ordering share a budget of 65,536 code-unit pairs
    /// per call. Equality charges the full length for equal-length Strings and
    /// nothing otherwise. Ordering charges the shorter length. Both charge
    /// before comparison, even for aliases or an early mismatch. Mixed and
    /// skipped comparisons cost no pairs.
    /// String-to-Number conversion has a separate 262,144-credit budget per
    /// call, shared by unary `+`, `-`, and `~`, binary `-`, `*`, `/`, `%`, `**`,
    /// `&`, `^`, `|`, `<<`, `>>`, and `>>>` (including their eager compound
    /// forms), name `++` and `--` updates, and comparisons that coerce Strings.
    /// After dispatch, operand arity checks, and validation of the operand's
    /// String range, each conversion charges `1 +` the original UTF-16 length.
    /// Trimmed-empty text, exact optionally signed `Infinity`, and recognized
    /// unsigned radix prefixes only pay this base charge.
    /// All other candidates additionally pay
    /// `256 + 3 *` the trimmed magnitude length (excluding a leading sign),
    /// before validating decimal syntax. Skipped instructions, scalar operands,
    /// and noncoercing comparisons cost no conversion credits.
    /// Concatenations and template appends may produce at most 4,094 UTF-16
    /// units each and append at most 32,768 units in total per call, including
    /// scalar text, repeated template prefixes and discarded results. This
    /// append budget is independent of the other budgets. A reached `Add` or
    /// template append can format at most one Number, using bounded binary64
    /// arithmetic and at most 25 UTF-16 units of stack scratch without a
    /// separate formatting charge. Skipped and empty appends consume no units
    /// and do not allocate runtime String storage.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::LimitExceeded`] for an oversized prepared script,
    /// [`EvalError::DispatchLimitExceeded`] if another dispatch would exceed
    /// its budget, [`EvalError::InvalidScript`] for an invalid instruction
    /// sequence, or [`EvalError::AllocationFailed`] if storage cannot be allocated.
    /// Returns [`EvalError::StringComparisonLimitExceeded`] if a comparison would
    /// exceed its pair budget. Returns
    /// [`EvalError::StringNumericConversionLimitExceeded`] if an operator's
    /// String-to-Number conversion would exceed its credit budget. A reached
    /// concatenation or template append can return [`EvalError::StringConcatenationLengthLimitExceeded`]
    /// or [`EvalError::StringConcatenationBudgetExceeded`]. After successful
    /// execution this method returns [`EvalError::RuntimeStringResultRequiresScratch`]
    /// for a final runtime String (even an empty one). Use
    /// [`Script::evaluate_primitive_with_scratch`] to borrow such a result.
    pub fn evaluate_primitive(&self) -> Result<PrimitiveValue<'_>, EvalError> {
        match self.run(&mut EvalScratch::new())? {
            Operand::Scalar(value) => Ok(PrimitiveValue::Scalar(value)),
            Operand::String(StringOperand::Literal(range)) => range
                .get(self.string_units())
                .map(PrimitiveValue::String)
                .ok_or(EvalError::InvalidScript),
            Operand::String(StringOperand::TypeName(name)) => {
                Ok(PrimitiveValue::String(name.units()))
            }
            Operand::String(StringOperand::Runtime(_)) => {
                Err(EvalError::RuntimeStringResultRequiresScratch)
            }
            Operand::Function(_)
            | Operand::String(
                StringOperand::Empty
                | StringOperand::Realm(_)
                | StringOperand::ArtifactLiteral { .. },
            ) => Err(EvalError::InvalidScript),
        }
    }

    /// Evaluate the Script, accepting literal, static, and runtime String results.
    ///
    /// Even for static `typeof` spellings, the signature ties the result to
    /// the Script and scratch for its lifetime. Each
    /// call resets scratch's logical length while retaining its capacity; it
    /// does not zeroize the allocation. The execution semantics and errors are
    /// those of [`Script::evaluate_primitive`], except that runtime String
    /// results are returned rather than producing a result-type error.
    ///
    /// ```compile_fail,E0499
    /// use swayze_js::{EvalScratch, parse_script};
    /// let script = parse_script("'a' + 'b'").unwrap();
    /// let mut scratch = EvalScratch::new();
    /// let first = script.evaluate_primitive_with_scratch(&mut scratch).unwrap();
    /// let second = script.evaluate_primitive_with_scratch(&mut scratch).unwrap();
    /// assert_eq!(first, second); // `first` still borrows `scratch`
    /// ```
    pub fn evaluate_primitive_with_scratch<'a>(
        &'a self,
        scratch: &'a mut EvalScratch,
    ) -> Result<PrimitiveValue<'a>, EvalError> {
        match self.run(scratch)? {
            Operand::Scalar(value) => Ok(PrimitiveValue::Scalar(value)),
            Operand::String(StringOperand::Empty | StringOperand::Realm(_)) => {
                Err(EvalError::InvalidScript)
            }
            Operand::Function(_) => Err(EvalError::InvalidScript),
            Operand::String(StringOperand::ArtifactLiteral { .. }) => Err(EvalError::InvalidScript),
            Operand::String(range) => range
                .get(scratch.pools(self.string_units()))
                .map(PrimitiveValue::String)
                .ok_or(EvalError::InvalidScript),
        }
    }

    fn run(&self, scratch: &mut EvalScratch) -> Result<Operand, EvalError> {
        self.0
            .run(ExitStack::Value, scratch)?
            .ok_or(EvalError::InvalidScript)
    }
}

impl CompletionScript {
    /// Execute the Script to normal completion without returning a result value.
    ///
    /// A final expression, if present, is evaluated normally and its value is
    /// discarded without coercion. Execution uses the same fresh binding slots,
    /// scratch reset, and budgets as [`Script::evaluate_primitive_with_scratch`].
    ///
    /// # Errors
    ///
    /// Returns the same execution errors as the value API, including errors in
    /// reached statements and in the discarded expression. There are no
    /// result-type errors.
    pub fn execute_with_scratch(&self, scratch: &mut EvalScratch) -> Result<(), EvalError> {
        self.program.run(self.exit, scratch).map(|_| ())
    }
}

impl PreparedScript {
    fn run(
        &self,
        exit: ExitStack,
        scratch: &mut EvalScratch,
    ) -> Result<Option<Operand>, EvalError> {
        scratch.reset();
        let result = evaluate(
            &self.instructions,
            self.max_stack_depth,
            self.binding_slots,
            &self.string_units,
            exit,
            scratch,
        );
        if result.is_err() {
            scratch.reset();
        }
        result
    }
}

fn validate_layout(
    ops: &[Instruction],
    max_operands: usize,
    exit: ExitStack,
) -> Result<(), EvalError> {
    if ops.len() > MAX_INSTRUCTIONS {
        return Err(EvalError::LimitExceeded);
    }
    // Back edges require an empty stack. A path to the peak needs that many
    // distinct pushes and at least peak - exit_arity distinct pops to finish
    // or reach a back edge.
    let exit_arity = exit.arity();
    if max_operands < exit_arity || max_operands > (ops.len() + exit_arity) / 2 {
        return Err(EvalError::InvalidScript);
    }
    Ok(())
}

fn new_stack(max_operands: usize) -> Result<Vec<Operand>, EvalError> {
    let mut stack = Vec::new();
    stack
        .try_reserve_exact(max_operands)
        .map_err(|_| EvalError::AllocationFailed)?;
    Ok(stack)
}

fn evaluate(
    ops: &[Instruction],
    max_operands: usize,
    binding_slots: usize,
    string_units: &[u16],
    exit: ExitStack,
    scratch: &mut EvalScratch,
) -> Result<Option<Operand>, EvalError> {
    validate_layout(ops, max_operands, exit)?;
    // Each legacy slot has a producer and initializer, independent of the
    // final result. This check is not an assumption about global metadata.
    if binding_slots > ops.len().saturating_sub(exit.arity()) / 2 {
        return Err(EvalError::InvalidScript);
    }
    let stack = new_stack(max_operands)?;
    let mut bindings = LegacyBindings::new(binding_slots)?;
    dispatch(
        Frame::root(ops, max_operands, exit),
        stack,
        string_units,
        realm::CallableCode::default(),
        scratch,
        &mut bindings,
    )
}

const MAX_GUEST_FRAMES: usize = 32;
const MAX_LIVE_SLOTS: usize = 4096;
const UNDEFINED: Operand = Operand::Scalar(Value::Undefined);

// The active frame and suspended caller frames borrow immutable code only;
// the VM owns every formal/temporary slot. No Rust recursion or code copies.
struct Frame<'code> {
    ops: &'code [Instruction],
    max_operands: usize,
    exit: ExitStack,
    body: Option<realm::FunctionOrigin>,
    ip: usize,
    base: usize, // formals start here; this is also the caller's result slot
    temps: usize,
}

impl<'code> Frame<'code> {
    fn root(ops: &'code [Instruction], max_operands: usize, exit: ExitStack) -> Self {
        Self {
            ops,
            max_operands,
            exit,
            body: None,
            ip: 0,
            base: 0,
            temps: 0,
        }
    }
}

fn validate_body(
    ops: &[Instruction],
    max_operands: usize,
    exit: ExitStack,
) -> Result<(), EvalError> {
    if ops.len() > MAX_INSTRUCTIONS {
        return Err(EvalError::LimitExceeded);
    }
    // Unlike Legacy, one call can consume many operands at once. Reached
    // operations enforce the actual frame-relative bounds and exit arities.
    if max_operands < exit.arity() || max_operands > ops.len() || max_operands > MAX_LIVE_SLOTS {
        return Err(EvalError::InvalidScript);
    }
    Ok(())
}

struct Stack<'a> {
    slots: &'a mut Vec<Operand>,
    base: usize,
    temps: usize,
}

impl Stack<'_> {
    #[cfg(test)]
    fn root(slots: &mut Vec<Operand>) -> Stack<'_> {
        Stack {
            slots,
            base: 0,
            temps: 0,
        }
    }
    fn as_slice(&self) -> &[Operand] {
        &self.slots[self.temps..]
    }
    fn as_mut_slice(&mut self) -> &mut [Operand] {
        &mut self.slots[self.temps..]
    }
    fn len(&self) -> usize {
        self.slots.len() - self.temps
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn last(&self) -> Option<&Operand> {
        self.as_slice().last()
    }
    fn last_mut(&mut self) -> Option<&mut Operand> {
        self.as_mut_slice().last_mut()
    }
    fn pop(&mut self) -> Option<Operand> {
        if self.is_empty() {
            None
        } else {
            self.slots.pop()
        }
    }
    fn formal(&mut self, slot: u8) -> Result<&mut Operand, EvalError> {
        self.slots[self.base..self.temps]
            .get_mut(usize::from(slot))
            .ok_or(EvalError::InvalidScript)
    }
}

fn dispatch<'code, B: Bindings>(
    mut frame: Frame<'code>,
    mut slots: Vec<Operand>,
    string_units: &[u16],
    callable: realm::CallableCode<'code>,
    scratch: &mut EvalScratch,
    bindings: &mut B,
) -> Result<Option<Operand>, B::Error> {
    // `ops.len()` denotes exit, which is not a dispatch and needs no fuel.
    let mut remaining = MAX_DISPATCHES;
    let mut string_pairs_left = MAX_STRING_COMPARISON_PAIRS;
    let mut string_conversion_credits_left = string_number::MAX_CREDITS;
    let mut callers = Vec::<Frame<'_>>::new();
    loop {
        let Frame {
            ops,
            max_operands,
            body,
            base,
            temps,
            exit,
            ref mut ip,
        } = frame;
        let mut stack = Stack {
            slots: &mut slots,
            base,
            temps,
        };
        if *ip == ops.len() {
            // Frame frontier is not dispatched; ordinary body fallthrough
            // is undefined, independent of preceding expression statements.
            if B::CALLS && body.is_some() {
                if !stack.is_empty() {
                    return Err(EvalError::InvalidScript.into());
                }
                frame = resume(&mut callers, &mut slots, base, UNDEFINED, bindings)?;
                continue;
            }
            return match (exit, stack.as_slice()) {
                (ExitStack::Empty, []) => Ok(None),
                (ExitStack::Value, [value]) => Ok(Some(*value)),
                _ => Err(EvalError::InvalidScript.into()),
            };
        }
        remaining = remaining
            .checked_sub(1)
            .ok_or(EvalError::DispatchLimitExceeded)?;
        let op = *ops.get(*ip).ok_or(EvalError::InvalidScript)?;
        *ip += 1;
        let source = bindings.transient(scratch, string_units, callable.artifacts);
        match op {
            Instruction::Call { start, end, argc } if B::CALLS => {
                let argc = usize::from(argc);
                if argc > usize::from(realm::MAX_PARAMETERS) || start >= end {
                    return Err(EvalError::InvalidScript.into());
                }
                let pos = temps
                    + stack
                        .len()
                        .checked_sub(argc + 1)
                        .ok_or(EvalError::InvalidScript)?;
                let Operand::Function(id) = slots[pos] else {
                    return Err(bindings.non_callable(crate::Span {
                        start: start as usize,
                        end: end as usize,
                    }));
                };
                // Only after GetValue and all argument expressions succeed.
                let (origin, target) = callable.resolve(id)?;
                if callers.len() == MAX_GUEST_FRAMES {
                    return Err(EvalError::CallDepthLimitExceeded.into());
                }
                let formals = usize::from(target.formals);
                let needed = pos + formals + target.max_stack_depth;
                if needed > MAX_LIVE_SLOTS {
                    return Err(EvalError::LiveSlotLimitExceeded.into());
                }
                bindings.charge_call(1 + formals)?;
                if callers.capacity() == 0 {
                    callers
                        .try_reserve_exact(MAX_GUEST_FRAMES)
                        .map_err(|_| EvalError::AllocationFailed)?;
                }
                if needed > slots.capacity() {
                    // Geometric startup; at most 4096 requested slots, and
                    // <8192 old initialized slots moved over the execution.
                    let capacity = needed.next_power_of_two();
                    slots
                        .try_reserve_exact(capacity - slots.len())
                        .map_err(|_| EvalError::AllocationFailed)?;
                }
                let supplied = argc.min(formals);
                slots.copy_within(pos + 1..pos + 1 + supplied, pos);
                slots.truncate(pos + supplied);
                slots.resize(pos + formals, UNDEFINED);
                let callee = Frame {
                    ops: &target.instructions,
                    max_operands: target.max_stack_depth,
                    exit: ExitStack::Empty,
                    body: Some(origin),
                    ip: 0,
                    base: pos,
                    temps: pos + formals,
                };
                callers.push(std::mem::replace(&mut frame, callee));
                bindings.select_body(Some(origin));
            }
            Instruction::Return { value } if B::CALLS => {
                if body.is_none() || stack.len() != usize::from(value) {
                    return Err(EvalError::InvalidScript.into());
                }
                let value = stack.pop().unwrap_or(UNDEFINED);
                frame = resume(&mut callers, &mut slots, base, value, bindings)?;
            }
            Instruction::LoadFormal(slot) if B::CALLS => {
                let value = *stack.formal(slot)?;
                push_operand(&mut stack, max_operands, value)?;
            }
            Instruction::StoreFormal { slot, keep } if B::CALLS => {
                let value = store_operand(&mut stack, keep)?;
                *stack.formal(slot)? = value;
            }
            Instruction::UpdateFormal { slot, op, result } if B::CALLS => {
                let old = stack.formal(slot)?.to_number(
                    source.with_retained(bindings.retained_units()),
                    &mut string_conversion_credits_left,
                )?;
                let new = update_number(op, old);
                update_result(&mut stack, max_operands, result, old, new)?;
                *stack.formal(slot)? = Operand::Scalar(Value::Number(new));
            }
            Instruction::Call { .. }
            | Instruction::Return { .. }
            | Instruction::LoadFormal(_)
            | Instruction::StoreFormal { .. }
            | Instruction::UpdateFormal { .. } => return Err(EvalError::InvalidScript.into()),
            Instruction::Number(value) => {
                push_operand(
                    &mut stack,
                    max_operands,
                    Operand::Scalar(Value::Number(value)),
                )?;
            }
            Instruction::String(range) => {
                let artifact = body.map(|body| body.artifact);
                bindings
                    .external_strings(string_units)
                    .literal(artifact, range)
                    .ok_or(EvalError::InvalidScript)?;
                push_operand(
                    &mut stack,
                    max_operands,
                    Operand::String(StringOperand::literal(artifact, range)),
                )?;
            }
            Instruction::Boolean(value) => {
                push_operand(
                    &mut stack,
                    max_operands,
                    Operand::Scalar(Value::Boolean(value)),
                )?;
            }
            Instruction::Null => {
                push_operand(&mut stack, max_operands, Operand::Scalar(Value::Null))?
            }
            Instruction::Undefined => {
                push_operand(&mut stack, max_operands, Operand::Scalar(Value::Undefined))?
            }
            Instruction::LoadBinding(slot) => {
                push_operand(&mut stack, max_operands, bindings.load(slot)?)?;
            }
            Instruction::InitBinding { slot, kind } => {
                bindings.init(slot, kind, *ip - 1, &mut stack, source)?;
            }
            Instruction::StoreBinding(slot) | Instruction::StoreBindingKeep(slot) => {
                bindings.store(
                    slot,
                    matches!(op, Instruction::StoreBindingKeep(_)),
                    &mut stack,
                    source,
                )?;
            }
            Instruction::UpdateBinding { slot, op, result } => {
                bindings.update(
                    slot,
                    op,
                    result,
                    &mut stack,
                    max_operands,
                    UpdateNumeric {
                        source,
                        credits: &mut string_conversion_credits_left,
                    },
                )?;
            }
            Instruction::Discard => {
                stack.pop().ok_or(EvalError::InvalidScript)?;
            }
            Instruction::UpdateNumber(op) => apply_binary(&mut stack, |lhs, rhs| {
                if !matches!(rhs, Operand::Scalar(Value::Number(1.0))) {
                    return Err(EvalError::InvalidScript);
                }
                let old = lhs.to_number(
                    bindings.string_pools(scratch, string_units),
                    &mut string_conversion_credits_left,
                )?;
                Ok(Value::Number(update_number(op, old)))
            })?,
            Instruction::Add => apply_binary_operand(&mut stack, |lhs, rhs| {
                add(lhs, rhs, bindings.external_strings(string_units), scratch)
            })?,
            Instruction::TemplateAppend(following) => {
                append_template(
                    &mut stack,
                    bindings.external_strings(string_units),
                    scratch,
                    following,
                )?;
            }
            Instruction::Subtract => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                |lhs, rhs| lhs - rhs,
            )?,
            Instruction::Multiply => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                |lhs, rhs| lhs * rhs,
            )?,
            Instruction::Divide => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                |lhs, rhs| lhs / rhs,
            )?,
            // Floating-point `%` uses a quotient truncated toward zero, as in JS.
            Instruction::Remainder => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                |lhs, rhs| lhs % rhs,
            )?,
            Instruction::Exponentiate => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                exponentiate,
            )?,
            Instruction::BitwiseAnd => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                signed_bitwise(|lhs, rhs| lhs & rhs),
            )?,
            Instruction::BitwiseXor => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                signed_bitwise(|lhs, rhs| lhs ^ rhs),
            )?,
            Instruction::BitwiseOr => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                signed_bitwise(|lhs, rhs| lhs | rhs),
            )?,
            // Rust's wrapping shifts use only the low five count bits for i32/u32.
            Instruction::ShiftLeft => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                |lhs, rhs| f64::from(number_to_int32(lhs).wrapping_shl(number_to_uint32(rhs))),
            )?,
            Instruction::ShiftRight => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                |lhs, rhs| f64::from(number_to_int32(lhs).wrapping_shr(number_to_uint32(rhs))),
            )?,
            Instruction::ShiftRightUnsigned => apply_numeric_binary(
                &mut stack,
                bindings.string_pools(scratch, string_units),
                &mut string_conversion_credits_left,
                |lhs, rhs| f64::from(number_to_uint32(lhs).wrapping_shr(number_to_uint32(rhs))),
            )?,
            Instruction::LooseEqual
            | Instruction::LooseNotEqual
            | Instruction::StrictEqual
            | Instruction::StrictNotEqual => {
                let strict = matches!(op, Instruction::StrictEqual | Instruction::StrictNotEqual);
                let negate = matches!(op, Instruction::LooseNotEqual | Instruction::StrictNotEqual);
                apply_binary(&mut stack, |lhs, rhs| {
                    let equal = operands_equal(
                        lhs,
                        rhs,
                        strict,
                        bindings.string_pools(scratch, string_units),
                        &mut string_pairs_left,
                        &mut string_conversion_credits_left,
                    )?;
                    Ok(Value::Boolean(equal ^ negate))
                })?;
            }
            Instruction::LessThan
            | Instruction::LessThanOrEqual
            | Instruction::GreaterThan
            | Instruction::GreaterThanOrEqual => {
                apply_binary(&mut stack, |lhs, rhs| {
                    let order = operands_order(
                        lhs,
                        rhs,
                        bindings.string_pools(scratch, string_units),
                        &mut string_pairs_left,
                        &mut string_conversion_credits_left,
                    )?;
                    let result = match order {
                        Some(Ordering::Less) => {
                            matches!(op, Instruction::LessThan | Instruction::LessThanOrEqual)
                        }
                        Some(Ordering::Equal) => matches!(
                            op,
                            Instruction::LessThanOrEqual | Instruction::GreaterThanOrEqual
                        ),
                        Some(Ordering::Greater) => matches!(
                            op,
                            Instruction::GreaterThan | Instruction::GreaterThanOrEqual
                        ),
                        None => false, // All four relations are false for NaN operands.
                    };
                    Ok(Value::Boolean(result))
                })?;
            }
            Instruction::Positive => {
                apply_unary(stack.as_mut_slice(), |value| {
                    let number = value.to_number(
                        bindings.string_pools(scratch, string_units),
                        &mut string_conversion_credits_left,
                    )?;
                    Ok(Value::Number(number))
                })?;
            }
            Instruction::Negate => {
                apply_unary(stack.as_mut_slice(), |value| {
                    let number = value.to_number(
                        bindings.string_pools(scratch, string_units),
                        &mut string_conversion_credits_left,
                    )?;
                    Ok(Value::Number(-number))
                })?;
            }
            Instruction::Not => {
                apply_unary(stack.as_mut_slice(), |value| {
                    Ok(Value::Boolean(!value.to_boolean()))
                })?;
            }
            Instruction::BitwiseNot => {
                apply_unary(stack.as_mut_slice(), |value| {
                    let number = value.to_number(
                        bindings.string_pools(scratch, string_units),
                        &mut string_conversion_credits_left,
                    )?;
                    Ok(Value::Number(f64::from(!number_to_int32(number))))
                })?;
            }
            Instruction::Void => apply_unary(stack.as_mut_slice(), |_| Ok(Value::Undefined))?,
            Instruction::Typeof => {
                let value = stack.last_mut().ok_or(EvalError::InvalidScript)?;
                let name = match *value {
                    Operand::Scalar(Value::Number(_)) => TypeName::Number,
                    Operand::Scalar(Value::Boolean(_)) => TypeName::Boolean,
                    Operand::Scalar(Value::Null) => TypeName::Object,
                    Operand::Scalar(Value::Undefined) => TypeName::Undefined,
                    Operand::String(_) => TypeName::String,
                    Operand::Function(_) => TypeName::Function,
                };
                *value = Operand::String(StringOperand::TypeName(name));
            }
            Instruction::StringLength => {
                apply_unary(stack.as_mut_slice(), |value| {
                    string_length(value, bindings.string_pools(scratch, string_units))
                })?;
            }
            Instruction::JumpIfFalse(target) | Instruction::JumpIfFalseStatement(target) => {
                let target = check_forward_target(*ip, target, ops.len())?;
                if matches!(op, Instruction::JumpIfFalseStatement(_)) && stack.len() != 1 {
                    return Err(EvalError::InvalidScript.into());
                }
                let condition = stack.pop().ok_or(EvalError::InvalidScript)?;
                if !condition.to_boolean() {
                    *ip = target;
                }
            }
            Instruction::ShortCircuit { target, condition } => {
                let target = check_forward_target(*ip, target, ops.len())?;
                let lhs = *stack.last().ok_or(EvalError::InvalidScript)?;
                if condition.skips_rhs(lhs) {
                    *ip = target;
                } else {
                    stack.pop();
                }
            }
            Instruction::ShortCircuitStatement { target, condition } => {
                let target = check_forward_target(*ip, target, ops.len())?;
                if stack.len() != 1 {
                    return Err(EvalError::InvalidScript.into());
                }
                let lhs = stack.pop().ok_or(EvalError::InvalidScript)?;
                if condition.skips_rhs(lhs) {
                    *ip = target;
                }
            }
            Instruction::Jump(target) => *ip = check_forward_target(*ip, target, ops.len())?,
            Instruction::JumpStatement(target) => {
                if !stack.is_empty() {
                    return Err(EvalError::InvalidScript.into());
                }
                *ip = check_forward_target(*ip, target, ops.len())?;
            }
            Instruction::LoopBack(target) => {
                // `ip` already points past this instruction. Reject self and
                // forward edges, and require statement depth on entry.
                if target >= *ip - 1 || !stack.is_empty() {
                    return Err(EvalError::InvalidScript.into());
                }
                *ip = target;
            }
        }
    }
}

fn resume<'code, B: Bindings>(
    callers: &mut Vec<Frame<'code>>,
    slots: &mut Vec<Operand>,
    base: usize,
    result: Operand,
    bindings: &mut B,
) -> Result<Frame<'code>, EvalError> {
    let caller = callers.pop().ok_or(EvalError::InvalidScript)?;
    slots.truncate(base);
    let mut destination = Stack {
        slots,
        base: caller.base,
        temps: caller.temps,
    };
    push_operand(&mut destination, caller.max_operands, result)?;
    bindings.select_body(caller.body);
    Ok(caller)
}

fn string_length(value: Operand, pools: StringPools<'_>) -> Result<Value, EvalError> {
    let Operand::String(string) = value else {
        return Err(EvalError::UnsupportedPropertyReceiver);
    };
    let units = string.get(pools).ok_or(EvalError::InvalidScript)?;
    // Use the checked slice length. Range endpoints are u16 and the static
    // spellings are shorter still, so the Number is exact.
    Ok(Value::Number(units.len() as f64))
}

fn operands_equal(
    lhs: Operand,
    rhs: Operand,
    strict: bool,
    strings: StringPools<'_>,
    string_pairs_left: &mut usize,
    credits_left: &mut usize,
) -> Result<bool, EvalError> {
    match (lhs, rhs) {
        (Operand::Scalar(lhs), Operand::Scalar(rhs)) => Ok(if strict {
            lhs.strictly_equals(rhs)
        } else {
            lhs.loosely_equals(rhs)
        }),
        (Operand::Function(lhs), Operand::Function(rhs)) => Ok(lhs == rhs),
        (Operand::Function(_), Operand::Scalar(Value::Null | Value::Undefined))
        | (Operand::Scalar(Value::Null | Value::Undefined), Operand::Function(_)) => Ok(false),
        (Operand::Function(_), _) | (_, Operand::Function(_)) if strict => Ok(false),
        (Operand::Function(_), _) | (_, Operand::Function(_)) => {
            Err(EvalError::UnsupportedCallableConversion)
        }
        (Operand::String(lhs), Operand::String(rhs)) => {
            let lhs = lhs.get(strings).ok_or(EvalError::InvalidScript)?;
            let rhs = rhs.get(strings).ok_or(EvalError::InvalidScript)?;
            if lhs.len() != rhs.len() {
                return Ok(false);
            }
            charge_string_pairs(string_pairs_left, lhs.len())?;
            Ok(lhs == rhs)
        }
        (Operand::String(_), Operand::Scalar(scalar))
        | (Operand::Scalar(scalar), Operand::String(_)) => {
            if strict || matches!(scalar, Value::Null | Value::Undefined) {
                Ok(false)
            } else {
                let lhs = lhs.to_number(strings, credits_left)?;
                let rhs = rhs.to_number(strings, credits_left)?;
                Ok(lhs == rhs)
            }
        }
    }
}

fn operands_order(
    lhs: Operand,
    rhs: Operand,
    strings: StringPools<'_>,
    string_pairs_left: &mut usize,
    credits_left: &mut usize,
) -> Result<Option<Ordering>, EvalError> {
    match (lhs, rhs) {
        // IsLessThan completes ToPrimitive on both operands before numeric
        // conversion. Both operand expressions have already been evaluated.
        (Operand::Function(_), _) | (_, Operand::Function(_)) => {
            Err(EvalError::UnsupportedCallableConversion)
        }
        (Operand::String(lhs), Operand::String(rhs)) => {
            let lhs = lhs.get(strings).ok_or(EvalError::InvalidScript)?;
            let rhs = rhs.get(strings).ok_or(EvalError::InvalidScript)?;
            charge_string_pairs(string_pairs_left, lhs.len().min(rhs.len()))?;
            // Slice ordering compares u16 values, then lengths if the prefix matches.
            Ok(Some(lhs.cmp(rhs)))
        }
        _ => {
            let lhs = lhs.to_number(strings, credits_left)?;
            let rhs = rhs.to_number(strings, credits_left)?;
            Ok(lhs.partial_cmp(&rhs))
        }
    }
}

fn charge_string_pairs(remaining: &mut usize, pairs: usize) -> Result<(), EvalError> {
    *remaining = remaining
        .checked_sub(pairs)
        .ok_or(EvalError::StringComparisonLimitExceeded)?;
    Ok(())
}

fn check_forward_target(next: usize, target: usize, exit: usize) -> Result<usize, EvalError> {
    if (next..=exit).contains(&target) {
        Ok(target)
    } else {
        Err(EvalError::InvalidScript)
    }
}

fn push_operand(
    stack: &mut Stack<'_>,
    max_operands: usize,
    value: Operand,
) -> Result<(), EvalError> {
    if stack.len() >= max_operands || stack.slots.len() == stack.slots.capacity() {
        return Err(EvalError::InvalidScript);
    }
    stack.slots.push(value);
    Ok(())
}

fn apply_unary(
    stack: &mut [Operand],
    op: impl FnOnce(Operand) -> Result<Value, EvalError>,
) -> Result<(), EvalError> {
    let value = stack.last_mut().ok_or(EvalError::InvalidScript)?;
    *value = Operand::Scalar(op(*value)?);
    Ok(())
}

fn apply_binary(
    stack: &mut Stack<'_>,
    op: impl FnOnce(Operand, Operand) -> Result<Value, EvalError>,
) -> Result<(), EvalError> {
    apply_binary_operand(stack, |lhs, rhs| op(lhs, rhs).map(Operand::Scalar))
}

fn apply_binary_operand(
    stack: &mut Stack<'_>,
    op: impl FnOnce(Operand, Operand) -> Result<Operand, EvalError>,
) -> Result<(), EvalError> {
    let rhs = stack.pop().ok_or(EvalError::InvalidScript)?;
    let lhs = stack.last_mut().ok_or(EvalError::InvalidScript)?;
    *lhs = op(*lhs, rhs)?;
    Ok(())
}

fn update_number(op: Update, old: f64) -> f64 {
    match op {
        Update::Increment => old + 1.0,
        Update::Decrement => old - 1.0,
    }
}

// Neither the accumulated prefix nor the stack is changed until the
// entire substitution append succeeds. Keep this path out of dispatch.
#[inline(never)]
fn append_template(
    stack: &mut Stack<'_>,
    external: ExternalStrings<'_>,
    scratch: &mut EvalScratch,
    following: crate::parser::StringRange,
) -> Result<(), EvalError> {
    let [.., lhs, rhs] = stack.as_mut_slice() else {
        return Err(EvalError::InvalidScript);
    };
    let Operand::String(prefix) = *lhs else {
        return Err(EvalError::InvalidScript);
    };
    *lhs = Operand::String(scratch.append_template(external, prefix, *rhs, following)?);
    stack.pop();
    Ok(())
}

fn add(
    lhs: Operand,
    rhs: Operand,
    external: ExternalStrings<'_>,
    scratch: &mut EvalScratch,
) -> Result<Operand, EvalError> {
    match (lhs, rhs) {
        (Operand::Function(_), _) | (_, Operand::Function(_)) => {
            Err(EvalError::UnsupportedCallableConversion)
        }
        (Operand::Scalar(lhs), Operand::Scalar(rhs)) => Ok(Operand::Scalar(Value::Number(
            lhs.to_number() + rhs.to_number(),
        ))),
        (Operand::String(lhs), Operand::String(rhs)) => {
            scratch.concatenate(external, lhs, rhs).map(Operand::String)
        }
        (Operand::String(string), Operand::Scalar(scalar)) => scratch
            .concatenate_scalar(external, string, scalar, false)
            .map(Operand::String),
        (Operand::Scalar(scalar), Operand::String(string)) => scratch
            .concatenate_scalar(external, string, scalar, true)
            .map(Operand::String),
    }
}

fn apply_numeric_binary(
    stack: &mut Stack<'_>,
    strings: StringPools<'_>,
    credits_left: &mut usize,
    op: impl FnOnce(f64, f64) -> f64,
) -> Result<(), EvalError> {
    // Both expressions have run; check arity before coercing left then right.
    apply_binary(stack, |lhs, rhs| {
        let lhs = lhs.to_number(strings, credits_left)?;
        let rhs = rhs.to_number(strings, credits_left)?;
        Ok(Value::Number(op(lhs, rhs)))
    })
}

// ECMA-262 Number::exponentiate, with the platform's powf for the
// implementation-approximated magnitude of ordinary finite powers.
fn exponentiate(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() {
        return f64::NAN;
    }
    // Both signs of zero produce +1, even for a NaN base.
    if exponent == 0.0 {
        return 1.0;
    }
    if base.is_nan() {
        return f64::NAN;
    }

    let magnitude = base.abs();
    if exponent.is_infinite() {
        return if magnitude == 1.0 {
            f64::NAN
        } else if (magnitude > 1.0) == (exponent > 0.0) {
            f64::INFINITY
        } else {
            0.0
        };
    }

    // Test parity of the represented Number without narrowing to an integer:
    // ±1 is the remainder only for odd integers, and all Numbers at or above
    // 2^53 in magnitude are even integers.
    let negative_result = base.is_sign_negative() && (exponent % 2.0).abs() == 1.0;
    let result = if magnitude == 0.0 || magnitude.is_infinite() {
        if exponent > 0.0 {
            magnitude
        } else if magnitude == 0.0 {
            f64::INFINITY
        } else {
            0.0
        }
    } else if base < 0.0 && exponent != exponent.trunc() {
        return f64::NAN;
    } else {
        magnitude.powf(exponent)
    };
    if negative_result { -result } else { result }
}

fn signed_bitwise(op: impl FnOnce(i32, i32) -> i32) -> impl FnOnce(f64, f64) -> f64 {
    move |lhs, rhs| f64::from(op(number_to_int32(lhs), number_to_int32(rhs)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Keep the scalar instruction fixtures independent of the String pool.
    fn evaluate(ops: &[Instruction], peak: usize, slots: usize) -> Result<Value, EvalError> {
        match super::evaluate(
            ops,
            peak,
            slots,
            &[],
            ExitStack::Value,
            &mut EvalScratch::new(),
        )? {
            Some(Operand::Scalar(value)) => Ok(value),
            Some(Operand::String(_) | Operand::Function(_)) => {
                panic!("unexpected operand in scalar fixture")
            }
            None => panic!("missing result in scalar fixture"),
        }
    }

    fn evaluate_for_error(script: &Script, ops: &[Instruction]) -> Result<(), EvalError> {
        super::evaluate(
            ops,
            script.max_stack_depth(),
            script.binding_slots(),
            script.string_units(),
            ExitStack::Value,
            &mut EvalScratch::new(),
        )
        .map(|_| ())
    }

    #[test]
    fn normal_exit_checks_declared_arity_even_after_a_jump() {
        use Instruction::{Discard, Jump, JumpStatement, Number};
        let run = |ops: &[Instruction], peak, exit| {
            super::evaluate(ops, peak, 0, &[], exit, &mut EvalScratch::new())
                .map(|result| result.is_some())
        };
        assert_eq!(run(&[], 0, ExitStack::Empty), Ok(false));
        assert_eq!(run(&[], 0, ExitStack::Value), Err(EvalError::InvalidScript));
        assert_eq!(run(&[JumpStatement(1)], 0, ExitStack::Empty), Ok(false));
        let one = Number(1.0);
        assert_eq!(run(&[one, Jump(2)], 1, ExitStack::Value), Ok(true));
        assert_eq!(
            run(&[one, Jump(2)], 1, ExitStack::Empty),
            Err(EvalError::InvalidScript)
        );
        let consumed = [one, Discard, JumpStatement(3)];
        assert_eq!(run(&consumed, 1, ExitStack::Empty), Ok(false));
        assert_eq!(
            run(&consumed, 1, ExitStack::Value),
            Err(EvalError::InvalidScript)
        );
        assert_eq!(
            run(&[one, one, Jump(3)], 2, ExitStack::Value),
            Err(EvalError::InvalidScript)
        );
    }

    #[test]
    fn completion_uses_shared_instructions_and_resets_scratch() {
        use crate::{parse_script, parse_script_for_completion};
        let legacy = parse_script("let x = 'a' + 'b'; x").unwrap();
        let completion = parse_script_for_completion("#! prefix\nlet x = 'a' + 'b'; x").unwrap();
        assert_eq!(completion.program.instructions, legacy.instructions());
        assert_eq!(completion.exit, ExitStack::Value);
        let empty = parse_script_for_completion("#! no instructions\n; {} ;").unwrap();
        assert_eq!(empty.exit, ExitStack::Empty);
        assert!(empty.program.instructions.is_empty());
        assert_eq!(empty.program.max_stack_depth, 0);
        assert_eq!(empty.program.binding_slots, 0);
        let mut scratch = EvalScratch::new();
        let Operand::String(first_range) = legacy.run(&mut scratch).unwrap() else {
            panic!("expected a runtime String");
        };
        completion.execute_with_scratch(&mut scratch).unwrap();
        assert_eq!(first_range.get(scratch.pools(&[])), Some(&[97, 98][..]));
        empty.execute_with_scratch(&mut scratch).unwrap();
        assert_eq!(first_range.get(scratch.pools(&[])), None);
        let error = parse_script_for_completion("'a' + 'b'; false.length").unwrap();
        assert_eq!(
            error.execute_with_scratch(&mut scratch),
            Err(EvalError::UnsupportedPropertyReceiver)
        );
        assert_eq!(first_range.get(scratch.pools(&[])), None);
    }

    const INIT_CONST: Instruction = Instruction::InitBinding {
        slot: 0,
        kind: BindingKind::Const,
    };
    const INIT_LET: Instruction = Instruction::InitBinding {
        slot: 0,
        kind: BindingKind::Let,
    };

    #[test]
    fn typeof_checks_its_operand_only_when_reached() {
        use Instruction::{Discard, JumpStatement, Number, Typeof};
        assert_eq!(
            evaluate(&[Typeof, Number(1.0)], 1, 0),
            Err(EvalError::InvalidScript)
        );
        assert_eq!(
            evaluate(&[Number(1.0), Discard, Typeof, Number(2.0)], 1, 0),
            Err(EvalError::InvalidScript)
        );
        assert_eq!(
            evaluate(&[JumpStatement(2), Typeof, Number(42.0)], 1, 0),
            Ok(Value::Number(42.0))
        );
    }

    #[test]
    fn template_append_is_string_only_and_checks_arity_only_when_reached() {
        let script = crate::parse_script("`${1}`").unwrap();
        let [prefix, rhs, append @ Instruction::TemplateAppend(_)] = *script.instructions() else {
            panic!("expected exactly one template append");
        };
        for ops in [&[append][..], &[prefix, append], &[rhs, prefix, append]] {
            assert_eq!(
                evaluate_for_error(&script, ops),
                Err(EvalError::InvalidScript)
            );
        }
        assert_eq!(
            evaluate(&[Instruction::JumpStatement(2), append, rhs], 1, 0),
            Ok(Value::Number(1.0))
        );
    }

    #[test]
    fn compound_add_failure_stops_before_the_store() {
        for (rhs, expected) in [
            ("", EvalError::InvalidScript),
            ("b", EvalError::StringConcatenationLengthLimitExceeded),
        ] {
            let lhs = "a".repeat(4094);
            let script = crate::parse_script(&format!("let x = '{lhs}'; x += '{rhs}'; x")).unwrap();
            let mut ops = script.instructions().to_vec();
            let [
                ..,
                Instruction::Add,
                store @ Instruction::StoreBinding(0),
                Instruction::LoadBinding(0),
            ] = ops.as_mut_slice()
            else {
                panic!("compound store must follow Add");
            };
            // Any attempt to dispatch the invalid store returns InvalidScript.
            // It is only reached when the Add succeeds.
            *store = Instruction::StoreBinding(usize::MAX);
            assert_eq!(evaluate_for_error(&script, &ops), Err(expected));
        }
    }

    #[test]
    fn mixed_add_formats_raw_binary64_values() {
        use Instruction::{Add, Number, String};
        let script = crate::parse_script(r"'\uD800'").unwrap();
        let &[literal @ String(_)] = script.instructions() else {
            panic!("expected a String literal");
        };
        for (bits, text) in [
            (0x8000_0000_0000_0000, "0"),
            (0x7ff0_0000_0000_0001, "NaN"),
            (0xffff_ffff_ffff_ffff, "NaN"),
            (0x0000_0000_0000_0001, "5e-324"),
            (0x7fef_ffff_ffff_ffff, "1.7976931348623157e+308"),
            (0x4310_0000_0000_0001, "1125899906842624.2"),
            (0x4310_0000_0000_0003, "1125899906842624.8"),
            (0x4490_17f7_df96_be17, "1.8999999999999998e+22"),
            (0x4490_17f7_df96_be18, "1.9e+22"),
            (0x3eb0_c6f7_a0b5_ed8d, "0.000001"),
            (0xbeb0_c6f7_a0b5_ed8e, "-0.0000010000000000000002"),
            (0x444b_1ae4_d6e2_ef4f, "999999999999999900000"),
            (0x444b_1ae4_d6e2_ef50, "1e+21"),
        ] {
            let number = Number(f64::from_bits(bits));
            for (ops, scalar_first) in [
                ([literal, number, Add], false),
                ([number, literal, Add], true),
            ] {
                let mut scratch = EvalScratch::new();
                let literals = script.string_units();
                let Some(Operand::String(result)) =
                    super::evaluate(&ops, 2, 0, literals, ExitStack::Value, &mut scratch).unwrap()
                else {
                    panic!("mixed Add must return a String");
                };
                let expected: Vec<_> = if scalar_first {
                    text.encode_utf16().chain([0xD800]).collect()
                } else {
                    [0xD800].into_iter().chain(text.encode_utf16()).collect()
                };
                assert_eq!(
                    result.get(scratch.pools(literals)),
                    Some(expected.as_slice()),
                    "{bits:016x}"
                );
            }
        }
    }

    #[test]
    fn mixed_compound_add_failure_stops_before_either_store_form() {
        let wide = "x".repeat(4094);
        let full = format!(
            "const s = '{wide}'; let i = 0; while (i++ < 8) {{ s + ''; }} '1234567890123456' + ''; "
        );
        for (prefix, lhs, expected) in [
            ("", "'a'".to_owned(), EvalError::InvalidScript),
            (
                "",
                format!("'{wide}'"),
                EvalError::StringConcatenationLengthLimitExceeded,
            ),
            (
                &full,
                "'a'".to_owned(),
                EvalError::StringConcatenationBudgetExceeded,
            ),
        ] {
            for tail in ["x += 1; x", "(x += 1)"] {
                let script =
                    crate::parse_script(&format!("{prefix}let x = {lhs}; {tail}")).unwrap();
                let mut ops = script.instructions().to_vec();
                let add = ops.iter().rposition(|op| *op == Instruction::Add).unwrap();
                let store = &mut ops[add + 1];
                *store = match *store {
                    Instruction::StoreBinding(_) => Instruction::StoreBinding(usize::MAX),
                    Instruction::StoreBindingKeep(_) => Instruction::StoreBindingKeep(usize::MAX),
                    _ => panic!("compound store must follow Add"),
                };
                // Only a successful Add reaches the deliberately invalid store.
                assert_eq!(evaluate_for_error(&script, &ops), Err(expected), "{tail}");
            }
        }
    }

    #[test]
    fn string_literal_dispatch_checks_range_even_if_empty_or_discarded() {
        use Instruction::{Discard, JumpStatement, Number};
        let script = crate::parse_script("'a', ''").unwrap();
        let [nonempty, Discard, empty] = script.instructions() else {
            panic!("expected two literal instructions and a discard");
        };
        for &literal in [nonempty, empty] {
            assert_eq!(
                evaluate(&[literal, Discard, Number(1.0)], 1, 0),
                Err(EvalError::InvalidScript),
            );
            assert_eq!(
                evaluate(&[JumpStatement(3), literal, Discard, Number(1.0)], 1, 0),
                Ok(Value::Number(1.0)),
            );
        }
        assert_eq!(script.evaluate_primitive(), Ok(PrimitiveValue::String(&[])));
    }

    #[test]
    fn string_length_dispatch_checks_arity_and_only_reached_instructions() {
        use Instruction::{Discard, JumpStatement, Number, StringLength};
        assert_eq!(
            evaluate(&[StringLength], 1, 0),
            Err(EvalError::InvalidScript)
        );
        assert_eq!(
            evaluate(&[Number(1.0), Discard, StringLength, Number(2.0)], 1, 0),
            Err(EvalError::InvalidScript),
        );
        assert_eq!(
            evaluate(&[JumpStatement(2), StringLength, Number(2.0)], 1, 0),
            Ok(Value::Number(2.0)),
        );
        assert_eq!(
            evaluate(&[Number(1.0), StringLength, Discard, Number(2.0)], 1, 0),
            Err(EvalError::UnsupportedPropertyReceiver),
        );
    }

    #[test]
    fn string_length_checks_literal_range_including_empty_wrong_offset() {
        use Instruction::{Discard, String};
        let script = crate::parse_script("'x', ''").unwrap();
        let [String(nonempty), Discard, String(empty)] = *script.instructions() else {
            panic!("expected one unit followed by an empty range at offset 1");
        };
        for range in [nonempty, empty] {
            let value = Operand::String(StringOperand::Literal(range));
            assert_eq!(
                string_length(value, StringPools::literals(&[])),
                Err(EvalError::InvalidScript)
            );
        }
        assert_eq!(
            string_length(
                Operand::String(StringOperand::Literal(empty)),
                StringPools::literals(&[0xD800])
            ),
            Ok(Value::Number(0.0)),
        );
    }

    #[test]
    fn numeric_unary_instructions_require_a_checked_operand_when_reached() {
        use Instruction::{BitwiseNot, Discard, JumpStatement, Negate, Number, Positive};
        for (source, op, expected) in [
            ("+'1'", Positive, 1.0),
            ("-'1'", Negate, -1.0),
            ("~'1'", BitwiseNot, -2.0),
        ] {
            let script = crate::parse_script(source).unwrap();
            let [literal @ Instruction::String(range), actual] = script.instructions() else {
                panic!("expected a String and one unary instruction");
            };
            assert_eq!(*actual, op);
            assert_eq!(
                Operand::String(StringOperand::Literal(*range))
                    .to_number(StringPools::literals(&[]), &mut 0),
                Err(EvalError::InvalidScript),
            );
            assert_eq!(
                evaluate(&[*literal, op], 1, 0),
                Err(EvalError::InvalidScript)
            );
            assert_eq!(script.evaluate_value(), Ok(Value::Number(expected)));
            assert_eq!(
                evaluate(
                    &[JumpStatement(4), *literal, op, Discard, Number(42.0)],
                    1,
                    0
                ),
                Ok(Value::Number(42.0)),
            );
        }
    }

    #[test]
    fn numeric_binary_checks_arity_before_left_to_right_conversion() {
        let script = crate::parse_script("'1' - '2'").unwrap();
        let [
            Instruction::String(lhs),
            Instruction::String(rhs),
            Instruction::Subtract,
        ] = script.instructions()
        else {
            panic!("expected two String instructions and a subtract");
        };
        // A malformed operand cannot pass literal dispatch. Inject it here to
        // distinguish which conversion ran first when both would fail.
        let pool = StringPools::literals(&script.string_units()[..1]);
        let valid = Operand::String(StringOperand::Literal(*lhs));
        let invalid = Operand::String(StringOperand::Literal(*rhs));
        let no_math = |_, _| panic!("numeric operation ran after a failed check");
        for mut stack in [vec![], vec![valid]] {
            let mut credits = 261;
            assert_eq!(
                apply_numeric_binary(&mut Stack::root(&mut stack), pool, &mut credits, no_math),
                Err(EvalError::InvalidScript)
            );
            assert_eq!(credits, 261);
        }
        for (mut stack, expected, remaining) in [
            (
                vec![valid, invalid],
                EvalError::StringNumericConversionLimitExceeded,
                258,
            ),
            (vec![invalid, valid], EvalError::InvalidScript, 260),
        ] {
            let mut credits = 260;
            assert_eq!(
                apply_numeric_binary(&mut Stack::root(&mut stack), pool, &mut credits, no_math),
                Err(expected)
            );
            assert_eq!(credits, remaining);
        }
    }

    #[test]
    fn subtract_checks_arity_before_converting_the_available_operand() {
        let source = format!(
            "const ws = '{}'; let i = 0; while (i++ < 64) {{ +ws; }} +'{}'; ''",
            " ".repeat(4094),
            " ".repeat(63),
        );
        let script = crate::parse_script(&source).unwrap();
        let mut ops = script.instructions().to_vec();
        ops.push(Instruction::Subtract);
        // There is only one String on the stack and no conversion credit left.
        assert_eq!(
            evaluate_for_error(&script, &ops),
            Err(EvalError::InvalidScript)
        );
    }

    #[test]
    fn compound_subtraction_failure_stops_before_either_store_form() {
        for (padding, expected) in [
            (61, EvalError::InvalidScript),
            (62, EvalError::StringNumericConversionLimitExceeded),
        ] {
            for tail in ["x -= ''; x", "(x -= '')"] {
                let source = format!(
                    "const ws = '{}'; let i = 0; while (i++ < 64) {{ +ws; }} +'{}'; \
                     let x = '' + ''; {tail}",
                    " ".repeat(4094),
                    " ".repeat(padding),
                );
                let script = crate::parse_script(&source).unwrap();
                let mut ops = script.instructions().to_vec();
                let subtract = ops
                    .iter()
                    .position(|op| *op == Instruction::Subtract)
                    .unwrap();
                let store = &mut ops[subtract + 1];
                *store = match *store {
                    Instruction::StoreBinding(_) => Instruction::StoreBinding(usize::MAX),
                    Instruction::StoreBindingKeep(_) => Instruction::StoreBindingKeep(usize::MAX),
                    _ => panic!("compound store must follow Subtract"),
                };
                // Only a successful subtraction dispatches the invalid store.
                assert_eq!(evaluate_for_error(&script, &ops), Err(expected), "{tail}");
            }
        }
    }

    #[test]
    fn update_instructions_check_operands_and_slots_before_conversion() {
        use Instruction::{Boolean, Discard, LoadBinding, Number, UpdateBinding, UpdateNumber};
        let prefix = format!(
            "const ws = '{}'; let i = 0; while (i++ < 64) {{ +ws; }} +'{}'; ",
            " ".repeat(4094),
            " ".repeat(63),
        );
        let script = crate::parse_script(&format!("{prefix}let x = ''; ++x; x")).unwrap();
        let mut ops = script.instructions().to_vec();
        let rhs_index = ops.len() - 4;
        let slot = script.binding_slots() - 1;
        assert_eq!(ops[rhs_index], Number(1.0));
        let run = |ops: &[Instruction]| evaluate_for_error(&script, ops);
        assert_eq!(
            run(&ops),
            Err(EvalError::StringNumericConversionLimitExceeded)
        );
        for rhs in [
            Number(2.0),
            Boolean(true),
            LoadBinding(slot),
            Discard,
            UpdateNumber(Update::Increment),
        ] {
            // A malformed RHS or too few operands fails before converting even
            // though evaluating the original LHS would exhaust String credits.
            ops[rhs_index] = rhs;
            assert_eq!(run(&ops), Err(EvalError::InvalidScript), "{rhs:?}");
        }

        let script = crate::parse_script(&format!("{prefix}const x = ''; x")).unwrap();
        let mut ops = script.instructions().to_vec();
        for slot in [slot, usize::MAX] {
            *ops.last_mut().unwrap() = UpdateBinding {
                slot,
                op: Update::Increment,
                result: UpdateResult::Old,
            };
            assert_eq!(
                evaluate_for_error(&script, &ops),
                Err(EvalError::InvalidScript),
            );
        }
    }

    #[test]
    fn string_comparisons_check_ranges_before_charging() {
        let script = crate::parse_script("'a' < 'b'").unwrap();
        let [
            Instruction::String(a),
            Instruction::String(b),
            Instruction::LessThan,
        ] = script.instructions()
        else {
            panic!("expected two literal instructions and a comparison");
        };
        // Only the first range fits in this truncated pool.
        let short_pool = StringPools::literals(&script.string_units()[..1]);
        let (a, b) = (
            Operand::String(StringOperand::Literal(*a)),
            Operand::String(StringOperand::Literal(*b)),
        );
        let number = Operand::Scalar(Value::Number(0.0));
        for (lhs, rhs) in [(a, b), (b, a), (b, number), (number, b)] {
            assert_eq!(
                operands_order(lhs, rhs, short_pool, &mut 0, &mut 0),
                Err(EvalError::InvalidScript)
            );
            assert_eq!(
                operands_equal(lhs, rhs, false, short_pool, &mut 0, &mut 0),
                Err(EvalError::InvalidScript)
            );
        }
    }

    #[test]
    fn comparisons_check_arity_even_when_conversion_credits_are_exhausted() {
        let source = format!(
            "const ws = '{}'; let i = 0; while (i++ < 64) {{ +ws; }} +'{}'; ''",
            " ".repeat(4094),
            " ".repeat(63),
        );
        let script = crate::parse_script(&source).unwrap();
        let mut ops = script.instructions().to_vec();
        for op in [
            Instruction::LessThan,
            Instruction::LessThanOrEqual,
            Instruction::GreaterThan,
            Instruction::GreaterThanOrEqual,
            Instruction::LooseEqual,
            Instruction::LooseNotEqual,
        ] {
            // Only the empty String remains on the stack; converting it before
            // detecting the missing operand would fail with conversion exhaustion.
            ops.push(op);
            assert_eq!(
                evaluate_for_error(&script, &ops),
                Err(EvalError::InvalidScript)
            );
            ops.pop();
        }
    }

    #[test]
    fn rejects_malformed_postfix() {
        let one = Instruction::Number(1.0);
        for (ops, peak) in [
            (&[][..], 0),
            (&[one, one][..], 2),
            (&[Instruction::Discard, one][..], 1), // Discard on an empty stack.
            (&[one, Instruction::Discard][..], 1), // No final result.
            (&[Instruction::Subtract][..], 1),
            (&[Instruction::Positive, one][..], 1),
            (&[Instruction::Negate, one][..], 1),
            (&[Instruction::Not, one][..], 1),
            (&[Instruction::BitwiseNot, one][..], 1),
            (&[Instruction::Void, one][..], 1),
            (&[one, Instruction::BitwiseAnd][..], 1),
            (&[one, Instruction::BitwiseXor][..], 1),
            (&[one, Instruction::BitwiseOr][..], 1),
            (&[one, Instruction::ShiftLeft][..], 1),
            (&[one, Instruction::ShiftRight][..], 1),
            (&[one, Instruction::ShiftRightUnsigned][..], 1),
            (&[one, Instruction::Divide][..], 1),
            (&[one, Instruction::Remainder][..], 1),
            (&[Instruction::Exponentiate, one][..], 1),
            (&[one, Instruction::Exponentiate][..], 1),
            (&[Instruction::LooseEqual][..], 1),
            (&[one, Instruction::LooseNotEqual][..], 1),
            (&[Instruction::StrictEqual][..], 1),
            (&[one, Instruction::LessThanOrEqual][..], 1),
            (&[one, Instruction::Subtract, one][..], 1),
            (&[one, one, one][..], 2),
            (&[one, one, one, Instruction::Add, one][..], 3),
            (&[one][..], 0),
            (&[one][..], 2),
            (&[one, Instruction::Boolean(true), Instruction::Add][..], 1),
        ] {
            assert_eq!(evaluate(ops, peak, 0), Err(EvalError::InvalidScript));
        }
    }

    #[test]
    fn checks_forward_jump_targets_and_allows_one_past_end() {
        use Instruction::{Jump, JumpIfFalse, Number};
        let one = Number(1.0);
        let zero = Number(0.0);
        for ops in [
            &[one, Jump(0)][..], // backward
            &[one, Jump(1)][..], // self
            &[one, Jump(3)][..], // past exit
            &[one, Jump(usize::MAX)][..],
            &[zero, JumpIfFalse(1), one][..],
            &[one, JumpIfFalse(4), one][..], // invalid even if not taken
            &[JumpIfFalse(2), one][..],      // missing test operand
            &[zero, JumpIfFalse(3), one][..], // taken to exit without a result
        ] {
            assert_eq!(
                evaluate(ops, 1, 0),
                Err(EvalError::InvalidScript),
                "{ops:?}"
            );
        }
        assert_eq!(evaluate(&[one, Jump(2)], 1, 0), Ok(Value::Number(1.0)));
    }

    #[test]
    fn checks_statement_tests_and_backward_edges() {
        use Instruction::{Boolean, Jump, JumpIfFalseStatement, LoopBack, Number};
        let one = Number(1.0);
        for target in [1, 2, 3, usize::MAX] {
            let ops = [Jump(1), LoopBack(target), one];
            assert_eq!(evaluate(&ops, 1, 0), Err(EvalError::InvalidScript));
        }
        assert_eq!(
            evaluate(&[LoopBack(0), one], 1, 0),
            Err(EvalError::InvalidScript)
        );
        assert_eq!(
            evaluate(&[one, LoopBack(0)], 1, 0),
            Err(EvalError::InvalidScript)
        );
        assert_eq!(
            evaluate(&[JumpIfFalseStatement(1), one], 1, 0),
            Err(EvalError::InvalidScript)
        );
        for truthy in [true, false] {
            let mut ops = [Boolean(truthy), JumpIfFalseStatement(3), LoopBack(0), one];
            let expected = if truthy {
                Err(EvalError::DispatchLimitExceeded)
            } else {
                Ok(Value::Number(1.0))
            };
            assert_eq!(evaluate(&ops, 1, 0), expected);
            // A statement exit is forward, even if the branch is not taken.
            ops[1] = JumpIfFalseStatement(0);
            assert_eq!(evaluate(&ops, 1, 0), Err(EvalError::InvalidScript));
            let extra_operand = [
                one,
                Boolean(truthy),
                JumpIfFalseStatement(4),
                LoopBack(0),
                one,
            ];
            assert_eq!(
                evaluate(&extra_operand, 2, 0),
                Err(EvalError::InvalidScript)
            );
        }
    }

    #[test]
    fn statement_forward_jump_checks_target_and_empty_stack() {
        use Instruction::{JumpStatement, Number};
        let one = Number(1.0);
        for target in [0, 3, usize::MAX] {
            assert_eq!(
                evaluate(&[JumpStatement(target), one], 1, 0),
                Err(EvalError::InvalidScript)
            );
        }
        // A regular expression jump could finish with this result on the stack.
        assert_eq!(
            evaluate(&[one, JumpStatement(2)], 1, 0),
            Err(EvalError::InvalidScript)
        );
        assert_eq!(
            evaluate(&[JumpStatement(2), Instruction::LoadBinding(0), one], 1, 0),
            Ok(Value::Number(1.0))
        );
    }

    #[test]
    fn loop_test_short_circuits_on_every_path() {
        use Instruction::{
            Boolean, JumpIfFalseStatement, LoadBinding, LoopBack, ShortCircuit, StoreBinding,
        };
        // `while (x && (true || bad_slot)) { x = false; } x` with an
        // invalid private load to make execution of the skipped operand observable.
        let mut ops = [
            Boolean(true),
            INIT_LET,
            LoadBinding(0),
            ShortCircuit {
                target: 7,
                condition: ShortCircuitCondition::Falsy,
            },
            Boolean(true),
            ShortCircuit {
                target: 7,
                condition: ShortCircuitCondition::Truthy,
            },
            LoadBinding(1),
            JumpIfFalseStatement(11),
            Boolean(false),
            StoreBinding(0),
            LoopBack(2),
            LoadBinding(0),
        ];
        assert_eq!(evaluate(&ops, 1, 1), Ok(Value::Boolean(false)));
        ops[4] = Boolean(false);
        assert_eq!(evaluate(&ops, 1, 1), Err(EvalError::InvalidScript));
    }

    #[test]
    fn checks_fuel_before_dispatching_even_an_invalid_operation() {
        use Instruction::LoadBinding;
        // Seven dispatches per iteration, two for initialization, two for the
        // final falsy test, and seven for the final expression. No exit dispatch.
        let script = crate::parse_script("let x=2339; while(x){x=x-1;} + + + + + + x").unwrap();
        assert_eq!(2339 * 7 + 2 + 2 + 7, MAX_DISPATCHES);
        assert_eq!(script.evaluate(), Ok(0.0));
        let mut ops = script.instructions().to_vec();
        *ops.last_mut().unwrap() = LoadBinding(1);
        assert_eq!(evaluate(&ops, 2, 1), Err(EvalError::InvalidScript));
        *ops.last_mut().unwrap() = Instruction::Positive;
        ops.push(LoadBinding(1));
        assert_eq!(evaluate(&ops, 2, 1), Err(EvalError::DispatchLimitExceeded));
    }

    #[test]
    fn executes_only_the_selected_arm() {
        use Instruction::{Boolean, Jump, JumpIfFalse, LoadBinding, Number};
        // A bad slot is observable if executed, unlike ordinary arithmetic in
        // the subset. The parser would never emit this load.
        let bad = LoadBinding(0);
        let answer = Boolean(false);
        for (truthy, falsy) in [(Number(1.0), Number(0.0)), (Boolean(true), Boolean(false))] {
            for (test, flipped, consequent, alternate) in
                [(truthy, falsy, answer, bad), (falsy, truthy, bad, answer)]
            {
                let mut ops = [test, JumpIfFalse(4), consequent, Jump(5), alternate];
                assert_eq!(evaluate(&ops, 1, 0), Ok(Value::Boolean(false)));
                ops[0] = flipped;
                assert_eq!(evaluate(&ops, 1, 0), Err(EvalError::InvalidScript));
            }
        }
    }

    #[test]
    fn short_circuit_skips_rhs_or_pops_lhs_and_evaluates_rhs() {
        use Instruction::{Boolean, LoadBinding, Null, Number, ShortCircuit, Undefined};
        use ShortCircuitCondition::{Falsy, NonNullish, Truthy};
        // An uninitialized slot makes RHS execution observable. Valid scripts
        // cannot include the bad load, even when it would never be executed.
        for (condition, skip, fall_through, expected) in [
            (Falsy, Boolean(false), Boolean(true), Value::Boolean(false)),
            (Truthy, Number(7.0), Number(0.0), Value::Number(7.0)),
            (NonNullish, Boolean(false), Null, Value::Boolean(false)),
            (NonNullish, Number(0.0), Undefined, Value::Number(0.0)),
        ] {
            let mut ops = [
                skip,
                ShortCircuit {
                    target: 3,
                    condition,
                },
                LoadBinding(0),
            ];
            assert_eq!(evaluate(&ops, 1, 0), Ok(expected));
            ops[0] = fall_through;
            assert_eq!(evaluate(&ops, 1, 0), Err(EvalError::InvalidScript));
            ops[2] = Boolean(true);
            assert_eq!(evaluate(&ops, 1, 0), Ok(Value::Boolean(true)));
        }
    }

    #[test]
    fn short_circuit_requires_a_forward_target_and_an_operand() {
        use Instruction::{Boolean, Null, ShortCircuit};
        use ShortCircuitCondition::{Falsy, NonNullish, Truthy};
        for (condition, taken, fallthrough) in [
            (Falsy, Boolean(false), Boolean(true)),
            (Truthy, Boolean(true), Boolean(false)),
            (NonNullish, Boolean(false), Null),
        ] {
            for target in [0, 1, 4, usize::MAX] {
                for lhs in [taken, fallthrough] {
                    let ops = [lhs, ShortCircuit { target, condition }, Boolean(true)];
                    assert_eq!(evaluate(&ops, 1, 0), Err(EvalError::InvalidScript));
                }
            }
            let ops = [
                ShortCircuit {
                    target: 2,
                    condition,
                },
                Boolean(true),
            ];
            assert_eq!(evaluate(&ops, 1, 0), Err(EvalError::InvalidScript));
        }
    }

    #[test]
    fn statement_short_circuit_checks_target_and_requires_exactly_one_operand() {
        use Instruction::{Boolean, ShortCircuitStatement};
        let branch = |target| ShortCircuitStatement {
            target,
            condition: ShortCircuitCondition::Truthy,
        };
        for test in [Boolean(true), Boolean(false)] {
            for target in [0, 1, 4, usize::MAX] {
                let ops = [test, branch(target), Boolean(true)];
                assert_eq!(evaluate(&ops, 1, 0), Err(EvalError::InvalidScript));
            }
            let ops = [Boolean(true), test, branch(3), Boolean(true)];
            assert_eq!(evaluate(&ops, 2, 0), Err(EvalError::InvalidScript));
        }
        let ops = [branch(1), Boolean(true)];
        assert_eq!(evaluate(&ops, 1, 0), Err(EvalError::InvalidScript));
    }

    #[test]
    fn comparison_results_branch_without_running_the_other_arm() {
        use Instruction::{Boolean, Jump, JumpIfFalse, LessThan, LoadBinding, Number};
        // A load of a nonexistent slot fails if executed. Comparisons drive
        // both paths, and the selected value retains its tag.
        let mut ops = [
            Boolean(false),
            Number(1.0),
            LessThan,
            JumpIfFalse(6),
            Boolean(true),
            Jump(7),
            LoadBinding(0),
        ];
        assert_eq!(evaluate(&ops, 2, 0), Ok(Value::Boolean(true)));
        ops[0] = Boolean(true);
        assert_eq!(evaluate(&ops, 2, 0), Err(EvalError::InvalidScript));
        ops[4] = LoadBinding(0);
        ops[6] = Number(17.0);
        assert_eq!(evaluate(&ops, 2, 0), Ok(Value::Number(17.0)));
    }

    #[test]
    fn checks_work_budget_before_allocating_operand_storage() {
        let ops = vec![Instruction::Add; MAX_INSTRUCTIONS + 1];
        assert_eq!(evaluate(&ops, 0, usize::MAX), Err(EvalError::LimitExceeded));
    }

    #[test]
    fn evaluates_at_the_maximum_valid_operand_count() {
        let operands = MAX_INSTRUCTIONS.div_ceil(2);
        let mut ops = vec![Instruction::Number(1.0); operands];
        ops.extend(std::iter::repeat_n(Instruction::Add, operands - 1));
        ops.push(Instruction::Positive);
        assert_eq!(ops.len(), MAX_INSTRUCTIONS);
        assert_eq!(
            evaluate(&ops, operands, 0),
            Ok(Value::Number(operands as f64))
        );
    }

    #[test]
    fn checks_binding_access_and_initializer_shape() {
        use Instruction::{LoadBinding, Number};
        let one = Number(1.0);
        for (ops, peak, slots) in [
            (&[one, INIT_CONST, LoadBinding(0)][..], 1, 0), // undeclared initialization
            (&[LoadBinding(0), INIT_CONST, one][..], 1, 1), // read before initialization
            (&[one, INIT_CONST, LoadBinding(1)][..], 1, 1), // out-of-range read
            (&[one, INIT_CONST, LoadBinding(usize::MAX)][..], 1, 1),
            (&[one, one, INIT_CONST, one][..], 2, 1), // excess operands at initialization
            (&[INIT_CONST, one, one][..], 2, 1),      // missing operand at initialization
            (&[one, INIT_CONST, one, INIT_CONST, one][..], 1, 1), // distinct initializers of slot 0
            (
                &[
                    one,
                    Instruction::InitBinding {
                        slot: usize::MAX,
                        kind: BindingKind::Const,
                    },
                    one,
                ][..],
                1,
                1,
            ),
            (&[one, INIT_CONST][..], 1, 1), // no final expression
            (&[one][..], 1, usize::MAX),    // invalid reservation
        ] {
            assert_eq!(evaluate(ops, peak, slots), Err(EvalError::InvalidScript));
        }
        assert_eq!(
            evaluate(&[one, INIT_CONST, LoadBinding(0)], 1, 1),
            Ok(Value::Number(1.0))
        );
    }

    #[test]
    fn initialization_can_skip_slots_but_access_requires_initialization() {
        use Instruction::{JumpStatement, LoadBinding, Number, StoreBinding};
        let mut ops = [
            JumpStatement(3),
            Number(2.0),
            INIT_LET,
            Number(3.0),
            Instruction::InitBinding {
                slot: 1,
                kind: BindingKind::Const,
            },
            LoadBinding(1),
        ];
        assert_eq!(evaluate(&ops, 1, 2), Ok(Value::Number(3.0)));
        ops[5] = LoadBinding(0);
        assert_eq!(evaluate(&ops, 1, 2), Err(EvalError::InvalidScript));
        ops[4] = StoreBinding(0);
        assert_eq!(evaluate(&ops, 1, 2), Err(EvalError::InvalidScript));
    }

    #[test]
    fn checks_assignment_slot_mutability_and_operand_shape() {
        use Instruction::{LoadBinding, Number, StoreBinding};
        let one = Number(1.0);
        let two = Number(2.0);
        let store = StoreBinding(0);
        let read = LoadBinding(0);
        for (ops, peak) in [
            (&[one, store, one, INIT_LET, read][..], 1), // write before initialization
            (&[one, INIT_CONST, two, store, read][..], 1), // write to const
            (&[one, INIT_LET, two, StoreBinding(1), read][..], 1), // write out of range
            (&[one, INIT_LET, two, StoreBinding(usize::MAX), read][..], 1),
            (&[one, INIT_LET, store, read][..], 1), // missing operand
            (&[one, INIT_LET, one, two, store, read][..], 2), // excess operands
            (&[one, INIT_LET, two, store][..], 1),  // no final expression
        ] {
            assert_eq!(evaluate(ops, peak, 1), Err(EvalError::InvalidScript));
        }
        assert_eq!(
            evaluate(&[one, INIT_LET, two, store, read], 1, 1),
            Ok(Value::Number(2.0))
        );
    }

    #[test]
    fn store_keep_checks_the_slot_and_preserves_the_operand_stack() {
        use Instruction::{Add, LoadBinding, Multiply, Number, StoreBindingKeep};
        let one = Number(1.0);
        let two = Number(2.0);
        let store_keep = StoreBindingKeep(0);
        let read = LoadBinding(0);
        for ops in [
            &[one, store_keep, one, INIT_LET, read][..], // before initialization
            &[one, INIT_CONST, two, store_keep, read][..], // immutable slot
            &[one, INIT_LET, two, StoreBindingKeep(1)][..], // out of range
            &[one, INIT_LET, two, StoreBindingKeep(usize::MAX)][..],
            &[one, INIT_LET, store_keep, read][..], // missing operand
        ] {
            assert_eq!(evaluate(ops, 1, 1), Err(EvalError::InvalidScript));
        }
        assert_eq!(
            evaluate(&[one, INIT_LET, two, store_keep], 1, 1),
            Ok(Value::Number(2.0))
        );
        assert_eq!(
            evaluate(
                &[one, INIT_LET, one, two, store_keep, read, Multiply, Add],
                3,
                1,
            ),
            Ok(Value::Number(5.0)),
        );
    }

    #[test]
    fn grouped_update_checks_mutable_initialization_slot_bounds_and_push_capacity() {
        use Instruction::{Add, Boolean, UpdateBinding};
        let update = |slot| UpdateBinding {
            slot,
            op: Update::Increment,
            result: UpdateResult::Old,
        };
        let value = Boolean(true);
        for ops in [
            [update(0), INIT_LET, value],   // read/write before initialization
            [value, INIT_CONST, update(0)], // immutable slot
            [value, INIT_LET, update(1)],   // out of range
            [value, INIT_LET, update(usize::MAX)],
        ] {
            assert_eq!(evaluate(&ops, 1, 1), Err(EvalError::InvalidScript));
        }
        let ops = [value, INIT_LET, value, update(0), Add];
        assert_eq!(evaluate(&ops, 1, 1), Err(EvalError::InvalidScript));
        assert_eq!(evaluate(&ops, 2, 1), Ok(Value::Number(2.0)));
    }

    #[test]
    fn evaluates_at_the_maximum_valid_slot_count() {
        let slots = (MAX_INSTRUCTIONS - 1) / 2;
        let mut ops = Vec::new();
        for slot in 0..slots {
            ops.extend([
                Instruction::Number(slot as f64),
                Instruction::InitBinding {
                    slot,
                    kind: BindingKind::Const,
                },
            ]);
        }
        ops.extend([Instruction::LoadBinding(slots - 1), Instruction::Positive]);
        assert_eq!(ops.len(), MAX_INSTRUCTIONS);
        assert_eq!(
            evaluate(&ops, 1, slots),
            Ok(Value::Number((slots - 1) as f64))
        );
        ops.push(Instruction::Positive);
        assert_eq!(evaluate(&ops, 1, slots), Err(EvalError::LimitExceeded));
    }

    #[test]
    fn non_finite_results_are_numbers() {
        let largest = Instruction::Number(f64::MAX);
        let overflow = [largest, Instruction::Number(2.0), Instruction::Multiply];
        assert_eq!(evaluate(&overflow, 2, 0), Ok(Value::Number(f64::INFINITY)));

        let mut ops = overflow.to_vec();
        ops.extend([Instruction::Number(0.0), Instruction::Multiply]);
        assert!(matches!(evaluate(&ops, 2, 0), Ok(Value::Number(value)) if value.is_nan()));
    }
}
