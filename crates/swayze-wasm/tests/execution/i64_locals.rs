use super::*;
use swayze_wasm::ValType::{I32, I64};

#[test]
fn mixed_groups_follow_parameter_order_and_keep_full_width_values() {
    let body = [
        5, 2, 0x7e, 0, 0x7f, 1, 0x7f, 0, 0x7e, 1, 0x7e, // i64 slots 2, 3, 5; i32 slot 4.
        0x20, 2, 0x50, 0x20, 3, 0x50, 0x71, // Both i64s start at zero.
        0x20, 4, 0x45, 0x71, 0x20, 5, 0x50, 0x71, // So do the last two slots.
        0x20, 0, 0x22, 4, 0x1a, // Tee arg 0 into the i32 local.
        0x20, 1, 0xac, 0x42, 32, 0x86, 0x21, 2, // Store arg 1 in the high word of slot 2.
        0x20, 2, 0x42, 1, 0x7c, 0x22, 5, 0x20, 5, 0x51, 0x71, // Tee and read full width.
        0x20, 5, 0x42, 1, 0x7d, 0x20, 2, 0x51, 0x71, // Slot 5 must hold slot 2 + 1.
        0x20, 2, 0x42, 32, 0x87, 0xa7, 0x20, 1, 0x46, 0x71, // Check the saved high word.
        0x20, 3, 0x50, 0x71, // The other local from the same group stays zero.
        0x20, 4, 0x20, 0, 0x46, 0x71, 0x0b,
    ];
    let module = exported_function(&i32_type(2), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for args in [[9, -4], [-7, i32::MIN], [2, 1]] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(1));
    }
}

#[test]
fn mixed_groups_share_the_exact_declaration_and_index_bounds() {
    let ty = i32_type(4);
    // Padded local group count, last nonempty group's count, and final index.
    // Slots 4..11 are i32 and 12..19 are i64. The final empty group fits at the cap.
    let locals = [0x83, 0, 8, 0x7f, 0x88, 0, 0x7e, 0, 0x7f];
    let ops = [0x20, 3, 0xac, 0x21, 0x93, 0, 0x20, 0x93, 0, 0xa7, 0x0b];
    let body = [&locals[..], &ops].concat();
    let module = exported_function(&ty, &code_body(&body));
    assert_eq!(
        module
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[1, 2, 3, 4]),
        Ok(4)
    );

    // Count rejection precedes reading a missing or unsupported type byte.
    for tail in [&[1][..], &[1, 0x7d]] {
        let body = [&locals[..7], tail].concat();
        bad_typed_body(
            &ty,
            &body,
            7,
            ErrorKind::UnsupportedCount {
                field: IntegerField::LocalCount,
                count: 1,
            },
        );
    }

    for head in [&locals[..], &[&locals[..], &[0x00]].concat()] {
        for op in [0x20, 0x21, 0x22] {
            // Bounds win before any stack effect (there is no explicit operand).
            let body = [head, &[op, 20, 0x0b]].concat();
            bad_typed_body(
                &ty,
                &body,
                head.len() + 1,
                ErrorKind::InvalidIndex {
                    field: IntegerField::LocalIndex,
                    index: 20,
                },
            );
        }
    }
}

#[test]
fn local_index_framing_and_cap_diagnostics_keep_their_precedence() {
    let mut code = code_body(&[1, 1, 0x7e, 0x22, 0x80]);
    let boundary = code.len();
    code.extend([0, 0x0b]); // Bytes outside the body cannot complete the index.
    bad_code(
        &code,
        boundary,
        ErrorKind::TruncatedInteger {
            field: IntegerField::LocalIndex,
        },
    );

    let limit = Limit::BodyInstructions;
    let mut body = vec![1, 1, 0x7e];
    body.extend(std::iter::repeat_n(0x01, limit.maximum()));
    body.push(0x20); // The instruction cap wins before decoding the missing index.
    bad_body(&body, body.len() - 1, ErrorKind::LimitExceeded { limit });

    let limit = Limit::OperandStackDepth;
    let mut body = vec![1, 1, 0x7e];
    body.extend([0x20, 0].repeat(limit.maximum()));
    let opcode_offset = body.len();
    body.extend([0x20, 0, 0x0b]);
    bad_body(&body, opcode_offset, ErrorKind::LimitExceeded { limit });
    body[opcode_offset + 1] = 1;
    bad_body(
        &body,
        opcode_offset + 1,
        ErrorKind::InvalidIndex {
            field: IntegerField::LocalIndex,
            index: 1,
        },
    );
}

