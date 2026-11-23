use std::{collections::TryReserveError, ops::Range};

use super::{EvalError, Operand, Value};
use crate::{
    number_format,
    parser::{MAX_STRING_LITERAL_CODE_UNITS, StringRange},
};

// Keep every result within the fixed String-to-Number conversion bound.
const MAX_CONCAT_UNITS: usize = MAX_STRING_LITERAL_CODE_UNITS;
const MAX_APPENDED_UNITS: usize = 32_768;
const _: () = assert!(MAX_APPENDED_UNITS <= u16::MAX as usize);
const CHUNK_UNITS: usize = 256;
const _: () = assert!(MAX_APPENDED_UNITS.is_multiple_of(CHUNK_UNITS));
const _: () = assert!((MAX_APPENDED_UNITS / CHUNK_UNITS).is_power_of_two());
type Chunk = [u16; CHUNK_UNITS];

// Frozen UTF-16 spellings: no per-Script or scratch allocation. The only
// way to refer to one in an operand is by the private, payload-free tag.
#[derive(Clone, Copy)]
pub(super) enum TypeName {
    Number,
    Boolean,
    String,
    Object,
    Undefined,
}

impl TypeName {
    pub(super) fn units(self) -> &'static [u16] {
        static NUMBER: [u16; 6] = ascii_units(b"number");
        static BOOLEAN: [u16; 7] = ascii_units(b"boolean");
        static STRING: [u16; 6] = ascii_units(b"string");
        static OBJECT: [u16; 6] = ascii_units(b"object");
        static UNDEFINED: [u16; 9] = ascii_units(b"undefined");
        match self {
            Self::Number => &NUMBER,
            Self::Boolean => &BOOLEAN,
            Self::String => &STRING,
            Self::Object => &OBJECT,
            Self::Undefined => &UNDEFINED,
        }
    }
}

const fn ascii_units<const N: usize>(bytes: &[u8; N]) -> [u16; N] {
    assert!(N > 0 && N <= 9 && N <= MAX_CONCAT_UNITS && bytes.is_ascii());
    let mut units = [0; N];
    let mut i = 0;
    while i < N {
        units[i] = bytes[i] as u16;
        i += 1;
    }
    units
}

