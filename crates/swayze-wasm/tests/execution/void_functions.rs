use super::*;

const NO_ARGS_I32: u8 = 0;
const NO_ARGS_VOID: u8 = 1;
const ONE_ARG_VOID: u8 = 2;
const ONE_ARG_I32: u8 = 3;
const FOUR_ARGS_VOID: u8 = 4;
const TYPES: &[u8] = &[
    5, // Number of types.
    0x60, 0, 1, 0x7f, // () -> i32
    0x60, 0, 0, // () -> ()
    0x60, 1, 0x7f, 0, // (i32) -> ()
    0x60, 1, 0x7f, 1, 0x7f, // (i32) -> i32
    0x60, 4, 0x7f, 0x7f, 0x7f, 0x7f, 0, // (i32, i32, i32, i32) -> ()
];
const MUTABLE_ZERO: &[u8] = &[1, 0x7f, 1, 0x41, 0, 0x0b];
const EMPTY_BODY: &[u8] = &[0, 0x0b];
const ANSWER_42: &[u8] = &[0, 0x41, 42, 0x0b];
const CALL_HELPER_AND_READ_GLOBAL: &[u8] = &[
    0, 0x41, 40, 0x20, 0, 0x10, 1, 0x23, 0, 0x6a, 0x0b, // 40 + global, after f1(arg).
];

fn declarations(type_indices: &[u8]) -> Vec<u8> {
    let mut declarations = vec![u8::try_from(type_indices.len()).unwrap()];
    declarations.extend_from_slice(type_indices);
    module(&[(1, TYPES), (3, &declarations)])
}

fn mixed_module(
    type_indices: &[u8],
    globals: Option<&[u8]>,
    exports: &[(&[u8], usize)],
    bodies: &[&[u8]],
) -> Module {
    let mut bytes = declarations(type_indices);
    if let Some(globals) = globals {
        push_section(&mut bytes, 6, globals);
    }
    push_section(&mut bytes, 7, &function_exports(exports));
    push_section(&mut bytes, 10, &code_bodies(bodies));
    validate_module(&bytes).unwrap()
}

fn bad_last_body(type_indices: &[u8], bodies: &[&[u8]], offset: usize, kind: ErrorKind) {
    let mut bytes = declarations(type_indices);
    let code = code_bodies(bodies);
    let start = push_section(&mut bytes, 10, &code);
    push_section(&mut bytes, 0, &[0]);
    expect_error(
        &bytes,
        start + code.len() - bodies.last().unwrap().len() + offset,
        kind,
    );
}

#[test]
fn accepts_unexported_void_function_with_padded_zero_result_count() {
    let types = [1, 0x60, 0, 0x80, 0x80, 0x80, 0x80, 0];
    let code = code_body(EMPTY_BODY);
    let module = validate_module(&module(&[(1, &types), (3, FUNCTION), (10, &code)])).unwrap();
    assert_eq!(
        module.instantiate().unwrap().invoke_i32(""),
        Err(InvokeError::ExportNotFound),
    );
}

#[test]
fn rejects_void_export_at_start_of_function_index_after_decoding_and_bounds_check() {
    for (immediate, offset, kind) in [
        (
            &[0x81, 0x80, 0x80, 0x80, 0][..],
            0,
            ErrorKind::VoidFunctionExport { index: 1 },
        ),
        (
            &[2],
            0,
            ErrorKind::InvalidIndex {
                field: IntegerField::FunctionIndex,
                index: 2,
            },
        ),
        (
            &[0x81],
            1,
            ErrorKind::TruncatedInteger {
                field: IntegerField::FunctionIndex,
            },
        ),
    ] {
        let mut bytes = declarations(&[NO_ARGS_I32, NO_ARGS_VOID]);
        let prefix = b"\x02\x06answer\0\0\x06helper\0";
        let start = push_section(&mut bytes, 7, &[prefix, immediate].concat());
        push_section(&mut bytes, 10, &code_bodies(&[ANSWER_42, EMPTY_BODY]));
        expect_error(&bytes, start + prefix.len() + offset, kind);
    }
}

#[test]
fn mixed_forward_calls_preserve_caller_operands_and_locals_and_share_globals() {
    let entry = [
        1, 1, 0x7f, // One local after the parameter.
        0x20, 0, 0x22, 1, 0x02, 0x7f, // Store and freeze the argument.
        0x41, 20, 0x41, 3, 0x41, 4, 0x41, 6, 0x10, 1, // Call void f1 with four args.
        0x23, 0, 0x20, 1, 0x6a, 0x0b, 0x6a, 0x0b, // global + local + prefix.
    ];
    let helper = [
        1, 1, 0x7f, // Local at index 4.
        0x20, 0, 0x20, 1, 0x6b, 0x10, 2, 0x21, 4, // f2(a - b) returns one value.
        0x20, 4, 0x20, 2, 0x6a, 0x20, 3, 0x6b, 0x24, 0, 0x0b, // global = local + c - d.
    ];
    let double = [0, 0x20, 0, 0x41, 2, 0x6c, 0x0b];
    let read = [0, 0x23, 0, 0x0b];
    let module = mixed_module(
        &[ONE_ARG_I32, FOUR_ARGS_VOID, ONE_ARG_I32, NO_ARGS_I32],
        Some(MUTABLE_ZERO),
        &[(b"answer", 0), (b"read", 3)],
        &[&entry, &helper, &double, &read],
    );
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("read"), Ok(0));
    assert_eq!(
        instance.invoke_i32("helper"),
        Err(InvokeError::ExportNotFound)
    );
    for (arg, result) in [(5, 42), (-5, 22)] {
        assert_eq!(instance.invoke_i32_with_args("answer", &[arg]), Ok(result));
        assert_eq!(instance.invoke_i32("read"), Ok(32));
    }
}

