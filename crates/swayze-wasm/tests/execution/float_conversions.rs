use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

const DEMOTE: u8 = 0xb6;
const PROMOTE: u8 = 0xbb;
const F32: u8 = 0x7d;
const F64: u8 = 0x7c;
const SIGN32: u32 = 1 << 31;
const SIGN64: u64 = 1 << 63;
const INF32: u32 = 0x7f80_0000;
const INF64: u64 = 0x7ff0_0000_0000_0000;
const NAN32: u32 = 0x7fc0_0000;
const NAN64: u64 = 0x7ff8_0000_0000_0000;
const F32_ZERO: &[u8] = b"\x43\0\0\0\0";
const F64_ZERO: &[u8] = b"\x44\0\0\0\0\0\0\0\0";

fn conversion(src: u8, dst: u8, ops: &[u8]) -> Module {
    let mut body = vec![0, 0x20, 0];
    body.extend_from_slice(ops);
    body.push(0x0b);
    exported_function(&types(&[(&[src], Some(dst))]), &code_body(&body))
}

const EXACT_WIDTH_PAIRS: &[(u32, u64)] = &[
    (0, 0),
    (1, 0x36a0_0000_0000_0000), // Smallest f32 subnormal: 2^-149.
    (3, 0x36b8_0000_0000_0000), // 1.5 * 2^-148, with a nonzero fraction.
    (0x0040_0000, 0x3800_0000_0000_0000),
    (0x007f_ffff, 0x380f_ffff_c000_0000), // Largest f32 subnormal: 2^-126 - 2^-149.
    (0x0080_0000, 0x3810_0000_0000_0000), // Smallest f32 normal.
    (0x0080_0001, 0x3810_0000_2000_0000),
    (0x3fa0_0001, 0x3ff4_0000_2000_0000),
    (0x7f7f_ffff, 0x47ef_ffff_e000_0000), // Largest finite f32.
    (INF32, INF64),
];

#[test]
fn promotes_to_exact_f64_bits_including_both_subnormal_extremes() {
    let module = conversion(F32, F64, &[PROMOTE]);
    let promote = module.instantiate().unwrap();
    for &(narrow, wide) in EXACT_WIDTH_PAIRS {
        for (sign32, sign64) in [(0, 0), (SIGN32, SIGN64)] {
            let v32 = Value::F32(sign32 | narrow);
            let v64 = Value::F64(sign64 | wide);
            assert_eq!(
                promote.invoke_with_values("answer", &[v32], ResultKind::F64),
                Ok(Some(v64)),
                "promote {v32:?}"
            );
        }
    }
}

#[test]
fn round_trips_representable_values() {
    let modules = [
        conversion(F64, F32, &[DEMOTE]),
        conversion(F32, F32, &[PROMOTE, DEMOTE]),
        conversion(F64, F64, &[DEMOTE, PROMOTE]),
    ];
    let [demote, original32, original64] = modules.each_ref().map(|m| m.instantiate().unwrap());
    for &(narrow, wide) in EXACT_WIDTH_PAIRS {
        for (sign32, sign64) in [(0, 0), (SIGN32, SIGN64)] {
            let v32 = Value::F32(sign32 | narrow);
            let v64 = Value::F64(sign64 | wide);
            assert_eq!(
                demote.invoke_with_values("answer", &[v64], ResultKind::F32),
                Ok(Some(v32)),
                "demote {v64:?}"
            );
            assert_eq!(
                original32.invoke_with_values("answer", &[v32], ResultKind::F32),
                Ok(Some(v32)),
                "round trip {v32:?}"
            );
            assert_eq!(
                original64.invoke_with_values("answer", &[v64], ResultKind::F64),
                Ok(Some(v64)),
                "round trip {v64:?}"
            );
        }
    }
}

