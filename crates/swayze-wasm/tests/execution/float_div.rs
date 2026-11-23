use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

#[derive(Clone, Copy)]
struct Div {
    opcode: u8,
    float: u8,
    result: ResultKind,
    value: fn(u64) -> Value,
    h: u32,
    bias: u64,
    sign: u64,
}

const F32: Div = Div {
    opcode: 0x95,
    float: 0x7d,
    result: ResultKind::F32,
    value: |bits| Value::F32(bits as u32),
    h: 23,
    bias: 127,
    sign: 1 << 31,
};
const F64: Div = Div {
    opcode: 0xa3,
    float: 0x7c,
    result: ResultKind::F64,
    value: Value::F64,
    h: 52,
    bias: 1023,
    sign: 1 << 63,
};

impl Div {
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

    fn signed_pairs(
        self,
        lhs: u64,
        rhs: u64,
        result: u64,
    ) -> impl Iterator<Item = (u64, u64, u64)> {
        let nan = ((2 * self.bias + 1) << self.h) | (1 << (self.h - 1));
        [0, self.sign].into_iter().flat_map(move |a| {
            [0, self.sign].into_iter().map(move |b| {
                (
                    lhs | a,
                    rhs | b,
                    if result == nan { nan } else { result | (a ^ b) },
                )
            })
        })
    }
}

#[test]
fn div_checks_the_special_value_cross_product_and_signs() {
    for op in [F32, F64] {
        let one = op.bias << op.h;
        let inf = (2 * op.bias + 1) << op.h;
        let nan = inf | (1 << (op.h - 1));
        let inputs = [0, 1, one, inf, nan | 0x1234, inf | 1];
        // Rows are numerators; columns are denominators in the same order.
        let expected = [
            [nan, 0, 0, 0, nan, nan],
            [inf, one, 1, 0, nan, nan],
            [inf, inf, one, 0, nan, nan],
            [inf, inf, inf, nan, nan, nan],
            [nan; 6],
            [nan; 6],
        ];
        op.check_bits(inputs.into_iter().zip(expected).flat_map(|(lhs, row)| {
            inputs
                .into_iter()
                .zip(row)
                .flat_map(move |(rhs, result)| op.signed_pairs(lhs, rhs, result))
        }));
    }
}

#[test]
fn div_rounds_normal_results_and_crosses_the_overflow_boundary() {
    for (op, third) in [(F32, 0x3eaa_aaab), (F64, 0x3fd5_5555_5555_5555)] {
        let n = 1 << op.h;
        let one = op.bias << op.h;
        let half = one - n;
        let two = one + n;
        let three = two + n / 2;
        let four = two + n;
        let six = three + n;
        let inf = (2 * op.bias + 1) << op.h;
        let max = inf - 1;
        // In a normal quantum, the numerator is shifted by h or h+1, so
        // 2*numerator has at least h+1 low zero bits. An exact midpoint would
        // make this equal to an odd multiple of the h+1-bit denominator,
        // which has at most h low zero bits. These cases miss by just one.
        let near_midpoints = [
            (one, two - 1, half + 1),                // 2*remainder - denominator = +1.
            (one + n / 2 + 2, one + 1, one + n / 2), // ... = -1.
        ];
        // The exact normalized significand cannot exceed 2 - 1/n. Its
        // maximum for lhs_m >= rhs_m is (2n-1)/n; for lhs_m < rhs_m the
        // maximum is 2*(2n-2)/(2n-1), which is smaller. Thus there is no
        // finite ratio in the half-gap between max and the next binade,
        // and no overflow tie or normal rounding carry can occur.
        let cases = [
            (one, three, third),
            (four, six, third + n),
            (six, four, one + n / 2),
            (1, 2, half), // Both operands are subnormal.
            (n, n - 1, one + 1),
            (max, one + 1, max - 2), // Largest-binade significand: 2n-3 + 3/(n+1).
            (max, one, max),
            (max, one - 1, inf), // Exactly 2^(emax+1) before overflow.
            (max, 1, inf),
        ];
        op.check_bits(
            cases
                .into_iter()
                .chain(near_midpoints)
                .flat_map(|(lhs, rhs, result)| op.signed_pairs(lhs, rhs, result)),
        );
    }
}

