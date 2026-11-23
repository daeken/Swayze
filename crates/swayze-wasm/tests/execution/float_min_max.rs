use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

const F32_OPS: [u8; 2] = [0x96, 0x97];
const F64_OPS: [u8; 2] = [0xa4, 0xa5];
const F32_ZERO: &[u8] = b"\x43\0\0\0\0";
const F64_ZERO: &[u8] = b"\x44\0\0\0\0\0\0\0\0";

fn check_bits(
    ops: [u8; 2],
    float: u8,
    result: ResultKind,
    value: impl Fn(u64) -> Value,
    [sign, min_normal, one, infinity, canonical]: [u64; 5],
) {
    let ty = types(&[(&[float, float], Some(float))]);
    // Numeric order, with -0 placed before +0 to encode the tie rule. Cross
    // every value with every other value (including itself) in both orders.
    let ordered = [
        sign | infinity,
        sign | (infinity - 1),
        sign | (one + 1),
        sign | one,
        sign | min_normal,
        sign | (min_normal - 1),
        sign | 1,
        sign,
        0,
        1,
        min_normal - 1,
        min_normal,
        one,
        one + 1,
        infinity - 1,
        infinity,
    ];
    let nan_bits = [infinity + 1, canonical, canonical | 0x1234];
    let nans = [0, sign].into_iter().flat_map(|s| nan_bits.map(|n| s | n));
    for (op_index, opcode) in ops.into_iter().enumerate() {
        let module = exported_function(&ty, &code_body(&[0, 0x20, 0, 0x20, 1, opcode, 0x0b]));
        let instance = module.instantiate().unwrap();
        let check = |lhs, rhs, expected| {
            assert_eq!(
                instance.invoke_with_values("answer", &[value(lhs), value(rhs)], result),
                Ok(Some(value(expected))),
                "opcode {opcode:#x}, lhs {lhs:#x}, rhs {rhs:#x}"
            );
        };
        for (i, lhs) in ordered.into_iter().enumerate() {
            for (j, rhs) in ordered.into_iter().enumerate() {
                check(lhs, rhs, ordered[[i.min(j), i.max(j)][op_index]]);
            }
        }
        for nan in nans.clone() {
            for other in ordered.into_iter().chain(nans.clone()) {
                check(nan, other, canonical);
                check(other, nan, canonical);
            }
        }
    }
}

#[test]
fn min_max_select_exact_operand_bits_or_canonical_nan() {
    check_bits(
        F32_OPS,
        0x7d,
        ResultKind::F32,
        |bits| Value::F32(bits as u32),
        [
            0x8000_0000,
            0x0080_0000,
            0x3f80_0000,
            0x7f80_0000,
            0x7fc0_0000,
        ],
    );
    check_bits(
        F64_OPS,
        0x7c,
        ResultKind::F64,
        Value::F64,
        [
            0x8000_0000_0000_0000,
            0x0010_0000_0000_0000,
            0x3ff0_0000_0000_0000,
            0x7ff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
        ],
    );
}

#[test]
fn min_max_validate_live_and_dead_floors_and_concrete_types() {
    use ValType::{F32, F64, I32, I64};
    const I32_ZERO: &[u8] = &[0x41, 0];
    const I64_ZERO: &[u8] = &[0x42, 0];
    for (ops, ty, zero, other, wrong_width) in [
        (F32_OPS, F32, F32_ZERO, F64, F64_ZERO),
        (F64_OPS, F64, F64_ZERO, F32, F32_ZERO),
    ] {
        for opcode in ops {
            // The matching float below the block's floor is not an input.
            let floor = [&[0, 0x41, 42][..], zero, &[0x02, 0x40]].concat();
            let tail = [opcode, 0x1a, 0x0b, 0x1a, 0x0b];
            let bad = |inputs: &[u8], kind| {
                let body = [&floor[..], inputs, &tail].concat();
                bad_body(&body, floor.len() + inputs.len(), kind);
            };
            bad(&[], ErrorKind::StackUnderflow { depth: 0 });
            bad(zero, ErrorKind::StackUnderflow { depth: 1 });
            // Underflow wins even if the only available input has the wrong type.
            bad(wrong_width, ErrorKind::StackUnderflow { depth: 1 });
            for prefix in [&[][..], &[0x00][..]] {
                for (wrong, actual) in [(wrong_width, other), (I32_ZERO, I32), (I64_ZERO, I64)] {
                    for pair in [[wrong, zero], [zero, wrong]] {
                        bad(
                            &[prefix, pair[0], pair[1]].concat(),
                            ErrorKind::TypeMismatch {
                                expected: ty,
                                actual,
                            },
                        );
                    }
                }
                // If both are wrong, the right-hand operand is diagnosed first.
                bad(
                    &[prefix, I32_ZERO, I64_ZERO].concat(),
                    ErrorKind::TypeMismatch {
                        expected: ty,
                        actual: I64,
                    },
                );
            }
            bad(
                &[&[0x00][..], wrong_width].concat(),
                ErrorKind::TypeMismatch {
                    expected: ty,
                    actual: other,
                },
            );
            assert_body_result(&[&floor[..], zero, zero, &tail].concat(), 42);
            for [lhs, rhs] in [[&[][..], &[][..]], [&[][..], zero], [&[0x1b][..], zero]] {
                // Dead floor supplies missing inputs, and select can push an unknown slot.
                let body = [&floor[..], &[0x00], lhs, rhs, &tail].concat();
                let module = exported_function(TYPE, &code_body(&body));
                assert_eq!(
                    module.instantiate().unwrap().invoke_i32("answer"),
                    Err(InvokeError::Trap(Trap::Unreachable))
                );
            }
            bad_body(
                &[0, 0x00, opcode, 0x0b],
                3,
                ErrorKind::TypeMismatch {
                    expected: I32,
                    actual: ty,
                },
            );
        }
    }
}

