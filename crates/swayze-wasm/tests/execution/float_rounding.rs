use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

const F32_ROUND: [u8; 4] = [0x8d, 0x8e, 0x8f, 0x90];
const F64_ROUND: [u8; 4] = [0x9b, 0x9c, 0x9d, 0x9e];
const F32_ZERO: &[u8] = b"\x43\0\0\0\0";
const F64_ZERO: &[u8] = b"\x44\0\0\0\0\0\0\0\0";

fn assert_rounding_cases(
    ops: [u8; 4],
    ty: u8,
    result: ResultKind,
    value: impl Fn(u64) -> Value,
    [
        sign,
        min_normal,
        half,
        one,
        two,
        three,
        four,
        integral,
        infinity,
        canonical,
    ]: [u64; 10],
) {
    let ty = types(&[(&[ty], Some(ty))]);
    let one_and_half = one + min_normal / 2;
    let two_and_half = two + min_normal / 4;
    // Fixed bit vectors, ordered ceil/floor/trunc/nearest. Immediately below
    // `integral` the representable spacing is 1/2, and above it all values
    // are already integral. `four - 1` exercises a carry into the exponent.
    let cases = [
        (0, [0; 4]),
        (1, [one, 0, 0, 0]),
        (min_normal - 1, [one, 0, 0, 0]),
        (min_normal, [one, 0, 0, 0]),
        (half - 1, [one, 0, 0, 0]),
        (half, [one, 0, 0, 0]),
        (half + 1, [one, 0, 0, one]),
        (one - 1, [one, 0, 0, one]),
        (one, [one; 4]),
        (one + 1, [two, one, one, one]),
        (one_and_half - 1, [two, one, one, one]),
        (one_and_half, [two, one, one, two]),
        (one_and_half + 1, [two, one, one, two]),
        (two - 1, [two, one, one, two]),
        (two, [two; 4]),
        (two + 1, [three, two, two, two]),
        (two_and_half - 1, [three, two, two, two]),
        (two_and_half, [three, two, two, two]),
        (two_and_half + 1, [three, two, two, three]),
        (four - 1, [four, three, three, four]),
        (
            integral - 3,
            [integral - 2, integral - 4, integral - 4, integral - 4],
        ),
        (integral - 2, [integral - 2; 4]),
        (
            integral - 1,
            [integral, integral - 2, integral - 2, integral],
        ),
        (integral, [integral; 4]),
        (integral + 1, [integral + 1; 4]),
        (infinity - 1, [infinity - 1; 4]),
        (infinity, [infinity; 4]),
    ];
    for (index, opcode) in ops.into_iter().enumerate() {
        let module = exported_function(&ty, &code_body(&[0, 0x20, 0, opcode, 0x0b]));
        let instance = module.instantiate().unwrap();
        let assert_result = |input, expected| {
            assert_eq!(
                instance.invoke_with_values("answer", &[value(input)], result),
                Ok(Some(value(expected))),
                "opcode {opcode:#x}, operand bits {input:#x}"
            );
        };
        for (magnitude, [ceil, floor, trunc, nearest]) in cases {
            assert_result(magnitude, [ceil, floor, trunc, nearest][index]);
            // Negation swaps ceil and floor and preserves the result's sign even at zero.
            assert_result(
                sign | magnitude,
                sign | [floor, ceil, trunc, nearest][index],
            );
        }
        for bits in [infinity + 1, canonical, canonical | 0x1234, sign - 1] {
            assert_result(bits, canonical);
            assert_result(sign | bits, canonical);
        }
    }
}

#[test]
fn rounding_has_exact_bits_for_both_signs_and_widths() {
    assert_rounding_cases(
        F32_ROUND,
        0x7d,
        ResultKind::F32,
        |bits| Value::F32(bits as u32),
        [
            0x8000_0000,
            0x0080_0000,
            0x3f00_0000,
            0x3f80_0000,
            0x4000_0000,
            0x4040_0000,
            0x4080_0000,
            0x4b00_0000,
            0x7f80_0000,
            0x7fc0_0000,
        ],
    );
    assert_rounding_cases(
        F64_ROUND,
        0x7c,
        ResultKind::F64,
        Value::F64,
        [
            0x8000_0000_0000_0000,
            0x0010_0000_0000_0000,
            0x3fe0_0000_0000_0000,
            0x3ff0_0000_0000_0000,
            0x4000_0000_0000_0000,
            0x4008_0000_0000_0000,
            0x4010_0000_0000_0000,
            0x4330_0000_0000_0000,
            0x7ff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
        ],
    );
}

