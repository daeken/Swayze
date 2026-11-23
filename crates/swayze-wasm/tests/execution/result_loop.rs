use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

#[test]
fn all_numeric_loop_results_fall_through_with_exact_tags_and_bits() {
    for (ty, constant, kind, value) in [
        (0x7f, &b"\x41\x7f"[..], ResultKind::I32, Value::I32(-1)),
        (
            0x7e,
            &b"\x42\x80\x80\x80\x80\x80\x01"[..],
            ResultKind::I64,
            Value::I64(1 << 35),
        ),
        (
            0x7d,
            &b"\x43\x12\x0b\x80\x7f"[..],
            ResultKind::F32,
            Value::F32(0x7f80_0b12), // Signaling NaN.
        ),
        (
            0x7c,
            &b"\x44\xef\xcd\xab\x89\x0b\x00\xf0\xff"[..],
            ResultKind::F64,
            Value::F64(0xfff0_000b_89ab_cdef), // Negative signaling NaN.
        ),
    ] {
        let body = [&[0, 0x03, ty][..], constant, &[0x0b, 0x0b]].concat();
        let module = exported_function(&types(&[(&[], Some(ty))]), &code_body(&body));
        assert_eq!(
            module
                .instantiate()
                .unwrap()
                .invoke_with_values("answer", &[], kind),
            Ok(Some(value)),
        );
    }
}

#[test]
fn conditional_continue_discards_temporaries_only_when_taken() {
    let body = [
        1, 1, 0x7f, // Local 1 counts trips.
        0x41, 40, 0x03, 0x7f, // Save 40 below the i32-result loop floor.
        0x20, 1, 0x41, 1, 0x6a, 0x21, 1, // Increment the trip count.
        0x41, 7, 0x02, 0x7e, // The nested i64-result block freezes 7.
        0x42, 2, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, // Push i64 temporary and --param.
        0x0d, 1, // Only consumes the condition; on continue, drop both 7 and the i64.
        0x0b, 0xa7, 0x6a, 0x20, 1, 0x6a, // Fallthrough computes 7 + 2 + trips.
        0x0b, 0x6a, 0x0b, // Add the saved 40 to the loop result.
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    // Forty trips would overflow the operand storage if temporaries accumulated.
    for (trips, expected) in [(40, 89), (1, 50)] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[trips]),
            Ok(expected)
        );
    }
}

#[test]
fn unconditional_continue_needs_no_payload() {
    let body = [
        0, 0x03, 0x7e, 0x20, 0, 0x04, 0x40, // Inside an i64-result loop and a void if.
        0x41, 0, 0x21, 0, 0x0c, 1, // Clear param and continue with no payload.
        0x00, 0x0b, // Taken continue skips the trap; next trip skips the arm.
        0x42, 42, 0x0b, 0xa7, 0x0b, // Produce the i64 result, then wrap outside the loop.
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32_with_args("answer", &[1]), Ok(42));
}

#[test]
fn table_can_share_void_block_label_but_not_an_equally_typed_result_label() {
    let body = [
        1, 1, 0x7f, // Local 1 counts trips.
        0x41, 40, 0x02, 0x40, 0x03, 0x7f, // Void block and i32-result loop.
        0x20, 1, 0x41, 1, 0x6a, 0x21, 1, // Increment the trip count.
        0x20, 0, 0x41, 1, 0x21, 0, // Select with the old param; default next time.
        0x0e, 1, 0, 1, // Case 0 continues; default exits the block. Both labels are void.
        0x0b, 0x1a, 0x0b, // Loop end pushes a concrete i32 even in the dead tail.
        0x20, 1, 0x6a, 0x0b,
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (selector, expected) in [(0, 42), (1, 41), (-1, 41)] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[selector]),
            Ok(expected)
        );
    }

    // Both constructs have i32 end results, but only the block's label does.
    let prefix = &[0, 0x02, 0x7f, 0x03, 0x7f, 0x00, 0x0e][..];
    for (labels, expected, actual) in [([0, 1], 0, 1), ([1, 0], 1, 0)] {
        bad_body(
            &[prefix, &[1, labels[0], labels[1], 0x0b, 0x0b, 0x0b]].concat(),
            prefix.len() + 2, // The mismatching default label byte, even in dead code.
            ErrorKind::BranchTableArityMismatch { expected, actual },
        );
    }
}

