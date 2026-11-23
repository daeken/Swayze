use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

const I32: u8 = 0x7f;
const I64: u8 = 0x7e;
const F32: u8 = 0x7d;
const F64: u8 = 0x7c;
const OVERFLOW: Trap = Trap::IntegerOverflow;
const INVALID: Trap = Trap::InvalidConversionToInteger;

// Opcode, input type byte, output type byte, signed result.
const OPS: [(u8, u8, u8, bool); 8] = [
    (0xa8, F32, I32, true),
    (0xa9, F32, I32, false),
    (0xaa, F64, I32, true),
    (0xab, F64, I32, false),
    (0xae, F32, I64, true),
    (0xaf, F32, I64, false),
    (0xb0, F64, I64, true),
    (0xb1, F64, I64, false),
];

fn float(src: u8, bits: u64) -> Value {
    match src {
        F32 => Value::F32(u32::try_from(bits).unwrap()),
        F64 => Value::F64(bits),
        _ => unreachable!(),
    }
}

// Encode signed results or unsigned high-bit results as Wasm integer bits.
fn integer(dst: u8, n: i128) -> Value {
    match dst {
        I32 => Value::I32(n as i32),
        I64 => Value::I64(n as i64),
        _ => unreachable!(),
    }
}

fn result_kind(dst: u8) -> ResultKind {
    match dst {
        I32 => ResultKind::I32,
        I64 => ResultKind::I64,
        _ => unreachable!(),
    }
}

fn check_cases(
    (op, src, dst, _): (u8, u8, u8, bool),
    cases: impl IntoIterator<Item = (Value, Result<i128, Trap>)>,
) {
    let module = exported_function(
        &types(&[(&[src], Some(dst))]),
        &code_body(&[0, 0x20, 0, op, 0x0b]),
    );
    let instance = module.instantiate().unwrap();
    for (input, expected) in cases {
        assert_eq!(
            instance.invoke_with_values("answer", &[input], result_kind(dst)),
            expected
                .map(|n| Some(integer(dst, n)))
                .map_err(InvokeError::Trap),
            "opcode {op:#x}, input {input:?}"
        );
    }
}

#[test]
fn conversion_classifies_raw_special_values_and_unsigned_negative_inputs() {
    for spec @ (_, src, _, signed) in OPS {
        let (sign, one, infinity, quiet_bit, fraction_mask) = if src == F32 {
            (1 << 31, 0x3f80_0000, 0x7f80_0000, 1 << 22, (1 << 23) - 1)
        } else {
            (
                1 << 63,
                0x3ff0_0000_0000_0000,
                0x7ff0_0000_0000_0000,
                1 << 51,
                (1 << 52) - 1,
            )
        };
        let negative_one = if signed { Ok(-1) } else { Err(OVERFLOW) };
        let cases = [0, sign]
            .into_iter()
            .flat_map(|s| {
                [
                    (s, Ok(0)),                          // ±0
                    (s | 1, Ok(0)),                      // Smallest subnormal.
                    (s | fraction_mask, Ok(0)),          // Largest subnormal.
                    (s | (fraction_mask + 1), Ok(0)),    // Smallest normal.
                    (s | (one - 1), Ok(0)),              // Neighbor toward zero, also for unsigned.
                    (s | (infinity - 1), Err(OVERFLOW)), // Largest finite magnitude.
                    (s | infinity, Err(OVERFLOW)),
                    (s | infinity | 1, Err(INVALID)), // Signaling NaN, low payload bit only.
                    (s | infinity | quiet_bit, Err(INVALID)),
                    (s | infinity | quiet_bit | 0x1234, Err(INVALID)),
                ]
            })
            .chain([
                (one, Ok(1)),
                (one + 1, Ok(1)),
                (sign | one, negative_one),
                (sign | (one + 1), negative_one),
            ]);
        check_cases(
            spec,
            cases.map(|(bits, expected)| (float(src, bits), expected)),
        );
    }
    assert_eq!(INVALID.to_string(), "invalid conversion to integer");
    assert_eq!(OVERFLOW.to_string(), "integer overflow");
}

#[test]
fn conversion_checks_the_truncated_integer_at_exact_range_boundaries() {
    // Raw encoding of the positive upper cutoff: 2^31, 2^32, 2^63 or 2^64.
    let cutoffs = [
        (0x4f00_0000, 31),
        (0x4f80_0000, 32),
        (0x41e0_0000_0000_0000, 31),
        (0x41f0_0000_0000_0000, 32),
        (0x5f00_0000, 63),
        (0x5f80_0000, 64),
        (0x43e0_0000_0000_0000, 63),
        (0x43f0_0000_0000_0000, 64),
    ];
    for (spec @ (_, src, _, signed), (bits, e)) in OPS.into_iter().zip(cutoffs) {
        let (sign, precision) = if src == F32 {
            (1 << 31, 24)
        } else {
            (1 << 63, 53)
        };
        let power = 1_i128 << e;
        // The ULP immediately below 2^e is 2^(e-precision). If the ULP
        // is fractional, the previous float still truncates to 2^e - 1.
        let previous = power - (1 << (e - precision).max(0));
        let mut cases = vec![
            (bits - 1, Ok(previous)),
            (bits, Err(OVERFLOW)),
            (bits + 1, Err(OVERFLOW)),
        ];
        if signed {
            cases.extend([
                (sign | (bits - 1), Ok(-previous)),
                (sign | bits, Ok(-power)),
            ]);
            if src == F64 && e == 31 {
                // f64 neighbors below i32::MIN at this power are spaced 2^-21.
                // The exact cutoff is MIN - 1, not MIN or its predecessor.
                let cutoff = bits + (1 << 21);
                cases.extend([
                    (sign | (bits + 1), Ok(-power)),
                    (sign | (cutoff - 1), Ok(-power)),
                    (sign | cutoff, Err(OVERFLOW)),
                    (sign | (cutoff + 1), Err(OVERFLOW)),
                ]);
            } else {
                cases.push((sign | (bits + 1), Err(OVERFLOW)));
            }
        }
        check_cases(
            spec,
            cases
                .into_iter()
                .map(|(bits, expected)| (float(src, bits), expected)),
        );
    }
}

