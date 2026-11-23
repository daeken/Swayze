#[derive(Default)]
pub(crate) struct RadixInteger {
    bit_len: u32,
    top: u64,
    sticky: bool,
}

impl RadixInteger {
    const PRECISION: u32 = f64::MANTISSA_DIGITS;
    const MAX_BITS: u32 = f64::MAX_EXP as u32;

    pub(crate) fn push(&mut self, digit: u32, bits_per_digit: u32) {
        if self.bit_len > Self::MAX_BITS || (self.bit_len == 0 && digit == 0) {
            return;
        }
        // Each radix digit appends 1, 3 or 4 bits. Trim leading zeros only
        // from the first nonzero digit. Keep the top 53 bits and one guard
        // bit; all remaining bits contribute only to the sticky flag.
        let width = if self.bit_len == 0 {
            u32::BITS - digit.leading_zeros()
        } else {
            bits_per_digit
        };
        let keep = width.min((Self::PRECISION + 1).saturating_sub(self.bit_len));
        let discard = width - keep;
        self.top = (self.top << keep) | u64::from(digit >> discard);
        self.sticky |= digit & ((1 << discard) - 1) != 0;
        // Anything with more than 1024 significant bits is already >= 2^1024.
        // Saturate at that sentinel, independent of source length.
        self.bit_len = self.bit_len.saturating_add(width).min(Self::MAX_BITS + 1);
    }

    pub(crate) fn number(self) -> f64 {
        if self.bit_len <= Self::PRECISION {
            return self.top as f64; // Exact, including positive zero.
        }
        if self.bit_len > Self::MAX_BITS {
            return f64::INFINITY;
        }

        // The guard bit indicates at least half an ULP. Sticky distinguishes
        // larger remainders from exact ties, which round to an even low bit.
        let significand = self.top >> 1;
        let round_up = self.top & 1 != 0 && (self.sticky || significand & 1 != 0);
        let fraction_bits = Self::PRECISION - 1;
        let exponent_bias = Self::MAX_BITS - 1;
        let biased_exponent = u64::from(self.bit_len - 1 + exponent_bias);
        let fraction = significand + u64::from(round_up) - (1 << fraction_bits);
        // Addition (not OR) carries a rounded significand of 2^53 into
        // the exponent. At 2^1024 - 2^970 (and up), the carry produces
        // exponent 2047 and a zero fraction: positive infinity.
        f64::from_bits((biased_exponent << fraction_bits) + fraction)
    }
}