#[test]
fn rounding_checks_concrete_types_and_respects_live_and_dead_floors() {
    use ValType::{F32, F64, I32, I64};
    for (ops, ty, zero, other, wrong_width) in [
        (F32_ROUND, F32, F32_ZERO, F64, F64_ZERO),
        (F64_ROUND, F64, F64_ZERO, F32, F32_ZERO),
    ] {
        for opcode in ops {
            // Freeze a correctly typed operand below an inner void block's floor.
            let floor = [&[0][..], zero, &[0x02, 0x40]].concat();
            let tail = [opcode, 0x1a, 0x0b, 0x1a, 0x41, 42, 0x0b];
            bad_body(
                &[&floor[..], &tail].concat(),
                floor.len(),
                ErrorKind::StackUnderflow { depth: 0 },
            );
            for prefix in [&[][..], &[0x00][..]] {
                for (wrong, actual) in [
                    (wrong_width, other),
                    (&[0x41, 0][..], I32),
                    (&[0x42, 0], I64),
                ] {
                    let operands = [&floor[..], prefix, wrong].concat();
                    bad_body(
                        &[&operands[..], &tail].concat(),
                        operands.len(),
                        ErrorKind::TypeMismatch {
                            expected: ty,
                            actual,
                        },
                    );
                }
            }
            for input in [&[][..], &[0x1b][..]] {
                // Dead floor synthesizes an input; a polymorphic select can also supply it.
                let body = [&floor[..], &[0x00], input, &tail].concat();
                let module = exported_function(TYPE, &code_body(&body));
                assert_eq!(
                    module.instantiate().unwrap().invoke_i32("answer"),
                    Err(InvokeError::Trap(Trap::Unreachable))
                );
            }
            bad_body(
                &[0, 0, opcode, 0x0b],
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
fn rounding_at_a_full_stack_uses_existing_caps() {
    for (zero, ops) in [(F32_ZERO, F32_ROUND), (F64_ZERO, F64_ROUND)] {
        let mut body = vec![0, 0x41, 42];
        for _ in 1..31 {
            body.extend([0x41, 0]);
        }
        body.extend(zero);
        let mut at_floor = body.clone();
        body.extend(ops);
        body.extend([0x1a; 31]);
        body.extend([0x01; 60]); // 32 pushes + 4 rounding + 31 drops + 60 nops = 127.
        let end = body.len();
        body.push(0x0b);
        assert_body_result(&body, 42);
        body.insert(end, 0x01);
        bad_body(
            &body,
            end,
            ErrorKind::LimitExceeded {
                limit: Limit::BodyInstructions,
            },
        );

        // An implicit input at the full dead floor needs no slot, but its result does.
        let offset = at_floor.len() + 3;
        at_floor.extend([0x02, 0x40, 0x00, ops[0], 0x1a, 0x0b, 0x0b]);
        bad_body(
            &at_floor,
            offset,
            ErrorKind::LimitExceeded {
                limit: Limit::OperandStackDepth,
            },
        );
    }
}

#[test]
fn each_rounding_opcode_costs_one_dispatch_at_the_fuel_boundary() {
    for (ops, float, result, input, negative_zero, negative_one) in [
        (
            F32_ROUND,
            0x7d,
            ResultKind::F32,
            Value::F32(0x8000_0001),
            Value::F32(0x8000_0000),
            Value::F32(0xbf80_0000),
        ),
        (
            F64_ROUND,
            0x7c,
            ResultKind::F64,
            Value::F64(0x8000_0000_0000_0001),
            Value::F64(0x8000_0000_0000_0000),
            Value::F64(0xbff0_0000_0000_0000),
        ),
    ] {
        let ty = types(&[(&[0x7f, float], Some(float))]);
        for (opcode, expected) in
            ops.into_iter()
                .zip([negative_zero, negative_one, negative_zero, negative_zero])
        {
            let args = [Value::I32(127), input];
            let mut body = vec![0, 0x03, 0x40, 0x20, 1, opcode, 0x1a]; // Loop until --p0 == 0.
            body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]); // 8 per loop.
            body.extend([0x01; 6]);
            body.extend([0x20, 1]);
            let last = body.len();
            body.extend([opcode, 0x0b]); // 8 * 127 + 8 = 1024, ending with this rounding.
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
