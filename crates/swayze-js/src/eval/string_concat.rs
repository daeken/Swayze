use std::ops::Range;

use super::{EvalError, Value};
use crate::{
    number_format,
    parser::{MAX_STRING_LITERAL_CODE_UNITS, StringRange},
};

// Keep every result within the fixed String-to-Number conversion bound.
const MAX_CONCAT_UNITS: usize = MAX_STRING_LITERAL_CODE_UNITS;
const MAX_APPENDED_UNITS: usize = 32_768;
const _: () = assert!(MAX_APPENDED_UNITS <= u16::MAX as usize);

/// Reusable storage for runtime Strings produced by Script evaluation.
///
/// The arena starts unallocated and grows only for nonempty concatenations.
/// Each evaluation resets its logical length but may reuse capacity. This is
/// not zeroization: previously written units may remain in the allocation.
#[derive(Default)]
pub struct EvalScratch {
    units: Vec<u16>,
}

impl EvalScratch {
    /// Create empty evaluation scratch without allocating.
    pub const fn new() -> Self {
        Self { units: Vec::new() }
    }

    pub(super) fn reset(&mut self) {
        self.units.clear();
    }

    pub(super) fn pools<'a>(&'a self, literals: &'a [u16]) -> StringPools<'a> {
        StringPools {
            literals,
            runtime: &self.units,
        }
    }

    pub(super) fn concatenate(
        &mut self,
        literals: &[u16],
        lhs: StringOperand,
        rhs: StringOperand,
    ) -> Result<StringOperand, EvalError> {
        let start = self.units.len();
        // Validate both ranges against initialized storage before checking
        // limits or growing. Runtime sources keep indices, not slices.
        let lhs = AppendSource::checked(lhs, literals, start)?;
        let rhs = AppendSource::checked(rhs, literals, start)?;
        self.append_pair(lhs, rhs)
    }

    pub(super) fn concatenate_scalar(
        &mut self,
        literals: &[u16],
        string: StringOperand,
        scalar: Value,
        scalar_first: bool,
    ) -> Result<StringOperand, EvalError> {
        let string = AppendSource::checked(string, literals, self.units.len())?;
        let units: &[u16] = match scalar {
            Value::Boolean(true) => &[116, 114, 117, 101], // true
            Value::Boolean(false) => &[102, 97, 108, 115, 101], // false
            Value::Null => &[110, 117, 108, 108],          // null
            Value::Undefined => &[117, 110, 100, 101, 102, 105, 110, 101, 100], // undefined
            Value::Number(number) => return self.concatenate_number(string, number, scalar_first),
        };
        self.append_scalar(string, units, scalar_first)
    }

    // Keep the renderer's temporary storage off the dispatch loop and the
    // other paths. Only the emitted prefix is passed to the checked append.
    #[inline(never)]
    fn concatenate_number(
        &mut self,
        string: AppendSource<'_>,
        number: f64,
        scalar_first: bool,
    ) -> Result<StringOperand, EvalError> {
        let mut buffer = [0; number_format::MAX_UNITS];
        let units = number_format::format_bits(number.to_bits(), &mut buffer);
        self.append_scalar(string, units, scalar_first)
    }

    fn append_scalar(
        &mut self,
        string: AppendSource<'_>,
        scalar_units: &[u16],
        scalar_first: bool,
    ) -> Result<StringOperand, EvalError> {
        let scalar = AppendSource::Borrowed(scalar_units);
        if scalar_first {
            self.append_pair(scalar, string)
        } else {
            self.append_pair(string, scalar)
        }
    }

    fn append_pair(
        &mut self,
        lhs: AppendSource<'_>,
        rhs: AppendSource<'_>,
    ) -> Result<StringOperand, EvalError> {
        let start = self.units.len();
        let length = lhs
            .len()
            .checked_add(rhs.len())
            .filter(|&length| length <= MAX_CONCAT_UNITS)
            .ok_or(EvalError::StringConcatenationLengthLimitExceeded)?;
        let end = start
            .checked_add(length)
            .filter(|&end| end <= MAX_APPENDED_UNITS)
            .ok_or(EvalError::StringConcatenationBudgetExceeded)?;
        let result = StringOperand::Runtime(RuntimeRange {
            // Both bounds fit by the inclusive aggregate check above.
            start: start as u16,
            end: end as u16,
        });
        if length == 0 {
            return Ok(result);
        }
        if end > self.units.capacity() {
            // Geometric growth with no request above the logical ceiling;
            // additional capacity is measured from len, not capacity.
            let target = end
                .max(self.units.capacity().saturating_mul(2))
                .min(MAX_APPENDED_UNITS);
            self.units
                .try_reserve_exact(target - start)
                .map_err(|_| EvalError::AllocationFailed)?;
        }
        // The entire append fits in reserved capacity: before each push,
        // len < end <= capacity, so Vec::push is guaranteed not to reallocate.
        // Runtime sources still lie within the old prefix, even for aliases.
        lhs.append_to(&mut self.units);
        rhs.append_to(&mut self.units);
        Ok(result)
    }
}

