use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

#[derive(Clone, Copy)]
struct Mul {
    opcode: u8,
    float: u8,
    result: ResultKind,
    value: fn(u64) -> Value,
}

const F32: Mul = Mul {
    opcode: 0x94,
    float: 0x7d,
    result: ResultKind::F32,
    value: |bits| Value::F32(bits as u32),
};
const F64: Mul = Mul {
    opcode: 0xa2,
    float: 0x7c,
    result: ResultKind::F64,
    value: Value::F64,
};

impl Mul {
    fn check_bits(self, cases: impl IntoIterator<Item = (u64, u64, u64)>) {
        let ty = types(&[(&[self.float, self.float], Some(self.float))]);
        let module = exported_function(&ty, &code_body(&[0, 0x20, 0, 0x20, 1, self.opcode, 0x0b]));
        let instance = module.instantiate().unwrap();
        for (lhs, rhs, expected) in cases {
            let args = [(self.value)(lhs), (self.value)(rhs)];
            assert_eq!(
                instance.invoke_with_values("answer", &args, self.result),
                Ok(Some((self.value)(expected))),
                "opcode {:#x}, lhs {lhs:#x}, rhs {rhs:#x}",
                self.opcode
            );
        }
    }
}

#[test]
fn mul_checks_all_special_pairings_in_both_orders_and_signs() {
    for (op, sign, one, inf, nan) in [
        (F32, 1 << 31, 0x3f80_0000, 0x7f80_0000, 0x7fc0_0000),
        (
            F64,
            1 << 63,
            0x3ff0_0000_0000_0000,
            0x7ff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
        ),
    ] {
        let inputs = [0, 1, one, inf, nan | 0x1234, inf | 1];
        let expected = [
            [0, 0, 0, nan, nan, nan],
            [0, 0, 1, inf, nan, nan],
            [0, 1, one, inf, nan, nan],
            [nan, inf, inf, inf, nan, nan],
            [nan; 6],
            [nan; 6],
        ];
        op.check_bits(inputs.into_iter().zip(expected).flat_map(|(lhs, row)| {
            inputs.into_iter().zip(row).flat_map(move |(rhs, result)| {
                [0, sign].into_iter().flat_map(move |a| {
                    [0, sign].map(|b| {
                        (
                            lhs | a,
                            rhs | b,
                            if result == nan { nan } else { result | (a ^ b) },
                        )
                    })
                })
            })
        }));
    }
}

// Algebraically constructed bit vectors (no native floating-point oracle).
fn rounding_cases(
    h: u32,
    bias: u64,
    [a, b]: [u64; 2],
) -> impl IntoIterator<Item = (u64, u64, u64)> {
    let n = 1 << h;
    let one = bias << h;
    let two = one + n;
    let half = one - n;
    let one_and_half = one + n / 2;
    let two_and_half = two + n / 4;
    let largest_binade = (2 * bias) << h;
    let inf = largest_binade + n;
    let max_finite = inf - 1;
    let k = 1 << ((h - 2) / 2);
    let j = n / (2 * k);
    let even = one + j + k;
    // With these bounds both adjustments of a and b are representable and
    // change the product by less than one largest-binade ulp (N product units).
    assert!(n < a && a + 1 < b && b < 2 * n - 1);
    let wide_n = u128::from(n);
    assert_eq!(u128::from(a) * u128::from(b), (4 * wide_n - 1) * wide_n / 2);
    [
        // (1+k/N)(1+j/N) is exactly halfway above the even encoding.
        // Preserve k+j and decrease/increase k*j to sit just below/above.
        (one + k - 1, one + j + 1, even),
        (one + k, one + j, even),
        (one + k + 1, one + j - 1, even + 1),
        // 1.5*(1+1/N) is exactly halfway above an odd encoding.
        (one_and_half, one + 1, one_and_half + 2),
        // (N+1)(2N-2) = 2N²-2, just below 2 and rounds up to it.
        (one + 1, two - 2, two),
        // In units of the least subnormal, products are 0.5, 1.5, 2.5
        // and both sides of each tie.
        (1, half - 1, 0),
        (1, half, 0),
        (1, half + 1, 1),
        (1, one_and_half - 1, 1),
        (1, one_and_half, 2),
        (1, one_and_half + 1, 2),
        (1, two_and_half - 1, 2),
        (1, two_and_half, 2),
        (1, two_and_half + 1, 3),
        // The predecessor of 1 is 1 - 1/(2N). Multiplying it by min
        // normal gives N - 0.5 subnormal units, which ties to min normal.
        // The adjacent factors give N - 1 and N subnormal units exactly.
        (n, one - 2, n - 1),
        (n, one - 1, n),
        (n, one, n),
        // (N+1)(2N-3)/(2N) = N - 0.5 - 3/(2N) subnormal units.
        // A first rounding to normal precision would create a false tie.
        (n + 1, one - 3, n - 1),
        (n, half, n / 2),
        // Guard edges: for these powers of two times the least subnormal,
        // normalized product N² would need right shifts of 127 and 128.
        (1, (bias + 2 * u64::from(h) - 127) << h, 0),
        (1, (bias + 2 * u64::from(h) - 128) << h, 0),
        // a*b = (4N-1)*N/2. Scaling by the largest binade gives the
        // exact midpoint above max finite. a<b, so exchanging one unit
        // between them gives products just below and above that midpoint.
        (largest_binade + a - n - 1, one + b - n + 1, max_finite),
        (largest_binade + a - n, one + b - n, inf),
        (largest_binade + a - n + 1, one + b - n - 1, inf),
        (max_finite, one, max_finite),
        (max_finite, two, inf),
    ]
}