#[test]
fn result_loops_use_the_same_nested_floors_and_dead_tail_rules() {
    let prefix = &[0, 0x41, 42, 0x03, 0x7e][..];
    let underflow = ErrorKind::StackUnderflow { depth: 0 };
    let mismatch = |expected, actual| ErrorKind::TypeMismatch { expected, actual };
    for (tail, at, kind) in [
        (&b"\x0d\0"[..], 0, underflow), // Condition cannot use the frozen prefix.
        (
            &b"\x41\x01\x0d\0\x0b"[..],
            4,
            ErrorKind::InvalidResultDepth { depth: 0 },
        ),
        // A new inner block still starts reachable, with no selector above its floor.
        (&b"\0\x02\x40\x0e\0\x01"[..], 3, underflow),
        // Explicit wrong type and excess results still fail at the dead loop end.
        (
            &b"\x0c\0\x41\0\x0b"[..],
            4,
            mismatch(ValType::I64, ValType::I32),
        ),
        (
            &b"\x0c\0\x42\0\x42\x01\x0b"[..],
            6,
            ErrorKind::InvalidResultDepth { depth: 2 },
        ),
        (
            &b"\x0c\0\x12"[..],
            2,
            ErrorKind::UnsupportedOpcode { opcode: 0x12 },
        ),
        // An implicit result becomes a concrete i64 at the parent, which stays reachable.
        (
            &b"\x0c\0\x0b\x45"[..],
            3,
            mismatch(ValType::I32, ValType::I64),
        ),
        (&b"\x0c\0\x0b\x1a\x1a\x1a"[..], 5, underflow),
    ] {
        bad_body(
            &[prefix, tail, &[0x0b, 0x0b]].concat(),
            prefix.len() + at,
            kind,
        );
    }
}

#[test]
fn loop_end_result_counts_against_the_operand_cap() {
    let limit = Limit::OperandStackDepth;
    let prefix_len = limit.maximum() - 1;
    let mut body = vec![0];
    body.extend([0x41, 1].repeat(prefix_len));
    let opener = body.len();
    body.extend([0x03, 0x7f, 0x41, 0, 0x0b]);
    body.extend(std::iter::repeat_n(0x6a, prefix_len));
    body.push(0x0b);
    assert_body_result(&body, prefix_len as i32);

    // Fill the last slot before entry. The loop's dead tail may supply its
    // result implicitly, but pushing it at end would exceed the cap.
    body.splice(opener..opener + 4, [0x41, 0, 0x03, 0x7f, 0x00]);
    bad_body(&body, opener + 5, ErrorKind::LimitExceeded { limit });
}

#[test]
fn result_loop_dispatches_hit_the_exact_shared_budget() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let module = exported_functions(
        1,
        &[(b"exact", 0), (b"over", 1)],
        &[
            b"\0\x20\0\x10\x02\x0b",     // local.get, call
            b"\0\x01\x20\0\x10\x02\x0b", // Add one nop.
            b"\0\x41\x28\x03\x7f\x41\x02\x20\0\x41\x01\x6b\x22\0\x0d\0\x0b\x6a\x0b",
        ],
    );
    let instance = module.instantiate().unwrap();
    let invoke = |name, arg| instance.invoke_i32_with_args(name, &[arg]);
    // 2 caller ops + 1 const + 170 * 6 loop ops + 1 add = 1024. The last
    // br_if leaves 2 as the loop result; loop delimiters and returns cost nothing.
    assert_eq!(invoke("exact", 170), Ok(42));
    assert_eq!(
        invoke("over", 170),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(invoke("over", 169), Ok(42));
    assert_eq!(invoke("exact", 170), Ok(42));
}