#[derive(Clone, Copy)]
pub(super) struct RuntimeRange {
    start: u16,
    end: u16,
}

impl RuntimeRange {
    fn indices(self) -> Range<usize> {
        usize::from(self.start)..usize::from(self.end)
    }
}

#[derive(Clone, Copy)]
pub(super) enum StringOperand {
    Literal(StringRange),
    Runtime(RuntimeRange),
}

impl StringOperand {
    pub(super) fn get(self, pools: StringPools<'_>) -> Option<&[u16]> {
        match self {
            Self::Literal(range) => range.get(pools.literals),
            Self::Runtime(range) => pools.runtime.get(range.indices()),
        }
    }

    pub(super) fn is_empty(self) -> bool {
        match self {
            Self::Literal(range) => range.is_empty(),
            Self::Runtime(range) => range.start == range.end,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct StringPools<'a> {
    literals: &'a [u16],
    runtime: &'a [u16],
}

#[cfg(test)]
impl<'a> StringPools<'a> {
    pub(super) fn literals(literals: &'a [u16]) -> Self {
        Self {
            literals,
            runtime: &[],
        }
    }
}

enum AppendSource<'a> {
    Borrowed(&'a [u16]),
    Runtime(Range<usize>),
}

impl<'a> AppendSource<'a> {
    fn checked(
        string: StringOperand,
        literals: &'a [u16],
        initialized: usize,
    ) -> Result<Self, EvalError> {
        match string {
            StringOperand::Literal(range) => range
                .get(literals)
                .map(Self::Borrowed)
                .ok_or(EvalError::InvalidScript),
            StringOperand::Runtime(range) => {
                let indices = range.indices();
                if indices.start > indices.end || indices.end > initialized {
                    return Err(EvalError::InvalidScript);
                }
                Ok(Self::Runtime(indices))
            }
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Borrowed(units) => units.len(),
            Self::Runtime(indices) => indices.len(),
        }
    }

    fn append_to(self, units: &mut Vec<u16>) {
        match self {
            Self::Borrowed(source) => {
                for &unit in source {
                    units.push(unit);
                }
            }
            Self::Runtime(indices) => {
                for index in indices {
                    let unit = units[index];
                    units.push(unit);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PrimitiveValue, Value, parse_script, parser::Instruction};

    #[test]
    fn aliases_across_growth_and_mixed_pool_appends_leave_snapshots_intact() {
        let text = "a".repeat(1023);
        let script = parse_script(&format!("'{text}' + 'b'")).unwrap();
        let [
            Instruction::String(a),
            Instruction::String(b),
            Instruction::Add,
        ] = script.instructions()
        else {
            panic!("expected two literals and an Add");
        };
        let mut scratch = EvalScratch::new();
        let literals = script.string_units();
        let a = StringOperand::Literal(*a);
        let b = StringOperand::Literal(*b);
        let first = scratch.concatenate(literals, a, b).unwrap();
        let before_growth = scratch.units.capacity();
        // Force the reserve branch for the two references into the old arena
        // on the pinned allocator; movement of the allocation is not assumed.
        assert!(before_growth < 3072);
        let twice = scratch.concatenate(literals, first, first).unwrap();
        assert!(scratch.units.capacity() >= 3072);
        let mixed = scratch.concatenate(literals, b, twice).unwrap();
        let mixed = scratch.concatenate(literals, mixed, b).unwrap();
        let block = format!("{text}b");
        let pools = scratch.pools(literals);
        assert_eq!(
            first.get(pools).unwrap(),
            block.encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(
            twice.get(pools).unwrap(),
            block.repeat(2).encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(
            mixed.get(pools).unwrap(),
            format!("b{block}{block}b")
                .encode_utf16()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn invalid_ranges_fail_before_growth_even_if_the_lhs_would_initialize_the_rhs() {
        let script = parse_script("'a'").unwrap();
        let [Instruction::String(a)] = script.instructions() else {
            panic!("expected a literal");
        };
        let a = StringOperand::Literal(*a);
        let uninitialized = StringOperand::Runtime(RuntimeRange { start: 0, end: 1 });
        let mut scratch = EvalScratch::new();
        let literals = script.string_units();
        assert_eq!(
            scratch.concatenate(literals, a, uninitialized).err(),
            Some(EvalError::InvalidScript)
        );
        assert_eq!(scratch.units.capacity(), 0);
        let first = scratch.concatenate(literals, a, a).unwrap();
        let old_capacity = scratch.units.capacity();
        for (lhs, rhs) in [
            (
                first,
                StringOperand::Runtime(RuntimeRange { start: 3, end: 3 }),
            ),
            (
                StringOperand::Runtime(RuntimeRange { start: 1, end: 0 }),
                first,
            ),
        ] {
            assert_eq!(
                scratch.concatenate(literals, lhs, rhs).err(),
                Some(EvalError::InvalidScript)
            );
            assert_eq!(scratch.units.capacity(), old_capacity);
            assert_eq!(scratch.units, [97, 97]);
        }
    }

    #[test]
    fn mixed_sources_preserve_old_ranges_across_growth_and_reject_invalid_ranges() {
        let text = "x".repeat(1024);
        let script = parse_script(&format!("'{text}'")).unwrap();
        let [Instruction::String(range)] = script.instructions() else {
            panic!("expected a literal");
        };
        let checked = StringOperand::Literal(*range);
        let literals = script.string_units();
        let number = Value::Number(-0.0);
        let mut scratch = EvalScratch::new();
        for scalar_first in [false, true] {
            assert_eq!(
                scratch
                    .concatenate_scalar(&literals[..1023], checked, number, scalar_first)
                    .err(),
                Some(EvalError::InvalidScript),
            );
        }
        assert_eq!(scratch.units.capacity(), 0);
        let first = scratch
            .concatenate_scalar(literals, checked, number, true)
            .unwrap();
        assert!(scratch.units.capacity() < 2051);
        let next = scratch
            .concatenate_scalar(literals, first, number, false)
            .unwrap();
        assert!(scratch.units.capacity() >= 2051);
        let pools = scratch.pools(literals);
        assert_eq!(
            first.get(pools).unwrap(),
            format!("0{text}").encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(
            next.get(pools).unwrap(),
            format!("0{text}0").encode_utf16().collect::<Vec<_>>()
        );
        let capacity = scratch.units.capacity();
        for range in [
            RuntimeRange {
                start: 2051,
                end: 2052,
            },
            RuntimeRange { start: 2, end: 1 },
        ] {
            for scalar_first in [false, true] {
                assert_eq!(
                    scratch
                        .concatenate_scalar(
                            literals,
                            StringOperand::Runtime(range),
                            number,
                            scalar_first
                        )
                        .err(),
                    Some(EvalError::InvalidScript),
                );
                assert_eq!(scratch.units.len(), 2051);
                assert_eq!(scratch.units.capacity(), capacity);
            }
        }
    }

    #[test]
    fn cold_scratch_is_lazy_and_warm_scratch_is_reset_on_reuse_and_error() {
        let mut scratch = EvalScratch::new();
        for (source, expected) in [
            ("42", PrimitiveValue::Scalar(Value::Number(42.0))),
            ("'a'", PrimitiveValue::String(&[97])),
            (
                "false && ('a' + 'b')",
                PrimitiveValue::Scalar(Value::Boolean(false)),
            ),
            ("let x = ''; x += ''; x", PrimitiveValue::String(&[])),
        ] {
            assert_eq!(
                parse_script(source)
                    .unwrap()
                    .evaluate_primitive_with_scratch(&mut scratch),
                Ok(expected)
            );
            assert_eq!(scratch.units.capacity(), 0, "{source}");
        }
        let part = "x".repeat(4094);
        let full = format!(
            "const s = '{part}'; let i = 0; while (i++ < 8) {{ s + ''; }} '1234567890123456' + ''; "
        );
        let at_limit = parse_script(&format!("{full}'' + ''")).unwrap();
        let over_limit = parse_script(&format!("{full}'a' + ''")).unwrap();
        for _ in 0..2 {
            assert_eq!(
                at_limit.evaluate_primitive_with_scratch(&mut scratch),
                Ok(PrimitiveValue::String(&[]))
            );
            assert_eq!(scratch.units.len(), MAX_APPENDED_UNITS);
            let capacity = scratch.units.capacity();
            assert_eq!(
                over_limit.evaluate_primitive_with_scratch(&mut scratch),
                Err(EvalError::StringConcatenationBudgetExceeded)
            );
            assert_eq!(scratch.units.len(), 0);
            assert_eq!(scratch.units.capacity(), capacity);
            assert_eq!(
                parse_script("'y' + ''")
                    .unwrap()
                    .evaluate_primitive_with_scratch(&mut scratch),
                Ok(PrimitiveValue::String(&[121]))
            );
            assert_eq!(scratch.units, [121]);
            assert_eq!(scratch.units.capacity(), capacity);
        }
    }
}
