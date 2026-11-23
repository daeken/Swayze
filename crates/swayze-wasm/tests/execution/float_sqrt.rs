use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

const F32_SQRT: u8 = 0x91;
const F64_SQRT: u8 = 0x9f;
const F32_ZERO: &[u8] = b"\x43\0\0\0\0";
const F64_ZERO: &[u8] = b"\x44\0\0\0\0\0\0\0\0";

fn check_bits(
    opcode: u8,
    float: u8,
    result: ResultKind,
    value: impl Fn(u64) -> Value,
    cases: impl IntoIterator<Item = (u64, u64)>,
) {
    let ty = types(&[(&[float], Some(float))]);
    let module = exported_function(&ty, &code_body(&[0, 0x20, 0, opcode, 0x0b]));
    let instance = module.instantiate().unwrap();
    for (input, expected) in cases {
        assert_eq!(
            instance.invoke_with_values("answer", &[value(input)], result),
            Ok(Some(value(expected))),
            "opcode {opcode:#x}, input {input:#x}"
        );
    }
}

fn special_cases(sign: u64, infinity: u64, nan: u64, one: u64) -> impl Iterator<Item = (u64, u64)> {
    let preserved = [0, sign, infinity].map(|bits| (bits, bits));
    let invalid = [sign | 1, sign | one, sign | (infinity - 1), sign | infinity];
    let nans = [0, sign]
        .into_iter()
        .flat_map(move |s| [infinity + 1, nan, nan | 0x1234].map(|bits| s | bits));
    preserved
        .into_iter()
        .chain(invalid.into_iter().chain(nans).map(move |bits| (bits, nan)))
}

#[test]
fn sqrt_handles_special_values_and_rounds_edge_cases_to_exact_bits() {
    check_bits(
        F32_SQRT,
        0x7d,
        ResultKind::F32,
        |bits| Value::F32(bits as u32),
        special_cases(0x8000_0000, 0x7f80_0000, 0x7fc0_0000, 0x3f80_0000).chain([
            (0x0000_0001, 0x1a35_04f3), // Smallest subnormal, odd exponent.
            (0x0000_0002, 0x1a80_0000), // Exact subnormal input root.
            (0x007f_ffff, 0x1fff_ffff), // Just below the smallest normal input.
            (0x0080_0000, 0x2000_0000),
            (0x0080_0001, 0x2000_0000),
            (0x3f80_0001, 0x3f80_0000), // Just below the squared midpoint above 1.
            (0x3f80_0002, 0x3f80_0001),
            (0x3fc7_fffe, 0x3f9f_ffff), // Two input ulps below 1.25^2.
            (0x3fc7_ffff, 0x3fa0_0000), // Round up to 1.25, carrying within the significand.
            (0x407f_ffff, 0x3fff_ffff), // No exponent carry just below 4.
            (0x4080_0000, 0x4000_0000),
            (0x4080_0001, 0x4000_0000),
            (0x4110_0000, 0x4040_0000), // sqrt(9) = 3.
            (0x7f7f_ffff, 0x5f7f_ffff), // Largest finite input does not round up.
        ]),
    );
    check_bits(
        F64_SQRT,
        0x7c,
        ResultKind::F64,
        Value::F64,
        special_cases(
            0x8000_0000_0000_0000,
            0x7ff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
            0x3ff0_0000_0000_0000,
        )
        .chain([
            (0x0000_0000_0000_0001, 0x1e60_0000_0000_0000), // Smallest subnormal, exact root.
            (0x0000_0000_0000_0002, 0x1e66_a09e_667f_3bcd), // Odd exponent.
            (0x000f_ffff_ffff_ffff, 0x1fff_ffff_ffff_ffff),
            (0x0010_0000_0000_0000, 0x2000_0000_0000_0000),
            (0x0010_0000_0000_0001, 0x2000_0000_0000_0000),
            (0x3ff0_0000_0000_0001, 0x3ff0_0000_0000_0000),
            (0x3ff0_0000_0000_0002, 0x3ff0_0000_0000_0001),
            (0x3ff8_ffff_ffff_fffe, 0x3ff3_ffff_ffff_ffff),
            (0x3ff8_ffff_ffff_ffff, 0x3ff4_0000_0000_0000),
            (0x400f_ffff_ffff_ffff, 0x3fff_ffff_ffff_ffff),
            (0x4010_0000_0000_0000, 0x4000_0000_0000_0000),
            (0x4010_0000_0000_0001, 0x4000_0000_0000_0000),
            (0x4022_0000_0000_0000, 0x4008_0000_0000_0000),
            (0x7fef_ffff_ffff_ffff, 0x5fef_ffff_ffff_ffff),
        ]),
    );
}

