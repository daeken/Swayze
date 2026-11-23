use super::{
    ErrorKind, ExecutionLimit, InvokeError, Limit, assert_body_result, bad_body, code_body,
    exported_function, i32_type,
};

#[test]
fn counts_leading_zeros_trailing_zeros_and_ones_in_i32_bits() {
    // Columns distinguish clz, ctz, and popcnt, including a mixed pattern.
    for (index, opcode) in (0x67..=0x69).enumerate() {
        let code = code_body(&[0, 0x20, 0, opcode, 0x0b]);
        let module = exported_function(&i32_type(1), &code);
        let instance = module.instantiate().unwrap();
        for (arg, expected) in [
            (0, [32, 32, 0]),
            (i32::MIN, [0, 31, 1]),
            (-1, [0, 0, 32]),
            (0x4000_0020, [1, 5, 2]),
        ] {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &[arg]),
                Ok(expected[index]),
                "opcode {opcode:#x} with {arg:#010x}",
            );
        }
    }
}

#[test]
fn applies_distinct_bitwise_operations_to_i32_patterns() {
    for (index, opcode) in (0x71..=0x73).enumerate() {
        let code = code_body(&[0, 0x20, 0, 0x20, 1, opcode, 0x0b]);
        let module = exported_function(&i32_type(2), &code);
        let instance = module.instantiate().unwrap();
        for (args, expected) in [
            ([0, i32::MIN], [0, i32::MIN, i32::MIN]),
            ([-1, i32::MIN], [i32::MIN, -1, i32::MAX]),
            ([-1, -1], [-1, -1, 0]),
            (
                [i32::MIN | 0x5a, 0xb5],
                [0x10, i32::MIN | 0xff, i32::MIN | 0xef],
            ),
        ] {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &args),
                Ok(expected[index]),
                "opcode {opcode:#x} with {args:x?}",
            );
        }
    }
}

#[test]
fn shifts_and_rotates_by_only_the_low_five_count_bits() {
    // Columns: shl, shr_s, shr_u, rotl, rotr. At 31 all five differ.
    let lhs = i32::MIN | 5;
    let by_one = [10, -0x3fff_fffe, 0x4000_0002, 11, -0x3fff_fffe];
    let by_31 = [i32::MIN, -1, 1, -0x3fff_fffe, 11];
    let unchanged = [lhs; 5];
    let cases = [
        ([lhs, 0], unchanged),
        ([lhs, 1], by_one),
        ([lhs, 31], by_31),
        ([lhs, 32], unchanged),
        ([lhs, 33], by_one),
        ([lhs, -1], by_31),
        ([lhs, i32::MIN], unchanged),
        ([lhs, i32::MIN + 1], by_one),
        ([5, 1], [10, 2, 2, 10, i32::MIN | 2]),
        ([0, 31], [0; 5]),
        ([-1, 31], [i32::MIN, -1, 1, -1, -1]),
    ];
    for (index, opcode) in (0x74..=0x78).enumerate() {
        let code = code_body(&[0, 0x20, 0, 0x20, 1, opcode, 0x0b]);
        let module = exported_function(&i32_type(2), &code);
        let instance = module.instantiate().unwrap();
        for (args, expected) in cases {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &args),
                Ok(expected[index]),
                "opcode {opcode:#x} with {args:?}",
            );
        }
    }
}

#[test]
fn bit_results_leave_the_enclosing_operand_prefixes_untouched() {
    let code = code_body(&[
        0, 0x20, 0, 0x02, 0x7f, // p0 below the block floor.
        0x20, 1, 0x20, 2, 0x04, 0x7f, // Both p0 and p1 below either arm floor.
        0x20, 3, 0x68, // Then: ctz(p3).
        0x05, 0x20, 3, 0x41, 1, 0x76, // Else: shr_u(p3, 1).
        0x0b, 0x6b, 0x0b, 0x6b, 0x0b, // p0 - (p1 - result).
    ]);
    let module = exported_function(&i32_type(4), &code);
    let instance = module.instantiate().unwrap();
    for (condition, expected) in [(1, 35), (0, 0x4000_0023)] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[40, 7, condition, i32::MIN | 4]),
            Ok(expected),
        );
    }
}

#[test]
fn bit_ops_require_operands_above_the_floor_even_in_inactive_arms() {
    let push = [0x41, 1];
    let prefix = [0x41, 10, 0x41, 20];
    for (opcodes, arity) in [(0x67..=0x69, 1), (0x71..=0x78, 2)] {
        for opcode in opcodes {
            for depth in 0..arity {
                let operands = push.repeat(depth);
                let underflow = ErrorKind::StackUnderflow { depth };
                let body = [&[0][..], &operands, &[opcode, 0x0b]].concat();
                bad_body(&body, 1 + operands.len(), underflow);

                // Restore the depth that underflowing into the prefix would consume.
                let restore = push.repeat(arity - depth);
                let bad = [&operands[..], &[opcode], &restore].concat();
                for (condition, then, otherwise, arm_start) in
                    [(0, &bad[..], &push[..], 9), (1, &push[..], &bad[..], 12)]
                {
                    let body = [
                        &[0][..],
                        &prefix,
                        &[0x41, condition, 0x04, 0x7f],
                        then,
                        &[0x05],
                        otherwise,
                        &[0x0b, 0x6a, 0x6a, 0x0b],
                    ]
                    .concat();
                    bad_body(&body, arm_start + operands.len(), underflow);
                }

                let opener = [1, 1, 0x7f]; // Declare local zero to consume the loop result.
                for (control, tail) in [
                    (&[0x02, 0x7f][..], &[0x0b, 0x6a, 0x6a, 0x0b][..]),
                    (&[0x03, 0x40][..], &[0x21, 0, 0x0b, 0x6a, 0x0b][..]),
                ] {
                    let body = [&opener[..], &prefix, control, &bad, tail].concat();
                    bad_body(
                        &body,
                        opener.len() + prefix.len() + control.len() + operands.len(),
                        underflow,
                    );
                }
            }
        }
    }
}

#[test]
fn bit_ops_count_toward_the_decoded_instruction_limit() {
    let limit = Limit::BodyInstructions;
    let mut body = vec![0, 0x41, 0x7f]; // Push -1.
    body.extend(std::iter::repeat_n(0x69, limit.maximum() - 3));
    body.extend([0x41, 1, 0x71, 0x0b]); // Result: repeated popcnt & 1.
    assert_body_result(&body, 1);
    let and = body.len() - 2;
    body.insert(and, 0x69); // One more unary instruction before the and.
    bad_body(&body, and + 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn bit_ops_use_one_dispatch_each() {
    let body = [
        0, 0x41, 6, 0x41, 3, 0x71, // Keep 6 & 3 as the loop's prefix.
        0x03, 0x40, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x69, 0x0d, 0, 0x0b,
        // The loop repeats while popcnt(--p0) is nonzero.
        0x68, 0x0b, // Return ctz(6 & 3).
    ];
    let mut over = body.to_vec();
    over.insert(body.len() - 1, 0x69);
    for (body, expected) in [
        (&body[..], Ok(1)),
        (
            &over[..],
            Err(InvokeError::LimitExceeded {
                limit: ExecutionLimit::InstructionDispatches,
            }),
        ),
    ] {
        let module = exported_function(&i32_type(1), &code_body(body));
        // Three dispatches before the loop, six per iteration, and one after:
        // 3 + 170 * 6 + 1 = 1024. The extra popcnt goes over budget.
        assert_eq!(
            module
                .instantiate()
                .unwrap()
                .invoke_i32_with_args("answer", &[170]),
            expected,
        );
    }
}
