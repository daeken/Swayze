use super::*;

#[test]
fn result_table_exits_if_block_or_function_with_the_selected_floor() {
    let body = [
        0, 0x41, 40, 0x02, 0x7f, // Result block preserves 40.
        0x41, 7, 0x20, 0, 0x04, 0x7f, // Result if preserves 40 and 7.
        0x41, 9, 0x02, 0x40, // The inner frame freezes 9 as well.
        0x41, 5, 0x41, 2, 0x20, 1, 0x0e, 3, 1, 2, 1, 3, // if, block, if; default function.
        0x00, 0x0b, 0x1a, 0x41, 3, // Trap and normal continuation are skipped.
        0x05, 0x41, 4, 0x20, 1, 0x0d, 0, // Else: inline fixup to the same if.
        0x41, 6, 0x20, 1, 0x0e, 1, 1, 0, // Another table: block; default if.
        0x0b, 0x6a, 0x0b, 0x6a, 0x0b,
    ];
    let caller = [0, 0x41, 1, 0x20, 0, 0x20, 1, 0x10, 0, 0x6a, 0x0b];
    let module = exported_functions(2, &[(b"answer", 0), (b"caller", 1)], &[&body, &caller]);
    let instance = module.instantiate().unwrap();
    for (args, expected) in [
        ([1, 0], 49),
        ([1, 1], 42),
        ([1, 2], 49),
        ([1, 3], 2),
        ([1, -1], 2),
        ([1, i32::MIN], 2),
        ([1, i32::MAX], 2),
        ([0, 0], 46),
        ([0, 1], 51),
    ] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(expected));
        assert_eq!(
            instance.invoke_i32_with_args("caller", &args),
            Ok(expected + 1)
        );
    }
}

#[test]
fn void_table_continues_loop_or_exits_if_or_block_with_repeated_default() {
    let body = [
        1, 1, 0x7f, // One local counts entries to the loop.
        0x41, 40, 0x02, 0x40, 0x41, 9, 0x03, 0x40, // Loop preserves 40 and 9.
        0x20, 1, 0x41, 1, 0x6a, 0x21, 1, // Count this iteration.
        0x41, 7, 0x41, 1, 0x04, 0x40, // If additionally preserves 7, no else.
        0x41, 5, 0x20, 0, 0x41, 1, 0x21, 0, // Reentered loop selects the if exit.
        0x0e, 3, 1, 0, 2, 2, 0x00, // Loop, if, block; default is also block.
        0x0b, 0x1a, 0x41, 8, 0x21, 0, // If exit: drop 7 and set p0 to 8.
        0x0b, 0x1a, 0x0b, 0x20, 0, 0x6a, 0x20, 1, 0x6a, 0x0b,
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (selector, expected) in [(0, 50), (1, 49), (2, 42), (3, 42), (-1, 42)] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[selector]),
            Ok(expected)
        );
    }
}

#[test]
fn empty_table_still_takes_a_default_and_accepts_padded_uleb32() {
    assert_body_result(
        &[
            0, 0x41, 42, 0x41, 0x7f, 0x0e, 0x80, 0x80, 0x80, 0x80, 0, // Empty vector.
            0x80, 0x80, 0x80, 0x80, 0, // Mandatory function default.
            0x00, 0x0b,
        ],
        42,
    );
    assert_body_result(
        &[
            0, 0x02, 0x7f, 0x41, 42, 0x41, 0, 0x0e, 0x81, 0, // Padded count of one.
            0x81, 0x80, 0x80, 0x80, 0, 0, // Padded depth 1 (function); default block.
            0x0b, 0x00, 0x0b,
        ],
        42,
    );
}

#[test]
fn all_entries_and_default_must_agree_even_in_a_dead_tail() {
    // A void loop, i32-result block, and the i32 function are all in scope.
    let prefix = &[0, 0x03, 0x40, 0x02, 0x7f, 0x00, 0x0e][..];
    for (immediates, offset, expected, actual) in [
        (&[2, 0, 1, 2][..], 2, 1, 0), // Explicit mismatch against the first entry.
        (&[1, 2, 1], 2, 1, 0),        // Default mismatch.
        (&[1, 1, 0], 2, 0, 1),        // Mismatch in the other direction.
    ] {
        bad_body(
            &[prefix, immediates, &[0x0b, 0x0b, 0x0b]].concat(),
            prefix.len() + offset,
            ErrorKind::BranchTableArityMismatch { expected, actual },
        );
    }
}

#[test]
fn table_consumes_selector_and_result_only_above_active_floor() {
    let prefix = &[0, 0x41, 42, 0x02, 0x40][..]; // Freeze 42 in the void block.
    for (operands, label, depth) in [(&[][..], 0, 0), (&[][..], 1, 0), (&[0x41, 0], 1, 1)] {
        bad_body(
            &[prefix, operands, &[0x0e, 0, label, 0x0b, 0x0b]].concat(),
            prefix.len() + operands.len(),
            ErrorKind::StackUnderflow { depth },
        );
    }
}

#[test]
fn table_marks_only_the_active_frame_unreachable() {
    let prefix = &[
        0, 0x41, 42, 0x02, 0x40, 0x41, 9, 0x41, 7, 0x41, 0, 0x0e, 0, 1, // Take function exit.
        0x1a, 0x0e, 0, 1, // Dead drop and br_table can use implicit operands at the floor.
    ][..];
    assert_body_result(&[prefix, &[0x0b, 0x0b]].concat(), 7);
    bad_body(
        &[prefix, &[0x41, 1, 0x0b, 0x0b]].concat(),
        prefix.len() + 2,
        ErrorKind::InvalidResultDepth { depth: 1 },
    );
    bad_body(
        &[prefix, &[0x0b, 0x1a, 0x1a, 0x0b]].concat(),
        prefix.len() + 2,
        ErrorKind::StackUnderflow { depth: 0 },
    );
    bad_body(
        &[prefix, &[0x02, 0x40, 0x0e, 0, 0, 0x0b, 0x0b, 0x0b]].concat(),
        prefix.len() + 2,
        ErrorKind::StackUnderflow { depth: 0 },
    );
}

