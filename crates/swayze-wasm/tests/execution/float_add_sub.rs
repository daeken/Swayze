use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

#[derive(Clone, Copy)]
struct Float {
    add: u8,
    ty: u8,
    result: ResultKind,
    value: fn(u64) -> Value,
    h: u32,
    bias: u64,
    sign: u64,
}

const F32: Float = Float {
    add: 0x92,
    ty: 0x7d,
    result: ResultKind::F32,
    value: |bits| Value::F32(bits as u32),
    h: 23,
    bias: 127,
    sign: 1 << 31,
};
const F64: Float = Float {
    add: 0xa0,
    ty: 0x7c,
    result: ResultKind::F64,
    value: Value::F64,
    h: 52,
    bias: 1023,
    sign: 1 << 63,
};

impl Float {
    fn opcode(self, sub: bool) -> u8 {
        self.add + u8::from(sub)
    }

    fn infinity(self) -> u64 {
        (2 * self.bias + 1) << self.h
    }

    fn nan(self) -> u64 {
        self.infinity() | (1 << (self.h - 1))
    }

    fn check_bits(self, sub: bool, cases: impl IntoIterator<Item = (u64, u64, u64)>) {
        let opcode = self.opcode(sub);
        let ty = types(&[(&[self.ty, self.ty], Some(self.ty))]);
        let module = exported_function(&ty, &code_body(&[0, 0x20, 0, 0x20, 1, opcode, 0x0b]));
        let instance = module.instantiate().unwrap();
        for (lhs, rhs, expected) in cases {
            let args = [(self.value)(lhs), (self.value)(rhs)];
            assert_eq!(
                instance.invoke_with_values("answer", &args, self.result),
                Ok(Some((self.value)(expected))),
                "opcode {opcode:#x}, lhs {lhs:#x}, rhs {rhs:#x}"
            );
        }
    }

    // Finite nonzero inputs: check both operand orders as sums, their
    // sign reflections, and the corresponding differences. Cancellation
    // stays +0 on reflection; a nonzero result changes sign.
    fn check_effective_sums(self, cases: &[(u64, u64, u64)]) {
        for sub in [false, true] {
            let flip = if sub { self.sign } else { 0 };
            self.check_bits(
                sub,
                cases.iter().copied().flat_map(|(lhs, rhs, result)| {
                    let s = self.sign;
                    let reflected = if result == 0 { 0 } else { result ^ s };
                    [
                        (lhs, rhs ^ flip, result),
                        (rhs, lhs ^ flip, result),
                        (lhs ^ s, rhs ^ s ^ flip, reflected),
                        (rhs ^ s, lhs ^ s ^ flip, reflected),
                    ]
                }),
            );
        }
    }
}

#[test]
fn add_sub_checks_zero_infinity_and_nan_encodings() {
    for f in [F32, F64] {
        let [s, i, q] = [f.sign, f.infinity(), f.nan()];
        let one = f.bias << f.h;
        let values = [0, s, i, i | s];
        let sums = [
            [0, 0, i, i | s],
            [0, s, i, i | s],
            [i, i, i, q],
            [i | s, i | s, q, i | s],
        ];
        let differences = [
            [0, 0, i | s, i],
            [s, 0, i | s, i],
            [i, i, q, i],
            [i | s, i | s, i | s, q],
        ];
        for (sub, grid) in [(false, sums), (true, differences)] {
            let signed_specials = values.into_iter().zip(grid).flat_map(|(lhs, row)| {
                values
                    .into_iter()
                    .zip(row)
                    .map(move |(rhs, result)| (lhs, rhs, result))
            });
            let finite = [1, one, 1 | s, one | s];
            let flip = if sub { s } else { 0 };
            let mixed = values.into_iter().flat_map(|x| {
                finite.into_iter().flat_map(move |y| {
                    if x & !s == 0 {
                        [(x, y, y ^ flip), (y, x, y)]
                    } else {
                        [(x, y, x), (y, x, x ^ flip)]
                    }
                })
            });
            let nans = [i | 1, i | s | 1, q | 0x1234, q | s | 0x4321];
            let noncanonical_nans = nans.into_iter().flat_map(|nan| {
                values
                    .into_iter()
                    .chain(finite)
                    .chain(nans)
                    .flat_map(move |x| [(nan, x, q), (x, nan, q)])
            });
            f.check_bits(sub, signed_specials.chain(mixed).chain(noncanonical_nans));
        }
    }
}

