use super::{
    ErrorKind, ExecutionLimit, InvokeError, Limit, Trap, assert_body_result, bad_body, bad_code,
    code_body, exported_function, exported_functions, i32_type,
};
use swayze_wasm::ValType;

#[test]
fn select_keeps_the_first_value_for_every_nonzero_condition() {
    let code = code_body(b"\0\x01\x20\0\x20\x01\x20\x02\x1b\x0b");
    let module = exported_function(&i32_type(3), &code);
    let instance = module.instantiate().unwrap();
    for (condition, expected) in [
        (0, i32::MAX),
        (1, i32::MIN),
        (2, i32::MIN),
        (i32::MAX, i32::MIN),
        (-1, i32::MIN),
        (-2, i32::MIN),
        (i32::MIN, i32::MIN),
    ] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[i32::MIN, i32::MAX, condition]),
            Ok(expected),
            "condition {condition}",
        );
    }
}

#[test]
fn i64_select_preserves_high_bits_of_either_candidate() {
    // Both values have low bits 1, but differ above bit 31.
    let first = b"\x42\x81\x80\x80\x80\x10"; // 2^32 + 1
    let second = b"\x42\x81\x80\x80\x80\x70"; // -2^32 + 1
    let stem = [b"\0".as_slice(), first, second, b"\x20\0\x1b"].concat();
    let is_first = [stem.as_slice(), first, b"\x51\x0b"].concat();
    let is_second = [stem.as_slice(), second, b"\x51\x0b"].concat();
    let module = exported_functions(
        1,
        &[(b"is_first", 0), (b"is_second", 1)],
        &[&is_first, &is_second],
    );
    let instance = module.instantiate().unwrap();
    for condition in [0, 1, 2, -1, i32::MIN] {
        let args = &[condition];
        assert_eq!(
            instance.invoke_i32_with_args("is_first", args),
            Ok(i32::from(condition != 0)),
        );
        assert_eq!(
            instance.invoke_i32_with_args("is_second", args),
            Ok(i32::from(condition == 0)),
        );
    }
}

#[test]
fn stack_ops_preserve_enclosing_operands_across_arm_joins_and_block_exits() {
    let code = code_body(&[
        0, 0x41, 40, 0x02, 0x7f, // Preserve 40 below the block floor.
        0x20, 0, 0x04, 0x7f, // if p0
        0x41, 7, 0x01, 0x41, 12, 0x20, 1, 0x1b, // Then: select(7, 12, p1).
        0x41, 42, 0x01, 0x1a, // Discard 42, keeping the selected value.
        0x20, 1, 0x0d, 1, // Branch out of block with selected value if p1 != 0.
        0x05, 0x41, 33, 0x1a, 0x41, 5, 0x0b, // Else: discard 33 and yield 5.
        0x41, 1, 0x6a, 0x0b, // Normal block exit: add one to the arm result.
        0x6b, 0x0b, // Subtract the block result from 40.
    ]);
    let module = exported_function(&i32_type(2), &code);
    let instance = module.instantiate().unwrap();
    for (args, expected) in [([0, -1], 34), ([1, 0], 27), ([1, -1], 33)] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(expected));
    }
    bad_body(
        b"\0\x01\x41\x2a\x1a\x0b",
        5,
        ErrorKind::InvalidResultDepth { depth: 0 },
    );
}

#[test]
fn unselected_and_dropped_value_producers_still_trap() {
    let module = exported_functions(
        0,
        &[
            (b"first", 0),
            (b"second", 1),
            (b"discard", 2),
            (b"first_i64", 3),
            (b"second_i64", 4),
        ],
        &[
            b"\0\x41\x01\x41\0\x6d\x41\x07\x41\0\x1b\x0b", // select(1 / 0, 7, 0)
            b"\0\x41\x07\x41\x01\x41\0\x6d\x41\x7f\x1b\x0b", // select(7, 1 / 0, -1)
            b"\0\x41\x01\x41\0\x6d\x1a\x41\x07\x0b",       // drop(1 / 0); return 7
            b"\0\x42\x01\x42\0\x7f\x42\x07\x41\0\x1b\xa7\x0b", // i64: trap in first, then wrap
            b"\0\x42\x07\x42\x01\x42\0\x7f\x41\x7f\x1b\xa7\x0b", // i64: trap in second
        ],
    );
    let instance = module.instantiate().unwrap();
    for name in ["first", "second", "discard", "first_i64", "second_i64"] {
        assert_eq!(
            instance.invoke_i32(name),
            Err(InvokeError::Trap(Trap::IntegerDivideByZero))
        );
    }
}