#[test]
fn validates_table_immediates_exactly_within_the_body_frame() {
    use IntegerField::{BranchTableCount, LabelIndex};
    let prefix = &[0, 0x00, 0x0e][..]; // Dead code still decodes count and every target.
    for (immediates, offset, kind) in [
        (
            &[][..],
            0,
            ErrorKind::TruncatedInteger {
                field: BranchTableCount,
            },
        ),
        (
            &[0x80],
            1,
            ErrorKind::TruncatedInteger {
                field: BranchTableCount,
            },
        ),
        (
            &[0x80, 0x80, 0x80, 0x80, 0x10],
            4,
            ErrorKind::IntegerOverflow {
                field: BranchTableCount,
            },
        ),
        (&[1], 1, ErrorKind::TruncatedInteger { field: LabelIndex }),
        (
            &[1, 0x80],
            2,
            ErrorKind::TruncatedInteger { field: LabelIndex },
        ),
        (
            &[1, 0x80, 0x80, 0x80, 0x80, 0x80, 0],
            5,
            ErrorKind::IntegerOverflow { field: LabelIndex },
        ),
        (
            &[1, 1, 0],
            1,
            ErrorKind::InvalidIndex {
                field: LabelIndex,
                index: 1,
            },
        ),
        (&[0], 1, ErrorKind::TruncatedInteger { field: LabelIndex }),
        (
            &[1, 0],
            2,
            ErrorKind::TruncatedInteger { field: LabelIndex },
        ),
        (
            &[1, 0, 0x80],
            3,
            ErrorKind::TruncatedInteger { field: LabelIndex },
        ),
        (
            &[0, 0x80, 0x80, 0x80, 0x80, 0x10],
            5,
            ErrorKind::IntegerOverflow { field: LabelIndex },
        ),
        (
            &[1, 0, 0xff, 0xff, 0xff, 0xff, 0x0f],
            2,
            ErrorKind::InvalidIndex {
                field: LabelIndex,
                index: u32::MAX,
            },
        ),
        // With no bytes after the default, there is still no function end.
        (&[1, 0, 0], 3, ErrorKind::TruncatedByte { expected: 0x0b }),
    ] {
        bad_body(&[prefix, immediates].concat(), prefix.len() + offset, kind);
    }
}

#[test]
fn exact_entry_and_instruction_caps_at_maximum_control_depth() {
    let limit = Limit::BodyInstructions;
    let mut body = vec![0];
    body.extend([0x02, 0x7f].repeat(16));
    body.extend([0x41, 42, 0x20, 0]);
    body.extend(std::iter::repeat_n(0x01, limit.maximum() - (3 + 16 * 2)));
    body.extend([0x0e, 16]);
    let count_offset = body.len() - 1;
    body.extend(0..=16); // Every block once, and the function default.
    let first_join_op = body.len() + 1;
    body.extend([0x0b, 0x41, 1, 0x6a].repeat(16)); // Each reached join adds one.
    body.push(0x0b);
    let ty = i32_type(1);
    let module = exported_function(&ty, &code_body(&body));
    let instance = module.instantiate().unwrap();
    for selector in 0..=16 {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[selector]),
            Ok(42 + 16 - selector)
        );
    }
    body[count_offset] = 17;
    bad_typed_body(
        &ty,
        &body,
        count_offset,
        ErrorKind::UnsupportedCount {
            field: IntegerField::BranchTableCount,
            count: 17,
        },
    );
    body[count_offset] = 16;
    body.insert(first_join_op, 0x01);
    bad_typed_body(
        &ty,
        &body,
        body.len() - 2,
        ErrorKind::LimitExceeded { limit },
    );
}

#[test]
fn aggregate_table_targets_fit_at_the_inclusive_code_body_byte_cap() {
    let limit = Limit::CodeBodyBytes;
    let mut body = vec![0, 0x02, 0x7f, 0x41, 42, 0x41, 0];
    for count in [16; 12].into_iter().chain([13, 0]) {
        body.extend([0x0e, count]);
        body.extend(std::iter::repeat_n(0, usize::from(count) + 1));
    }
    // 219 fixups point to the same forward label, with only 16 decoded ops.
    body.extend([0x0b, 0x0b]);
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 42);
    body.insert(1, 0x01);
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn each_table_dispatch_costs_one_including_loop_back_edges_and_final_exit() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let ops = [
        0x02, 0x40, 0x03, 0x40, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x45, // Is --p0 zero?
        0x0e, 1, 0, 1, 0x0b, 0x0b, // Loop if nonzero, exit the block otherwise.
        0x41, 42, 0x41, 0, 0x0e, 0, 0, 0x00, 0x0b, // Exit function before the trap.
    ];
    let module = exported_functions(
        1,
        &[(b"exact", 0), (b"over", 1)],
        &[
            &[&[0, 0x01][..], &ops].concat(),
            &[&[0, 0x01, 0x01][..], &ops].concat(),
        ],
    );
    let instance = module.instantiate().unwrap();
    // One nop, 170 * 6 loop dispatches, and three dispatches for the exit.
    assert_eq!(instance.invoke_i32_with_args("exact", &[170]), Ok(42));
    assert_eq!(
        instance.invoke_i32_with_args("over", &[170]),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(instance.invoke_i32_with_args("over", &[1]), Ok(42));
}