#[test]
fn add_sub_preserves_exact_cancellation_and_subnormal_transitions() {
    for f in [F32, F64] {
        let n = 1 << f.h;
        let one = f.bias << f.h;
        let half_ulp = one - (u64::from(f.h) + 1) * n;
        let s = f.sign;
        f.check_effective_sums(&[
            (one, one | s, 0),
            (1, 1 | s, 0),
            (one, (one - 1) | s, half_ulp), // Adjacent binades, cancels h+1 leading places.
            (one, (one - 2) | s, half_ulp + n),
            (one + 1, one | s, half_ulp + n),
            (one + n, (one + 1) | s, one - 2),
            (n - 1, 1, n),       // Two subnormals sum to the least normal.
            (n, (n - 1) | s, 1), // Normal minus subnormal.
            (n, 1 | s, n - 1),
            (n + 1, n | s, 1),           // Two normals in the same binade.
            (2 * n, (2 * n - 1) | s, 1), // Two normals across a binade boundary.
            (2 * n + 1, 3 | s, 2 * n - 1),
            (n, n - 1, 2 * n - 1),
            (2 * n, 1, 2 * n), // First possible positive midpoint, even lower.
            (2 * n + 1, 1, 2 * n + 2), // First binade with an odd lower midpoint neighbor.
            (2 * n + 1, 2, 2 * n + 2),
        ]);
    }
}

#[test]
fn add_sub_rounds_once_at_precision_gaps_and_binade_edges() {
    for f in [F32, F64] {
        let n = 1 << f.h;
        let one = f.bias << f.h;
        let two = one + n;
        let half_ulp = one - (u64::from(f.h) + 1) * n;
        let inward_half = half_ulp - n;
        let s = f.sign;
        let quarter = one - 2 * n;
        let three_quarters = one - n / 2;
        f.check_effective_sums(&[
            (one, half_ulp - 1, one),
            (one, half_ulp, one), // Even lower; the next wider exponent gap is below half.
            (one, half_ulp + 1, one + 1),
            (one + 1, half_ulp - 1, one + 1),
            (one + 1, half_ulp, one + 2), // Odd lower.
            (one + 1, half_ulp + 1, one + 2),
            (one + 1, (half_ulp - 1) | s, one + 1),
            (one + 1, half_ulp | s, one), // Even lower.
            (one + 1, (half_ulp + 1) | s, one),
            (one + 2, (half_ulp - 1) | s, one + 2),
            (one + 2, half_ulp | s, one + 2), // Odd lower.
            (one + 2, (half_ulp + 1) | s, one + 1),
            // One's inward spacing is half its outward spacing. At gap
            // p+1, the first two cases are at and above the inward midpoint.
            // At cutoff p+2, the largest possible smaller operand (midpoint
            // predecessor) is strictly below, in either direction.
            (one, inward_half | s, one),
            (one, (inward_half + 1) | s, one - 1),
            (one, (inward_half - 1) | s, one),
            (one, inward_half + 1, one),
            (one, inward_half - 1, one),
            (one, 1 | s, one),
            (one, 1, one),
            // Crossing below a power of two makes the destination lattice
            // twice as fine; odd low input bits make ties on that lattice.
            (one, (quarter + 2) | s, three_quarters - 1),
            (one, (quarter + 3) | s, three_quarters - 2),
            (one, (quarter + 4) | s, three_quarters - 2),
            (one, (quarter + 5) | s, three_quarters - 2),
            (two - 1, half_ulp - 1, two - 1),
            (two - 1, half_ulp, two),
            (two - 1, half_ulp + 1, two),
        ]);
    }
}

#[test]
fn add_sub_handles_both_neighbors_of_the_max_finite_midpoints() {
    for f in [F32, F64] {
        let i = f.infinity();
        let max = i - 1;
        let half_ulp = (2 * f.bias - u64::from(f.h) - 1) << f.h;
        let s = f.sign;
        f.check_effective_sums(&[
            (max, half_ulp - 1, max),
            (max, half_ulp, i), // Odd retained max vs. even hypothetical next binade.
            (max, half_ulp + 1, i),
            (max, (half_ulp - 1) | s, max),
            (max, half_ulp | s, max - 1), // Even predecessor wins the inward tie.
            (max, (half_ulp + 1) | s, max - 1),
            (max, max, i), // Exact leading exponent already exceeds emax.
            (max, 1, max),
            (max, 1 | s, max),
        ]);
    }
}

fn finite_pairs(f: Float) -> impl Iterator<Item = (u64, u64)> {
    let mut state = 0xa6ba_cd57_9e22_4f31_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let n = 1 << f.h;
    let fields = 2 * f.bias + 1;
    (0..fields).flat_map(move |e| {
        let h = u64::from(f.h);
        let neighbors = [
            e,
            e.saturating_sub(1),
            e.saturating_sub(2),
            e.saturating_sub(h + 1),
            e.saturating_sub(h + 2),
            e.saturating_sub(h + 3),
            next() % fields,
        ];
        let random = neighbors.map(|other| {
            let a = (e << f.h) | (next() & (n - 1)) | (next() & f.sign);
            let b = (other << f.h) | (next() & (n - 1));
            [(a, b), (a, b | f.sign), (b, a), (b | f.sign, a)]
        });
        let edge = (e << f.h).max(1);
        random
            .into_iter()
            .flatten()
            .chain([(edge, edge - 1), (edge, (edge - 1) | f.sign)])
    })
}