#[test]
fn mul_rounds_exact_midpoints_and_their_neighbors_at_each_width() {
    for (op, h, bias, sign, factors) in [
        (F32, 23, 127, 1 << 31, [0x96_4000, 0xda_1700]),
        (
            F64,
            52,
            1023,
            1 << 63,
            [0x0016_2ab0_3556_b800, 0x0017_1900_0000_0000],
        ),
    ] {
        // Exercise all sign combinations, including negative zero at the
        // min-subnormal times 0.5 tie with exactly one negative operand.
        op.check_bits(rounding_cases(h, bias, factors).into_iter().flat_map(
            |(lhs, rhs, result)| {
                [(lhs, rhs), (rhs, lhs)]
                    .into_iter()
                    .flat_map(move |(lhs, rhs)| {
                        [0, sign].into_iter().flat_map(move |a| {
                            [0, sign].map(|b| (lhs | a, rhs | b, result | (a ^ b)))
                        })
                    })
            },
        ));
    }
}

fn finite_pairs(h: u32, bias: u64) -> impl Iterator<Item = (u64, u64)> {
    let mut state = 0xb5ad_4ece_da1c_e2a9_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let fields = 2 * bias + 1;
    let sign_bit = h + fields.ilog2() + 1;
    let bits = move |e: u64, random: u64| {
        (e << h) | (random & ((1 << h) - 1)) | ((random >> 63) << sign_bit)
    };
    (0..fields).flat_map(move |lhs_e| {
        // An unrelated exponent, plus three biased exponent sums targeting
        // the least normal, values near one, and the highest binade.
        let [low, middle, high] =
            [bias + 1, 2 * bias, 3 * bias].map(|sum| sum.saturating_sub(lhs_e).min(fields - 1));
        [next() % fields, low, middle, high].map(|rhs_e| (bits(lhs_e, next()), bits(rhs_e, next())))
    })
}

#[test]
fn mul_matches_independent_native_reference_on_deterministic_finite_pairs() {
    // Independent test-only comparisons, not a proof of rounding correctness.
    F32.check_bits(finite_pairs(23, 127).map(|(lhs, rhs)| {
        let native = f32::from_bits(lhs as u32) * f32::from_bits(rhs as u32);
        (lhs, rhs, u64::from(native.to_bits()))
    }));
    F64.check_bits(finite_pairs(52, 1023).map(|(lhs, rhs)| {
        let native = f64::from_bits(lhs) * f64::from_bits(rhs);
        (lhs, rhs, native.to_bits())
    }));
}