#[test]
fn demote_rounds_midpoints_and_their_neighbors_to_exact_bits() {
    let module = conversion(F64, F32, &[DEMOTE]);
    let demote = module.instantiate().unwrap();
    // Expected magnitude bits at the adjacent f64 values and exact tie:
    // [one ulp below the midpoint, at the midpoint, one ulp above].
    let midpoints = [
        // Half of the smallest f32 subnormal: tie to signed zero.
        (0x3690_0000_0000_0000, [0, 0, 1]),
        (0x36a8_0000_0000_0000, [1, 2, 2]), // Odd lower subnormal.
        (0x36b4_0000_0000_0000, [2, 2, 3]), // Even lower subnormal.
        // Subnormal/normal boundary: tie to smallest normal.
        (
            0x380f_ffff_e000_0000,
            [0x007f_ffff, 0x0080_0000, 0x0080_0000],
        ),
        // Even and odd lower normals, then carry across a binade.
        (
            0x3ff0_0000_1000_0000,
            [0x3f80_0000, 0x3f80_0000, 0x3f80_0001],
        ),
        (
            0x3ff0_0000_3000_0000,
            [0x3f80_0001, 0x3f80_0002, 0x3f80_0002],
        ),
        (
            0x3fff_ffff_f000_0000,
            [0x3fff_ffff, 0x4000_0000, 0x4000_0000],
        ),
        // 2^128 - 2^103: tie to signed infinity.
        (0x47ef_ffff_f000_0000, [0x7f7f_ffff, INF32, INF32]),
    ];
    let neighbors = midpoints.into_iter().flat_map(|(midpoint, outputs)| {
        [midpoint - 1, midpoint, midpoint + 1]
            .into_iter()
            .zip(outputs)
    });
    for (bits, expected) in neighbors.chain([
        (1, 0), // Smallest f64 subnormal.
        (0x000f_ffff_ffff_ffff, 0),
        (0x0010_0000_0000_0000, 0),     // Smallest f64 normal.
        (0x47f0_0000_0000_0000, INF32), // 2^128.
        (INF64 - 1, INF32),
    ]) {
        for (sign32, sign64) in [(0, 0), (SIGN32, SIGN64)] {
            let input = sign64 | bits;
            assert_eq!(
                demote.invoke_with_values("answer", &[Value::F64(input)], ResultKind::F32),
                Ok(Some(Value::F32(sign32 | expected))),
                "input {input:#018x}"
            );
        }
    }
}

#[test]
fn width_conversions_canonicalize_nan_instead_of_preserving_sign_or_payload() {
    let promotion = conversion(F32, F64, &[PROMOTE]);
    let demotion = conversion(F64, F32, &[DEMOTE]);
    let promote = promotion.instantiate().unwrap();
    let demote = demotion.instantiate().unwrap();
    for (sign32, sign64) in [(0, 0), (SIGN32, SIGN64)] {
        for bits in [INF32 + 1, NAN32, NAN32 | 0x1234] {
            assert_eq!(
                promote.invoke_with_values("answer", &[Value::F32(sign32 | bits)], ResultKind::F64),
                Ok(Some(Value::F64(NAN64)))
            );
        }
        for bits in [INF64 + 1, NAN64, NAN64 | 0x1_2345_6789] {
            assert_eq!(
                demote.invoke_with_values("answer", &[Value::F64(sign64 | bits)], ResultKind::F32),
                Ok(Some(Value::F32(NAN32)))
            );
        }
    }
}

fn finite_samples(fraction_bits: u32, exponent_fields: u64) -> impl Iterator<Item = u64> {
    let mut state = 0x3aa8_2e3c_bae0_ec95_u64;
    (0..exponent_fields).flat_map(|e| [e; 4]).map(move |e| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (e << fraction_bits) | (state >> (64 - fraction_bits))
    })
}

