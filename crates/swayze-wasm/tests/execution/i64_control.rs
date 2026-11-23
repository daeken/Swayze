use super::{i64_operands::push_i64, *};
use swayze_wasm::ValType::{I32, I64};

const NEED_I32: ErrorKind = ErrorKind::TypeMismatch {
    expected: I32,
    actual: I64,
};
const NEED_I64: ErrorKind = ErrorKind::TypeMismatch {
    expected: I64,
    actual: I32,
};
const PREFIX: i64 = -0x1234_5678_0000_0000;
const PAYLOAD: i64 = 0x7123_4567_7654_3210;
const ALT: i64 = -0x1234_5678_89ab_cdf0; // Same low word, different upper word.

fn assert_full_result(mut body: Vec<u8>, args: &[i32], expected: i64) {
    push_i64(&mut body, expected);
    body.extend(b"\x51\x0b"); // i64.eq; end function
    let validated = exported_function(&i32_type(args.len().try_into().unwrap()), &code_body(&body));
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32_with_args("answer", args), Ok(1));
}

#[test]
fn normal_block_and_if_joins_keep_both_full_width_arm_results_and_the_prefix() {
    let mut body = vec![0];
    push_i64(&mut body, PREFIX);
    body.extend(b"\x02\x7e\x20\0\x04\x7e"); // block i64; local.get 0; if i64
    push_i64(&mut body, PAYLOAD);
    body.push(0x05);
    push_i64(&mut body, ALT);
    body.extend(b"\x0b\x0b\x7c"); // end if; end block; i64.add
    for (condition, result) in [(0, ALT), (1, PAYLOAD), (-1, PAYLOAD)] {
        assert_full_result(body.clone(), &[condition], PREFIX.wrapping_add(result));
    }
}

#[test]
fn br_if_fallthrough_keeps_the_payload_and_both_taken_branches_restore_the_target_floor() {
    let mut body = vec![0];
    push_i64(&mut body, PREFIX);
    body.extend(b"\x02\x7e\x41\x63\x02\x7f"); // block i64; i32 junk; block i32
    push_i64(&mut body, PAYLOAD);
    body.extend(b"\x20\0\x0d\x01\x42\x01\x7c\x0c\x01"); // br_if 1; add 1; br 1
    // The skipped tail is checked: the i32 join and the junk are counted,
    // then unreachable allows the outer i64 block to end without a value.
    body.extend(b"\x0b\x1a\x1a\0\x0b\x7c");
    for (condition, increment) in [(0, 1), (-1, 0)] {
        assert_full_result(body.clone(), &[condition], PREFIX + PAYLOAD + increment);
    }
}

#[test]
fn untaken_br_if_keeps_all_64_payload_bits_for_the_next_instruction() {
    let mut body = b"\0\x02\x7e".to_vec(); // block i64
    push_i64(&mut body, ALT);
    body.extend(b"\x20\0\x0d\0"); // local.get 0; br_if 0
    // XOR is zero only if every bit, including the high word, survived.
    // A taken branch skips the XOR and leaves the original nonzero payload.
    push_i64(&mut body, ALT);
    body.extend(b"\x85\x0b\x50\x0b"); // i64.xor; end block; i64.eqz; end function
    let validated = exported_function(&i32_type(1), &code_body(&body));
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32_with_args("answer", &[0]), Ok(1));
    assert_eq!(instance.invoke_i32_with_args("answer", &[1]), Ok(0));
}

