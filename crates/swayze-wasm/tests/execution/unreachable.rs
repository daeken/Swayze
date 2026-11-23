use super::*;

const TRAP: Result<i32, InvokeError> = Err(InvokeError::Trap(Trap::Unreachable));

fn assert_body_traps(body: &[u8]) {
    let module = exported_function(TYPE, &code_body(body));
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("answer"), TRAP);
    assert_eq!(instance.invoke_i32("answer"), TRAP);
}

#[test]
fn dead_function_tails_supply_missing_operands_and_results() {
    for body in [
        &[0, 0x00, 0x0b][..],                     // No explicit result.
        &[0, 0x41, 1, 0x41, 2, 0x00, 0x0b],       // Both previous operands are discarded.
        &[0, 0x00, 0x1a, 0x1a, 0x0b],             // Two implicit drops and an implicit result.
        &[0, 0x00, 0x41, 7, 0x6a, 0x0b],          // One explicit and one implicit input.
        &[0, 0x00, 0x1b, 0x0b],                   // All three select inputs are implicit.
        &[0, 0x00, 0x41, 7, 0x41, 8, 0x00, 0x0b], // A second trap discards again.
    ] {
        assert_body_traps(body);
    }
}

#[test]
fn block_floor_preserves_prefix_and_taken_branch_bypasses_dead_tail() {
    let body = [
        0, 0x41, 40, 0x02, 0x7f, // Save 40 below the block's floor.
        0x41, 2, 0x20, 0, 0x0d, 0, // Exit with 2 on a nonzero condition.
        0x41, 7, 0x00, 0x1a, 0x1a, 0x0b, // Discard both inputs; synthesize a result.
        0x6a, 0x0b, // Both 40 and the declared block result are required at the join.
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (arg, expected) in [(0, TRAP), (1, Ok(42)), (0, TRAP), (-1, Ok(42))] {
        assert_eq!(instance.invoke_i32_with_args("answer", &[arg]), expected);
    }
}

#[test]
fn frame_exit_keeps_the_parent_flag_and_pushes_declared_results() {
    assert_body_traps(&[
        0, 0x00, 0x02, 0x7f, 0x00, 0x0b, // Inner block supplies its missing result.
        0x1a, 0x1a, 0x0b, // Drop it, then use parent's original polymorphism.
    ]);
    bad_body(
        &[0, 0x02, 0x40, 0x00, 0x0b, 0x1a, 0x0b],
        5,
        ErrorKind::StackUnderflow { depth: 0 }, // Child's flag doesn't leak.
    );
}

#[test]
fn dead_tails_reject_excess_concrete_operands_at_delimiters() {
    for (body, offset, depth) in [
        (&[0, 0x00, 0x41, 1, 0x41, 2, 0x0b][..], 6, 2),
        (&[0, 0x02, 0x40, 0x00, 0x41, 1, 0x0b, 0x0b], 6, 1),
        (
            &[0, 0x41, 0, 0x04, 0x40, 0x00, 0x41, 1, 0x05, 0x0b, 0x0b],
            8,
            1,
        ),
        // The block's implicit result becomes explicit in the parent.
        (&[0, 0x00, 0x02, 0x7f, 0x00, 0x0b, 0x41, 7, 0x0b], 8, 2),
        // An implicit br_if result is pushed back on the fallthrough path.
        (&[0, 0x02, 0x7f, 0x00, 0x0d, 0, 0x41, 7, 0x0b, 0x0b], 8, 2),
        // Even a select with all implicit inputs pushes a counted result.
        (&[0, 0x00, 0x1b, 0x41, 7, 0x0b], 5, 2),
    ] {
        bad_body(body, offset, ErrorKind::InvalidResultDepth { depth });
    }
}

#[test]
fn inner_frames_and_alternate_arms_start_reachable() {
    for opcode in [0x02, 0x03, 0x04] {
        // In the if case, the opening condition is implicit, but the new
        // frame may not use the parent's implicit operand for drop.
        bad_body(
            &[0, 0x00, opcode, 0x40, 0x1a, 0x0b, 0x0b],
            4,
            ErrorKind::StackUnderflow { depth: 0 },
        );
    }
    let then = &[0, 0x41, 0, 0x04, 0x7f, 0x00, 0x05][..];
    assert_body_result(&[then, &[0x41, 42, 0x0b, 0x0b]].concat(), 42);
    assert_body_traps(&[then, &[0x00, 0x0b, 0x0b]].concat());
    for (tail, kind) in [
        (
            &[0x1a, 0x41, 42, 0x0b, 0x0b][..],
            ErrorKind::StackUnderflow { depth: 0 },
        ),
        (&[0x0b, 0x0b], ErrorKind::InvalidResultDepth { depth: 0 }),
    ] {
        bad_body(&[then, tail].concat(), then.len(), kind);
    }
}

#[test]
fn dead_code_still_checks_syntax_immediates_and_indices() {
    for (tail, offset, kind) in [
        (
            &[0x0e, 0x0b][..],
            0,
            ErrorKind::UnsupportedOpcode { opcode: 0x0e },
        ),
        (
            &[0x41, 0x80],
            2,
            ErrorKind::TruncatedInteger {
                field: IntegerField::I32Const,
            },
        ),
        (
            &[0x41, 0x80, 0x80, 0x80, 0x80, 8, 0x0b],
            5,
            ErrorKind::IntegerOverflow {
                field: IntegerField::I32Const,
            },
        ),
        (
            &[0x3f, 0x80, 0, 0x0b],
            1,
            ErrorKind::UnexpectedByte {
                expected: 0,
                found: 0x80,
            },
        ),
        (&[0x28, 0, 0, 0x0b], 0, ErrorKind::MissingMemory),
        (
            &[0x20, 0, 0x0b],
            1,
            ErrorKind::InvalidIndex {
                field: IntegerField::LocalIndex,
                index: 0,
            },
        ),
        (
            &[0x10, 1, 0x0b],
            1,
            ErrorKind::InvalidIndex {
                field: IntegerField::FunctionIndex,
                index: 1,
            },
        ),
        (
            &[0x0d, 1, 0x0b],
            1,
            ErrorKind::InvalidIndex {
                field: IntegerField::LabelIndex,
                index: 1,
            },
        ),
        (&[0x04, 0x7f, 0x00, 0x0b, 0x0b], 3, ErrorKind::MissingElse),
        (&[0x05, 0x0b], 0, ErrorKind::UnexpectedElse),
        (&[0x0b, 0x01], 1, ErrorKind::TrailingBodyBytes),
    ] {
        // Follow the malformed body with a custom section using bad_body.
        bad_body(&[&[0, 0x00][..], tail].concat(), 2 + offset, kind);
    }
    // Check an unreachable tail in an unexported body too.
    let code = code_bodies(&[&[0, 0x41, 42, 0x0b], &[0, 0x00, 0x24, 0, 0x0b]]);
    let mut bytes = module(&[
        (1, TYPE),
        (3, TWO_FUNCTIONS),
        (6, &[1, 0x7f, 0, 0x41, 0, 0x0b]),
        (7, EXPORT),
    ]);
    let start = push_section(&mut bytes, 10, &code);
    expect_error(
        &bytes,
        start + code.len() - 2,
        ErrorKind::ImmutableGlobal { index: 0 },
    );
}

#[test]
fn dead_code_still_counts_instructions_control_and_concrete_operands() {
    let limit = Limit::BodyInstructions;
    let mut body = vec![0, 0x00];
    body.extend(std::iter::repeat_n(0x1a, limit.maximum() - 1));
    body.push(0x0b);
    assert_body_traps(&body);
    let extra = body.len() - 1;
    body.insert(extra, 0x00);
    bad_body(&body, extra, ErrorKind::LimitExceeded { limit });

    let limit = Limit::ControlDepth;
    let mut body = vec![0, 0x00];
    body.extend([0x02, 0x40].repeat(limit.maximum()));
    let extra = body.len();
    body.extend(std::iter::repeat_n(0x0b, limit.maximum() + 1));
    assert_body_traps(&body);
    body.splice(extra..extra, [0x02, 0x40, 0x0b]);
    bad_body(&body, extra, ErrorKind::LimitExceeded { limit });

    let limit = Limit::OperandStackDepth;
    let mut body = vec![0, 0x00];
    body.extend([0x41, 0].repeat(limit.maximum()));
    let extra = body.len();
    body.extend([0x00, 0x0b]);
    assert_body_traps(&body);
    body.splice(extra..extra, [0x41, 0]);
    bad_body(&body, extra, ErrorKind::LimitExceeded { limit });

    // A push after a polymorphic pop can overflow when all 32 slots are below
    // the current floor. This includes pushing a block result onto the parent.
    let mut prefix = vec![0];
    prefix.extend([0x41, 0].repeat(limit.maximum()));
    prefix.extend([0x02, 0x7f, 0x00]);
    for tail in [&[0x0b, 0x00, 0x0b][..], &[0x0d, 0, 0x0b, 0x00, 0x0b]] {
        bad_body(
            &[&prefix[..], tail].concat(),
            prefix.len(),
            ErrorKind::LimitExceeded { limit },
        );
    }
}

#[test]
fn a_callee_trap_preserves_completed_writes_but_stops_the_invocation() {
    let run = [
        0, 0x20, 0, 0x04, 0x40, 0x10, 1, 0x1a, // Nonzero invokes the trapper.
        0x41, 9, 0x24, 0, 0x0b, 0x23, 0, 0x0b, // Caller write must not run.
    ];
    let trapper = [
        0, 0x23, 0, 0x41, 1, 0x6a, 0x24, 0, // Increment the shared global.
        0x00, 0x41, 5, 0x24, 0, 0x0b, // Callee's second write must not run either.
    ];
    let read = [0, 0x23, 0, 0x0b];
    let module = validate_module(&module(&[
        (1, &i32_types(&[1, 0])),
        (3, &[3, 0, 1, 1]),
        (6, &[1, 0x7f, 1, 0x41, 0, 0x0b]),
        (7, &function_exports(&[(b"run", 0), (b"read", 2)])),
        (10, &code_bodies(&[&run, &trapper, &read])),
    ]))
    .unwrap();
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("read"), Ok(0));
    for value in [1, 2] {
        assert_eq!(instance.invoke_i32_with_args("run", &[1]), TRAP);
        assert_eq!(instance.invoke_i32("read"), Ok(value));
        assert_eq!(instance.invoke_i32_with_args("run", &[0]), Ok(value));
    }
}

#[test]
fn unreachable_traps_only_if_its_dispatch_fits_the_budget() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    for (nops, expected) in [(3, TRAP), (4, Err(InvokeError::LimitExceeded { limit }))] {
        // 170 loop iterations at six dispatches each, then 3 or 4 nops.
        let mut body = vec![
            0, 0x03, 0x40, 0x01, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b,
        ];
        body.extend(std::iter::repeat_n(0x01, nops));
        body.extend([0x00, 0x0b]);
        let module = exported_function(&i32_type(1), &code_body(&body));
        let instance = module.instantiate().unwrap();
        for (iterations, result) in [(170, expected), (1, TRAP), (170, expected)] {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &[iterations]),
                result
            );
        }
    }
}
