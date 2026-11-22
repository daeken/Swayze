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
    /// The instruction sequence does not form a single well-balanced expression.
    InvalidScript,
    /// The evaluator could not allocate the bounded operand stack.
    AllocationFailed,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LimitExceeded => "arithmetic script exceeds the evaluation operation limit",
            Self::InvalidScript => "invalid prepared arithmetic instruction sequence",
            Self::AllocationFailed => "could not allocate the arithmetic operand stack",
        })
    }
}

impl std::error::Error for EvalError {}

impl Script {
    /// Evaluate this prepared arithmetic expression using JavaScript Number (binary64).
    ///
    /// Each binary `+`, `-`, or `*` operates on numbers in the order chosen by
    /// the parser. Signed zeros, infinities, and NaN are numeric results, not
    /// evaluation errors. This runs no host callbacks and does not read source text.
    /// The evaluation ceiling is 4,096 instructions (one per literal or operator),
    /// regardless of the parse limits used.
    ///
    /// # Errors
    ///
    /// Returns an error if operand storage could not be allocated, or if the
    /// prepared instruction sequence violates its internal shape or size limits.
    pub fn evaluate(&self) -> Result<f64, EvalError> {
        evaluate(self.instructions(), self.max_stack_depth())
    }
}

fn evaluate(ops: &[Instruction], max_operands: usize) -> Result<f64, EvalError> {
    if ops.len() > MAX_INSTRUCTIONS {
        return Err(EvalError::LimitExceeded);
    }
    // A valid sequence with n instructions has (n+1)/2 literals: each
    // binary operation reduces the stack depth by one, leaving one result.
    // Bound the parser's peak stack depth before using it to reserve storage.
    if ops.len().is_multiple_of(2) || max_operands == 0 || max_operands > ops.len().div_ceil(2) {
        return Err(EvalError::InvalidScript);
    }

    let mut stack = Vec::new();
    stack
        .try_reserve_exact(max_operands)
        .map_err(|_| EvalError::AllocationFailed)?;

    for op in ops {
        match *op {
            Instruction::Number(value) => {
                if stack.len() == max_operands {
                    return Err(EvalError::InvalidScript);
                }
                stack.push(value);
            }
            Instruction::Add => apply_binary(&mut stack, |lhs, rhs| lhs + rhs)?,
            Instruction::Subtract => apply_binary(&mut stack, |lhs, rhs| lhs - rhs)?,
            Instruction::Multiply => apply_binary(&mut stack, |lhs, rhs| lhs * rhs)?,
        }
    }

    match stack.as_slice() {
        [value] => Ok(*value),
        _ => Err(EvalError::InvalidScript),
    }
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
            (&[one, Instruction::Subtract, one][..], 1),
            (&[one, one, one][..], 2),
            (&[one, one, one, Instruction::Add, one][..], 3),
            (&[one][..], 0),
            (&[one][..], 2),
            (&[one, one, Instruction::Add][..], 1),
        ] {
            assert_eq!(evaluate(ops, peak), Err(EvalError::InvalidScript));
        }
    }

    #[test]
    fn checks_work_budget_before_allocating_operand_storage() {
        let ops = vec![Instruction::Add; MAX_INSTRUCTIONS + 1];
        assert_eq!(evaluate(&ops, 0), Err(EvalError::LimitExceeded));
    }

    #[test]
    fn evaluates_at_the_maximum_valid_operand_count() {
        let operands = MAX_INSTRUCTIONS.div_ceil(2);
        let mut ops = vec![Instruction::Number(1.0); operands];
        ops.extend(std::iter::repeat_n(Instruction::Add, operands - 1));
        assert_eq!(evaluate(&ops, operands), Ok(operands as f64));
    }

    #[test]
    fn non_finite_results_are_numbers() {
        let largest = Instruction::Number(f64::MAX);
        let overflow = [largest, Instruction::Number(2.0), Instruction::Multiply];
        assert_eq!(evaluate(&overflow, 2), Ok(f64::INFINITY));

        let mut ops = overflow.to_vec();
        ops.extend([Instruction::Number(0.0), Instruction::Multiply]);
        assert!(evaluate(&ops, 2).unwrap().is_nan());
    }
}