#[test]
fn br_table_transfers_to_if_or_block_and_else_br_uses_the_shared_if_label() {
    let mut body = vec![0];
    push_i64(&mut body, PREFIX);
    body.extend(b"\x02\x7e"); // block i64
    push_i64(&mut body, 3);
    body.extend(b"\x20\0\x04\x7e\x41\x08"); // local.get 0; if i64; i32 junk
    push_i64(&mut body, PAYLOAD);
    // The repeated explicit slots target the if, and the default exits the
    // outer block. A taken branch must skip the else jump and dead tail.
    body.extend(b"\x20\x01\x0e\x02\0\0\x01\0\x05");
    push_i64(&mut body, ALT);
    body.extend(b"\x0c\0\0\x0b\x7c\x0b\x7c"); // br 0; unreachable; end; add; end; add
    for (args, result) in [
        ([1, 0], PAYLOAD + 3),
        ([1, 1], PAYLOAD + 3),
        ([1, 2], PAYLOAD),
        ([1, -1], PAYLOAD),
        ([0, 0], ALT + 3),
    ] {
        assert_full_result(body.clone(), &args, PREFIX.wrapping_add(result));
    }

    // Maximum explicit count; the mandatory default is a separate edge.
    let mut body = b"\0\x02\x7e".to_vec();
    push_i64(&mut body, ALT);
    body.extend(b"\x20\0\x0e");
    let count = body.len();
    body.push(16);
    body.extend([0; 17]);
    body.push(0x0b);
    for index in [15, 16, -1] {
        assert_full_result(body.clone(), &[index], ALT);
    }
    body[count] = 17;
    bad_typed_body(
        &i32_type(1),
        &body,
        count,
        ErrorKind::UnsupportedCount {
            field: IntegerField::BranchTableCount,
            count: 17,
        },
    );
}

#[test]
fn i64_if_requires_an_i32_test_and_an_else_and_checks_both_arms() {
    use ErrorKind::*;
    for (body, at, kind) in [
        (&b"\0\x04\x7e"[..], 1, StackUnderflow { depth: 0 }),
        (b"\0\x42\0\x04\x7e", 3, NEED_I32),
        (b"\0\0\x42\0\x04\x7e", 4, NEED_I32),
        (b"\0\x41\x01\x04\x7e\x42\0\x0b", 7, MissingElse),
        (b"\0\x41\x01\x04\x7e\0\x0b", 6, MissingElse),
        (
            b"\0\x41\x01\x04\x7e\x42\0",
            7,
            TruncatedByte { expected: 5 },
        ),
        (b"\0\x41\0\x04\x7e\x41\0\x05", 7, NEED_I64),
        (b"\0\x41\x01\x04\x7e\x42\0\x05\x41\0\x0b", 10, NEED_I64),
        // A correct value below the then-arm floor cannot be used at else.
        (
            b"\0\x42\0\x41\0\x04\x7e\x05",
            7,
            InvalidResultDepth { depth: 0 },
        ),
        (
            b"\0\x02\x7e\x42\0\x42\0\x0b",
            7,
            InvalidResultDepth { depth: 2 },
        ),
        (b"\0\x02\x7e\x41\0\x0b", 5, NEED_I64),
    ] {
        bad_body(body, at, kind);
    }
}

#[test]
fn branches_check_selector_and_payload_types_and_cannot_borrow_below_the_active_floor() {
    for (transfer, has_selector) in [
        (&b"\x0c\x01"[..], false),
        (b"\x0d\x01", true),
        (b"\x0e\0\x01", true),
    ] {
        // Target the outer i64 block, optionally leaving the active void
        // frame dead. Its i64 prefix is at the active floor, not a payload.
        let prefix = &b"\0\x02\x7e\x42\0\x02\x40"[..];
        // Underflow takes precedence even if the only value has the wrong
        // selector type.
        let too_few = if has_selector { &b"\x42\0"[..] } else { &[] };
        bad_body(
            &[prefix, too_few, transfer].concat(),
            prefix.len() + too_few.len(),
            ErrorKind::StackUnderflow {
                depth: usize::from(has_selector),
            },
        );
        let selector = if has_selector { &b"\x41\0"[..] } else { &[] };
        for dead in [&[][..], &[0]] {
            let prefix = [prefix, dead].concat();
            let at = prefix.len() + 2 + selector.len();
            bad_body(
                &[prefix.as_slice(), b"\x41\0", selector, transfer].concat(),
                at,
                NEED_I64,
            );
            if has_selector {
                bad_body(
                    &[prefix.as_slice(), b"\x42\0\x42\0", transfer].concat(),
                    at,
                    NEED_I32,
                );
            }
        }
    }

    // The function label still requires i32, even inside an i64 block.
    for transfer in [
        &b"\x0f"[..],
        b"\x0c\x01",
        b"\x41\0\x0d\x01",
        b"\x41\0\x0e\0\x01",
    ] {
        let body = [&b"\0\x02\x7e\x42\0"[..], transfer].concat();
        bad_body(&body, if transfer[0] == 0x41 { 7 } else { 5 }, NEED_I32);
    }
}

