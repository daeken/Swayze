use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

const I32: u8 = 0x7f;
const I64: u8 = 0x7e;
const F32: u8 = 0x7d;
const F64: u8 = 0x7c;
const SIGN32: u32 = 1 << 31;
const SIGN64: u64 = 1 << 63;

// Source type byte and f32/f64 opcodes, in that order.
type ConversionPair = (u8, [u8; 2]);
const I32_S: ConversionPair = (I32, [0xb2, 0xb7]);
const I32_U: ConversionPair = (I32, [0xb3, 0xb8]);
const I64_S: ConversionPair = (I64, [0xb4, 0xb9]);
const I64_U: ConversionPair = (I64, [0xb5, 0xba]);

fn check_pair(
    (src, [op32, op64]): ConversionPair,
    cases: impl IntoIterator<Item = (Value, u32, u64)>,
) {
    let modules = [(op32, F32), (op64, F64)].map(|(op, dst)| {
        exported_function(
            &types(&[(&[src], Some(dst))]),
            &code_body(&[0, 0x20, 0, op, 0x0b]),
        )
    });
    let [to32, to64] = modules.each_ref().map(|m| m.instantiate().unwrap());
    for (input, bits32, bits64) in cases {
        assert_eq!(
            to32.invoke_with_values("answer", &[input], ResultKind::F32),
            Ok(Some(Value::F32(bits32))),
            "opcode {op32:#x}, input {input:?}"
        );
        assert_eq!(
            to64.invoke_with_values("answer", &[input], ResultKind::F64),
            Ok(Some(Value::F64(bits64))),
            "opcode {op64:#x}, input {input:?}"
        );
    }
}

#[test]
fn i32_conversions_pin_precision_boundary_sign_and_unsigned_bit_patterns() {
    const P: i32 = 1 << 24;
    const Q: i32 = 1 << 25;
    let cases = [
        (0, 0, 0),
        (1, 0x3f80_0000, 0x3ff0_0000_0000_0000),
        (P - 1, 0x4b7f_ffff, 0x416f_ffff_e000_0000),
        (P, 0x4b80_0000, 0x4170_0000_0000_0000),
        (P + 1, 0x4b80_0000, 0x4170_0000_1000_0000), // Even lower f32.
        (P + 2, 0x4b80_0001, 0x4170_0000_2000_0000),
        (P + 3, 0x4b80_0002, 0x4170_0000_3000_0000), // Odd lower f32.
        (P + 4, 0x4b80_0002, 0x4170_0000_4000_0000),
        (Q - 1, 0x4c00_0000, 0x417f_ffff_f000_0000), // Carry to the next exponent.
        (Q + 1, 0x4c00_0000, 0x4180_0000_0800_0000),
        (Q + 2, 0x4c00_0000, 0x4180_0000_1000_0000), // Even tie and its neighbors.
        (Q + 3, 0x4c00_0001, 0x4180_0000_1800_0000),
        (Q + 5, 0x4c00_0001, 0x4180_0000_2800_0000),
        (Q + 6, 0x4c00_0002, 0x4180_0000_3000_0000), // Odd tie and its neighbors.
        (Q + 7, 0x4c00_0002, 0x4180_0000_3800_0000),
        (i32::MAX, 0x4f00_0000, 0x41df_ffff_ffc0_0000),
    ];
    let positive = cases.map(|(n, f, d)| (Value::I32(n), f, d));
    let negative = cases[1..]
        .iter()
        .map(|&(n, f, d)| (Value::I32(-n), SIGN32 | f, SIGN64 | d));
    check_pair(
        I32_S,
        positive.into_iter().chain(negative).chain([(
            Value::I32(i32::MIN),
            0xcf00_0000,
            0xc1e0_0000_0000_0000,
        )]),
    );
    check_pair(
        I32_U,
        positive.into_iter().chain([
            (Value::I32(i32::MIN), 0x4f00_0000, 0x41e0_0000_0000_0000),
            (Value::I32(i32::MIN + 1), 0x4f00_0000, 0x41e0_0000_0020_0000),
            (Value::I32(-1), 0x4f80_0000, 0x41ef_ffff_ffe0_0000), // u32::MAX.
        ]),
    );
}

