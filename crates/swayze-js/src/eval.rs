use std::fmt;

use crate::{
    Script,
    parser::{BindingKind, Instruction, ShortCircuitCondition},
};

// Fixed evaluation ceilings, independent of custom parse limits.
const MAX_INSTRUCTIONS: usize = 4096;
const MAX_DISPATCHES: usize = 16_384;

/// A result or intermediate value in the supported JavaScript subset.
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

impl Value {
    fn to_number(self) -> f64 {
        match self {
            Self::Number(value) => value,
            Self::Boolean(true) => 1.0,
            Self::Boolean(false) | Self::Null => 0.0,
            Self::Undefined => f64::NAN,
        }
    }

    fn to_uint32(self) -> u32 {
        const MODULUS: f64 = 4_294_967_296.0; // 2^32
        let number = self.to_number();
        if !number.is_finite() {
            return 0;
        }
        // Reduce the represented Number, not its source literal. The truncated
        // integer's remainder is exact: the modulus is a power of two, and
        // both the remainder and its nonnegative residue fit exactly in f64.
        let remainder = number.trunc() % MODULUS;
        (if remainder < 0.0 {
            remainder + MODULUS
        } else {
            remainder
        }) as u32
    }

    fn to_int32(self) -> i32 {
        self.to_uint32() as i32 // Interpret the same bits as a signed integer.
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

impl ShortCircuitCondition {
    fn skips_rhs(self, lhs: Value) -> bool {
        match self {
            Self::Falsy => !lhs.to_boolean(),
            Self::Truthy => lhs.to_boolean(),
            Self::NonNullish => !matches!(lhs, Value::Null | Value::Undefined),
        }
    }
}

/// Failure to evaluate a prepared script or obtain a Number result from it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EvalError {
    /// The prepared instruction sequence exceeds the 4,096-instruction ceiling.
    LimitExceeded,
    /// Execution would dispatch more than 16,384 instructions in this evaluation.
    DispatchLimitExceeded,
    /// The instruction sequence has invalid control flow, operands, or binding-slot usage.
    InvalidScript,
    /// The evaluator could not allocate bounded operand or binding storage.
    AllocationFailed,
    /// The numeric convenience API received a non-Number result.
    NonNumericResult,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LimitExceeded => "prepared JavaScript instruction limit exceeded",
            Self::DispatchLimitExceeded => "JavaScript evaluation dispatch limit exceeded",
            Self::InvalidScript => "invalid prepared JavaScript instruction sequence",
            Self::AllocationFailed => "could not allocate JavaScript operand or binding storage",
            Self::NonNumericResult => "script result is not a Number",
        })
    }
}

impl std::error::Error for EvalError {}

impl Script {
    /// Evaluate the Script and return its Number result without coercing it.
    ///
    /// Use [`Script::evaluate_value`] to accept any supported value.
    ///
    /// # Errors
    ///
    /// Returns any error from [`Script::evaluate_value`], or
    /// [`EvalError::NonNumericResult`] if the final value is not a Number.
    pub fn evaluate(&self) -> Result<f64, EvalError> {
        match self.evaluate_value()? {
            Value::Number(value) => Ok(value),
            Value::Boolean(_) | Value::Null | Value::Undefined => Err(EvalError::NonNumericResult),
        }
    }