enum ScalarSpelling {
    Units(&'static [u16]),
    Number(f64),
}

fn scalar_spelling(value: Value) -> ScalarSpelling {
    let units: &[u16] = match value {
        Value::Boolean(true) => &[116, 114, 117, 101], // true
        Value::Boolean(false) => &[102, 97, 108, 115, 101], // false
        Value::Null => &[110, 117, 108, 108],          // null
        Value::Undefined => TypeName::Undefined.units(),
        Value::Number(number) => return ScalarSpelling::Number(number),
    };
    ScalarSpelling::Units(units)
}

/// Reusable storage for runtime Strings produced by Script evaluation.
///
/// The arena starts unallocated and grows only for nonempty appends.
/// Each evaluation resets its logical length but may reuse capacity. This is
/// not zeroization: previously written units may remain in the allocation.
#[derive(Default)]
pub struct EvalScratch {
    chunks: Vec<Chunk>,
    used: usize,
}

impl EvalScratch {
    /// Create empty evaluation scratch without allocating.
    pub const fn new() -> Self {
        Self {
            chunks: Vec::new(),
            used: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.used = 0;
    }

    fn runtime_units(&self) -> &[u16] {
        &self.chunks.as_flattened()[..self.used]
    }

    pub(super) fn pools<'a>(&'a self, literals: &'a [u16]) -> StringPools<'a> {
        StringPools {
            literals,
            runtime: self.runtime_units(),
        }
    }

    pub(super) fn concatenate(
        &mut self,
        literals: &[u16],
        lhs: StringOperand,
        rhs: StringOperand,
    ) -> Result<StringOperand, EvalError> {
        // Validate both ranges against their logical pools before checking
        // limits or growing. Runtime sources keep indices, not slices.
        let lhs = AppendSource::checked(lhs, literals, self.used)?;
        let rhs = AppendSource::checked(rhs, literals, self.used)?;
        self.append_pair(lhs, rhs)
    }

    pub(super) fn concatenate_scalar(
        &mut self,
        literals: &[u16],
        string: StringOperand,
        scalar: Value,
        scalar_first: bool,
    ) -> Result<StringOperand, EvalError> {
        let string = AppendSource::checked(string, literals, self.used)?;
        match scalar_spelling(scalar) {
            ScalarSpelling::Units(units) => self.append_scalar(string, units, scalar_first),
            ScalarSpelling::Number(number) => self.concatenate_number(string, number, scalar_first),
        }
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
        self.append_pair_with(lhs, rhs, Vec::try_reserve_exact)
    }

    fn append_pair_with(
        &mut self,
        lhs: AppendSource<'_>,
        rhs: AppendSource<'_>,
        reserve: impl FnOnce(&mut Vec<Chunk>, usize) -> Result<(), TryReserveError>,
    ) -> Result<StringOperand, EvalError> {
        let lhs_len = lhs.len();
        let range = self.prepare_append(lhs_len.checked_add(rhs.len()), reserve)?;
        let (start, end) = (usize::from(range.start), usize::from(range.end));
        if start == end {
            return Ok(StringOperand::Runtime(range));
        }
        // Borrow runtime sources only after growth. Both checked source ranges
        // lie in the old prefix, disjoint from both destinations even if the
        // sources alias each other. Do not publish the new length until copied.
        let (prefix, tail) = self.chunks.as_flattened_mut()[..end].split_at_mut(start);
        let (lhs_dest, rhs_dest) = tail.split_at_mut(lhs_len);
        lhs.copy_to(prefix, lhs_dest);
        rhs.copy_to(prefix, rhs_dest);
        self.used = end;
        Ok(StringOperand::Runtime(range))
    }

    #[inline(never)]
    pub(super) fn append_template(
        &mut self,
        literals: &[u16],
        prefix: StringOperand,
        substitution: Operand,
        following: StringRange,
    ) -> Result<StringOperand, EvalError> {
        let prefix = AppendSource::checked(prefix, literals, self.used)?;
        let following = following.get(literals).ok_or(EvalError::InvalidScript)?;
        let substitution = match substitution {
            Operand::String(string) => AppendSource::checked(string, literals, self.used)?,
            Operand::Scalar(scalar) => match scalar_spelling(scalar) {
                ScalarSpelling::Units(units) => AppendSource::Borrowed(units),
                ScalarSpelling::Number(number) => {
                    return self.append_template_number(prefix, number, following);
                }
            },
        };
        self.append_three(prefix, substitution, following, Vec::try_reserve_exact)
    }

    #[inline(never)]
    fn append_template_number(
        &mut self,
        prefix: AppendSource<'_>,
        number: f64,
        following: &[u16],
    ) -> Result<StringOperand, EvalError> {
        let mut buffer = [0; number_format::MAX_UNITS];
        let units = number_format::format_bits(number.to_bits(), &mut buffer);
        self.append_three(
            prefix,
            AppendSource::Borrowed(units),
            following,
            Vec::try_reserve_exact,
        )
    }

    fn append_three(
        &mut self,
        lhs: AppendSource<'_>,
        rhs: AppendSource<'_>,
        following: &[u16],
        reserve: impl FnOnce(&mut Vec<Chunk>, usize) -> Result<(), TryReserveError>,
    ) -> Result<StringOperand, EvalError> {
        let lhs_len = lhs.len();
        let rhs_len = rhs.len();
        let length = lhs_len
            .checked_add(rhs_len)
            .and_then(|sum| sum.checked_add(following.len()));
        let range = self.prepare_append(length, reserve)?;
        // As with the pair path, runtime indices point strictly into the old
        // logical prefix (or are empty at its end), even if they alias each
        // other. The three destinations are disjoint from it and each other.
        if range.start != range.end {
            let (prefix, tail) = self.chunks.as_flattened_mut()[..usize::from(range.end)]
                .split_at_mut(usize::from(range.start));
            let (lhs_dest, tail) = tail.split_at_mut(lhs_len);
            let (rhs_dest, following_dest) = tail.split_at_mut(rhs_len);
            lhs.copy_to(prefix, lhs_dest);
            rhs.copy_to(prefix, rhs_dest);
            following_dest.copy_from_slice(following);
            self.used = usize::from(range.end);
        }
        Ok(StringOperand::Runtime(range))
    }

    #[inline]
    fn prepare_append(
        &mut self,
        length: Option<usize>,
        reserve: impl FnOnce(&mut Vec<Chunk>, usize) -> Result<(), TryReserveError>,
    ) -> Result<RuntimeRange, EvalError> {
        let length = length
            .filter(|&length| length <= MAX_CONCAT_UNITS)
            .ok_or(EvalError::StringConcatenationLengthLimitExceeded)?;
        let start = self.used;
        let end = start
            .checked_add(length)
            .filter(|&end| end <= MAX_APPENDED_UNITS)
            .ok_or(EvalError::StringConcatenationBudgetExceeded)?;
        if end > self.chunks.len() * CHUNK_UNITS {
            self.grow(end, reserve)?;
        }
        // Both bounds fit by the inclusive aggregate check above.
        Ok(RuntimeRange {
            start: start as u16,
            end: end as u16,
        })
    }

    #[inline(never)]
    fn grow(
        &mut self,
        end: usize,
        reserve: impl FnOnce(&mut Vec<Chunk>, usize) -> Result<(), TryReserveError>,
    ) -> Result<(), EvalError> {
        let needed = end.div_ceil(CHUNK_UNITS);
        if needed > self.chunks.capacity() {
            // The rounded target fits the power-of-two chunk ceiling. Vec
            // measures additional capacity from initialized len, not capacity.
            let additional = needed.next_power_of_two() - self.chunks.len();
            reserve(&mut self.chunks, additional).map_err(|_| EvalError::AllocationFailed)?;
        }
        // Initialize only new chunks reached by this append. Vec::push is
        // guaranteed not to allocate while len < needed <= capacity.
        assert!(self.chunks.capacity() >= needed);
        while self.chunks.len() < needed {
            self.chunks.push([0; CHUNK_UNITS]);
        }
        Ok(())
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
    TypeName(TypeName),
}

impl StringOperand {
    pub(super) fn get(self, pools: StringPools<'_>) -> Option<&[u16]> {
        match self {
            Self::Literal(range) => range.get(pools.literals),
            Self::Runtime(range) => pools.runtime.get(range.indices()),
            Self::TypeName(name) => Some(name.units()),
        }
    }

    pub(super) fn is_empty(self) -> bool {
        match self {
            Self::Literal(range) => range.is_empty(),
            Self::Runtime(range) => range.start == range.end,
            Self::TypeName(_) => false,
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
    fn checked(string: StringOperand, literals: &'a [u16], used: usize) -> Result<Self, EvalError> {
        match string {
            StringOperand::Literal(range) => range
                .get(literals)
                .map(Self::Borrowed)
                .ok_or(EvalError::InvalidScript),
            StringOperand::TypeName(name) => Ok(Self::Borrowed(name.units())),
            StringOperand::Runtime(range) => {
                let indices = range.indices();
                if indices.start > indices.end || indices.end > used {
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

    fn copy_to(self, prefix: &[u16], dest: &mut [u16]) {
        let source = match self {
            Self::Borrowed(source) => source,
            Self::Runtime(indices) => &prefix[indices],
        };
        dest.copy_from_slice(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PrimitiveValue, Value, parse_script, parser::Instruction};

    #[test]
    fn template_appends_check_all_ranges_and_publish_only_after_success() {
        let script = parse_script("'abc'; ''").unwrap();
        let [
            Instruction::String(abc),
            Instruction::Discard,
            Instruction::String(empty),
        ] = script.instructions()
        else {
            panic!("expected literal ranges");
        };
        let literals = script.string_units();
        let mut scratch = EvalScratch::new();
        let stale = StringOperand::Runtime(RuntimeRange { start: 1, end: 1 });
        let literal = StringOperand::Literal(*abc);
        let empty_literal = StringOperand::Literal(*empty);
        for (prefix, sub, following, pool) in [
            (stale, Operand::Scalar(Value::Null), *empty, literals),
            (literal, Operand::String(stale), *empty, literals),
            (
                literal,
                Operand::Scalar(Value::Number(1e21)),
                *empty,
                &literals[..2],
            ),
            (
                StringOperand::TypeName(TypeName::String),
                Operand::String(StringOperand::TypeName(TypeName::String)),
                *empty,
                &literals[..2],
            ),
        ] {
            assert_eq!(
                scratch.append_template(pool, prefix, sub, following).err(),
                Some(EvalError::InvalidScript)
            );
            assert_eq!(scratch.used, 0);
            assert_eq!(scratch.chunks.capacity(), 0);
        }
        let output = scratch
            .append_template(
                literals,
                empty_literal,
                Operand::String(empty_literal),
                *empty,
            )
            .unwrap();
        assert!(matches!(output, StringOperand::Runtime(_)));
        assert_eq!(scratch.chunks.capacity(), 0);

        let text = [97; 200];
        let old = scratch
            .append_pair(AppendSource::Borrowed(&text), AppendSource::Borrowed(&[]))
            .unwrap();
        let old_capacity = scratch.chunks.capacity();
        let lhs = AppendSource::checked(old, literals, scratch.used).unwrap();
        let rhs = AppendSource::checked(old, literals, scratch.used).unwrap();
        assert_eq!(
            scratch
                .append_three(lhs, rhs, literals, |chunks, _| chunks
                    .try_reserve_exact(usize::MAX))
                .err(),
            Some(EvalError::AllocationFailed)
        );
        assert_eq!(scratch.chunks.capacity(), old_capacity);
        assert_eq!(scratch.chunks.len(), 1);
        assert_eq!(scratch.runtime_units(), text);
        let out = scratch
            .append_template(literals, old, Operand::String(old), *abc)
            .unwrap();
        assert_eq!(old.get(scratch.pools(literals)), Some(&text[..]));
        let result = out.get(scratch.pools(literals)).unwrap();
        assert_eq!(&result[..400], &[97; 400]);
        assert_eq!(&result[400..], literals);
        assert_eq!(scratch.used, 603);

        // An invalid empty range wins over both length and budget failures.
        scratch
            .grow(MAX_APPENDED_UNITS, Vec::try_reserve_exact)
            .unwrap();
        scratch.used = MAX_APPENDED_UNITS;
        let long = StringOperand::Runtime(RuntimeRange {
            start: 0,
            end: MAX_CONCAT_UNITS as u16,
        });
        let scalar = Operand::Scalar(Value::Number(1.0));
        assert_eq!(
            scratch.append_template(&[], long, scalar, *empty).err(),
            Some(EvalError::InvalidScript)
        );
        assert_eq!(
            scratch
                .append_template(literals, long, scalar, *empty)
                .err(),
            Some(EvalError::StringConcatenationLengthLimitExceeded)
        );
        assert_eq!(
            scratch
                .append_template(literals, empty_literal, scalar, *empty)
                .err(),
            Some(EvalError::StringConcatenationBudgetExceeded)
        );
        assert_eq!(scratch.used, MAX_APPENDED_UNITS);
    }

    #[test]
    fn typeof_spellings_do_not_use_the_script_pool_or_append_arena() {
        let script = parse_script("typeof x").unwrap();
        assert_eq!(
            script.instructions(),
            &[Instruction::Undefined, Instruction::Typeof]
        );
        assert!(script.string_units().is_empty());

        let mut scratch = EvalScratch::new();
        assert_eq!(
            script.evaluate_primitive_with_scratch(&mut scratch),
            Ok(PrimitiveValue::String(TypeName::Undefined.units()))
        );
        assert_eq!(scratch.used, 0);
        assert_eq!(scratch.chunks.capacity(), 0);

        let warm = parse_script("typeof ('a' + 'b')").unwrap();
        assert_eq!(
            warm.evaluate_primitive_with_scratch(&mut scratch),
            Ok(PrimitiveValue::String(TypeName::String.units()))
        );
        assert_eq!(scratch.used, 2); // Only the operand's concatenation appends.
        let chunks = scratch.chunks.capacity();
        assert!(chunks > 0);
        assert_eq!(
            script.evaluate_primitive_with_scratch(&mut scratch),
            Ok(PrimitiveValue::String(TypeName::Undefined.units()))
        );
        assert_eq!(scratch.used, 0);
        assert_eq!(scratch.chunks.capacity(), chunks);
    }

    #[test]
    fn string_length_checks_full_ranges_against_warm_logical_prefix() {
        use crate::eval::{Operand, string_length};
        let script = parse_script("'a' + 'b'").unwrap();
        let [
            Instruction::String(a),
            Instruction::String(b),
            Instruction::Add,
        ] = *script.instructions()
        else {
            panic!("expected two String operands and Add");
        };
        let mut scratch = EvalScratch::new();
        let literals = script.string_units();
        let warm = scratch
            .concatenate(
                literals,
                StringOperand::Literal(a),
                StringOperand::Literal(b),
            )
            .unwrap();
        assert_eq!(
            string_length(Operand::String(warm), scratch.pools(literals)),
            Ok(Value::Number(2.0))
        );
        scratch.reset();
        assert!(!scratch.chunks.is_empty());
        let read = |start, end| {
            string_length(
                Operand::String(StringOperand::Runtime(RuntimeRange { start, end })),
                scratch.pools(literals),
            )
        };
        assert_eq!(read(0, 0), Ok(Value::Number(0.0)));
        assert_eq!(read(0, 2), Err(EvalError::InvalidScript)); // stale initialized units
        assert_eq!(read(2, 2), Err(EvalError::InvalidScript)); // empty past logical end
        assert_eq!(read(1, 0), Err(EvalError::InvalidScript)); // inverted range
    }

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
        let before_growth = scratch.chunks.capacity();
        // Force the reserve branch for the two references into the old arena
        // on the pinned allocator; movement of the allocation is not assumed.
        assert!(before_growth < 12);
        let old_chunks = scratch.chunks.clone();
        let mut calls = 0;
        let lhs = AppendSource::checked(first, literals, scratch.used).unwrap();
        let rhs = AppendSource::checked(first, literals, scratch.used).unwrap();
        assert_eq!(
            scratch
                .append_pair_with(lhs, rhs, |chunks, additional| {
                    calls += 1;
                    assert_eq!(chunks.len(), 4);
                    assert_eq!(additional, 12); // target: 16, not 12 needed
                    chunks.try_reserve_exact(usize::MAX)
                })
                .err(),
            Some(EvalError::AllocationFailed)
        );
        assert_eq!(calls, 1);
        assert_eq!(scratch.used, 1024);
        assert_eq!(scratch.chunks, old_chunks);
        assert_eq!(scratch.chunks.capacity(), before_growth);
        let unpublished = StringOperand::Runtime(RuntimeRange {
            start: 1024,
            end: 3072,
        });
        assert_eq!(unpublished.get(scratch.pools(literals)), None);
        let twice = scratch.concatenate(literals, first, first).unwrap();
        assert!(scratch.chunks.capacity() >= 12);
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
        assert_eq!(scratch.chunks.capacity(), 0);
        assert!(scratch.chunks.is_empty());
        let first = scratch.concatenate(literals, a, a).unwrap();
        let old_capacity = scratch.chunks.capacity();
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
            assert_eq!(scratch.chunks.capacity(), old_capacity);
            assert_eq!(scratch.chunks.len(), 1);
            assert_eq!(scratch.runtime_units(), [97, 97]);
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
        assert_eq!(scratch.chunks.capacity(), 0);
        assert!(scratch.chunks.is_empty());
        let first = scratch
            .concatenate_scalar(literals, checked, number, true)
            .unwrap();
        assert!(scratch.chunks.capacity() < 9);
        let next = scratch
            .concatenate_scalar(literals, first, number, false)
            .unwrap();
        assert!(scratch.chunks.capacity() >= 9);
        let pools = scratch.pools(literals);
        assert_eq!(
            first.get(pools).unwrap(),
            format!("0{text}").encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(
            next.get(pools).unwrap(),
            format!("0{text}0").encode_utf16().collect::<Vec<_>>()
        );
        let capacity = scratch.chunks.capacity();
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
                assert_eq!(scratch.runtime_units().len(), 2051);
                assert_eq!(scratch.chunks.len(), 9);
                assert_eq!(scratch.chunks.capacity(), capacity);
            }
        }
    }

    #[test]
    fn requests_round_up_but_only_reached_chunks_are_initialized() {
        let mut scratch = EvalScratch::new();
        let mut requests = Vec::new();
        let mut previous_end = 0;
        for (end, needed, target) in [
            (513, 3, 4),
            (1024, 4, 4),
            (1025, 5, 8),
            (2048, 8, 8),
            (2049, 9, 16),
            (4097, 17, 32),
            (8191, 32, 32),
            (8193, 33, 64),
            (12287, 48, 64),
            (16381, 64, 64),
            (16385, 65, 128),
        ] {
            let before_capacity = scratch.chunks.capacity();
            let before_len = scratch.chunks.len();
            let before_requests = requests.len();
            let input = vec![97; end - previous_end];
            let result = scratch
                .append_pair_with(
                    AppendSource::Borrowed(&[]),
                    AppendSource::Borrowed(&input),
                    |chunks, additional| {
                        assert_eq!(chunks.len(), before_len);
                        requests.push(additional);
                        chunks.try_reserve_exact(additional)
                    },
                )
                .unwrap();
            let expected: &[_] = if needed > before_capacity {
                &[target - before_len]
            } else {
                &[]
            };
            assert_eq!(&requests[before_requests..], expected);
            assert_eq!(scratch.chunks.len(), needed);
            assert_eq!(scratch.used, end);
            assert_eq!(result.get(scratch.pools(&[])), Some(input.as_slice()));
            if end < needed * CHUNK_UNITS {
                assert_eq!(scratch.chunks.as_flattened()[end], 0);
            }
            previous_end = end;
        }
    }

    #[test]
    fn empty_and_limit_rejections_do_not_reserve_or_initialize() {
        let mut scratch = EvalScratch::new();
        let empty = AppendSource::Borrowed(&[]);
        assert_eq!(
            scratch
                .append_pair_with(empty, AppendSource::Borrowed(&[]), |_, _| panic!(
                    "empty reserve"
                ))
                .unwrap()
                .get(scratch.pools(&[])),
            Some(&[][..])
        );
        let long = [97; MAX_CONCAT_UNITS];
        assert_eq!(
            scratch
                .append_pair_with(
                    AppendSource::Borrowed(&long),
                    AppendSource::Borrowed(&[98]),
                    |_, _| panic!("rejected reserve")
                )
                .err(),
            Some(EvalError::StringConcatenationLengthLimitExceeded)
        );
        assert!(scratch.chunks.is_empty());
        assert_eq!(scratch.chunks.capacity(), 0);
        assert_eq!(scratch.used, 0);
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
            assert_eq!(scratch.chunks.capacity(), 0, "{source}");
            assert!(scratch.chunks.is_empty(), "{source}");
        }
        let part = "x".repeat(4094);
        let full = format!(
            "const s = '{part}'; let i = 0; while (i++ < 8) {{ s + ''; }} '1234567890123456' + ''; "
        );
        let at_limit = parse_script(&format!("{full}'' + ''")).unwrap();
        let over_limit = parse_script(&format!("{full}'a' + ''")).unwrap();
        let stale = StringOperand::Runtime(RuntimeRange { start: 1, end: 2 });
        let empty_at_end = StringOperand::Runtime(RuntimeRange { start: 1, end: 1 });
        let empty_past_end = StringOperand::Runtime(RuntimeRange { start: 2, end: 2 });
        for _ in 0..2 {
            assert_eq!(
                at_limit.evaluate_primitive_with_scratch(&mut scratch),
                Ok(PrimitiveValue::String(&[]))
            );
            assert_eq!(scratch.runtime_units().len(), MAX_APPENDED_UNITS);
            assert_eq!(scratch.chunks.len(), 128);
            let capacity = scratch.chunks.capacity();
            assert_eq!(
                scratch
                    .append_pair_with(
                        AppendSource::Borrowed(&[97]),
                        AppendSource::Borrowed(&[]),
                        |_, _| panic!("over-budget reserve")
                    )
                    .err(),
                Some(EvalError::StringConcatenationBudgetExceeded)
            );
            assert_eq!(scratch.used, MAX_APPENDED_UNITS);
            assert_eq!(scratch.chunks.len(), 128);
            assert_eq!(scratch.chunks.capacity(), capacity);
            assert_eq!(
                over_limit.evaluate_primitive_with_scratch(&mut scratch),
                Err(EvalError::StringConcatenationBudgetExceeded)
            );
            assert!(scratch.runtime_units().is_empty());
            assert_eq!(scratch.chunks.len(), 128);
            assert_eq!(scratch.chunks.capacity(), capacity);
            assert_eq!(stale.get(scratch.pools(&[])), None);
            assert_eq!(scratch.chunks[0][1], u16::from(b'x'));
            assert_eq!(
                parse_script("'y' + ''")
                    .unwrap()
                    .evaluate_primitive_with_scratch(&mut scratch),
                Ok(PrimitiveValue::String(&[121]))
            );
            assert_eq!(scratch.runtime_units(), [121]);
            assert_eq!(scratch.chunks.len(), 128);
            assert_eq!(scratch.chunks.capacity(), capacity);
            assert_eq!(stale.get(scratch.pools(&[])), None);
            assert_eq!(empty_past_end.get(scratch.pools(&[])), None);
            assert_eq!(
                scratch.concatenate(&[], empty_at_end, stale).err(),
                Some(EvalError::InvalidScript)
            );
            let empty = scratch
                .concatenate(&[], empty_at_end, empty_at_end)
                .unwrap();
            assert_eq!(empty.get(scratch.pools(&[])), Some(&[][..]));
            assert_eq!(scratch.runtime_units(), [121]);
            assert_eq!(scratch.chunks[0][1], u16::from(b'x'));
        }
    }
}
