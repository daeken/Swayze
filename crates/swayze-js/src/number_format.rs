//! Private ECMAScript Number::toString (radix 10) for mixed concatenation.
//! The digit interval and decimal tie selection are handled by Ryū; notation
//! is chosen here, after rounding.

mod ryu;
mod tables;

#[cfg(test)]
mod tests;

pub(crate) const MAX_UNITS: usize = 25;
const FRACTION_MASK: u64 = (1 << 52) - 1;
const EXPONENT_MASK: u64 = (1 << 11) - 1;

/// Write one binary64 value as ASCII code points in a UTF-16 stack buffer.
pub(crate) fn format_bits(bits: u64, output: &mut [u16; MAX_UNITS]) -> &[u16] {
    let fraction = bits & FRACTION_MASK;
    let exponent = ((bits >> 52) & EXPONENT_MASK) as i32;
    let mut out = Writer { output, len: 0 };

    // Inspect raw bits: all NaN signs/payloads collapse to NaN, and both zeros
    // collapse to 0. All remaining values retain their sign.
    if exponent == 0x7ff && fraction != 0 {
        out.ascii(b"NaN");
    } else if exponent == 0 && fraction == 0 {
        out.push(b'0');
    } else {
        if bits >> 63 != 0 {
            out.push(b'-');
        }
        if exponent == 0x7ff {
            out.ascii(b"Infinity");
        } else {
            let (digits, power) = ryu::shortest(fraction, exponent);
            out.finite(digits, power);
        }
    }
    &out.output[..out.len]
}

struct Writer<'a> {
    output: &'a mut [u16; MAX_UNITS],
    len: usize,
}

impl Writer<'_> {
    fn push(&mut self, byte: u8) {
        self.output[self.len] = u16::from(byte);
        self.len += 1;
    }

    fn ascii(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.push(byte);
        }
    }

    fn zeros(&mut self, count: i32) {
        for _ in 0..count {
            self.push(b'0');
        }
    }

    // At most 17 iterations; no digit staging buffer. The first divisor is
    // at most 10^16, and is positive throughout the loop.
    fn digits(&mut self, mut value: u64, k: i32, point: i32) {
        let mut place = 10u64.pow((k - 1) as u32);
        for index in 0..k {
            if index == point {
                self.push(b'.');
            }
            self.push(b'0' + (value / place) as u8);
            value %= place;
            place /= 10;
        }
    }

    // With k shortest digits and n = k + power, the decimal point is n
    // places from the left. These are ECMA-262's n/k branches, not comparisons
    // against binary64 approximations of 1e-6 and 1e21.
    fn finite(&mut self, digits: u64, power: i32) {
        let k = digits.ilog10() as i32 + 1;
        let n = k + power;
        debug_assert!(k <= 17 && (-324..=308).contains(&(n - 1)));

        if k <= n && n <= 21 {
            // At most 21 digits (22 units with sign).
            self.digits(digits, k, -1);
            self.zeros(n - k);
        } else if 0 < n && n <= 21 {
            // 0 < n < k: at most 17 digits, one point and one sign.
            self.digits(digits, k, n);
        } else if -6 < n && n <= 0 {
            // At most 1 sign + 2 for "0." + 5 zeros + 17 digits = 25 units.
            self.ascii(b"0.");
            self.zeros(-n);
            self.digits(digits, k, -1);
        } else {
            // At most 1 sign + 17 digits + 1 point + 2 for "e±" + 3
            // exponent digits = 24 units. There is no leading exponent zero.
            self.digits(digits, k, 1);
            let exp = n - 1;
            self.ascii(if exp < 0 { b"e-" } else { b"e+" });
            let exp = u64::from(exp.unsigned_abs());
            self.digits(exp, exp.ilog10() as i32 + 1, -1);
        }
    }
}