#[test]
fn i64_conversions_pin_precision_boundary_single_rounding_and_extremes() {
    const P: i64 = 1 << 53;
    const Q: i64 = 1 << 54;
    const HALF32: i64 = 1 << 30; // Half an f32 ULP at Q, versus f64's half ULP of 2.
    let cases = [
        (0, 0, 0),
        (1, 0x3f80_0000, 0x3ff0_0000_0000_0000),
        (P - 1, 0x5a00_0000, 0x433f_ffff_ffff_ffff),
        (P, 0x5a00_0000, 0x4340_0000_0000_0000),
        (P + 1, 0x5a00_0000, 0x4340_0000_0000_0000), // Even lower f64.
        (P + 2, 0x5a00_0000, 0x4340_0000_0000_0001),
        (P + 3, 0x5a00_0000, 0x4340_0000_0000_0002), // Odd lower f64.
        (P + 4, 0x5a00_0000, 0x4340_0000_0000_0002),
        (Q - 1, 0x5a80_0000, 0x4350_0000_0000_0000), // Carry to the next exponent.
        (Q + 1, 0x5a80_0000, 0x4350_0000_0000_0000),
        (Q + 2, 0x5a80_0000, 0x4350_0000_0000_0000), // Even tie and its neighbors.
        (Q + 3, 0x5a80_0000, 0x4350_0000_0000_0001),
        (Q + 5, 0x5a80_0000, 0x4350_0000_0000_0001),
        (Q + 6, 0x5a80_0000, 0x4350_0000_0000_0002), // Odd tie and its neighbors.
        (Q + 7, 0x5a80_0000, 0x4350_0000_0000_0002),
        // Each f32 midpoint and its neighbors round to the same f64.
        // Going via f64 would incorrectly turn the neighbors into ties.
        (Q + HALF32 - 1, 0x5a80_0000, 0x4350_0000_1000_0000),
        (Q + HALF32, 0x5a80_0000, 0x4350_0000_1000_0000),
        (Q + HALF32 + 1, 0x5a80_0001, 0x4350_0000_1000_0000),
        (Q + 3 * HALF32 - 1, 0x5a80_0001, 0x4350_0000_3000_0000),
        (Q + 3 * HALF32, 0x5a80_0002, 0x4350_0000_3000_0000),
        (Q + 3 * HALF32 + 1, 0x5a80_0002, 0x4350_0000_3000_0000),
        (i64::MAX, 0x5f00_0000, 0x43e0_0000_0000_0000),
    ];
    let positive = cases.map(|(n, f, d)| (Value::I64(n), f, d));
    let negative = cases[1..]
        .iter()
        .map(|&(n, f, d)| (Value::I64(-n), SIGN32 | f, SIGN64 | d));
    check_pair(
        I64_S,
        positive.into_iter().chain(negative).chain([(
            Value::I64(i64::MIN),
            0xdf00_0000,
            0xc3e0_0000_0000_0000,
        )]),
    );
    // Negative host arguments carry unsigned bit patterns; the high midpoints
    // are 2^64 - 2^39 for f32 and 2^64 - 2^10 for f64.
    let high = [
        (1 << 63, 0x5f00_0000, 0x43e0_0000_0000_0000),
        (u64::MAX - (1 << 39), 0x5f7f_ffff, 0x43ef_ffff_f000_0000),
        (u64::MAX - (1 << 39) + 1, 0x5f80_0000, 0x43ef_ffff_f000_0000),
        (u64::MAX - (1 << 39) + 2, 0x5f80_0000, 0x43ef_ffff_f000_0000),
        (u64::MAX - 1024, 0x5f80_0000, 0x43ef_ffff_ffff_ffff),
        (u64::MAX - 1023, 0x5f80_0000, 0x43f0_0000_0000_0000),
        (u64::MAX - 1022, 0x5f80_0000, 0x43f0_0000_0000_0000),
        (u64::MAX, 0x5f80_0000, 0x43f0_0000_0000_0000),
    ];
    check_pair(
        I64_U,
        positive
            .into_iter()
            .chain(high.map(|(n, f, d)| (Value::I64(n as i64), f, d))),
    );
}

fn samples(width: u32) -> impl Iterator<Item = u64> {
    let mut state = 0xd0c5_a11d_10e5_7a3b_u64;
    (0..width)
        .flat_map(|e| [1_u64 << e; 32])
        .map(move |leading| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            leading | (state & (leading - 1))
        })
}

#[test]
fn integer_conversions_match_direct_rust_casts_for_deterministic_samples() {
    // Test-only evidence: 32 samples for each leading bit, both signs for
    // signed operands. Never cast via another float or integer type as oracle.
    check_pair(
        I32_S,
        samples(31)
            .flat_map(|n| [n as i32, -(n as i32)])
            .map(|n| (Value::I32(n), (n as f32).to_bits(), (n as f64).to_bits())),
    );
    check_pair(
        I32_U,
        samples(32).map(|n| {
            let n = n as u32;
            (
                Value::I32(n as i32),
                (n as f32).to_bits(),
                (n as f64).to_bits(),
            )
        }),
    );
    check_pair(
        I64_S,
        samples(63)
            .flat_map(|n| [n as i64, -(n as i64)])
            .map(|n| (Value::I64(n), (n as f32).to_bits(), (n as f64).to_bits())),
    );
    check_pair(
        I64_U,
        samples(64).map(|n| {
            (
                Value::I64(n as i64),
                (n as f32).to_bits(),
                (n as f64).to_bits(),
            )
        }),
    );
}