#[test]
fn add_sub_matches_native_arithmetic_on_deterministic_finite_pairs() {
    // Test-only independent reference; evidence, not the alignment/rounding proof.
    for sub in [false, true] {
        F32.check_bits(
            sub,
            finite_pairs(F32).map(|(lhs, rhs)| {
                let (a, b) = (f32::from_bits(lhs as u32), f32::from_bits(rhs as u32));
                let expected = if sub { a - b } else { a + b };
                (lhs, rhs, u64::from(expected.to_bits()))
            }),
        );
        F64.check_bits(
            sub,
            finite_pairs(F64).map(|(lhs, rhs)| {
                let (a, b) = (f64::from_bits(lhs), f64::from_bits(rhs));
                let expected = if sub { a - b } else { a + b };
                (lhs, rhs, expected.to_bits())
            }),
        );
    }
}

#[test]
fn add_sub_checks_live_and_polymorphic_operand_and_result_types() {
    const F32_ZERO: &[u8] = b"\x43\0\0\0\0";
    const F64_ZERO: &[u8] = b"\x44\0\0\0\0\0\0\0\0";
    for (f, ty, zero, other_ty, other_zero) in [
        (F32, ValType::F32, F32_ZERO, ValType::F64, F64_ZERO),
        (F64, ValType::F64, F64_ZERO, ValType::F32, F32_ZERO),
    ] {
        for sub in [false, true] {
            let opcode = f.opcode(sub);
            let floor = [&[0, 0x41, 42][..], zero, &[0x02, 0x40]].concat();
            let tail = [opcode, 0x1a, 0x0b, 0x1a, 0x0b];
            let bad = |inputs: &[u8], kind| {
                bad_body(
                    &[&floor[..], inputs, &tail].concat(),
                    floor.len() + inputs.len(),
                    kind,
                );
            };
            bad(&[], ErrorKind::StackUnderflow { depth: 0 });
            bad(zero, ErrorKind::StackUnderflow { depth: 1 });
            bad(
                &[&[0x00][..], other_zero].concat(),
                ErrorKind::TypeMismatch {
                    expected: ty,
                    actual: other_ty,
                },
            );
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
            assert_body_result(&[&floor[..], zero, zero, &tail].concat(), 42);
            for inputs in [&[][..], zero] {
                let body = [&floor[..], &[0x00], inputs, &tail].concat();
                assert_eq!(
                    exported_function(TYPE, &code_body(&body))
                        .instantiate()
                        .unwrap()
                        .invoke_i32("answer"),
                    Err(InvokeError::Trap(Trap::Unreachable))
                );
            }
            for inputs in [[zero, zero].concat(), vec![0x00]] {
                let body = [&[0][..], &inputs, &[opcode, 0x0b]].concat();
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
}

#[test]
fn add_sub_costs_one_dispatch_at_the_fuel_boundary() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    for f in [F32, F64] {
        for sub in [false, true] {
            let opcode = f.opcode(sub);
            let ty = types(&[(&[0x7f, f.ty, f.ty], Some(f.ty))]);
            let mut body = vec![0, 0x03, 0x40, 0x20, 1, 0x20, 2, opcode, 0x1a];
            body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]); // 9 per loop.
            body.extend([0x01; 4]);
            body.extend([0x20, 1, 0x20, 2]);
            let final_op = body.len();
            body.extend([opcode, 0x0b]); // 113 * 9 + 7 = 1024.
            let run = |body: &[u8], lhs, rhs| {
                let args = [Value::I32(113), (f.value)(lhs), (f.value)(rhs)];
                exported_function(&ty, &code_body(body))
                    .instantiate()
                    .unwrap()
                    .invoke_with_values("answer", &args, f.result)
            };
            let n = 1 << f.h;
            let one = f.bias << f.h;
            let cases = [
                (n, n - 1, if sub { 1 } else { 2 * n - 1 }),
                (one, 1, one),
                (one, f.infinity() | 1, f.nan()),
            ];
            for (lhs, rhs, expected) in cases {
                assert_eq!(run(&body, lhs, rhs), Ok(Some((f.value)(expected))));
            }
            body.insert(final_op, 0x01);
            for (lhs, rhs, _) in cases {
                assert_eq!(
                    run(&body, lhs, rhs),
                    Err(InvokeError::LimitExceeded { limit })
                );
            }
        }
    }
}