#[test]
fn min_max_use_the_existing_operand_stack_limit() {
    let limit = Limit::OperandStackDepth;
    let max = limit.maximum();
    for (ops, zero) in [(F32_OPS, F32_ZERO), (F64_OPS, F64_ZERO)] {
        for opcode in ops {
            let mut body = vec![0, 0x41, 42];
            body.extend([0x41, 0].repeat(max - 3));
            body.extend(zero);
            body.extend(zero);
            let at_max = body.len();
            body.push(opcode);
            body.extend(vec![0x1a; max - 2]);
            body.push(0x0b);
            assert_body_result(&body, 42);
            body.splice(at_max..at_max, zero.iter().copied());
            bad_body(&body, at_max, ErrorKind::LimitExceeded { limit });

            // Implicit dead inputs take no slots, but a concrete result does.
            let dead = |depth| {
                let mut body = vec![0, 0x41, 42];
                body.extend([0x41, 0].repeat(depth - 1));
                body.extend([0x02, 0x40, 0x00]);
                let op_offset = body.len();
                body.extend([opcode, 0x1a, 0x0b]);
                body.extend(vec![0x1a; depth - 1]);
                body.push(0x0b);
                (body, op_offset)
            };
            let module = exported_function(TYPE, &code_body(&dead(max - 1).0));
            assert_eq!(
                module.instantiate().unwrap().invoke_i32("answer"),
                Err(InvokeError::Trap(Trap::Unreachable))
            );
            let (body, offset) = dead(max);
            bad_body(&body, offset, ErrorKind::LimitExceeded { limit });
        }
    }
}

#[test]
fn min_max_fit_the_exact_body_byte_and_instruction_caps() {
    let instructions = Limit::BodyInstructions;
    let bytes = Limit::CodeBodyBytes;
    for (ops, zero, ty, result, value) in [
        (F32_OPS, F32_ZERO, 0x7d, ResultKind::F32, Value::F32(0)),
        (F64_OPS, F64_ZERO, 0x7c, ResultKind::F64, Value::F64(0)),
    ] {
        let ty = types(&[(&[], Some(ty))]);
        let check = |body: &[u8]| {
            let module = exported_function(&ty, &code_body(body));
            assert_eq!(
                module
                    .instantiate()
                    .unwrap()
                    .invoke_with_values("answer", &[], result),
                Ok(Some(value))
            );
        };
        for opcode in ops {
            let mut body = vec![0];
            body.extend(zero);
            body.extend(zero);
            body.extend(vec![0x01; instructions.maximum() - 3]);
            let last = body.len();
            body.extend([opcode, 0x0b]);
            check(&body);
            body.insert(last, 0x01);
            bad_typed_body(
                &ty,
                &body,
                last + 1,
                ErrorKind::LimitExceeded {
                    limit: instructions,
                },
            );

            let mut body = vec![0];
            body.extend(zero);
            while body.len() + zero.len() + 2 <= bytes.maximum() {
                body.extend(zero);
                body.push(opcode);
            }
            body.resize(bytes.maximum() - 1, 0x01);
            body.push(0x0b);
            check(&body);
            body.insert(body.len() - 1, 0x01);
            bad_code(
                &code_body(&body),
                1,
                ErrorKind::LimitExceeded { limit: bytes },
            );
        }
    }
}

#[test]
fn each_min_max_costs_one_dispatch_at_the_fuel_boundary() {
    for (ops, ty, result, pos, neg) in [
        (
            F32_OPS,
            0x7d,
            ResultKind::F32,
            Value::F32(0),
            Value::F32(1 << 31),
        ),
        (
            F64_OPS,
            0x7c,
            ResultKind::F64,
            Value::F64(0),
            Value::F64(1 << 63),
        ),
    ] {
        let ty = types(&[(&[0x7f, ty, ty], Some(ty))]);
        for (opcode, expected) in ops.into_iter().zip([neg, pos]) {
            let args = [Value::I32(113), pos, neg];
            let mut body = vec![0, 0x03, 0x40, 0x20, 1, 0x20, 2, opcode, 0x1a];
            body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]); // 9 per loop.
            body.extend([0x01; 4]);
            body.extend([0x20, 1, 0x20, 2]);
            let last = body.len();
            body.extend([opcode, 0x0b]); // 113 * 9 + 7 = 1024.
            let run = |body: &[u8]| {
                exported_function(&ty, &code_body(body))
                    .instantiate()
                    .unwrap()
                    .invoke_with_values("answer", &args, result)
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
}
