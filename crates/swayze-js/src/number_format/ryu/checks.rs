//! Independent exact-integer checks, used only in tests. No Ryū log
//! constants, bit-split product, or precomputed data in the regeneration.

use std::cmp::Ordering;

use super::{POW5, POW5_INV, pow5_bits, scale};

/// Little-endian base-2^32 limbs, with no high zero limbs.
#[derive(Clone)]
struct Big(Vec<u32>);

impl Big {
    fn times(&mut self, factor: u32) {
        let mut carry = 0;
        for limb in &mut self.0 {
            let word = u64::from(*limb) * u64::from(factor) + carry;
            *limb = word as u32;
            carry = word >> 32;
        }
        if carry != 0 {
            self.0.push(carry as u32);
        }
    }

    fn cmp(&self, rhs: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&rhs.0.len())
            .then_with(|| self.0.iter().rev().cmp(rhs.0.iter().rev()))
    }

    fn sub(&mut self, rhs: &Self) {
        let mut borrow = false;
        for (i, limb) in self.0.iter_mut().enumerate() {
            let (difference, borrow_rhs) = limb.overflowing_sub(rhs.0.get(i).copied().unwrap_or(0));
            let (difference, borrow_low) = difference.overflowing_sub(u32::from(borrow));
            *limb = difference;
            borrow = borrow_rhs || borrow_low;
        }
        assert!(!borrow);
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    fn bit_length(&self) -> i32 {
        let most = self.0.last().unwrap();
        (32 * self.0.len() as i32) - most.leading_zeros() as i32
    }

    fn bit(&self, i: i32) -> u128 {
        if i < 0 {
            return 0;
        }
        let word = self.0.get(i as usize / 32).copied().unwrap_or(0);
        u128::from((word >> (i % 32)) & 1)
    }

    fn normalized_125(&self) -> u128 {
        let low = self.bit_length() - 125;
        (low..low + 125)
            .rev()
            .fold(0, |out, i| 2 * out + self.bit(i))
    }

    fn floor_pow2_div(&self, power: i32) -> u128 {
        // Binary long division of 1 followed by `power` zero bits. The
        // running remainder is a Big; the resulting quotient fits in u128.
        let mut remainder = Self(vec![1]);
        let mut quotient = 0;
        for index in 0..=power {
            if index != 0 {
                remainder.times(2);
            }
            quotient *= 2;
            if remainder.cmp(self) != Ordering::Less {
                remainder.sub(self);
                quotient += 1;
            }
        }
        quotient
    }
}

fn u128_from_limbs((lo, hi): (u64, u64)) -> u128 {
    (u128::from(hi) << 64) | u128::from(lo)
}

#[test]
fn regenerates_every_retained_multiplier() {
    let mut five = Big(vec![1]);
    for i in 0..=POW5.len() {
        let bits = five.bit_length();
        assert_eq!(pow5_bits(i as i32), bits, "5^{i}");
        if let Some(&inv) = POW5_INV.get(i) {
            let regenerated = five.floor_pow2_div(124 + bits) + 1;
            assert_eq!(u128_from_limbs(inv), regenerated, "inverse 5^{i}");
        }
        if i != 0 {
            assert_eq!(u128_from_limbs(POW5[i - 1]), five.normalized_125(), "5^{i}");
        }
        five.times(5);
    }
    assert!(
        POW5_INV
            .iter()
            .chain(&POW5)
            .all(|&limbs| { (1 << 124..=(1 << 125) + 1).contains(&u128_from_limbs(limbs)) })
    );
}

// Advance exact powers of ten and assert the defining log inequality. The
// caller visits increasing powers of two or five, starting with 1.
fn floor_log10(value: &Big, decade: &mut Big, log: &mut i32) {
    while decade.cmp(value) != Ordering::Greater {
        decade.times(10);
        *log += 1;
    }
}

#[test]
fn all_binary64_scales_use_exact_logs_and_reach_only_the_retained_indices() {
    let mut inverse_seen = [false; POW5_INV.len()];
    let mut direct_seen = [false; POW5.len()];
    let mut two = Big(vec![1]);
    let mut five = Big(vec![1]);
    let mut two_decade = Big(vec![1]);
    let mut five_decade = Big(vec![1]);
    let (mut two_log, mut five_log) = (-1, -1);
    for t in 0..=1076 {
        if t <= 969 {
            floor_log10(&two, &mut two_decade, &mut two_log);
            let (q, power, mul, shift) = scale(t);
            assert_eq!(q, two_log - i32::from(t > 3), "e2={t}");
            assert_eq!(power, q);
            inverse_seen[q as usize] = true;
            assert_eq!(mul, POW5_INV[q as usize]);
            assert!((118..=125).contains(&shift));
            two.times(2);
        }
        if t != 0 {
            floor_log10(&five, &mut five_decade, &mut five_log);
            let (q, power, mul, shift) = scale(-t);
            assert_eq!(q, five_log - i32::from(t > 1), "e2=-{t}");
            assert_eq!(power, q - t);
            let index = (t - q - 1) as usize;
            direct_seen[index] = true;
            assert_eq!(mul, POW5[index]);
            assert!((118..=122).contains(&shift));
        }
        five.times(5);
    }
    assert!(inverse_seen.into_iter().all(|seen| seen));
    assert!(direct_seen.into_iter().all(|seen| seen));
}
