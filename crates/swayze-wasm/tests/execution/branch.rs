use super::*;

#[test]
fn patches_both_branch_kinds_on_each_of_two_nested_block_chains() {
    let body = [
        0, 0x41, 40, 0x02, 0x7f, 0x02, 0x7f, // Both blocks preserve 40.
        0x41, 7, 0x41, 1, 0x20, 0, 0x0d, 0, // Conditional inner exit keeps 1.
        0x41, 2, 0x20, 1, 0x0d, 1, // Conditional outer exit keeps 2.
        0x03, 0x40, 0x20, 2, 0x04, 0x40, // Loop and if freeze the previous temporaries.
        0x41, 9, 0x41, 3, 0x0c, 0x83, 0x80, 0x80, 0x80, 0, // br 3 exits outer with 3.
        0x00, 0x0b, 0x0b, // Trap is skipped; close if and loop.
        0x41, 0, 0x0c, 0, // Unconditional inner exit keeps 0.
        0x0c, 0, 0x1a, // Dead br has an implicit operand; so does drop.
        0x0b, 0x0b, 0x6a, 0x0b, // Join with the preserved prefix.
    ];
    let module = exported_function(&i32_type(3), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (args, result) in [
        ([1, 1, 1], 41),
        ([0, 1, 1], 42),
        ([0, 0, 1], 43),
        ([0, 0, 0], 40),
    ] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(result));
    }
}

#[test]
fn void_exit_from_else_preserves_the_target_prefix_only() {
    assert_body_result(
        &[
            0, 0x41, 42, 0x02, 0x40, // Preserve 42 below the target floor.
            0x41, 17, 0x41, 0, 0x04, 0x40, // Freeze a temporary below the active floor.
            0x01, 0x05, 0x41, 0, 0x0c, 1, // Leave the block, discarding both temporaries.
            0x1a, 0x00, 0x0b, // The implicit drop uses the else floor.
            0x1a, 0x00, 0x0b, 0x0b, // Skipped parent tail must still validate.
        ],
        42,
    );
}

#[test]
fn result_branch_cannot_use_an_operand_below_the_active_floor() {
    bad_body(
        &[
            0, 0x02, 0x7f, 0x41, 7, 0x03, 0x40, // 7 is available to the block, not the loop.
            0x0c, 1, 0x0b, 0x0b, 0x0b,
        ],
        7,
        ErrorKind::StackUnderflow { depth: 0 },
    );
}

#[test]
fn dead_result_branch_does_not_push_an_operand_at_the_cap() {
    let limit = Limit::OperandStackDepth;
    let mut body = vec![0, 0x02, 0x7f, 0x41, 42];
    body.extend([0x41, 0].repeat(limit.maximum() - 1));
    // All 32 operands are frozen. br 0 skips the dead br 1, whose implicit
    // result does not become concrete as it would on br_if fallthrough.
    body.extend([0x02, 0x40, 0x0c, 0, 0x0c, 1]);
    let extra = body.len();
    body.push(0x0b);
    body.extend(std::iter::repeat_n(0x6a, limit.maximum() - 1));
    body.extend([0x0b, 0x0b]);
    assert_body_result(&body, 42);
    body.splice(extra..extra, [0x41, 0]);
    bad_body(&body, extra, ErrorKind::LimitExceeded { limit });
}

#[test]
fn dead_branch_tails_still_check_syntax_indices_and_stack_rules() {
    let branch = &[0, 0x41, 42, 0x02, 0x40, 0x0c, 0][..];
    for (tail, offset, kind) in [
        (
            &[0x0f, 0x0b, 0x0b][..],
            0,
            ErrorKind::UnsupportedOpcode { opcode: 0x0f },
        ),
        (
            &[0x0c, 0x80],
            2,
            ErrorKind::TruncatedInteger {
                field: IntegerField::LabelIndex,
            },
        ),
        (
            &[0x20, 0, 0x0b, 0x0b],
            1,
            ErrorKind::InvalidIndex {
                field: IntegerField::LocalIndex,
                index: 0,
            },
        ),
        (&[0x05, 0x0b, 0x0b], 0, ErrorKind::UnexpectedElse),
        (
            &[0x41, 0, 0x0b, 0x0b],
            2,
            ErrorKind::InvalidResultDepth { depth: 1 },
        ),
        (
            &[0x02, 0x40, 0x1a, 0x0b, 0x0b, 0x0b],
            2,
            ErrorKind::StackUnderflow { depth: 0 },
        ),
        (
            &[0x0b, 0x1a, 0x1a, 0x0b],
            2,
            ErrorKind::StackUnderflow { depth: 0 },
        ),
    ] {
        bad_body(&[branch, tail].concat(), branch.len() + offset, kind);
    }
    // A then-arm branch leaves the else arm reachable, even if it is untaken.
    bad_body(
        &[
            0, 0x02, 0x40, 0x41, 1, 0x04, 0x40, 0x0c, 1, 0x05, 0x1a, 0x0b, 0x0b, 0x0b,
        ],
        10,
        ErrorKind::StackUnderflow { depth: 0 },
    );
    // Marking the active frame is not permission to mark its target/parent.
    bad_body(
        &[0, 0x02, 0x40, 0x02, 0x40, 0x0c, 1, 0x0b, 0x1a, 0x0b, 0x0b],
        8,
        ErrorKind::StackUnderflow { depth: 0 },
    );
}

#[test]
fn back_edges_and_block_exits_each_dispatch_once_at_the_budget() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let entry = |nops| {
        let mut body = vec![0];
        body.extend(std::iter::repeat_n(0x01, nops));
        body.extend([
            0x02, 0x7f, 0x03, 0x40, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x45, 0x04,
            0x40, // --param == 0?
            0x41, 42, 0x0c, 2, 0x0b, // Last trip: exit block with 42.
            0x41, 9, 0x02, 0x40, 0x41, 7, 0x0c, 1, // Restart loop, dropping both temps.
            0x00, 0x0b, 0x1a, 0x00, 0x0b, 0x00, 0x0b, 0x0b, // Skipped traps and return.
        ]);
        body
    };
    let module = exported_functions(
        1,
        &[(b"exact", 0), (b"over", 1), (b"spin", 2)],
        &[
            &entry(8),
            &entry(9),
            &[0, 0x41, 42, 0x03, 0x40, 0x0c, 0, 0x0b, 0x0b],
        ],
    );
    let instance = module.instantiate().unwrap();
    let invoke = |name, n| instance.invoke_i32_with_args(name, &[n]);
    // 8 nops + 112 back edges at nine dispatches each + 8 final dispatches.
    // The forward branch is the last allowed dispatch; one extra nop blocks it.
    assert_eq!(invoke("exact", 113), Ok(42));
    assert_eq!(
        invoke("over", 113),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(invoke("spin", 0), Err(InvokeError::LimitExceeded { limit }));
    assert_eq!(invoke("over", 1), Ok(42));
    assert_eq!(invoke("exact", 113), Ok(42));
}
