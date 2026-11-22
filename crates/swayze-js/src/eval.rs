use std::fmt;

use crate::{Script, parser::Instruction};

// This is an execution ceiling, independent of custom parse limits.
const MAX_INSTRUCTIONS: usize = 4096;

/// Failure to evaluate a prepared arithmetic script.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EvalError {
    /// The instruction sequence exceeds the evaluator's operation budget.
    LimitExceeded,
    /// The instruction sequence has invalid operand or binding-slot usage.
    InvalidScript,
    /// The evaluator could not allocate bounded operand or binding storage.
    AllocationFailed,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LimitExceeded => "arithmetic script exceeds the evaluation operation limit",
            Self::InvalidScript => "invalid prepared arithmetic instruction sequence",
            Self::AllocationFailed => "could not allocate arithmetic operand or binding storage",
        })
    }
}

impl std::error::Error for EvalError {}

impl Script {
    /// Evaluate this prepared arithmetic Script using JavaScript Number (binary64).
    ///
    /// Each operator works on numbers in the order chosen by the parser, rounding
    /// each operation to binary64. Signed zeros, infinities, and NaN are numeric
    /// results, not evaluation errors. This runs no host callbacks and does not
    /// read source text. Const initializers run in order, with fresh immutable
    /// slots for each call. The final expression determines the result.
    /// The evaluation ceiling is 4,096 instructions (one per literal, operator,
    /// binding read or binding initialization), regardless of the parse limits used.
    ///
    /// # Errors
    ///
    /// Returns an error if operand or binding storage could not be allocated, or if the
    /// prepared instruction sequence violates its internal shape or size limits.
    pub fn evaluate(&self) -> Result<f64, EvalError> {
        evaluate(
            self.instructions(),
            self.max_stack_depth(),
            self.const_slots(),
        )
    }
}

fn evaluate(
    ops: &[Instruction],
    max_operands: usize,
    const_slots: usize,
) -> Result<f64, EvalError> {
    if ops.len() > MAX_INSTRUCTIONS {
        return Err(EvalError::LimitExceeded);
    }
    // Within any expression, each push after the first requires a binary
    // operation before that expression can yield one value. Each binding also
    // needs at least a push and an initializer instruction, and the final
    // expression needs at least one instruction of its own.
    if max_operands == 0
        || max_operands > ops.len().div_ceil(2)
        || const_slots > ops.len().saturating_sub(1) / 2
    {
        return Err(EvalError::InvalidScript);
    }

    let mut stack = Vec::new();
    stack
        .try_reserve_exact(max_operands)
        .map_err(|_| EvalError::AllocationFailed)?;

    // An append-only prefix of initialized slots, local to this evaluation.
    let mut bindings = Vec::new();
    bindings
        .try_reserve_exact(const_slots)
        .map_err(|_| EvalError::AllocationFailed)?;

    for op in ops {
        match *op {
            Instruction::Number(value) => push_operand(&mut stack, max_operands, value)?,
            Instruction::LoadConst(slot) => {
                let value = *bindings.get(slot).ok_or(EvalError::InvalidScript)?;
                push_operand(&mut stack, max_operands, value)?;
            }
            Instruction::InitConst => {
                if bindings.len() == const_slots || stack.len() != 1 {
                    return Err(EvalError::InvalidScript);
                }
                bindings.push(stack.pop().ok_or(EvalError::InvalidScript)?);
            }
            Instruction::Add => apply_binary(&mut stack, |lhs, rhs| lhs + rhs)?,
            Instruction::Subtract => apply_binary(&mut stack, |lhs, rhs| lhs - rhs)?,
            Instruction::Multiply => apply_binary(&mut stack, |lhs, rhs| lhs * rhs)?,
            Instruction::Divide => apply_binary(&mut stack, |lhs, rhs| lhs / rhs)?,
            // Floating-point `%` uses a quotient truncated toward zero, as in JS.
            Instruction::Remainder => apply_binary(&mut stack, |lhs, rhs| lhs % rhs)?,
            Instruction::Positive => apply_unary(&mut stack, |value| value)?,
            Instruction::Negate => apply_unary(&mut stack, |value| -value)?,
        }
    }

    match stack.as_slice() {
        [value] if bindings.len() == const_slots => Ok(*value),
        _ => Err(EvalError::InvalidScript),
    }
}

