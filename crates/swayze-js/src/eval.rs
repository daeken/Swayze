use std::fmt;

use crate::{
    Script,
    parser::{BindingKind, Instruction},
};

// This is an execution ceiling, independent of custom parse limits.
const MAX_INSTRUCTIONS: usize = 4096;

/// A result or intermediate value in the supported JavaScript subset.
///
/// Rust `PartialEq` compares tagged host values; it is not a guest equality operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    /// A JavaScript Number (binary64), including signed zeros, infinities, and NaN.
    Number(f64),
    /// A JavaScript Boolean.
    Boolean(bool),
}

impl Value {
    fn to_number(self) -> f64 {
        match self {
            Self::Number(value) => value,
            Self::Boolean(true) => 1.0,
            Self::Boolean(false) => 0.0,
        }
    }

    fn to_boolean(self) -> bool {
        match self {
            Self::Number(value) => value != 0.0 && !value.is_nan(),
            Self::Boolean(value) => value,
        }
    }

    fn strictly_equals(self, other: Self) -> bool {
        match (self, other) {
            // Binary64 equality equates signed zeros and never equates NaN.
            (Self::Number(lhs), Self::Number(rhs)) => lhs == rhs,
            (Self::Boolean(lhs), Self::Boolean(rhs)) => lhs == rhs,
            _ => false,
        }
    }
}

/// Failure to evaluate a prepared script or obtain a Number result from it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EvalError {
    /// The instruction sequence exceeds the evaluator's operation budget.
    LimitExceeded,
    /// The instruction sequence has invalid control flow, operands, or binding-slot usage.
    InvalidScript,
    /// The evaluator could not allocate bounded operand or binding storage.
    AllocationFailed,
    /// The numeric convenience API received a Boolean result instead of a Number.
    NonNumericResult,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LimitExceeded => "arithmetic script exceeds the evaluation operation limit",
            Self::InvalidScript => "invalid prepared arithmetic instruction sequence",
            Self::AllocationFailed => "could not allocate arithmetic operand or binding storage",
            Self::NonNumericResult => "script result is a Boolean, not a Number",
        })
    }
}

impl std::error::Error for EvalError {}

impl Script {
    /// Evaluate the Script and return its Number result without coercing it.
    ///
    /// Use [`Script::evaluate_value`] to accept Boolean results as well.
    ///
    /// # Errors
    ///
    /// Returns any error from [`Script::evaluate_value`], or
    /// [`EvalError::NonNumericResult`] if the final value is a Boolean.
    pub fn evaluate(&self) -> Result<f64, EvalError> {
        match self.evaluate_value()? {
            Value::Number(value) => Ok(value),
            Value::Boolean(_) => Err(EvalError::NonNumericResult),
        }
    }

    /// Evaluate the Script and return the final expression's Number or Boolean value.
    ///
    /// Arithmetic and relational operators convert Boolean operands to `1` for
    /// `true` and `+0` for `false`. Arithmetic returns a Number; relational
    /// operators return a Boolean, always `false` if either operand is NaN.
    /// Strict equality and inequality compare tags and values without coercion
    /// and return a Boolean.
    ///
    /// A conditional evaluates its test and only the selected arm, preserving
    /// the arm's value and type. `false`, either signed zero, and NaN select the
    /// false arm.
    ///
    /// This runs no host callbacks and does not read source text. Initializations
    /// and assignments run in source order with fresh immutable `const` and
    /// mutable `let` slots for each call. The evaluation ceiling is 4,096 total
    /// instructions, including both conditional arms and their jumps, regardless
    /// of the parse limits used.
    ///
    /// # Errors
    ///
    /// Returns an error if operand or binding storage could not be allocated, or if the
    /// prepared instruction sequence violates its internal shape or size limits.
    pub fn evaluate_value(&self) -> Result<Value, EvalError> {
        evaluate(
            self.instructions(),
            self.max_stack_depth(),
            self.binding_slots(),
        )
    }
}

enum Slot {
    Const(Value),
    Let(Value),
}