fn finite_samples(fraction_bits: u32, exponent_fields: u64) -> impl Iterator<Item = u64> {
    let mut state = 0x6c4e_72bf_af49_0315_u64;
    (0..exponent_fields)
        .flat_map(|exponent| [exponent; 4])
        .map(move |exponent| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (exponent << fraction_bits) | (state >> (64 - fraction_bits))
        })
}

#[test]
fn sqrt_matches_native_reference_for_fixed_samples_at_every_finite_exponent_field() {
    // Test-only independent reference, with four deterministic fractions per field.
    // The native comparisons are evidence, not a proof of rounding correctness.
    check_bits(
        F32_SQRT,
        0x7d,
        ResultKind::F32,
        |bits| Value::F32(bits as u32),
        finite_samples(23, 255).map(|bits| {
            (
                bits,
                u64::from(f32::from_bits(bits as u32).sqrt().to_bits()),
            )
        }),
    );
    check_bits(
        F64_SQRT,
        0x7c,
        ResultKind::F64,
        Value::F64,
        finite_samples(52, 2047).map(|bits| (bits, f64::from_bits(bits).sqrt().to_bits())),
    );
}

#[test]
fn sqrt_requires_its_width_in_live_and_polymorphic_code_and_pushes_a_concrete_result() {
    use ValType::{F32, F64, I32, I64};
    for (opcode, ty, zero, other, wrong_width) in [
        (F32_SQRT, F32, F32_ZERO, F64, F64_ZERO),
        (F64_SQRT, F64, F64_ZERO, F32, F32_ZERO),
    ] {
        // A matching operand below the active floor cannot be consumed.
        let floor = [&[0, 0x41, 42][..], zero, &[0x02, 0x40]].concat();
        let tail = [opcode, 0x1a, 0x0b, 0x1a, 0x0b];
        bad_body(
            &[&floor[..], &tail].concat(),
            floor.len(),
            ErrorKind::StackUnderflow { depth: 0 },
        );
        assert_body_result(&[&floor[..], zero, &tail].concat(), 42);
        for prefix in [&[][..], &[0x00]] {
            for (wrong, actual) in [(wrong_width, other), (&[0x41, 0], I32), (&[0x42, 0], I64)] {
                bad_body(
                    &[&floor[..], prefix, wrong, &tail].concat(),
                    floor.len() + prefix.len() + wrong.len(),
                    ErrorKind::TypeMismatch {
                        expected: ty,
                        actual,
                    },
                );
            }
        }
        for input in [&[][..], &[0x1b]] {
            // An implicit operand at the dead floor or an unknown from select can be used.
            let body = [&floor[..], &[0x00], input, &tail].concat();
            let module = exported_function(TYPE, &code_body(&body));
            assert_eq!(
                module.instantiate().unwrap().invoke_i32("answer"),
                Err(InvokeError::Trap(Trap::Unreachable))
            );
        }
        for input in [zero, &[0x00]] {
            // Both reachable and unreachable sqrt produce a known float, not an i32.
            bad_body(
                &[&[0][..], input, &[opcode, 0x0b]].concat(),
                2 + input.len(),
                ErrorKind::TypeMismatch {
                    expected: I32,
                    actual: ty,
                },
            );
        }
    }
}

#[test]
fn sqrt_costs_one_dispatch_even_for_the_smallest_subnormal_at_the_fuel_boundary() {
    for (opcode, float, result, input, expected) in [
        (
            F32_SQRT,
            0x7d,
            ResultKind::F32,
            Value::F32(1),
            Value::F32(0x1a35_04f3),
        ),
        (
            F64_SQRT,
            0x7c,
            ResultKind::F64,
            Value::F64(1),
            Value::F64(0x1e60_0000_0000_0000),
        ),
    ] {
        let ty = types(&[(&[0x7f, float], Some(float))]);
        let mut body = vec![0, 0x03, 0x40, 0x20, 1, opcode, 0x1a];
        body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]); // 8 per iteration.
        body.extend([0x01; 6]);
        body.extend([0x20, 1]);
        let last = body.len();
        body.extend([opcode, 0x0b]); // 127 * 8 + 8 = 1024.
        let run = |body: &[u8]| {
            exported_function(&ty, &code_body(body))
                .instantiate()
                .unwrap()
                .invoke_with_values("answer", &[Value::I32(127), input], result)
        };
        assert_eq!(run(&body), Ok(Some(expected)), "opcode {opcode:#x}");
        body.insert(last, 0x01);
        assert_eq!(
            run(&body),
            Err(InvokeError::LimitExceeded {
                limit: ExecutionLimit::InstructionDispatches
            }),
            "opcode {opcode:#x}"
        );
    }
}
