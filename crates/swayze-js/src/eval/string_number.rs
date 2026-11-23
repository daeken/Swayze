use super::EvalError;
use crate::{
    parser::{MAX_STRING_LITERAL_CODE_UNITS, is_trivia_space},
    radix_integer::RadixInteger,
};

pub(super) const MAX_CREDITS: usize = 262_144;

// `units` is the original (untrimmed) slice from a checked StringRange.
pub(super) fn convert(mut units: &[u16], remaining: &mut usize) -> Result<f64, EvalError> {
    if units.len() > MAX_STRING_LITERAL_CODE_UNITS {
        return Err(EvalError::InvalidScript);
    }
    charge(remaining, 1 + units.len())?;
    while let Some((_, rest)) = units.split_first().filter(|&(&u, _)| is_space(u)) {
        units = rest;
    }
    while let Some((_, rest)) = units.split_last().filter(|&(&u, _)| is_space(u)) {
        units = rest;
    }
    let Some((&first, rest)) = units.split_first() else {
        return Ok(0.0);
    };

    if let [0x30, prefix, digits @ ..] = units {
        let width = match prefix {
            0x78 | 0x58 => 4, // x/X
            0x6F | 0x4F => 3, // o/O
            0x62 | 0x42 => 1, // b/B
            _ => 0,
        };
        if width != 0 {
            return Ok(radix_integer(digits, width).unwrap_or(f64::NAN));
        }
    }

    let magnitude = match first {
        0x2B | 0x2D => rest, // + or -
        _ => units,
    };
    let number = if magnitude
        .iter()
        .copied()
        .eq(b"Infinity".iter().copied().map(u16::from))
    {
        f64::INFINITY
    } else {
        // All remaining candidates pay for the decimal path before the
        // digit/dot gate, even an empty magnitude or invalid first unit.
        charge(remaining, 256 + 3 * magnitude.len())?;
        if matches!(magnitude.first(), Some(0x30..=0x39 | 0x2E)) {
            decimal(magnitude).unwrap_or(f64::NAN)
        } else {
            f64::NAN
        }
    };
    Ok(if first == u16::from(b'-') {
        -number
    } else {
        number
    })
}

fn charge(remaining: &mut usize, credits: usize) -> Result<(), EvalError> {
    *remaining = remaining
        .checked_sub(credits)
        .ok_or(EvalError::StringNumericConversionLimitExceeded)?;
    Ok(())
}

fn is_space(unit: u16) -> bool {
    // Every ECMAScript WhiteSpace/LineTerminator is BMP. Surrogate units
    // (paired or not) cannot be part of any such character.
    char::from_u32(u32::from(unit)).is_some_and(is_trivia_space)
}

fn radix_integer(digits: &[u16], width: u32) -> Option<f64> {
    if digits.is_empty() {
        return None;
    }
    let mut value = RadixInteger::default();
    for &unit in digits {
        let digit = char::from(u8::try_from(unit).ok()?).to_digit(1 << width)?;
        // Even once the accumulator saturates, keep validating digits.
        value.push(digit, width);
    }
    Some(value.number())
}

// Keep the fixed scratch on the decimal path, outside the dispatch loop.
#[inline(never)]
fn decimal(magnitude: &[u16]) -> Option<f64> {
    let mut scratch = [0_u8; MAX_STRING_LITERAL_CODE_UNITS];
    let bytes = scratch.get_mut(..magnitude.len())?;
    for (byte, &unit) in bytes.iter_mut().zip(magnitude) {
        let ascii = u8::try_from(unit).ok()?;
        if !ascii.is_ascii() {
            return None;
        }
        *byte = ascii;
    }
    // The digit/dot start gate excludes Rust's non-decimal spellings (e.g.
    // inf/NaN). For the remaining ASCII strings, the pinned Rust parser
    // accepts exactly the unsigned StringNumericLiteral decimal grammar and
    // rounds the full decimal, with no intermediate truncation here.
    std::str::from_utf8(bytes).ok()?.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_base_then_decimal_before_validating_the_suffix() {
        let error = EvalError::StringNumericConversionLimitExceeded;
        // Trim to `-1` and a lone surrogate. Only the two-unit magnitude
        // counts toward the decimal surcharge, even for this non-ASCII suffix.
        let units = [0x20, 0x2D, 0x31, 0xD800, 0x20];
        let base = 6;
        let surcharge = 262;
        let mut remaining = base - 1;
        assert_eq!(convert(&units, &mut remaining), Err(error));
        assert_eq!(remaining, base - 1);
        let mut remaining = base + surcharge - 1;
        assert_eq!(convert(&units, &mut remaining), Err(error));
        assert_eq!(remaining, surcharge - 1);
        let mut remaining = base + surcharge;
        assert!(convert(&units, &mut remaining).unwrap().is_nan());
        assert_eq!(remaining, 0);
    }

    #[test]
    fn only_empty_infinity_and_unsigned_radix_charge_base_alone() {
        for (text, expected) in [
            ("", 0.0),
            (" \t\n", 0.0),
            (" -Infinity ", f64::NEG_INFINITY),
            ("0b101", 5.0),
            ("0x", f64::NAN),
        ] {
            let units: Vec<_> = text.encode_utf16().collect();
            let mut remaining = 1 + units.len();
            let result = convert(&units, &mut remaining).unwrap();
            if expected.is_nan() {
                assert!(result.is_nan());
            } else {
                assert_eq!(result.to_bits(), expected.to_bits());
            }
            assert_eq!(remaining, 0, "{text:?}");
        }
    }

    #[test]
    fn charges_decimal_candidates_before_the_digit_or_dot_gate() {
        let error = EvalError::StringNumericConversionLimitExceeded;
        for (units, exact) in [
            (&[0x2D][..], 258),               // Sign only, zero-length magnitude.
            (&[0xD800], 261),                 // Unpaired surrogate, length one.
            (&[0x2B, 0x30, 0x62, 0x31], 270), // Signed 0b1, length three.
        ] {
            let mut remaining = exact - 1;
            assert_eq!(convert(units, &mut remaining), Err(error));
            assert_eq!(remaining, exact - 1 - (1 + units.len()));
            let mut remaining = exact;
            assert!(convert(units, &mut remaining).unwrap().is_nan());
            assert_eq!(remaining, 0);
        }
    }

    #[test]
    fn oversized_range_is_invalid_before_charging() {
        let units = [0x30; MAX_STRING_LITERAL_CODE_UNITS + 1];
        assert_eq!(convert(&units, &mut 0), Err(EvalError::InvalidScript));
    }
}