fn evaluate(
    ops: &[Instruction],
    max_operands: usize,
    binding_slots: usize,
) -> Result<Value, EvalError> {
    if ops.len() > MAX_INSTRUCTIONS {
        return Err(EvalError::LimitExceeded);
    }
    // On a path that reaches the peak, that many pushes and at least one less
    // pop are needed to finish with one value. Each binding also needs at least
    // a push and an initializer, and the final expression needs an instruction.
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

    // An initialized prefix in declaration order, local to this evaluation.
    // Only the value of a mutable slot can be replaced.
    let mut bindings = Vec::new();
    bindings
        .try_reserve_exact(binding_slots)
        .map_err(|_| EvalError::AllocationFailed)?;

    // Each step advances the instruction pointer, so the total instruction
    // ceiling bounds execution even with jumps. `ops.len()` denotes exit.
    let mut ip = 0;
    while let Some(&op) = ops.get(ip) {
        ip += 1;
        match op {
            Instruction::Number(value) => {
                push_operand(&mut stack, max_operands, Value::Number(value))?;
            }
            Instruction::Boolean(value) => {
                push_operand(&mut stack, max_operands, Value::Boolean(value))?;
            }
            Instruction::LoadBinding(slot) => {
                let (Slot::Const(value) | Slot::Let(value)) =
                    *bindings.get(slot).ok_or(EvalError::InvalidScript)?;
                push_operand(&mut stack, max_operands, value)?;
            }
            Instruction::InitBinding(kind) => {
                if bindings.len() == binding_slots || stack.len() != 1 {
                    return Err(EvalError::InvalidScript);
                }
                let value = stack.pop().ok_or(EvalError::InvalidScript)?;
                bindings.push(match kind {
                    BindingKind::Const => Slot::Const(value),
                    BindingKind::Let => Slot::Let(value),
                });
            }
            Instruction::StoreBinding(slot) => {
                let Some(Slot::Let(value)) = bindings.get_mut(slot) else {
                    return Err(EvalError::InvalidScript);
                };
                if stack.len() != 1 {
                    return Err(EvalError::InvalidScript);
                }
                *value = stack.pop().ok_or(EvalError::InvalidScript)?;
            }
            Instruction::Add => apply_arithmetic(&mut stack, |lhs, rhs| lhs + rhs)?,
            Instruction::Subtract => apply_arithmetic(&mut stack, |lhs, rhs| lhs - rhs)?,
            Instruction::Multiply => apply_arithmetic(&mut stack, |lhs, rhs| lhs * rhs)?,
            Instruction::Divide => apply_arithmetic(&mut stack, |lhs, rhs| lhs / rhs)?,
            // Floating-point `%` uses a quotient truncated toward zero, as in JS.
            Instruction::Remainder => apply_arithmetic(&mut stack, |lhs, rhs| lhs % rhs)?,
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
            Instruction::Positive => apply_unary(&mut stack, |value| value)?,
            Instruction::Negate => apply_unary(&mut stack, |value| -value)?,
            Instruction::JumpIfFalse(target) => {
                let target = check_target(ip, target, ops.len())?;
                let condition = stack.pop().ok_or(EvalError::InvalidScript)?;
                if !condition.to_boolean() {
                    ip = target;
                }
            }
            Instruction::Jump(target) => ip = check_target(ip, target, ops.len())?,
        }
    }

    match stack.as_slice() {
        [value] if bindings.len() == binding_slots => Ok(*value),
        _ => Err(EvalError::InvalidScript),
    }
}

fn check_target(next: usize, target: usize, exit: usize) -> Result<usize, EvalError> {
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

fn apply_unary(stack: &mut [Value], op: impl FnOnce(f64) -> f64) -> Result<(), EvalError> {
    let value = stack.last_mut().ok_or(EvalError::InvalidScript)?;
    *value = Value::Number(op(value.to_number()));
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

    const INIT_CONST: Instruction = Instruction::InitBinding(BindingKind::Const);
    const INIT_LET: Instruction = Instruction::InitBinding(BindingKind::Let);

    #[test]
    fn rejects_malformed_postfix() {
        let one = Instruction::Number(1.0);
        for (ops, peak) in [
            (&[][..], 0),
            (&[one, one][..], 2),
            (&[Instruction::Subtract][..], 1),
            (&[Instruction::Positive, one][..], 1),
            (&[Instruction::Negate, one][..], 1),
            (&[one, Instruction::Divide][..], 1),
            (&[one, Instruction::Remainder][..], 1),
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
            (&[one, INIT_CONST, one, INIT_CONST, one][..], 1, 1), // excess initializations
            (
                &[one, Instruction::Positive, Instruction::Positive][..],
                1,
                1,
            ), // missing initialization
            (&[one, INIT_CONST][..], 1, 1),           // no final expression
            (&[one][..], 1, usize::MAX),              // invalid reservation
        ] {
            assert_eq!(evaluate(ops, peak, slots), Err(EvalError::InvalidScript));
        }
        assert_eq!(
            evaluate(&[one, INIT_CONST, LoadBinding(0)], 1, 1),
            Ok(Value::Number(1.0))
        );
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
            ops.extend([Instruction::Number(slot as f64), INIT_CONST]);
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