#[test]
fn br_table_reports_the_first_incompatible_label_even_in_dead_code() {
    use ErrorKind::*;
    let prefix = &b"\0\x02\x7e\x02\x7f\x03\x40\0\x0e"[..];
    // Labels: 0 void loop, 1 i32 block, 2 i64 block, 3 i32 function.
    for (table, label, kind) in [
        (
            // Padded count and first entry; the mismatching explicit depth
            // occupies bytes 4..7. Report its start, not its last byte.
            &[0x82, 0, 0x81, 0, 0x82, 0x80, 0, 1][..],
            4,
            BranchTableTypeMismatch {
                expected: I32,
                actual: I64,
            },
        ),
        (
            // The mismatching default depth occupies bytes 3..8.
            &[1, 0x82, 0, 0x83, 0x80, 0x80, 0x80, 0],
            3,
            BranchTableTypeMismatch {
                expected: I64,
                actual: I32,
            },
        ),
        (
            &[1, 0, 2],
            2,
            BranchTableArityMismatch {
                expected: 0,
                actual: 1,
            },
        ),
        (
            &[1, 2, 0],
            2,
            BranchTableArityMismatch {
                expected: 1,
                actual: 0,
            },
        ),
    ] {
        bad_body(&[prefix, table].concat(), prefix.len() + label, kind);
    }
}

#[test]
fn dead_transfers_and_joins_accept_unknowns_but_leave_counted_concrete_i64_results() {
    // Reachable transfers skip the dead tails; the then arm still has a counted
    // unknown at else and the else arm supplies its end result implicitly.
    for (condition, result) in [(0, 12), (1, 34)] {
        assert_body_result(
            &[
                0, 0x41, condition, 0x04, 0x7e, 0x42, 34, 0x0c, 0, 0x1b, 0x05, 0x42, 12, 0x41, 0,
                0x0e, 0, 0, 0x0b, 0xa7, 0x0b,
            ],
            result,
        );
    }
    // Unknown result accepted as payload; implicit condition and result are
    // both accepted too. In either case br_if restores a concrete i64 slot.
    for inputs in [&b""[..], b"\x1b\x41\0"] {
        let head = [&b"\0\x02\x7e\0"[..], inputs, b"\x0d\0"].concat();
        bad_body(&[&head[..], b"\x45"].concat(), head.len(), NEED_I32);
        let body = [&head[..], b"\x0b\xa7\x0b"].concat();
        let validated = exported_function(TYPE, &code_body(&body));
        assert_eq!(
            validated.instantiate().unwrap().invoke_i32("answer"),
            Err(InvokeError::Trap(Trap::Unreachable))
        );
    }
    // A typed result pushed to a dead parent has the declared type, not the
    // child's dead flag or the unknown supplied at its delimiter.
    bad_body(b"\0\0\x02\x7e\0\x1b\x0b\x45", 7, NEED_I32);
    bad_body(
        b"\0\x02\x7e\0\x1b\x42\0\x0b",
        7,
        ErrorKind::InvalidResultDepth { depth: 2 },
    );
    bad_body(b"\0\x02\x7e\0\x41\0\x0b", 6, NEED_I64);
    // A newly entered block and a newly entered else arm are still reachable.
    bad_body(
        b"\0\0\x02\x7e\x0c\0",
        4,
        ErrorKind::StackUnderflow { depth: 0 },
    );
    bad_body(
        b"\0\x41\x01\x04\x7e\0\x05\x0c\0",
        7,
        ErrorKind::StackUnderflow { depth: 0 },
    );
}