#[test]
fn void_call_without_arguments_leaves_a_full_caller_operand_stack_unchanged() {
    let cap = Limit::OperandStackDepth.maximum();
    let mut entry = vec![0];
    for value in 1..=cap {
        entry.extend([0x41, u8::try_from(value).unwrap()]);
    }
    entry.extend([0x10, 1]);
    entry.extend(std::iter::repeat_n(0x6a, cap - 1));
    entry.push(0x0b);
    let module = mixed_module(
        &[NO_ARGS_I32, NO_ARGS_VOID],
        None,
        &[(b"answer", 0)],
        &[&entry, EMPTY_BODY],
    );
    assert_eq!(module.instantiate().unwrap().invoke_i32("answer"), Ok(528));
}

#[test]
fn early_void_exits_discard_callee_operands_but_not_the_callers() {
    for (transfer, conditional) in [
        (&[0x0f][..], false),
        (&[0x0c, 0x81, 0], false),
        (&[0x20, 0, 0x0d, 1], true),
    ] {
        let helper = [
            &[0, 0x41, 9, 0x02, 0x40, 0x41, 5][..],
            transfer,
            // Not taken: br_if removes just the condition. Taken: neither
            // the frozen 9 nor the extra 5 may bubble up into the caller.
            &[0x24, 0, 0x0b, 0x1a, 0x0b],
        ]
        .concat();
        let module = mixed_module(
            &[ONE_ARG_I32, ONE_ARG_VOID],
            Some(MUTABLE_ZERO),
            &[(b"answer", 0)],
            &[CALL_HELPER_AND_READ_GLOBAL, &helper],
        );
        let instance = module.instantiate().unwrap();
        assert_eq!(instance.invoke_i32_with_args("answer", &[-1]), Ok(40));
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[0]),
            Ok(if conditional { 45 } else { 40 }),
        );
    }
}

#[test]
fn table_can_mix_void_function_exit_and_loop_continue() {
    let helper = [
        0, 0x41, 9, 0x03, 0x40, // The loop freezes 9; the function exit discards it.
        0x23, 0, 0x41, 1, 0x6a, 0x24, 0, // Count iterations in the global.
        0x20, 0, 0x20, 0, 0x41, 1, 0x6b, 0x21,
        0, // Select with old argument, decrement local.
        0x0e, 2, 1, 0, 1, // 0 -> function, 1 -> loop, otherwise -> function.
        0x00, 0x0b, 0x1a, 0x0b,
    ];
    let module = mixed_module(
        &[ONE_ARG_I32, ONE_ARG_VOID],
        Some(MUTABLE_ZERO),
        &[(b"answer", 0)],
        &[CALL_HELPER_AND_READ_GLOBAL, &helper],
    );
    for (arg, expected) in [(0, 41), (1, 42), (2, 41), (-1, 41)] {
        assert_eq!(
            module
                .instantiate()
                .unwrap()
                .invoke_i32_with_args("answer", &[arg]),
            Ok(expected),
        );
    }
}

#[test]
fn table_cannot_mix_void_function_and_result_block_even_in_dead_code() {
    for (labels, expected, actual) in [([0, 1], 1, 0), ([1, 0], 0, 1)] {
        let body = [
            &[0, 0x02, 0x7f, 0x00, 0x0e, 1][..],
            &labels,
            &[0x0b, 0x1a, 0x0b],
        ]
        .concat();
        bad_last_body(
            &[NO_ARGS_VOID],
            &[&body],
            7,
            ErrorKind::BranchTableArityMismatch { expected, actual },
        );
    }
}

#[test]
fn rejects_wrong_operand_and_result_depths_for_void_signatures() {
    use ErrorKind::{InvalidResultDepth, StackUnderflow};
    for (body, offset) in [(&[0, 0x41, 1, 0x0b][..], 3), (&[0, 0x0f, 0x41, 1, 0x0b], 4)] {
        bad_last_body(
            &[NO_ARGS_VOID],
            &[body],
            offset,
            InvalidResultDepth { depth: 1 },
        );
    }
    for (body, offset, kind) in [
        (&[0, 0x10, 0, 0x0b][..], 3, InvalidResultDepth { depth: 0 }),
        (&[0, 0x10, 0, 0x1a, 0x0b], 3, StackUnderflow { depth: 0 }),
    ] {
        bad_last_body(
            &[NO_ARGS_VOID, NO_ARGS_I32],
            &[EMPTY_BODY, body],
            offset,
            kind,
        );
    }
    bad_last_body(
        &[NO_ARGS_I32, NO_ARGS_VOID],
        &[ANSWER_42, &[0, 0x10, 0, 0x0b]],
        3,
        InvalidResultDepth { depth: 1 },
    );
    for ops in [&[0x10, 0][..], &[0x0d, 1], &[0x0e, 0, 1]] {
        // The function's i32 below the block floor cannot supply an argument,
        // condition, or selector even if subsequent instructions would trap.
        let body = [
            &[0, 0x41, 1, 0x02, 0x40][..],
            ops,
            &[0x00, 0x0b, 0x1a, 0x0b],
        ]
        .concat();
        bad_last_body(
            &[ONE_ARG_VOID, NO_ARGS_VOID],
            &[EMPTY_BODY, &body],
            5,
            StackUnderflow { depth: 0 },
        );
    }
}