#[test]
fn width_conversions_match_native_casts_for_deterministic_finite_samples() {
    // Tests only: each finite exponent field, with four fractions and both signs.
    // Independent native casts provide evidence, not proof of the rounding rules.
    let promotion = conversion(F32, F64, &[PROMOTE]);
    let demotion = conversion(F64, F32, &[DEMOTE]);
    let promote = promotion.instantiate().unwrap();
    let demote = demotion.instantiate().unwrap();
    for magnitude in finite_samples(23, 255) {
        for bits in [magnitude as u32, magnitude as u32 | SIGN32] {
            let expected = f64::from(f32::from_bits(bits)).to_bits();
            assert_eq!(
                promote.invoke_with_values("answer", &[Value::F32(bits)], ResultKind::F64),
                Ok(Some(Value::F64(expected))),
                "promote {bits:#010x}"
            );
        }
    }
    for magnitude in finite_samples(52, 2047) {
        for bits in [magnitude, magnitude | SIGN64] {
            let expected = (f64::from_bits(bits) as f32).to_bits();
            assert_eq!(
                demote.invoke_with_values("answer", &[Value::F64(bits)], ResultKind::F32),
                Ok(Some(Value::F32(expected))),
                "demote {bits:#018x}"
            );
        }
    }
}

#[test]
fn width_conversions_check_live_and_dead_input_types_and_push_the_concrete_destination() {
    for (opcode, src, dst, src_zero, dst_zero) in [
        (PROMOTE, ValType::F32, ValType::F64, F32_ZERO, F64_ZERO),
        (DEMOTE, ValType::F64, ValType::F32, F64_ZERO, F32_ZERO),
    ] {
        // Matching input below an active frame's floor cannot be consumed.
        let floor = [&[0, 0x41, 42][..], src_zero, &[0x02, 0x40]].concat();
        let tail = [opcode, 0x1a, 0x0b, 0x1a, 0x0b];
        bad_body(
            &[&floor[..], &tail].concat(),
            floor.len(),
            ErrorKind::StackUnderflow { depth: 0 },
        );
        assert_body_result(&[&floor[..], src_zero, &tail].concat(), 42);
        for prefix in [&[][..], &[0x00]] {
            for (input, actual) in [
                (dst_zero, dst),
                (&[0x41, 0], ValType::I32),
                (&[0x42, 0], ValType::I64),
            ] {
                bad_body(
                    &[&floor[..], prefix, input, &tail].concat(),
                    floor.len() + prefix.len() + input.len(),
                    ErrorKind::TypeMismatch {
                        expected: src,
                        actual,
                    },
                );
            }
        }
        // At a dead floor a missing input (or unknown from select) is valid.
        for input in [&[][..], &[0x1b]] {
            let body = [&floor[..], &[0x00], input, &tail].concat();
            let module = exported_function(TYPE, &code_body(&body));
            assert_eq!(
                module.instantiate().unwrap().invoke_i32("answer"),
                Err(InvokeError::Trap(Trap::Unreachable))
            );
        }
        // Applying the same conversion twice is invalid, even when the first
        // conversion took an implicit dead operand: its result is concrete.
        for input in [src_zero, &[0x00]] {
            bad_body(
                &[&[0][..], input, &[opcode, opcode, 0x1a, 0x41, 42, 0x0b]].concat(),
                2 + input.len(),
                ErrorKind::TypeMismatch {
                    expected: src,
                    actual: dst,
                },
            );
        }
    }
}

#[test]
fn each_width_conversion_costs_one_dispatch_at_the_fuel_boundary() {
    for (opcode, src, dst, result, input, expected) in [
        (
            PROMOTE,
            F32,
            F64,
            ResultKind::F64,
            Value::F32(1),
            Value::F64(0x36a0_0000_0000_0000),
        ),
        (
            DEMOTE,
            F64,
            F32,
            ResultKind::F32,
            Value::F64(0xb690_0000_0000_0001),
            Value::F32(SIGN32 | 1),
        ),
    ] {
        let ty = types(&[(&[0x7f, src], Some(dst))]);
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