    /// Evaluate the Script and return the final expression's tagged primitive value.
    ///
    /// Arithmetic and relational operators leave Numbers unchanged and convert
    /// `true` to `1`, `false` and `null` to `+0`, and `undefined` to NaN.
    /// Arithmetic returns a Number; relational
    /// operators return a Boolean, always `false` if either operand is NaN.
    /// Bitwise operators also use this conversion, then truncate and wrap to 32
    /// bits (nonfinite values and signed zeros become zero). Shifts use the
    /// low five bits of the converted count. Bitwise results are Numbers
    /// representing signed 32-bit integers, except for zero-fill right shift
    /// (`>>>`), which returns an unsigned 32-bit result.
    /// Unary `void` evaluates its operand once, then returns [`Value::Undefined`]
    /// without coercing the operand's value.
    /// Equality and inequality return Booleans. Strict equality (`===`) never
    /// coerces. Loose equality (`==`) uses strict equality for matching tags,
    /// equates `null` with `undefined`, and compares a Number and a Boolean
    /// numerically after converting the Boolean. Other mixed tags are unequal.
    /// `!==` and `!=` negate their corresponding equality results.
    ///
    /// Logical negation (`!`), short-circuit operators (`&&`, `||`), and the
    /// conditional operator (`?:`) share truthiness: `false`, `null`, `undefined`,
    /// either signed zero, and NaN are falsy; all other supported values are truthy.
    /// `!` returns a Boolean. `&&` returns a falsy left operand, `||` returns a
    /// truthy left operand, and `??` returns a left operand that is neither `null`
    /// nor `undefined`. Otherwise they evaluate and return the right operand.
    /// Logical assignments (`&&=`, `||=`, `??=`) use the same tests to skip
    /// both RHS evaluation and the store on an initialized `let` target.
    /// They leave no result, and their RHS is still checked during parsing.
    /// Standalone `++` and `--` updates (also allowed in `for` initializers
    /// and updates) load one initialized `let`, convert its value to Number
    /// as above, and store the result of adding or subtracting `1`. Neither
    /// prefix nor postfix form leaves a result.
    /// A conditional expression evaluates its test and only the selected arm,
    /// preserving the selected value and type. A comma sequence evaluates its operands
    /// from left to right, discarding each but the last without coercion.
    /// An `if` statement uses the same
    /// truthiness but executes only the selected branch, leaving no operand
    /// stack result. A falsy test with no `else` performs no writes.
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
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::LimitExceeded`] for an oversized prepared script,
    /// [`EvalError::DispatchLimitExceeded`] if another dispatch would exceed
    /// its budget, [`EvalError::InvalidScript`] for an invalid instruction
    /// sequence, or [`EvalError::AllocationFailed`] if storage cannot be allocated.
    pub fn evaluate_value(&self) -> Result<Value, EvalError> {
        evaluate(
            self.instructions(),
            self.max_stack_depth(),
            self.binding_slots(),
        )
    }
}

struct Slot {
    init_at: usize,
    kind: BindingKind,
    value: Value,
}

fn evaluate(
    ops: &[Instruction],
    max_operands: usize,
    binding_slots: usize,
) -> Result<Value, EvalError> {
    if ops.len() > MAX_INSTRUCTIONS {
        return Err(EvalError::LimitExceeded);
    }
    // Back edges require an empty stack, so repeated passes cannot build up
    // operands. A path to the peak needs that many distinct pushes and at
    // least one less distinct pop to finish or reach a loop back edge. Each
    // binding also needs a push and initializer, plus a final expression.
    if max_operands == 0
        || max_operands > ops.len().div_ceil(2)
        || binding_slots > ops.len().saturating_sub(1) / 2
    {
        return Err(EvalError::InvalidScript);
    }

    let mut stack = Vec::new();
    stack
        .try_reserve_exact(max_operands)
        .map_err(|_| EvalError::AllocationFailed)?;

    // Each declaration has its own slot, local to this evaluation. Slots in
    // untaken paths may remain empty. Only an initializer can reset a const.
    // No clearing on scope exit is needed: the parser emits references only
    // within the declaration's scope after its initializer. A jump can skip a
    // block or leave it, but cannot enter a suffix from outside the block.
    // Loop back edges restart at a test, an update, or the start of a do body.
    let mut bindings = Vec::<Option<Slot>>::new();
    bindings
        .try_reserve_exact(binding_slots)
        .map_err(|_| EvalError::AllocationFailed)?;
    bindings.resize_with(binding_slots, || None);

    // `ops.len()` denotes exit, which is not a dispatch and needs no fuel.
    let mut remaining = MAX_DISPATCHES;
    let mut ip = 0;
    while ip != ops.len() {
        remaining = remaining
            .checked_sub(1)
            .ok_or(EvalError::DispatchLimitExceeded)?;
        let op = *ops.get(ip).ok_or(EvalError::InvalidScript)?;
        ip += 1;
        match op {
            Instruction::Number(value) => {
                push_operand(&mut stack, max_operands, Value::Number(value))?;
            }
            Instruction::Boolean(value) => {
                push_operand(&mut stack, max_operands, Value::Boolean(value))?;
            }
            Instruction::Null => push_operand(&mut stack, max_operands, Value::Null)?,
            Instruction::Undefined => push_operand(&mut stack, max_operands, Value::Undefined)?,
            Instruction::LoadBinding(slot) => {
                let binding = bindings
                    .get(slot)
                    .and_then(Option::as_ref)
                    .ok_or(EvalError::InvalidScript)?;
                push_operand(&mut stack, max_operands, binding.value)?;
            }
            Instruction::InitBinding { slot, kind } => {
                let binding = bindings.get_mut(slot).ok_or(EvalError::InvalidScript)?;
                let init_at = ip - 1;
                if stack.len() != 1 || binding.as_ref().is_some_and(|old| old.init_at != init_at) {
                    return Err(EvalError::InvalidScript);
                }
                let value = stack.pop().ok_or(EvalError::InvalidScript)?;
                *binding = Some(Slot {
                    init_at,
                    kind,
                    value,
                });
            }
            Instruction::StoreBinding(slot) => {
                let Some(Some(Slot {
                    kind: BindingKind::Let,
                    value,
                    ..
                })) = bindings.get_mut(slot)
                else {
                    return Err(EvalError::InvalidScript);
                };
                if stack.len() != 1 {
                    return Err(EvalError::InvalidScript);
                }
                *value = stack.pop().ok_or(EvalError::InvalidScript)?;
            }
            Instruction::Discard => {
                stack.pop().ok_or(EvalError::InvalidScript)?;
            }
            Instruction::Add => apply_arithmetic(&mut stack, |lhs, rhs| lhs + rhs)?,
            Instruction::Subtract => apply_arithmetic(&mut stack, |lhs, rhs| lhs - rhs)?,
            Instruction::Multiply => apply_arithmetic(&mut stack, |lhs, rhs| lhs * rhs)?,
            Instruction::Divide => apply_arithmetic(&mut stack, |lhs, rhs| lhs / rhs)?,
            // Floating-point `%` uses a quotient truncated toward zero, as in JS.
            Instruction::Remainder => apply_arithmetic(&mut stack, |lhs, rhs| lhs % rhs)?,
            Instruction::BitwiseAnd => apply_bitwise(&mut stack, |lhs, rhs| lhs & rhs)?,
            Instruction::BitwiseXor => apply_bitwise(&mut stack, |lhs, rhs| lhs ^ rhs)?,
            Instruction::BitwiseOr => apply_bitwise(&mut stack, |lhs, rhs| lhs | rhs)?,
            // Rust's wrapping shifts use only the low five count bits for i32/u32.
            Instruction::ShiftLeft => apply_binary(&mut stack, |lhs, rhs| {
                Value::Number(f64::from(lhs.to_int32().wrapping_shl(rhs.to_uint32())))
            })?,
            Instruction::ShiftRight => apply_binary(&mut stack, |lhs, rhs| {
                Value::Number(f64::from(lhs.to_int32().wrapping_shr(rhs.to_uint32())))
            })?,
            Instruction::ShiftRightUnsigned => apply_binary(&mut stack, |lhs, rhs| {
                Value::Number(f64::from(lhs.to_uint32().wrapping_shr(rhs.to_uint32())))
            })?,
            Instruction::LooseEqual => apply_binary(&mut stack, |lhs, rhs| {
                Value::Boolean(lhs.loosely_equals(rhs))
            })?,
            Instruction::LooseNotEqual => apply_binary(&mut stack, |lhs, rhs| {
                Value::Boolean(!lhs.loosely_equals(rhs))
            })?,
            Instruction::StrictEqual => apply_binary(&mut stack, |lhs, rhs| {
                Value::Boolean(lhs.strictly_equals(rhs))
            })?,
            Instruction::StrictNotEqual => apply_binary(&mut stack, |lhs, rhs| {
                Value::Boolean(!lhs.strictly_equals(rhs))
            })?,
            // Direct binary64 comparisons all return false for unordered operands.
            Instruction::LessThan => apply_relational(&mut stack, |lhs, rhs| lhs < rhs)?,
            Instruction::LessThanOrEqual => apply_relational(&mut stack, |lhs, rhs| lhs <= rhs)?,
            Instruction::GreaterThan => apply_relational(&mut stack, |lhs, rhs| lhs > rhs)?,
            Instruction::GreaterThanOrEqual => apply_relational(&mut stack, |lhs, rhs| lhs >= rhs)?,
            Instruction::Positive => {
                apply_unary(&mut stack, |value| Value::Number(value.to_number()))?;
            }
            Instruction::Negate => {
                apply_unary(&mut stack, |value| Value::Number(-value.to_number()))?;
            }
            Instruction::Not => {
                apply_unary(&mut stack, |value| Value::Boolean(!value.to_boolean()))?;
            }
            Instruction::BitwiseNot => {
                apply_unary(&mut stack, |value| {
                    Value::Number(f64::from(!value.to_int32()))
                })?;
            }
            Instruction::Void => apply_unary(&mut stack, |_| Value::Undefined)?,
            Instruction::JumpIfFalse(target) | Instruction::JumpIfFalseStatement(target) => {
                let target = check_forward_target(ip, target, ops.len())?;
                if matches!(op, Instruction::JumpIfFalseStatement(_)) && stack.len() != 1 {
                    return Err(EvalError::InvalidScript);
                }
                let condition = stack.pop().ok_or(EvalError::InvalidScript)?;
                if !condition.to_boolean() {
                    ip = target;
                }
            }
            Instruction::ShortCircuit { target, condition } => {
                let target = check_forward_target(ip, target, ops.len())?;
                let lhs = *stack.last().ok_or(EvalError::InvalidScript)?;
                if condition.skips_rhs(lhs) {
                    ip = target;
                } else {
                    stack.pop();
                }
            }
            Instruction::ShortCircuitStatement { target, condition } => {
                let target = check_forward_target(ip, target, ops.len())?;
                if stack.len() != 1 {
                    return Err(EvalError::InvalidScript);
                }
                let lhs = stack.pop().ok_or(EvalError::InvalidScript)?;
                if condition.skips_rhs(lhs) {
                    ip = target;
                }
            }
            Instruction::Jump(target) => ip = check_forward_target(ip, target, ops.len())?,
            Instruction::JumpStatement(target) => {
                if !stack.is_empty() {
                    return Err(EvalError::InvalidScript);
                }
                ip = check_forward_target(ip, target, ops.len())?;
            }
            Instruction::LoopBack(target) => {
                // `ip` already points past this instruction. Reject self and
                // forward edges, and require statement depth on entry.
                if target >= ip - 1 || !stack.is_empty() {
                    return Err(EvalError::InvalidScript);
                }
                ip = target;
            }
        }
    }

    match stack.as_slice() {
        [value] => Ok(*value),
        _ => Err(EvalError::InvalidScript),
    }
}

fn check_forward_target(next: usize, target: usize, exit: usize) -> Result<usize, EvalError> {
    if (next..=exit).contains(&target) {
        Ok(target)
    } else {
        Err(EvalError::InvalidScript)
    }
}

fn push_operand(
    stack: &mut Vec<Value>,
    max_operands: usize,
    value: Value,
) -> Result<(), EvalError> {
    if stack.len() == max_operands {
        return Err(EvalError::InvalidScript);
    }
    stack.push(value);
    Ok(())
}

fn apply_unary(stack: &mut [Value], op: impl FnOnce(Value) -> Value) -> Result<(), EvalError> {
    let value = stack.last_mut().ok_or(EvalError::InvalidScript)?;
    *value = op(*value);
    Ok(())
}

fn apply_binary(
    stack: &mut Vec<Value>,
    op: impl FnOnce(Value, Value) -> Value,
) -> Result<(), EvalError> {
    let rhs = stack.pop().ok_or(EvalError::InvalidScript)?;
    let lhs = stack.last_mut().ok_or(EvalError::InvalidScript)?;
    *lhs = op(*lhs, rhs);
    Ok(())
}

fn apply_arithmetic(
    stack: &mut Vec<Value>,
    op: impl FnOnce(f64, f64) -> f64,
) -> Result<(), EvalError> {
    apply_binary(stack, |lhs, rhs| {
        Value::Number(op(lhs.to_number(), rhs.to_number()))
    })
}

fn apply_bitwise(
    stack: &mut Vec<Value>,
    op: impl FnOnce(i32, i32) -> i32,
) -> Result<(), EvalError> {
    apply_binary(stack, |lhs, rhs| {
        Value::Number(f64::from(op(lhs.to_int32(), rhs.to_int32())))
    })
}

fn apply_relational(
    stack: &mut Vec<Value>,
    op: impl FnOnce(f64, f64) -> bool,
) -> Result<(), EvalError> {
    apply_binary(stack, |lhs, rhs| {
        Value::Boolean(op(lhs.to_number(), rhs.to_number()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const INIT_CONST: Instruction = Instruction::InitBinding {
        slot: 0,
        kind: BindingKind::Const,
    };
    const INIT_LET: Instruction = Instruction::InitBinding {
        slot: 0,
        kind: BindingKind::Let,
    };

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