#[test]
fn dead_tail_supplies_implicit_operands_without_materializing_void_results() {
    for tail in [
        &[0x0d, 1, 0x10, 2][..], // Implicit condition and argument, no restored or call result.
        &[0x0e, 0, 1],           // Implicit selector, no branch result.
    ] {
        let helper = [
            &[0, 0x41, 9, 0x02, 0x40, 0x0f][..], // Return void without popping below the floor.
            tail,
            &[0x0b, 0x1a, 0x0b],
        ]
        .concat();
        let module = mixed_module(
            &[NO_ARGS_I32, NO_ARGS_VOID, ONE_ARG_VOID],
            None,
            &[(b"answer", 0)],
            &[&[0, 0x41, 42, 0x10, 1, 0x0b], &helper, EMPTY_BODY],
        );
        assert_eq!(module.instantiate().unwrap().invoke_i32("answer"), Ok(42));
    }
}

#[test]
fn recursive_void_helper_counts_frames_and_allows_reuse_after_depth_limit() {
    let helper = [
        0, 0x23, 0, 0x41, 1, 0x6a, 0x24, 0, // Count each entered frame.
        0x20, 0, 0x04, 0x40, 0x20, 0, 0x41, 1, 0x6b, 0x10, 1, 0x0b, 0x0b,
    ];
    let entry = [
        &[0, 0x41, 0, 0x24, 0][..],
        &CALL_HELPER_AND_READ_GLOBAL[1..],
    ]
    .concat();
    let read = [0, 0x23, 0, 0x0b];
    let module = mixed_module(
        &[ONE_ARG_I32, ONE_ARG_VOID, NO_ARGS_I32],
        Some(MUTABLE_ZERO),
        &[(b"answer", 0), (b"read", 2)],
        &[&entry, &helper, &read],
    );
    let instance = module.instantiate().unwrap();
    let limit = ExecutionLimit::CallDepth;
    assert_eq!(limit.maximum(), 16);
    let invoke = |n| instance.invoke_i32_with_args("answer", &[n]);
    assert_eq!(invoke(14), Ok(55));
    assert_eq!(invoke(15), Err(InvokeError::LimitExceeded { limit }));
    assert_eq!(instance.invoke_i32("read"), Ok(15));
    assert_eq!(invoke(14), Ok(55));
}

#[test]
fn void_completion_is_free_on_the_last_dispatch_and_resume_shares_the_budget() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    for (transfer, cost) in [
        (&[][..], 0), // Fallthrough (no end dispatch).
        (&[0x0f, 0x00], 1),
        (&[0x0c, 0, 0x00], 1),
        (&[0x41, 1, 0x0d, 0, 0x00], 2),
        (&[0x41, 0, 0x0e, 0, 0, 0x00], 2),
    ] {
        // Loop five dispatches per trip, then exit without a value.
        let mut helper = vec![
            0, 0x03, 0x40, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b,
        ];
        helper.extend(transfer);
        helper.push(0x0b);
        let caller = |before, after| {
            let mut body = vec![0];
            body.extend(std::iter::repeat_n(0x01, before));
            body.extend([0x41, 42, 0x20, 0, 0x10, 3]);
            if after {
                body.push(0x45);
            }
            body.push(0x0b);
            body
        };
        let pad = limit.maximum() - (203 * 5 + 3 + cost);
        let module = mixed_module(
            &[ONE_ARG_I32, ONE_ARG_I32, ONE_ARG_I32, ONE_ARG_VOID],
            None,
            &[(b"exact", 0), (b"over", 1), (b"resume", 2)],
            &[
                &caller(pad, false),
                &caller(pad + 1, false),
                &caller(pad, true),
                &helper,
            ],
        );
        let instance = module.instantiate().unwrap();
        let invoke = |name, n| instance.invoke_i32_with_args(name, &[n]);
        assert_eq!(invoke("exact", 203), Ok(42));
        assert_eq!(
            invoke("over", 203),
            Err(InvokeError::LimitExceeded { limit })
        );
        assert_eq!(
            invoke("resume", 203),
            Err(InvokeError::LimitExceeded { limit })
        );
        assert_eq!(invoke("resume", 1), Ok(0));
        assert_eq!(invoke("exact", 203), Ok(42));
    }
}
