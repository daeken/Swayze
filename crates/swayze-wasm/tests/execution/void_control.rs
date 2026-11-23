use super::*;

#[test]
fn void_if_allows_implicit_empty_and_explicit_else_arms() {
    for (otherwise, when_false) in [
        (&[][..], 40),
        (&[0x05][..], 40),
        (&[0x05, 0x41, 3, 0x21, 1], 43),
    ] {
        let body = [
            &[
                1, 1, 0x7f, 0x41, 40, // Preserve 40 below the void if floor.
                0x20, 0, 0x04, 0x40, 0x41, 2, 0x21, 1, // Then writes local 1.
            ][..],
            otherwise,
            &[0x0b, 0x20, 1, 0x6a, 0x0b],
        ]
        .concat();
        let module = exported_function(&i32_type(1), &code_body(&body));
        let instance = module.instantiate().unwrap();
        // Repeated calls with different conditions must start with a fresh local.
        for (condition, expected) in [(-1, 42), (0, when_false), (7, 42), (0, when_false)] {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &[condition]),
                Ok(expected)
            );
        }
    }
}

#[test]
fn implicit_else_can_join_at_an_enclosing_else_or_at_function_exit() {
    assert_body_result(
        &[
            0, 0x41, 1, 0x04, 0x7f, // Outer result if, select then.
            0x41, 42, 0x41, 0, 0x04, 0x40, 0x01, 0x0b, // Inner false edge meets outer else.
            0x05, 0x41, 9, 0x0b, // Outer else jump must still skip its body.
            0x41, 0, 0x04, 0x40, 0x01, 0x0b, 0x0b, // This false edge meets function exit.
        ],
        42,
    );
}

#[test]
fn void_branches_cross_mixed_frames_and_truncate_only_on_taken_edges() {
    let body = [
        1, 1, 0x7f, 0x41, 40, 0x02, 0x40, // Target block preserves 40.
        0x41, 10, 0x03, 0x40, // Loop freezes 10.
        0x41, 20, 0x20, 0, 0x04, 0x40, // Void if freezes 20.
        0x41, 8, 0x02, 0x7f, // Result block freezes 8.
        0x41, 9, 0x20, 1, 0x0d, 0x83, 0, // Padded depth 3: discard 10, 20, 8 and 9.
        0x41, 2, 0x20, 2, 0x0d, 0, 0x6a, 0x0b, // Result block exits with 2 or 9 + 2.
        0x6a, 0x21, 3, 0x05, // Then saves 8 + the result block's result.
        0x20, 1, 0x0d, 2, // Else's branch has only a condition above its floor.
        0x41, 3, 0x21, 3, 0x0b, // Else saves 3 on fallthrough.
        0x20, 3, 0x6a, 0x21, 3, 0x0b, // Add 20 and save before leaving the loop.
        0x20, 3, 0x6a, 0x21, 3, 0x0b, // Add 10 and save before leaving the void block.
        0x20, 3, 0x6a, 0x0b, // Add the preserved 40 to the local.
    ];
    let module = exported_function(&i32_type(3), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (args, expected) in [
        ([1, 0, 1], 80),  // Outer fallthrough, inner taken.
        ([1, -1, 1], 40), // Outer taken in then; local 3 is fresh.
        ([1, 0, 0], 89),  // Both fall through.
        ([0, 1, 0], 40),  // Outer taken in else.
        ([0, 0, 0], 73),
    ] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(expected));
    }
}