#[test]
fn integer_conversions_validate_live_and_dead_operands_and_push_a_concrete_float() {
    for (src, [op32, op64]) in [I32_S, I32_U, I64_S, I64_U] {
        let (expected, input) = if src == I32 {
            (ValType::I32, &[0x41, 0])
        } else {
            (ValType::I64, &[0x42, 0])
        };
        for (op, dst) in [(op32, ValType::F32), (op64, ValType::F64)] {
            // Correct input below an active block's floor is not accessible.
            let prefix = [&[0, 0x41, 42][..], input, &[0x02, 0x40]].concat();
            let tail = [op, 0x1a, 0x0b, 0x1a, 0x0b];
            bad_body(
                &[&prefix[..], &tail].concat(),
                prefix.len(),
                ErrorKind::StackUnderflow { depth: 0 },
            );
            assert_body_result(&[&prefix[..], input, &tail].concat(), 42);
            for dead in [&[][..], &[0x00]] {
                for (actual, wrong) in [
                    (ValType::I32, &b"\x41\0"[..]),
                    (ValType::I64, &b"\x42\0"[..]),
                    (ValType::F32, &b"\x43\0\0\0\0"[..]),
                    (ValType::F64, &b"\x44\0\0\0\0\0\0\0\0"[..]),
                ] {
                    if actual != expected {
                        bad_body(
                            &[&prefix[..], dead, wrong, &tail].concat(),
                            prefix.len() + dead.len() + wrong.len(),
                            ErrorKind::TypeMismatch { expected, actual },
                        );
                    }
                }
            }
            let body = [&prefix[..], &[0x00], &tail].concat();
            let module = exported_function(TYPE, &code_body(&body));
            assert_eq!(
                module.instantiate().unwrap().invoke_i32("answer"),
                Err(InvokeError::Trap(Trap::Unreachable))
            );
            // A second conversion rejects that concrete float, even when
            // the first accepted a polymorphic operand at the dead floor.
            for first in [&input[..], &[0x00]] {
                bad_body(
                    &[&[0][..], first, &[op, op, 0x1a, 0x41, 42, 0x0b]].concat(),
                    2 + first.len(),
                    ErrorKind::TypeMismatch {
                        expected,
                        actual: dst,
                    },
                );
            }
        }
    }
}

#[test]
fn each_integer_conversion_costs_exactly_one_dispatch() {
    for ((src, [op32, op64]), input, f32_bits, f64_bits) in [
        (I32_S, Value::I32(-1), 0xbf80_0000, 0xbff0_0000_0000_0000),
        (I32_U, Value::I32(-1), 0x4f80_0000, 0x41ef_ffff_ffe0_0000),
        (
            I64_S,
            Value::I64(i64::MIN),
            0xdf00_0000,
            0xc3e0_0000_0000_0000,
        ),
        (I64_U, Value::I64(-1), 0x5f80_0000, 0x43f0_0000_0000_0000),
    ] {
        for (op, dst, result, expected) in [
            (op32, F32, ResultKind::F32, Value::F32(f32_bits)),
            (op64, F64, ResultKind::F64, Value::F64(f64_bits)),
        ] {
            let ty = types(&[(&[I32, src], Some(dst))]);
            let mut body = vec![0, 0x03, 0x40, 0x20, 1, op, 0x1a];
            body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]); // 8 per iteration.
            body.extend([0x01; 6]);
            body.extend([0x20, 1]);
            let last = body.len();
            body.extend([op, 0x0b]); // 127 * 8 + 8 = 1024.
            let run = |body: &[u8]| {
                exported_function(&ty, &code_body(body))
                    .instantiate()
                    .unwrap()
                    .invoke_with_values("answer", &[Value::I32(127), input], result)
            };
            assert_eq!(run(&body), Ok(Some(expected)), "opcode {op:#x}");
            body.insert(last, 0x01);
            assert_eq!(
                run(&body),
                Err(InvokeError::LimitExceeded {
                    limit: ExecutionLimit::InstructionDispatches
                }),
                "opcode {op:#x}"
            );
        }
    }
}
