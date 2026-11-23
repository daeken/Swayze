use super::*;

#[test]
fn exits_at_function_depth_zero() {
    for transfer in [&[0x0f][..], &[0x0c, 0x80, 0], &[0x41, 1, 0x0d, 0]] {
        let body = [&[0, 0x41, 9, 0x41, 42][..], transfer, &[0x00, 0x0b]].concat();
        assert_body_result(&body, 42);
    }
    // return has no immediate and can be immediately followed by end.
    assert_body_result(&[0, 0x41, 42, 0x0f, 0x0b], 42);
}

#[test]
fn counts_all_nested_frames_and_keeps_operands_on_conditional_fallthrough() {
    let body = [
        0, 0x41, 40, 0x02, 0x7f, // Block freezes 40.
        0x41, 10, 0x03, 0x40, // Loop freezes 10 as well.
        0x20, 0, 0x04, 0x7f, // Choose then or else.
        0x41, 20, 0x41, 2, 0x20, 1, 0x0d, 0x83, 0x80, 0x80, 0x80, 0,
        // Padded depth 3: return only 2, or preserve all but the condition.
        0x6a, 0x05, // On fallthrough, add 20 + 2.
        0x02, 0x7f, 0x41, 3, 0x0c, 4, 0x00, 0x0b, // Depth 4 from the else block.
        0x0b, 0x21, 0, 0x0b, // Save the if result and end the loop.
        0x20, 0, 0x6a, 0x0b, 0x6a, 0x0b, // Add the preserved 10 and 40.
    ];
    let module = exported_function(&i32_type(2), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (args, result) in [([1, 0], 72), ([1, -1], 2), ([0, 0], 3)] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(result));
    }
}

#[test]
fn function_exit_inputs_must_be_above_the_active_floor() {
    for (prefix, depth) in [(&[0][..], 0), (&[0, 0x41, 7, 0x41, 8, 0x03, 0x40], 1)] {
        for (ops, op, available) in [
            (&[0x0f][..], 0, 0),
            (&[0x0c, depth], 0, 0),
            (&[0x0d, depth], 0, 0),          // No condition.
            (&[0x41, 1, 0x0d, depth], 2, 1), // Condition but no result.
        ] {
            let body = [prefix, ops, &[0x0b, 0x0b][..=usize::from(depth)]].concat();
            bad_body(
                &body,
                prefix.len() + op,
                ErrorKind::StackUnderflow { depth: available },
            );
        }
    }
}

#[test]
fn early_exits_resume_each_caller_with_one_result() {
    let entry = [
        0, 0x41, 40, 0x02, 0x7f, 0x41, 8, 0x41, 7, 0x10, 1, 0x6a, 0x0b, 0x6a, 0x0b,
    ];
    let middle = [
        0, 0x41, 11, 0x02, 0x7f, 0x41, 6, 0x20, 0, 0x10, 2, 0x6a, 0x0f, 0x00, 0x0b, 0x00, 0x0b,
    ];
    let leaf = [
        0, 0x41, 13, 0x03, 0x40, 0x20, 0, 0x0c, 1, 0x00, 0x0b, 0x00, 0x0b,
    ];
    // f0: 40 + (8 + f1(7)); f1: return 6 + f2(arg); f2: br to function with arg.
    let module = exported_typed_functions(
        &[0, 1],
        &[0, 1, 1],
        &[(b"answer", 0)],
        &[&entry, &middle, &leaf],
    );
    assert_eq!(module.instantiate().unwrap().invoke_i32("answer"), Ok(61));
}

#[test]
fn dead_tail_is_polymorphic_only_in_the_active_frame_and_is_still_validated() {
    for transfer in [&[0x0f][..], &[0x0c, 1]] {
        let prefix = [&[0, 0x41, 42, 0x02, 0x40, 0x41, 7][..], transfer].concat();
        // return and br can take an implicit operand; br_if's restored result
        // is explicit and must be dropped before the void block ends.
        let dead = [&prefix[..], &[0x0f, 0x0c, 1, 0x0d, 1]].concat();
        assert_body_result(&[&dead[..], &[0x1a, 0x0b, 0x0b]].concat(), 7);
        bad_body(
            &[&dead[..], &[0x0b, 0x0b]].concat(),
            dead.len(),
            ErrorKind::InvalidResultDepth { depth: 1 },
        );
        for (tail, offset, kind) in [
            (
                &[0x0c, 0x80][..],
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
            (
                &[0x02, 0x40, 0x0f, 0x0b, 0x0b, 0x0b],
                2,
                ErrorKind::StackUnderflow { depth: 0 },
            ),
            (
                &[0x0b, 0x1a, 0x0b],
                2,
                ErrorKind::InvalidResultDepth { depth: 0 },
            ),
        ] {
            bad_body(&[&prefix[..], tail].concat(), prefix.len() + offset, kind);
        }
    }
}

#[test]
fn return_counts_toward_the_decoded_instruction_cap_even_in_a_dead_tail() {
    let limit = Limit::BodyInstructions;
    let mut body = vec![0, 0x41, 42, 0x0f];
    body.extend(std::iter::repeat_n(0x01, limit.maximum() - 2));
    body.push(0x0b);
    assert_body_result(&body, 42);
    let end = body.len() - 1;
    body.insert(end, 0x0f);
    bad_body(&body, end, ErrorKind::LimitExceeded { limit });
}

#[test]
fn function_exits_use_one_dispatch_and_unwinding_uses_none() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    for (transfer, cost) in [(&[0x0f][..], 1), (&[0x0c, 0], 1), (&[0x41, 1, 0x0d, 0], 2)] {
        let mut callee = vec![
            // Loop five dispatches per trip, then push 42 and exit.
            0, 0x03, 0x40, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b, 0x41, 42,
        ];
        callee.extend(transfer);
        callee.extend([0x00, 0x0b]); // Must not dispatch this trap.
        let caller = |before, after| {
            let mut body = vec![0];
            body.extend(std::iter::repeat_n(0x01, before));
            body.extend([0x20, 0, 0x10, 3]);
            if after {
                body.push(0x45);
            }
            body.push(0x0b);
            body
        };
        // 203 * 5 loop dispatches, two for the caller's local.get and call,
        // one for the result, plus the transfer and enough nops to reach 1024.
        let pad = limit.maximum() - (203 * 5 + 3 + cost);
        let module = exported_functions(
            1,
            &[(b"exact", 0), (b"over", 1), (b"resume", 2)],
            &[
                &caller(pad, false),
                &caller(pad + 1, false),
                &caller(pad, true),
                &callee,
            ],
        );
        let instance = module.instantiate().unwrap();
        let invoke = |name, n| instance.invoke_i32_with_args(name, &[n]);
        assert_eq!(invoke("exact", 203), Ok(42));
        // One extra dispatch before the exit or after resuming is disallowed.
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

#[test]
fn early_return_does_not_change_the_live_frame_limit() {
    let body = [
        0, 0x20, 0, 0x04, 0x40, // If nonzero, return 1 + recur(arg - 1).
        0x41, 1, 0x20, 0, 0x41, 1, 0x6b, 0x10, 0, 0x6a, 0x0f, 0x00, 0x0b, 0x41, 0, 0x0c, 0,
        0x0b, // Base case exits through br 0.
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    let limit = ExecutionLimit::CallDepth;
    assert_eq!(limit.maximum(), 16);
    let invoke = |n| instance.invoke_i32_with_args("answer", &[n]);
    assert_eq!(invoke(15), Ok(15));
    assert_eq!(invoke(16), Err(InvokeError::LimitExceeded { limit }));
    assert_eq!(invoke(15), Ok(15));
}
