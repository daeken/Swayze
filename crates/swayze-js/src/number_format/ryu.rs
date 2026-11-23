// Copyright 2018 Ulf Adams
// Adapted from dtolnay/ryu 1.0.23 under the Boost Software License 1.0.
// See ../../third-party/RYU.md for Rust and C sources and the full license.

//! Binary64's full-table Ryū digit core, without the upstream renderer.

use super::tables::{POW5, POW5_INV};

// For normalized multipliers, bit_length(5^e) = floor(log2(5^e)) + 1.
// This and the two log10 formulas below are the upstream fixed-point
// approximations. All arguments are <= 1076 and all products fit in i32.
fn pow5_bits(e: i32) -> i32 {
    ((e * 1_217_359) >> 19) + 1
}

// Returns (q, initial decimal exponent, multiplier, product shift).
// For finite nonzero binary64, e2 is in -1076..=969. See the crate-local
// notice for index bounds and the exact-arithmetic tests for the shift range.
fn scale(e2: i32) -> (i32, i32, (u64, u64), i32) {
    if e2 >= 0 {
        let q = ((e2 * 78_913) >> 18) - i32::from(e2 > 3);
        let shift = -e2 + q + 124 + pow5_bits(q);
        (q, q, POW5_INV[q as usize], shift)
    } else {
        let t = -e2;
        let q = ((t * 732_923) >> 20) - i32::from(t > 1);
        let i = t - q;
        let shift = q + 125 - pow5_bits(i);
        // Direct powers start at 5^1: 5^0 is unreachable in this branch.
        (q, q + e2, POW5[(i - 1) as usize], shift)
    }
}

fn mul_shift(m: u64, (lo, hi): (u64, u64), shift: i32) -> u64 {
    // m < 2^55, so the two limb products are below 2^119. Carrying
    // the high half of b0 into b2 computes floor(m * multiplier / 2^64).
    // The full product is < 2^181 and shift is in 118..=125; the final
    // right shift is in 54..=61 and the result is below 2^63.
    let b0 = u128::from(m) * u128::from(lo);
    let b2 = u128::from(m) * u128::from(hi);
    (((b0 >> 64) + b2) >> (shift - 64)) as u64
}

fn scaled_interval(mv: u64, mm: u64, mul: (u64, u64), shift: i32) -> (u64, u64, u64) {
    (
        mul_shift(mv, mul, shift),
        mul_shift(mv + 2, mul, shift),
        mul_shift(mm, mul, shift),
    )
}

fn multiple_of_power_of_5(mut value: u64, q: i32) -> bool {
    // Only called for q in 0..=21. Check exactly the needed divisibility,
    // without the upstream modular-inverse loop or an intermediate 5^q.
    for _ in 0..q {
        if !value.is_multiple_of(5) {
            return false;
        }
        value /= 5;
    }
    true
}

/// Return (integer significand, power of ten) for the magnitude of a finite
/// nonzero binary64. Inputs are the raw 52-bit fraction and 11-bit exponent.
/// At most 17 significant digits suffice (the binary64 round-trip bound).
pub(super) fn shortest(fraction: u64, exponent: i32) -> (u64, i32) {
    let (e2, m2) = if exponent == 0 {
        (-1076, fraction)
    } else {
        (exponent - 1077, (1 << 52) | fraction)
    };

    // Two extra low bits represent the exact half-ULP interval endpoints.
    // At a normal power of two above the smallest normal, the lower gap
    // is half the upper gap. An even binary significand owns its midpoints.
    let accept_bounds = m2 & 1 == 0;
    let mv = 4 * m2;
    let mm_shift = u64::from(fraction != 0 || exponent <= 1);
    let mm = mv - 1 - mm_shift;

    let (q, e10, mul, shift) = scale(e2);
    let (mut vr, mut vp, mut vm) = scaled_interval(mv, mm, mul, shift);
    let mut vm_is_trailing_zeros = false;
    let mut vr_is_trailing_zeros = false;
    if e2 >= 0 {
        if q <= 21 {
            // Only one of mv, mm and mv + 2 can be a multiple of five.
            if mv.is_multiple_of(5) {
                vr_is_trailing_zeros = multiple_of_power_of_5(mv, q);
            } else if accept_bounds {
                vm_is_trailing_zeros = multiple_of_power_of_5(mm, q);
            } else {
                // Exact upper endpoint is excluded for an odd significand.
                vp -= u64::from(multiple_of_power_of_5(mv + 2, q));
            }
        }
    } else if q <= 1 {
        vr_is_trailing_zeros = true;
        if accept_bounds {
            vm_is_trailing_zeros = mm_shift == 1;
        } else {
            vp -= 1;
        }
    } else if q < 63 {
        // No variable-width mask: count the factors of two in the nonzero mv.
        vr_is_trailing_zeros = mv.trailing_zeros() >= q as u32;
    }

    // Trim while the interval contains a shorter decimal. The main loops
    // divide a positive vp by ten without reaching zero (< 20 removals).
    // Initially vm >= 1 (mm >= 2, multiplier >= 2^124 and shift <= 125).
    // If division ever takes vm to zero here, its last digit was nonzero,
    // disabling the lower-bound loop. That second loop consumes only zeros
    // of a positive vm, so it also has fewer than 20 removals. Ryū's common
    // path initial /100 step is just two removals counted against vp.
    let mut removed = 0;
    let output = if vm_is_trailing_zeros || vr_is_trailing_zeros {
        let mut last_removed_digit = 0;
        while vp / 10 > vm / 10 {
            vm_is_trailing_zeros &= vm.is_multiple_of(10);
            vr_is_trailing_zeros &= last_removed_digit == 0;
            last_removed_digit = vr % 10;
            vr /= 10;
            vp /= 10;
            vm /= 10;
            removed += 1;
        }
        if vm_is_trailing_zeros {
            // The exact lower endpoint is available (even binary significand).
            while vm.is_multiple_of(10) {
                vr_is_trailing_zeros &= last_removed_digit == 0;
                last_removed_digit = vr % 10;
                vr /= 10;
                vm /= 10;
                removed += 1;
            }
        }
        if vr_is_trailing_zeros && last_removed_digit == 5 && vr % 2 == 0 {
            // Exactly halfway between two decimal candidates: prefer even.
            last_removed_digit = 4;
        }
        vr + u64::from(
            (vr == vm && (!accept_bounds || !vm_is_trailing_zeros)) || last_removed_digit >= 5,
        )
    } else {
        let mut round_up = false;
        if vp / 100 > vm / 100 {
            round_up = vr % 100 >= 50;
            vr /= 100;
            vp /= 100;
            vm /= 100;
            removed += 2;
        }
        while vp / 10 > vm / 10 {
            round_up = vr % 10 >= 5;
            vr /= 10;
            vp /= 10;
            vm /= 10;
            removed += 1;
        }
        vr + u64::from(vr == vm || round_up)
    };
    (output, e10 + removed)
}

#[cfg(test)]
mod checks;