fn push_operand(stack: &mut Vec<f64>, max_operands: usize, value: f64) -> Result<(), EvalError> {
    if stack.len() == max_operands {
        return Err(EvalError::InvalidScript);
    }
    stack.push(value);
    Ok(())
}

fn apply_unary(stack: &mut [f64], op: impl FnOnce(f64) -> f64) -> Result<(), EvalError> {
    let value = stack.last_mut().ok_or(EvalError::InvalidScript)?;
    *value = op(*value);
    Ok(())
}

fn apply_binary(stack: &mut Vec<f64>, op: impl FnOnce(f64, f64) -> f64) -> Result<(), EvalError> {
    let rhs = stack.pop().ok_or(EvalError::InvalidScript)?;
    let lhs = stack.last_mut().ok_or(EvalError::InvalidScript)?;
    *lhs = op(*lhs, rhs);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            (&[one, Instruction::Subtract, one][..], 1),
            (&[one, one, one][..], 2),
            (&[one, one, one, Instruction::Add, one][..], 3),
            (&[one][..], 0),
            (&[one][..], 2),
            (&[one, one, Instruction::Add][..], 1),
        ] {
            assert_eq!(evaluate(ops, peak, 0), Err(EvalError::InvalidScript));
        }
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
        assert_eq!(evaluate(&ops, operands, 0), Ok(operands as f64));
    }

    #[test]
    fn checks_binding_access_and_initializer_shape() {
        use Instruction::{InitConst, LoadConst, Number};
        let one = Number(1.0);
        for (ops, peak, slots) in [
            (&[one, InitConst, LoadConst(0)][..], 1, 0), // undeclared initialization
            (&[LoadConst(0), InitConst, one][..], 1, 1), // read before initialization
            (&[one, InitConst, LoadConst(1)][..], 1, 1), // out-of-range read
            (&[one, InitConst, LoadConst(usize::MAX)][..], 1, 1),
            (&[one, one, InitConst, one][..], 2, 1), // excess operands at initialization
            (&[InitConst, one, one][..], 2, 1),      // missing operand at initialization
            (&[one, InitConst, one, InitConst, one][..], 1, 1), // excess initializations
            (
                &[one, Instruction::Positive, Instruction::Positive][..],
                1,
                1,
            ), // missing initialization
            (&[one, InitConst][..], 1, 1),           // no final expression
            (&[one][..], 1, usize::MAX),             // invalid reservation
        ] {
            assert_eq!(evaluate(ops, peak, slots), Err(EvalError::InvalidScript));
        }
        assert_eq!(evaluate(&[one, InitConst, LoadConst(0)], 1, 1), Ok(1.0));
    }

    #[test]
    fn evaluates_at_the_maximum_valid_slot_count() {
        let slots = (MAX_INSTRUCTIONS - 1) / 2;
        let mut ops = Vec::new();
        for slot in 0..slots {
            ops.extend([Instruction::Number(slot as f64), Instruction::InitConst]);
        }
        ops.extend([Instruction::LoadConst(slots - 1), Instruction::Positive]);
        assert_eq!(ops.len(), MAX_INSTRUCTIONS);
        assert_eq!(evaluate(&ops, 1, slots), Ok((slots - 1) as f64));
        ops.push(Instruction::Positive);
        assert_eq!(evaluate(&ops, 1, slots), Err(EvalError::LimitExceeded));
    }

    #[test]
    fn non_finite_results_are_numbers() {
        let largest = Instruction::Number(f64::MAX);
        let overflow = [largest, Instruction::Number(2.0), Instruction::Multiply];
        assert_eq!(evaluate(&overflow, 2, 0), Ok(f64::INFINITY));

        let mut ops = overflow.to_vec();
        ops.extend([Instruction::Number(0.0), Instruction::Multiply]);
        assert!(evaluate(&ops, 2, 0).unwrap().is_nan());
    }
}