#[test]
fn void_branch_at_zero_floor_needs_no_result_and_skips_traps() {
    let body = [
        0, 0x02, 0x40, 0x20, 0, 0x0d, 0, // Condition is the only operand.
        0x41, 1, 0x41, 0, 0x6d, 0x1a, // Trap on fallthrough; statically balanced.
        0x0b, 0x41, 42, 0x0b,
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    let invoke = |arg| instance.invoke_i32_with_args("answer", &[arg]);
    assert_eq!(invoke(1), Ok(42));
    assert_eq!(invoke(0), Err(InvokeError::Trap(Trap::IntegerDivideByZero)));
    assert_eq!(invoke(i32::MIN), Ok(42));
}

#[test]
fn enforces_void_arm_depth_and_preserves_result_if_delimiter_diagnostics() {
    let void_then = &[0, 0x41, 42, 0x41, 0, 0x04, 0x40][..];
    for (tail, offset, kind) in [
        (&[][..], 0, ErrorKind::TruncatedByte { expected: 0x0b }),
        (
            &[0x41, 7, 0x0b, 0x0b][..],
            2,
            ErrorKind::InvalidResultDepth { depth: 1 },
        ),
        (
            &[0x41, 7, 0x05, 0x0b, 0x0b],
            2,
            ErrorKind::InvalidResultDepth { depth: 1 },
        ),
        (&[0x05, 0x05, 0x0b, 0x0b], 1, ErrorKind::UnexpectedElse),
        (
            &[0x06, 0x0b, 0x0b],
            0,
            ErrorKind::UnsupportedOpcode { opcode: 0x06 },
        ),
    ] {
        // Then is untaken, but still checked even without an else.
        bad_body(&[void_then, tail].concat(), void_then.len() + offset, kind);
    }
    // A selected then does not waive void else's requirement to leave no values.
    bad_body(
        &[0, 0x41, 42, 0x41, 1, 0x04, 0x40, 0x05, 0x41, 7, 0x0b, 0x0b],
        10,
        ErrorKind::InvalidResultDepth { depth: 1 },
    );
    for then in [&[][..], &[0x41, 7]] {
        let result_then = [&[0, 0x41, 0, 0x04, 0x7f][..], then].concat();
        bad_body(
            &result_then,
            result_then.len(),
            ErrorKind::TruncatedByte { expected: 0x05 },
        );
        bad_body(
            &[&result_then[..], &[0x0b, 0x0b]].concat(),
            result_then.len(),
            ErrorKind::MissingElse,
        );
    }
}

#[test]
fn void_floors_and_fallthrough_are_checked_even_on_untaken_paths() {
    // A literal taken branch does not waive checking depth or instructions after it.
    let block = &[0, 0x41, 42, 0x02, 0x40, 0x41, 1, 0x0d, 0][..];
    bad_body(
        &[block, &[0x41, 7, 0x0b, 0x0b]].concat(),
        block.len() + 2,
        ErrorKind::InvalidResultDepth { depth: 1 },
    );
    bad_body(
        &[block, &[0x12, 0x0b, 0x0b]].concat(),
        block.len(),
        ErrorKind::UnsupportedOpcode { opcode: 0x12 },
    );
    bad_body(
        &[block, &[0x1a, 0x41, 42, 0x0b, 0x0b]].concat(),
        block.len(),
        ErrorKind::StackUnderflow { depth: 0 },
    );

    for (condition, arm) in [(0, &[][..]), (1, &[0x05][..])] {
        let prefix = [
            &[
                0, 0x41, 42, 0x02, 0x40, 0x41, 9, 0x41, condition, 0x04, 0x40,
            ][..],
            arm,
        ]
        .concat();
        for bad in [&[0x0d, 1][..], &[0x1a, 0x41, 9], &[0x04, 0x40]] {
            // 9 is above the target block floor but cannot be accessed by the if arm.
            bad_body(
                &[&prefix[..], bad, &[0x0b, 0x1a, 0x0b, 0x0b]].concat(),
                prefix.len(),
                ErrorKind::StackUnderflow { depth: 0 },
            );
        }
    }
}

#[test]
fn conditional_branch_in_either_void_arm_counts_the_if_label() {
    for (condition, arm) in [(1, &[][..]), (0, &[0x05][..])] {
        let prefix = [&[0, 0x41, condition, 0x04, 0x40][..], arm, &[0x41, 1, 0x0d]].concat();
        let mut body = [&prefix[..], &[0, 0x00, 0x0b, 0x41, 42, 0x0b]].concat();
        assert_body_result(&body, 42);
        body[prefix.len()] = 2; // One beyond the function label.
        bad_body(
            &body,
            prefix.len(),
            ErrorKind::InvalidIndex {
                field: IntegerField::LabelIndex,
                index: 2,
            },
        );
    }
}

#[test]
fn void_constructs_share_the_inclusive_control_depth_cap() {
    let limit = Limit::ControlDepth;
    let nested = |depth| {
        let mut body = vec![0, 0x41, 42];
        let mut last_open = 0;
        for level in 0..depth {
            let op = [0x04, 0x03, 0x02][level % 3];
            if op == 0x04 {
                body.extend([0x41, 1]);
            }
            last_open = body.len();
            body.extend([op, 0x40]);
        }
        body.extend([0x41, 1, 0x0d, (depth - 1) as u8]); // Exit the outermost if.
        body.extend(std::iter::repeat_n(0x0b, depth + 1));
        (body, last_open)
    };
    assert_body_result(&nested(limit.maximum()).0, 42);
    let (over, last_open) = nested(limit.maximum() + 1);
    bad_body(&over, last_open, ErrorKind::LimitExceeded { limit });
}

#[test]
fn void_control_respects_inclusive_decoded_instruction_and_body_byte_caps() {
    let limit = Limit::BodyInstructions;
    let mut body = vec![0, 0x41, 42, 0x02, 0x40, 0x41, 1, 0x04, 0x40];
    body.extend(std::iter::repeat_n(0x01, limit.maximum() - 6));
    body.extend([0x05, 0x41, 1]);
    let branch = body.len();
    // br_if is instruction 127 (including if and else). The empty block at the cap is free.
    body.extend([0x0d, 1, 0x0b, 0x0b, 0x02, 0x40, 0x0b, 0x0b]);
    assert_body_result(&body, 42);
    body.insert(branch, 0x01);
    bad_body(&body, branch + 1, ErrorKind::LimitExceeded { limit });

    let limit = Limit::CodeBodyBytes;
    let ops = [
        0x41, 42, 0x02, 0x40, 0x41, 1, 0x04, 0x40, 0x41, 1, 0x0d, 0x80, 0x80, 0, 0x0b, 0x0b, 0x0b,
    ];
    let groups = (limit.maximum() - 1 - ops.len()) / 2;
    let mut body = vec![u8::try_from(groups).unwrap()];
    body.extend([0, 0x7f].repeat(groups));
    body.extend(ops);
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 42);
    body.insert(body.len() - 4, 0x80);
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn void_branch_operand_cap_counts_the_frozen_prefix() {
    let limit = Limit::OperandStackDepth;
    let floor = limit.maximum() - 1;
    let mut body = vec![0];
    body.extend([0x41, 1].repeat(floor));
    let if_condition = body.len() + 3;
    body.extend([0x02, 0x40, 0x41, 1, 0x04, 0x40]);
    let branch_condition = body.len();
    body.extend([0x41, 1, 0x0d, 1, 0x0b, 0x0b]);
    body.extend(std::iter::repeat_n(0x6a, floor - 1));
    body.push(0x0b);
    assert_body_result(&body, floor as i32);
    // Even in untaken code, a temporary below the condition exceeds the cap.
    body[if_condition] = 0;
    body.splice(
        branch_condition..branch_condition + 4,
        [0x41, 7, 0x41, 1, 0x0d, 1, 0x1a],
    );
    bad_body(
        &body,
        branch_condition + 2,
        ErrorKind::LimitExceeded { limit },
    );
}

#[test]
fn void_control_dispatches_share_loop_and_call_budget_and_allow_reuse() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let entry = |nops| {
        let mut body = vec![0];
        body.extend(std::iter::repeat_n(0x01, nops));
        body.extend([0x20, 0, 0x20, 1, 0x20, 2, 0x10, 2, 0x0b]);
        body
    };
    let leaf = [
        0, 0x41, 42, 0x03, 0x40, 0x02, 0x40, // Preserve 42 across the loop and block.
        0x20, 1, 0x04, 0x40, 0x20, 2, 0x0d, 1, // Then: exit the block if p2 is nonzero.
        0x01, 0x05, 0x01, 0x0b, 0x0b, // One nop in each arm.
        0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, // Continue until --p0 is zero.
        0x0b, 0x0b,
    ];
    let module = exported_functions(
        3,
        &[(b"exact", 0), (b"over", 1)],
        &[&entry(2), &entry(3), &leaf],
    );
    let instance = module.instantiate().unwrap();
    let invoke = |name, args| instance.invoke_i32_with_args(name, args);
    // Taken block exit: 6 caller ops + 1 const + 113 * (4 block ops + 5 loop ops) = 1024.
    assert_eq!(invoke("exact", &[113, 1, 1]), Ok(42));
    assert_eq!(
        invoke("over", &[113, 1, 1]),
        Err(InvokeError::LimitExceeded { limit })
    );
    // Zero if condition: 7 + 1 + 127 * (3 block ops + 5 loop ops) = 1024.
    assert_eq!(invoke("over", &[127, 0, 1]), Ok(42));
    // Then falls through to the else jump: 6 + 1 + 93 * (6 block ops + 5 loop ops) > 1024.
    assert_eq!(
        invoke("exact", &[93, 1, 0]),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(invoke("exact", &[113, 1, 1]), Ok(42));
}