#[test]
fn i64_control_uses_the_exact_shared_control_and_operand_caps() {
    let limit = Limit::ControlDepth;
    assert_eq!(limit.maximum(), 16);
    let mut body = [&b"\0"[..], &b"\x02\x7e".repeat(16), b"\x42\x2a"].concat();
    body.extend([0x0b; 16]);
    body.extend(b"\xa7\x0b");
    assert_body_result(&body, 42);
    body.splice(33..33, [0x02, 0x7e]);
    bad_body(&body, 33, ErrorKind::LimitExceeded { limit });

    let limit = Limit::OperandStackDepth;
    assert_eq!(limit.maximum(), 32);
    let mut body = [&b"\0"[..], &b"\x42\0".repeat(31), b"\x02\x7e\x42\x2a\x0b"].concat();
    body.extend([0x7c; 31]);
    body.extend(b"\xa7\x0b");
    assert_body_result(&body, 42);
    let full = [&b"\0"[..], &b"\x42\0".repeat(32), b"\x02\x7e\0"].concat();
    // Both the implicit end result and an implicit br_if fallthrough result
    // require a counted slot above the full frozen prefix.
    for suffix in [&b"\x0b"[..], b"\x0d\0"] {
        bad_body(
            &[&full, suffix].concat(),
            full.len(),
            ErrorKind::LimitExceeded { limit },
        );
    }
}

#[test]
fn i64_results_do_not_change_exact_body_byte_and_instruction_limits() {
    let limit = Limit::CodeBodyBytes;
    let mut body = b"\0\x02\x7e".to_vec();
    for _ in 0..20 {
        push_i64(&mut body, PAYLOAD);
        body.push(0x1a);
    }
    body.extend([0x01; 8]);
    body.extend(b"\x42\x2a\x0b\xa7\x0b");
    assert_eq!(body.len(), 256);
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 42);
    body.insert(3, 0x01);
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });

    let limit = Limit::BodyInstructions;
    assert_eq!(limit.maximum(), 127);
    let mut body = b"\0\x02\x7e".to_vec();
    body.extend([0x01; 125]);
    body.extend(b"\x42\x2a\x0b\xa7\x0b");
    assert_body_result(&body, 42);
    let end = body.len() - 1;
    body.insert(end, 0x01);
    bad_body(&body, end, ErrorKind::LimitExceeded { limit });
}

#[test]
fn typed_branch_and_join_keep_the_exact_dispatch_budget() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let mut body = b"\0\x03\x40\x20\0\x41\x01\x6b\x22\0\x04\x7e".to_vec();
    // 10 dispatches on continue, skipping the else jump via the i64 branch;
    // 9 on exit (else arm falls through its end with zero).
    body.extend(b"\x42\x01\x0c\0\x05\x42\0\x0b\x50\x45\x0d\0\x0b");
    body.extend([0x01; 14]);
    body.extend(b"\x41\x2a\x0b");
    let mut overflow = body.clone();
    overflow.insert(body.len() - 3, 0x01);
    let validated = exported_functions(1, &[(b"exact", 0), (b"overflow", 1)], &[&body, &overflow]);
    let instance = validated.instantiate().unwrap();
    // 100 continues + one exit + 14 nops + one i32.const = 1024.
    assert_eq!(
        instance.invoke_i32_with_args("overflow", &[101]),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(instance.invoke_i32_with_args("exact", &[101]), Ok(42));
}