#[test]
fn typed_local_consumers_and_producers_reject_wrong_types_in_live_and_dead_code() {
    // Slot 0 is an i32 parameter, slot 1 an i64 local.
    let ty = i32_type(1);
    for head in [&b"\x01\x01\x7e"[..], b"\x01\x01\x7e\0"] {
        for (push, op, index, expected, actual) in [
            (0x41, 0x21, 1, I64, I32),
            (0x41, 0x22, 1, I64, I32),
            (0x42, 0x21, 0, I32, I64),
            (0x42, 0x22, 0, I32, I64),
        ] {
            let body = [head, &[push, 0, op, index, 0x0b]].concat();
            bad_typed_body(
                &ty,
                &body,
                head.len() + 2,
                ErrorKind::TypeMismatch { expected, actual },
            );
        }
        for (index, op, expected, actual) in [(1, 0x45, I32, I64), (0, 0x50, I64, I32)] {
            let body = [head, &[0x20, index, op, 0x0b]].concat();
            bad_typed_body(
                &ty,
                &body,
                head.len() + 2,
                ErrorKind::TypeMismatch { expected, actual },
            );
        }
    }
}

#[test]
fn polymorphic_local_inputs_never_read_below_the_active_floor() {
    // The branch preserves the i32 prefix. The skipped set and the first tee
    // use implicit operands; the second tee uses an unknown result from select.
    assert_body_result(
        b"\x01\x01\x7e\x41\x2a\x02\x40\x0c\0\x21\0\x22\0\x50\x1a\x1b\x22\0\x50\x1a\x0b\x0b",
        42,
    );
    for source in [&b""[..], b"\x1b"] {
        // Even with an implicit or unknown input, tee's result is a concrete i64.
        let head = b"\x01\x01\x7e\0";
        let body = [&head[..], source, b"\x22\0\x45\x0b"].concat();
        bad_body(
            &body,
            head.len() + source.len() + 2,
            ErrorKind::TypeMismatch {
                expected: I32,
                actual: I64,
            },
        );
    }
    for op in [0x21, 0x22] {
        // A matching i64 below the floor cannot satisfy a reachable pop.
        bad_body(
            &[1, 1, 0x7e, 0x42, 0, 0x02, 0x40, op, 0, 0x0b],
            7,
            ErrorKind::StackUnderflow { depth: 0 },
        );
    }
}

#[test]
fn i64_locals_survive_calls_and_reused_callee_slots_get_fresh_zeros() {
    let ty = i32_types(&[0, 1]);
    // Both functions use i64 local 1. The callee checks its initial zero, then
    // stores a nonzero value entirely in the high word, computed from its arg.
    let callee = [
        1, 1, 0x7e, 0x20, 1, 0x50, // Was local 1 zero on entry?
        0x20, 0, 0xad, 0x42, 32, 0x86, 0x21, 1, // Save arg << 32.
        0x20, 1, 0x50, 0x45, 0x71, 0x0b, // Verify it was set before returning.
    ];
    for call in [&b"\x10\x01"[..], b"\x41\0\x11\x01\0"] {
        let mut caller = vec![2, 1, 0x7f, 1, 0x7e];
        let high_word = [0x42, 1, 0x42, 32, 0x86]; // 1 << 32.
        caller.extend(high_word);
        caller.extend([0x21, 1]);
        for arg in [2, 3] {
            caller.extend([0x41, arg]);
            caller.extend(call);
        }
        caller.extend([0x71, 0x20, 1]);
        caller.extend(high_word);
        caller.extend([0x51, 0x71, 0x0b]); // Confirm caller local 1 did not change.
        let bytes = module(&[
            (1, &ty),
            (3, &[2, 0, 1]),
            (4, &[1, 0x70, 0, 1]),
            (7, EXPORT),
            (9, &[1, 0, 0x41, 0, 0x0b, 1, 1]),
            (10, &code_bodies(&[&caller, &callee])),
        ]);
        let module = validate_module(&bytes).unwrap();
        let instance = module.instantiate().unwrap();
        assert_eq!(instance.invoke_i32("answer"), Ok(1));
    }
}

#[test]
fn every_i64_local_instruction_spends_exactly_one_dispatch() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    for (ops, dispatches) in [
        (&b"\x20\x01\x1a"[..], 2),    // get, drop
        (b"\x42\x01\x21\x01", 2),     // const, set
        (b"\x42\x01\x22\x01\x1a", 3), // const, tee, drop
    ] {
        let base = [
            &b"\x01\x01\x7e\x41\x2a\x03\x40"[..],
            ops,
            b"\x20\0\x41\x01\x6b\x22\0\x0d\0\x0b",
        ]
        .concat();
        // One dispatch before the loop and five more per iteration to count down.
        let per_iteration = dispatches + 5;
        let iterations = (limit.maximum() - 1) / per_iteration;
        let nops = (limit.maximum() - 1) % per_iteration;
        for (nops, expected) in [
            (nops, Ok(42)),
            (nops + 1, Err(InvokeError::LimitExceeded { limit })),
        ] {
            let body = [&base[..], &vec![0x01; nops], &[0x0b]].concat();
            let module = exported_function(&i32_type(1), &code_body(&body));
            assert_eq!(
                module
                    .instantiate()
                    .unwrap()
                    .invoke_i32_with_args("answer", &[i32::try_from(iterations).unwrap()]),
                expected,
                "loop ops {ops:x?}, {nops} trailing nops",
            );
        }
    }
}