#[test]
fn drop_and_select_require_operands_above_the_innermost_floor_even_in_skipped_code() {
    let push = [0x41, 1];
    let prefix = b"\0\x41\x0a\x41\x14\x41\x1e";
    let frames: [(&[u8], &[u8]); 4] = [
        // A block with three values protected below its floor.
        (b"\x02\x7f", b"\x0b\x6a\x6a\x6a\x0b"),
        // A loop with a higher floor than the active block. A taken branch skips it.
        (
            b"\x02\x7f\x41\x07\x41\x01\x0d\0\x03\x40",
            b"\x1a\x0b\x0b\x6a\x6a\x6a\x0b",
        ),
        // An untaken then arm and an untaken else arm, respectively.
        (b"\x41\0\x04\x7f", b"\x05\x41\x05\x0b\x6a\x6a\x6a\x0b"),
        (b"\x41\x01\x04\x7f\x41\x05\x05", b"\x0b\x6a\x6a\x6a\x0b"),
    ];
    for (opcode, count, results) in [(0x1a, 1, 0), (0x1b, 3, 1)] {
        for depth in 0..count {
            let operands = push.repeat(depth);
            let underflow = ErrorKind::StackUnderflow { depth };
            let body = [&[0][..], &operands, &[opcode, 0x0b]].concat();
            bad_body(&body, 1 + operands.len(), underflow);

            // Restore the expected depth if an implementation wrongly borrows
            // frozen operands. A later delimiter check must not mask the error.
            let restore = push.repeat(count - depth + 1 - results);
            let bad = [&operands[..], &[opcode], &restore].concat();
            for (head, tail) in frames {
                let body = [&prefix[..], head, &bad, tail].concat();
                bad_body(&body, prefix.len() + head.len() + operands.len(), underflow);
            }
        }
    }
}

#[test]
fn select_requires_an_i32_condition_for_i64_or_implicit_candidates() {
    let mismatch = ErrorKind::TypeMismatch {
        expected: ValType::I32,
        actual: ValType::I64,
    };
    bad_body(b"\0\x42\0\x42\0\x42\0\x1b\x0b", 7, mismatch);
    bad_body(b"\0\x00\x42\0\x42\0\x42\0\x1b\x0b", 8, mismatch);
    bad_body(
        b"\0\x00\x42\0\x1b\x0b", // The lone concrete input is the condition.
        4,
        mismatch,
    );
    bad_body(
        b"\0\x42\0\x1b\x0b", // In reachable code, underflow still wins.
        3,
        ErrorKind::StackUnderflow { depth: 1 },
    );
}

#[test]
fn counts_stack_ops_in_skipped_arms_and_checks_instruction_limit_first() {
    let limit = Limit::BodyInstructions;
    assert_eq!(limit.maximum(), 127);
    let mut body = b"\0\x41\x01\x04\x7f\x41\x05\x05".to_vec();
    body.extend([0x01; 117]);
    body.extend(b"\x41\x0c\x41\x0d\x41\0\x1b\x1a\x41\x07\x0b\x0b");
    assert_body_result(&body, 5); // 117 nops + 10 other decoded instructions.

    let extra = body.len() - 2;
    body.insert(extra, 0x01);
    for opcode in [0x01, 0x1a, 0x1b, 0x1c] {
        // At instruction 128 the count wins over select underflow and typed
        // select's unsupported opcode, even in the untaken arm.
        body[extra] = opcode;
        bad_body(&body, extra, ErrorKind::LimitExceeded { limit });
    }
}

#[test]
fn counts_nop_bytes_in_the_framed_body() {
    let limit = Limit::CodeBodyBytes;
    let mut body = vec![123];
    body.extend([0, 0x7f].repeat(123)); // 123 zero-count i32 local groups.
    body.extend(b"\x01\x01\x01\x01\x01\x01\x41\x2a\x0b");
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 42);
    body.insert(body.len() - 1, 0x01);
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn nop_and_select_work_at_the_operand_cap_including_a_frozen_prefix() {
    let limit = Limit::OperandStackDepth;
    assert_eq!(limit.maximum(), 32);
    for constant in [0x41, 0x42] {
        let mut body = vec![0];
        body.extend([0x41, 20].repeat(29));
        body.extend([0x03, 0x40, constant, 7, constant, 9, 0x41, 0]);
        let peak = body.len();
        body.extend(b"\x01\x1b\x01\x1a\x0b"); // nop; select; nop; drop; end loop.
        body.extend([0x1a; 28]);
        body.push(0x0b);
        assert_body_result(&body, 20);
        body.splice(peak..peak, [0x41, 1]);
        bad_body(&body, peak, ErrorKind::LimitExceeded { limit });
    }
}

#[test]
fn each_stack_op_costs_one_dispatch_and_limits_precede_the_next_trap() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let loop_body = |constant| {
        [
            0, 0x41, 42, 0x03, 0x40, // One dispatch before the loop, preserving 42.
            constant, 7, constant, 8, 0x41, 0, 0x1b, 0x1a, 0x01, // select, drop, nop.
            0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b, // Continue while --p0 != 0.
        ]
    };
    let trap = b"\x41\x01\x41\0\x6d\x1a"; // div_s would trap; drop its nominal result.
    for (tail, iterations, expected) in [
        // 1 + 93 * 11 = 1024. Ends and the return spend nothing; an extra nop does.
        (vec![], 93, Ok(42)),
        (vec![0x01], 93, Err(InvokeError::LimitExceeded { limit })),
        // 1 + 92 * 11 + 8 nops + 2 consts = 1023. The division can trap,
        // but with one more nop it cannot dispatch.
        (
            [&[0x01; 8][..], trap].concat(),
            92,
            Err(InvokeError::Trap(Trap::IntegerDivideByZero)),
        ),
        (
            [&[0x01; 9][..], trap].concat(),
            92,
            Err(InvokeError::LimitExceeded { limit }),
        ),
    ] {
        for constant in [0x41, 0x42] {
            let body = [&loop_body(constant)[..], &tail, &[0x0b]].concat();
            let module = exported_function(&i32_type(1), &code_body(&body));
            let instance = module.instantiate().unwrap();
            for _ in 0..2 {
                assert_eq!(
                    instance.invoke_i32_with_args("answer", &[iterations]),
                    expected
                );
            }
        }
    }
}