#[test]
fn div_rounds_once_at_the_subnormal_quantum() {
    for op in [F32, F64] {
        let n = 1 << op.h;
        let two = (op.bias + 1) << op.h;
        let four = two + n;
        let max = ((2 * op.bias + 1) << op.h) - 1;
        let cases = [
            // With rhs=2, exact results in least-subnormal units are
            // 0.5, 1.5, 2.5, and n-0.5. Larger/smaller rhs straddle each tie.
            (1, two + 1, 0),
            (1, two, 0),
            (1, two - 1, 1),
            (3, two + 1, 1),
            (3, two, 2),
            (3, two - 1, 2),
            (5, two + 1, 2),
            (5, two, 2),
            (5, two - 1, 3),
            // n-1.5 + 3/(2(n+1)): normal precision first creates a
            // false n-1.5 tie, incorrectly rounding to n-2.
            (2 * n - 1, two + 1, n - 1),
            (2 * n - 1, two, n), // An odd tie carries to least normal.
            (2 * n - 1, two - 1, n),
            // n-0.5 - 1/(2(2n-1)): rounding to normal precision first
            // creates a false n-0.5 tie and thus rounds incorrectly to n.
            (2 * n - 2, two - 1, n - 1),
            (n, two, n / 2),
            (1, four, 0), // Strictly below half a subnormal unit.
            (1, max, 0),
        ];
        op.check_bits(
            cases
                .into_iter()
                .flat_map(|(lhs, rhs, result)| op.signed_pairs(lhs, rhs, result)),
        );
    }
}

fn finite_pairs(op: Div) -> impl Iterator<Item = (u64, u64)> {
    let mut state = 0x8f6a_e894_1b93_4cde_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let fields = 2 * op.bias + 1;
    let bits = move |e, random: u64| {
        let fraction = (random & ((1 << op.h) - 1)).max(u64::from(e == 0));
        let sign = if random >> 63 == 0 { 0 } else { op.sign };
        (e << op.h) | fraction | sign
    };
    (0..fields).flat_map(move |lhs_e| {
        // Sample across all finite exponent fields, targeting both sides of
        // underflow and overflow, plus exponents near one and unrelated pairs.
        let exponents = [
            lhs_e,
            lhs_e.saturating_add(op.bias).min(fields - 1),
            lhs_e
                .saturating_add(op.bias + u64::from(op.h))
                .min(fields - 1),
            lhs_e.saturating_sub(op.bias),
            next() % fields,
        ];
        exponents
            .map(|rhs_e| {
                let (lhs, rhs) = (bits(lhs_e, next()), bits(rhs_e, next()));
                [(lhs, rhs), (rhs, lhs)]
            })
            .into_iter()
            .flatten()
    })
}

#[test]
fn div_matches_native_division_on_deterministic_finite_pairs() {
    // Test-only reference evidence; the bounds and exact rounding argument
    // do not depend on native floating-point operations.
    F32.check_bits(finite_pairs(F32).map(|(lhs, rhs)| {
        let native = f32::from_bits(lhs as u32) / f32::from_bits(rhs as u32);
        (lhs, rhs, u64::from(native.to_bits()))
    }));
    F64.check_bits(finite_pairs(F64).map(|(lhs, rhs)| {
        let native = f64::from_bits(lhs) / f64::from_bits(rhs);
        (lhs, rhs, native.to_bits())
    }));
}

#[test]
fn div_checks_live_and_polymorphic_operand_and_result_types() {
    const F32_ZERO: &[u8] = b"\x43\0\0\0\0";
    const F64_ZERO: &[u8] = b"\x44\0\0\0\0\0\0\0\0";
    for (op, ty, zero, other_ty, other_zero) in [
        (F32, ValType::F32, F32_ZERO, ValType::F64, F64_ZERO),
        (F64, ValType::F64, F64_ZERO, ValType::F32, F32_ZERO),
    ] {
        let floor = [&[0, 0x41, 42][..], zero, &[0x02, 0x40]].concat();
        let tail = [op.opcode, 0x1a, 0x0b, 0x1a, 0x0b];
        let bad = |inputs: &[u8], kind| {
            bad_body(
                &[&floor[..], inputs, &tail].concat(),
                floor.len() + inputs.len(),
                kind,
            );
        };
        // A value below the block floor is not an operand, even with the
        // right type. In a dead tail, only missing operands are polymorphic.
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
fn div_costs_one_dispatch_at_the_fuel_boundary() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    for op in [F32, F64] {
        let ty = types(&[(&[0x7f, op.float, op.float], Some(op.float))]);
        let mut body = vec![0, 0x03, 0x40, 0x20, 1, 0x20, 2, op.opcode, 0x1a];
        body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]); // 9 per loop.
        body.extend([0x01; 4]);
        body.extend([0x20, 1, 0x20, 2]);
        let div = body.len();
        body.extend([op.opcode, 0x0b]); // 113 * 9 + 7 = 1024.
        let two = (op.bias + 1) << op.h;
        let inf = (2 * op.bias + 1) << op.h;
        let run = |body: &[u8], rhs| {
            let args = [Value::I32(113), (op.value)(3), (op.value)(rhs)];
            exported_function(&ty, &code_body(body))
                .instantiate()
                .unwrap()
                .invoke_with_values("answer", &args, op.result)
        };
        assert_eq!(run(&body, two), Ok(Some((op.value)(2))));
        assert_eq!(run(&body, 0), Ok(Some((op.value)(inf))));
        body.insert(div, 0x01);
        for rhs in [two, 0] {
            assert_eq!(run(&body, rhs), Err(InvokeError::LimitExceeded { limit }));
        }
    }
}