#[test]
fn mul_validates_both_operands_and_the_result_in_live_and_dead_code() {
    const F32_ZERO: &[u8] = b"\x43\0\0\0\0";
    const F64_ZERO: &[u8] = b"\x44\0\0\0\0\0\0\0\0";
    for (op, ty, zero, other_ty, other_zero) in [
        (F32, ValType::F32, F32_ZERO, ValType::F64, F64_ZERO),
        (F64, ValType::F64, F64_ZERO, ValType::F32, F32_ZERO),
    ] {
        // Even a correctly typed value below the block floor is not an operand.
        let floor = [&[0, 0x41, 42][..], zero, &[0x02, 0x40]].concat();
        let tail = [op.opcode, 0x1a, 0x0b, 0x1a, 0x0b];
        let bad = |inputs: &[u8], kind| {
            bad_body(
                &[&floor[..], inputs, &tail].concat(),
                floor.len() + inputs.len(),
                kind,
            );
        };
        bad(&[], ErrorKind::StackUnderflow { depth: 0 });
        bad(zero, ErrorKind::StackUnderflow { depth: 1 });
        bad(other_zero, ErrorKind::StackUnderflow { depth: 1 });
        for prefix in [&[][..], &[0x00]] {
            for (wrong, actual) in [
                (other_zero, other_ty),
                (&[0x41, 0], ValType::I32),
                (&[0x42, 0], ValType::I64),
            ] {
                for [lhs, rhs] in [[wrong, zero], [zero, wrong]] {
                    bad(
                        &[prefix, lhs, rhs].concat(),
                        ErrorKind::TypeMismatch {
                            expected: ty,
                            actual,
                        },
                    );
                }
            }
        }
        bad(
            &[&[0x00][..], other_zero].concat(),
            ErrorKind::TypeMismatch {
                expected: ty,
                actual: other_ty,
            },
        );
        assert_body_result(&[&floor[..], zero, zero, &tail].concat(), 42);
        for inputs in [&[][..], zero] {
            // The dead floor supplies missing operands (but not wrong concrete types).
            let body = [&floor[..], &[0x00], inputs, &tail].concat();
            let module = exported_function(TYPE, &code_body(&body));
            assert_eq!(
                module.instantiate().unwrap().invoke_i32("answer"),
                Err(InvokeError::Trap(Trap::Unreachable))
            );
        }
        for inputs in [[zero, zero].concat(), vec![0x00]] {
            let body = [&[0][..], &inputs, &[op.opcode, 0x0b]].concat();
            bad_body(
                &body,
                body.len() - 1,
                ErrorKind::TypeMismatch {
                    expected: ValType::I32,
                    actual: ty,
                },
            );
        }
    }
}

#[test]
fn mul_costs_one_dispatch_at_the_fuel_boundary() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    for (op, one_and_half) in [(F32, 0x3fc0_0000), (F64, 0x3ff8_0000_0000_0000)] {
        let ty = types(&[(&[0x7f, op.float, op.float], Some(op.float))]);
        let args = [Value::I32(113), (op.value)(1), (op.value)(one_and_half)];
        let mut body = vec![0, 0x03, 0x40, 0x20, 1, 0x20, 2, op.opcode, 0x1a];
        body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]); // 9 per loop.
        body.extend([0x01; 4]);
        body.extend([0x20, 1, 0x20, 2]);
        let last = body.len();
        body.extend([op.opcode, 0x0b]); // 113 * 9 + 7 = 1024.
        let run = |body: &[u8]| {
            exported_function(&ty, &code_body(body))
                .instantiate()
                .unwrap()
                .invoke_with_values("answer", &args, op.result)
        };
        assert_eq!(
            run(&body),
            Ok(Some((op.value)(2))),
            "opcode {:#x}",
            op.opcode
        );
        body.insert(last, 0x01);
        assert_eq!(
            run(&body),
            Err(InvokeError::LimitExceeded { limit }),
            "opcode {:#x}",
            op.opcode
        );
    }
}
