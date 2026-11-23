use super::*;

#[test]
fn both_arms_contribute_fixups_to_the_same_result_if() {
    let body = [
        0, 0x41, 40, 0x20, 0, 0x04, 0x7f, // Preserve 40 below the if floor.
        0x41, 7, 0x41, 1, 0x20, 1, 0x0d, 0, // Then: keep 1 on a taken branch.
        0x1a, 0x41, 2, 0x0c, 0, 0x00, // Otherwise exit with 2, skipping the trap.
        0x05, 0x41, 8, 0x41, 3, 0x20, 1, 0x0d, 0, // Else: keep 3 on a taken branch.
        0x6a, 0x0b, 0x6a, 0x0b, // Untaken br_if leaves both 8 and 3, then join.
    ];
    let module = exported_function(&i32_type(2), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (args, expected) in [([1, -1], 41), ([1, 0], 42), ([0, 1], 43), ([0, 0], 51)] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(expected));
    }
}

#[test]
fn nested_branches_exit_enclosing_if_and_discard_intervening_operands() {
    let body = [
        0, 0x41, 40, 0x20, 0, 0x04, 0x7f, // Preserve 40 at the outer if floor.
        0x41, 9, 0x02, 0x40, 0x03, 0x40, 0x20, 1, 0x04, 0x40, // Freeze 9 at inner floors.
        0x41, 7, 0x41, 2, 0x20, 2, 0x0d, 3, // Cross the inner if, loop and block with 2.
        0x1a, 0x0c, 3, 0x00, // Or discard 2 and exit with 7, skipping the trap.
        0x0b, 0x0b, 0x0b, 0x1a, 0x41, 1, // Normal then path supplies 1.
        0x05, 0x41, 3, 0x0b, 0x6a, 0x0b, // Else supplies 3; join adds the prefix.
    ];
    let module = exported_function(&i32_type(3), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (args, expected) in [
        ([1, 0, 0], 41),
        ([1, 1, 1], 42),
        ([1, 1, 0], 47),
        ([0, 1, 1], 43),
    ] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(expected));
    }
}

#[test]
fn void_if_branch_and_implicit_else_join_at_end() {
    let body = [
        0, 0x41, 40, 0x20, 0, 0x04, 0x40, // Preserve 40 below the floor.
        0x41, 9, 0x0c, 0x80, 0, 0x00, 0x0b, // br 0 discards 9, skips the trap.
        0x41, 2, 0x6a, 0x0b,
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for condition in [0, 1] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[condition]),
            Ok(42)
        );
    }
}

#[test]
fn result_branches_pop_only_above_the_active_floor_in_either_arm() {
    for (condition, arm) in [(0, &[][..]), (1, &[0x41, 7, 0x05][..])] {
        let prefix = [
            &[0, 0x41, condition, 0x04, 0x7f][..],
            arm,
            &[0x41, 9, 0x03, 0x40], // 9 is above the target floor but frozen by the loop.
        ]
        .concat();
        for (operand, opcode, depth) in [(&[][..], 0x0c, 0), (&[0x41, 1][..], 0x0d, 1)] {
            let body = [&prefix[..], operand, &[opcode, 1, 0x0b, 0x0b, 0x0b]].concat();
            bad_body(
                &body,
                prefix.len() + operand.len(),
                ErrorKind::StackUnderflow { depth },
            );
        }
    }
}

#[test]
fn if_branches_do_not_relax_dead_tail_or_other_arm_validation() {
    let prefix = &[0, 0x41, 1, 0x04, 0x7f, 0x41, 42, 0x0c, 0][..];
    for (tail, offset, kind) in [
        (&[0x0b, 0x0b][..], 0, ErrorKind::MissingElse),
        (
            &[0x12, 0x05, 0x41, 7, 0x0b, 0x0b],
            0,
            ErrorKind::UnsupportedOpcode { opcode: 0x12 },
        ),
        (
            &[0x05, 0x1a, 0x41, 7, 0x0b, 0x0b],
            1,
            ErrorKind::StackUnderflow { depth: 0 },
        ),
    ] {
        bad_body(&[prefix, tail].concat(), prefix.len() + offset, kind);
    }
    // br makes the inner block polymorphic, but not its target if arm.
    bad_body(
        &[
            0, 0x41, 1, 0x04, 0x40, 0x02, 0x40, 0x0c, 1, 0x1a, 0x0b, 0x1a, 0x0b, 0x41, 42, 0x0b,
        ],
        11,
        ErrorKind::StackUnderflow { depth: 0 },
    );
}

#[test]
fn dead_result_branch_uses_active_polymorphism_at_operand_cap() {
    let limit = Limit::OperandStackDepth;
    let mut body = vec![0, 0x41, 1, 0x04, 0x7f, 0x41, 42];
    body.extend([0x41, 0].repeat(limit.maximum() - 1));
    body.extend([0x02, 0x40, 0x0c, 0]); // Exit the inner block, freezing all 32 operands.
    let dead_branch = body.len();
    body.extend([0x0c, 1, 0x0b]); // The result for the enclosing if is implicit here.
    body.extend(std::iter::repeat_n(0x6a, limit.maximum() - 1));
    body.extend([0x05, 0x41, 7, 0x0b, 0x0b]);
    assert_body_result(&body, 42);

    // br_if can pop the implicit condition and result, but pushes a concrete result.
    body[dead_branch] = 0x0d;
    bad_body(&body, dead_branch, ErrorKind::LimitExceeded { limit });
}

#[test]
fn patches_the_final_decoded_slot_to_the_one_past_last_join() {
    let limit = Limit::BodyInstructions;
    let mut body = vec![0, 0x41, 0, 0x04, 0x7f, 0x41, 7, 0x0c, 0];
    body.extend(std::iter::repeat_n(0x01, limit.maximum() - 7));
    body.extend([0x05, 0x41, 42]);
    let last_branch = body.len();
    body.extend([0x0c, 0, 0x0b, 0x0b]);
    assert_body_result(&body, 42);
    body[2] = 1;
    assert_body_result(&body, 7);

    // Even when the then arm is selected, the else branch counts toward admission.
    body.insert(last_branch, 0x01);
    bad_body(&body, last_branch + 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn taken_if_exit_can_be_the_last_allowed_dispatch() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let ops = [
        0x41, 1, 0x04, 0x7f, // Enter the then arm.
        0x03, 0x40, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b, // Loop until --p0 == 0.
        0x41, 42, 0x0c, 0, 0x00, 0x05, 0x41, 7, 0x0b, 0x0b, // Skip the trap and else arm.
    ];
    let module = exported_functions(
        1,
        &[(b"exact", 0), (b"over", 1)],
        &[&[&[0][..], &ops].concat(), &[&[0, 0x01][..], &ops].concat()],
    );
    let instance = module.instantiate().unwrap();
    // 2 to enter the arm + 204 * 5 in the loop + 2 to leave the if.
    assert_eq!(instance.invoke_i32_with_args("exact", &[204]), Ok(42));
    assert_eq!(
        instance.invoke_i32_with_args("over", &[204]),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(instance.invoke_i32_with_args("over", &[1]), Ok(42));
}