#[test]
fn conversion_agrees_with_native_truncation_on_fixed_in_range_fractions() {
    for spec @ (_, src, _, signed) in OPS {
        // Every float is exactly represented at both widths, and every
        // truncated result fits the destination. No cast is a trap oracle.
        let samples = [-1_234_567.75, -2.875, -0.75, 0.75, 2.875, 1_234_567.75];
        let cases = samples
            .into_iter()
            .filter(|&x| signed || x > -1.0)
            .map(|x: f64| {
                let value = if src == F32 {
                    Value::F32((x as f32).to_bits())
                } else {
                    Value::F64(x.to_bits())
                };
                (value, Ok(x.trunc() as i128))
            });
        check_cases(spec, cases);
    }
}

#[test]
fn conversion_validates_operands_and_concrete_results_in_live_and_dead_code() {
    for (op, src, dst, _) in OPS {
        let (expected, input): (ValType, &[u8]) = if src == F32 {
            (ValType::F32, b"\x43\0\0\0\0")
        } else {
            (ValType::F64, b"\x44\0\0\0\0\0\0\0\0")
        };
        let dest = if dst == I32 {
            ValType::I32
        } else {
            ValType::I64
        };
        // Keep a correctly typed input below a nested block's validation floor.
        let prefix = [&[0][..], input, &[0x02, 0x40]].concat();
        let tail = [op, 0x1a, 0x0b, 0x1a, 0x41, 42, 0x0b];
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
        for first in [input, &[0x00]] {
            // Conversion pushes a concrete integer even at a polymorphic floor.
            bad_body(
                &[&[0][..], first, &[op, op, 0x1a, 0x41, 42, 0x0b]].concat(),
                2 + first.len(),
                ErrorKind::TypeMismatch {
                    expected,
                    actual: dest,
                },
            );
        }
    }
}

#[test]
fn conversion_costs_one_dispatch_and_traps_neither_roll_back_nor_continue() {
    let limit = InvokeError::LimitExceeded {
        limit: ExecutionLimit::InstructionDispatches,
    };
    for (op, src, dst, _) in OPS {
        let types = types(&[(&[I32, src], Some(dst)), (&[], Some(I32))]);
        let exports = function_exports(&[(b"run", 0), (b"read", 1)]);
        let read = [0, 0x23, 0, 0x0b];
        let (nan, infinity) = if src == F32 {
            (0x7f80_0001, 0x7f80_0000)
        } else {
            (0x7ff0_0000_0000_0001, 0x7ff0_0000_0000_0000)
        };
        for (padding, bits, expected, written) in [
            (2, 0, Ok(Some(integer(dst, 0))), 13), // Last global.set is dispatch 1024.
            (3, 0, Err(limit), 42),                // Last global.set would be dispatch 1025.
            (4, nan, Err(InvokeError::Trap(INVALID)), 42), // Conversion is dispatch 1024.
            (4, infinity, Err(InvokeError::Trap(OVERFLOW)), 42),
            (5, nan, Err(limit), 42), // Conversion would be dispatch 1025.
            (5, infinity, Err(limit), 42),
        ] {
            // There are 127 * 8 = 1016 dispatches in the loop and another
            // three for writing 42 and pushing the float, before the nops.
            let mut run = vec![0, 0x03, 0x40, 0x01, 0x01, 0x01];
            run.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]);
            run.extend([0x41, 42, 0x24, 0, 0x20, 1]);
            run.extend(std::iter::repeat_n(0x01, padding));
            run.extend([op, 0x41, 13, 0x24, 0, 0x0b]);
            let module = validate_module(&module(&[
                (1, &types),
                (3, &[2, 0, 1]),
                (6, &[1, I32, 1, 0x41, 0, 0x0b]),
                (7, &exports),
                (10, &code_bodies(&[&run, &read])),
            ]))
            .unwrap();
            let instance = module.instantiate().unwrap();
            assert_eq!(instance.invoke_i32("read"), Ok(0));
            assert_eq!(
                instance.invoke_with_values(
                    "run",
                    &[Value::I32(127), float(src, bits)],
                    result_kind(dst)
                ),
                expected,
                "opcode {op:#x}, padding {padding}, bits {bits:#x}"
            );
            assert_eq!(instance.invoke_i32("read"), Ok(written));
        }
    }
}
