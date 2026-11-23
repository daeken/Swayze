use super::*;
use swayze_wasm::ValType::{I32, I64};

const NEED_I32: ErrorKind = ErrorKind::TypeMismatch {
    expected: I32,
    actual: I64,
};
const NEED_I64: ErrorKind = ErrorKind::TypeMismatch {
    expected: I64,
    actual: I32,
};

#[test]
fn wraps_low_bits_of_short_and_exactly_ten_byte_constants() {
    for (immediate, expected) in [
        (&[0x7f][..], -1),
        (&[0xc0, 0x00], 64),
        (&[0x80, 0x80, 0x80, 0x80, 0x18], i32::MIN), // Above u32::MAX.
        (
            &[0xaa, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00],
            42,
        ),
        (
            &[0xd6, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
            -42,
        ),
        (
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00],
            -1,
        ),
        (
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x7f],
            0,
        ),
    ] {
        assert_body_result(
            &[&[0, 0x42][..], immediate, &[0xa7, 0x0b]].concat(),
            expected,
        );
    }
}

#[test]
fn both_extensions_dispatch_and_wrap_back_to_i32_for_existing_ops() {
    for extend in [0xac, 0xad] {
        let body = [0, 0x20, 0, extend, 0xa7, 0x41, 1, 0x6a, 0x0b];
        let module = exported_function(&i32_type(1), &code_body(&body));
        let instance = module.instantiate().unwrap();
        for arg in [i32::MIN, i32::MAX, -1, 0] {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &[arg]),
                Ok(arg.wrapping_add(1))
            );
        }
    }
}

fn push_i64(body: &mut Vec<u8>, mut value: i64) {
    body.push(0x42);
    // Use ten-byte signed LEB64 so the fixture keeps the entire 64-bit pattern.
    for _ in 0..9 {
        body.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    body.push(value as u8 & 0x7f);
}

#[test]
fn compares_full_width_i64_values_and_produces_exact_i32_booleans() {
    for (value, truth) in [(0, 1), (i64::MIN, 0), (1, 0)] {
        let mut body = vec![0];
        push_i64(&mut body, value);
        body.extend([0x50, 0x0b]);
        assert_body_result(&body, truth);
    }

    // MIN and zero have the same low word, but opposite signed/unsigned order.
    // The equal case also distinguishes strict from inclusive comparisons.
    for (lhs, rhs, results) in [
        (i64::MIN, 0, [0, 1, 1, 0, 0, 1, 1, 0, 0, 1]),
        (0, i64::MIN, [0, 1, 0, 1, 1, 0, 0, 1, 1, 0]),
        (i64::MIN, i64::MIN, [1, 0, 0, 0, 0, 0, 1, 1, 1, 1]),
    ] {
        let mut operands = vec![0];
        push_i64(&mut operands, lhs);
        push_i64(&mut operands, rhs);
        for (opcode, expected) in (0x51..=0x5a).zip(results) {
            assert_body_result(&[&operands[..], &[opcode, 0x0b]].concat(), expected);
        }
    }
}

fn assert_i64_result(opcode: u8, operands: &[i64], expected: i64) {
    let mut body = vec![0];
    for &operand in operands {
        push_i64(&mut body, operand);
    }
    body.push(opcode);
    push_i64(&mut body, expected);
    body.extend([0x51, 0x0b]); // Compare all 64 result bits before returning i32.
    let module = exported_function(TYPE, &code_body(&body));
    assert_eq!(
        module.instantiate().unwrap().invoke_i32("answer"),
        Ok(1),
        "opcode {opcode:#x} with {operands:x?}, expected {expected:#018x}",
    );
}

#[test]
fn each_i64_arithmetic_op_wraps_and_keeps_the_full_i64_result() {
    for (opcode, lhs, rhs, expected) in [
        (0x7c, i64::MAX, 1, i64::MIN),
        (0x7c, -1, 1, 0),
        (0x7d, i64::MIN, 1, i64::MAX),
        (0x7d, 0, 1, -1),
        (0x7e, i64::MAX, 2, -2),
        (0x7e, 1 << 32, 1 << 32, 0),
    ] {
        assert_i64_result(opcode, &[lhs, rhs], expected);
    }
}

#[test]
fn i64_division_and_remainder_preserve_sign_rules_and_full_width_results() {
    for (opcode, lhs, rhs, expected) in [
        (0x7f, -7, 3, -2),
        (0x7f, 7, -3, -2),
        (0x7f, -7, -3, 2),
        (0x7f, i64::MIN, 2, -(1 << 62)),
        (0x7f, i64::MIN, i64::MIN, 1),
        (0x80, -1, 2, i64::MAX),
        (0x80, i64::MIN, 1, i64::MIN),
        (0x80, -1, i64::MIN, 1),
        (0x80, i64::MIN, -1, 0),
        (0x81, -7, 3, -1),
        (0x81, 7, -3, 1),
        (0x81, -7, -3, -1),
        (0x81, -(1 << 40), i64::MAX, -(1 << 40)),
        (0x81, i64::MIN, -1, 0),
        (0x82, -1, i64::MIN, i64::MAX),
        (0x82, i64::MIN, -1, i64::MIN),
        (0x82, -1, 2, 1),
    ] {
        assert_i64_result(opcode, &[lhs, rhs], expected);
    }
    for opcode in 0x7f..=0x82 {
        assert_i64_result(opcode, &[0, i64::MIN], 0);
    }
}

#[test]
fn i64_division_and_remainder_trap_only_if_the_dispatch_is_available() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let cases = (0x7f..=0x82)
        .map(|opcode| (opcode, 0, Trap::IntegerDivideByZero))
        .chain([(0x7f, -1, Trap::IntegerOverflow)]);
    for (opcode, rhs, trap) in cases {
        // Five dispatches per iteration (--p0; continue if nonzero).
        let mut body = b"\0\x03\x40\x20\0\x41\x01\x6b\x22\0\x0d\0\x0b".to_vec();
        body.push(0x01); // 204 * 5 + nop + two consts = 1023 before the op.
        push_i64(&mut body, i64::MIN);
        push_i64(&mut body, rhs);
        let op_offset = body.len();
        body.extend([opcode, 0x1a, 0x41, 42, 0x0b]);
        let module = exported_function(&i32_type(1), &code_body(&body));
        let instance = module.instantiate().unwrap();
        for count in [1, 204] {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &[count]),
                Err(InvokeError::Trap(trap)),
                "opcode {opcode:#x} with divisor {rhs} and {count} iterations",
            );
        }

        body.insert(op_offset, 0x01); // No dispatch left for the trapping op.
        let module = exported_function(&i32_type(1), &code_body(&body));
        assert_eq!(
            module
                .instantiate()
                .unwrap()
                .invoke_i32_with_args("answer", &[204]),
            Err(InvokeError::LimitExceeded { limit }),
            "opcode {opcode:#x} with divisor {rhs}",
        );
    }
}

#[test]
fn i64_traps_keep_prior_writes_skip_later_ones_and_allow_reinvocation() {
    let mut body = b"\0\x41\x07\x24\0".to_vec(); // global[0] = 7.
    push_i64(&mut body, i64::MIN);
    body.extend(b"\x20\0\xac\x7f\x1a"); // Drop MIN / sign_extend(p0).
    body.extend(b"\x42\x05\x20\x01\xad\x82\xa7\x24\0\x23\0\x0b"); // Store and return 5 % zero_extend(p1).
    let validated = validate_module(&module(&[
        (1, &i32_types(&[0, 2])),
        (3, &[2, 0, 1]),
        (6, &[1, 0x7f, 1, 0x41, 42, 0x0b]),
        (7, &function_exports(&[(b"read", 0), (b"run", 1)])),
        (10, &code_bodies(&[b"\0\x23\0\x0b", &body])),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("read"), Ok(42));
    for (args, trap) in [
        ([-1, 0], Trap::IntegerOverflow), // The later unsigned remainder is not run.
        ([2, 0], Trap::IntegerDivideByZero),
    ] {
        assert_eq!(
            instance.invoke_i32_with_args("run", &args),
            Err(InvokeError::Trap(trap)),
        );
        assert_eq!(instance.invoke_i32("read"), Ok(7));
        assert_eq!(instance.invoke_i32_with_args("run", &[2, 3]), Ok(2));
        assert_eq!(instance.invoke_i32("read"), Ok(2));
    }
    assert_eq!(validated.instantiate().unwrap().invoke_i32("read"), Ok(42));
}

#[test]
fn counts_leading_zeros_trailing_zeros_and_ones_in_i64_bits() {
    for (operand, expected) in [
        (0, [64, 64, 0]),
        (1, [63, 0, 1]),
        (i64::MIN, [0, 63, 1]),
        (-1, [0, 0, 64]),
        ((1 << 60) | (1 << 40), [3, 40, 2]),
    ] {
        for (opcode, result) in (0x79..=0x7b).zip(expected) {
            assert_i64_result(opcode, &[operand], result);
        }
    }
}

#[test]
fn applies_distinct_bitwise_operations_to_i64_patterns() {
    let high = (1 << 62) | i64::MIN;
    let common = 1 << 40;
    for (operands, expected) in [
        ([0, i64::MIN], [0, i64::MIN, i64::MIN]),
        ([-1, i64::MIN], [i64::MIN, -1, i64::MAX]),
        ([-1, -1], [-1, -1, 0]),
        (
            [i64::MIN | common | 0x5a, (1 << 62) | common | 0xb5],
            [common | 0x10, high | common | 0xff, high | 0xef],
        ),
    ] {
        for (opcode, result) in (0x83..=0x85).zip(expected) {
            assert_i64_result(opcode, &operands, result);
        }
    }
}

#[test]
fn shifts_and_rotates_by_only_the_low_six_count_bits() {
    // Columns: shl, shr_s, shr_u, rotl, rotr. At 63 all five differ.
    let lhs = i64::MIN | 5;
    let sign_filled = -(1 << 62) + 2;
    let by_one = [10, sign_filled, (1 << 62) + 2, 11, sign_filled];
    let by_63 = [i64::MIN, -1, 1, sign_filled, 11];
    let rotated_by_32 = (5 << 32) | (1 << 31);
    for (operands, expected) in [
        ([lhs, -1], by_63),
        ([lhs, 0], [lhs; 5]),
        ([lhs, 1], by_one),
        (
            [lhs, 32],
            [5 << 32, -(1 << 31), 1 << 31, rotated_by_32, rotated_by_32],
        ),
        ([lhs, 63], by_63),
        ([lhs, 64], [lhs; 5]),
        ([lhs, 65], by_one),
        ([lhs, i64::MIN], [lhs; 5]),
        ([lhs, i64::MIN + 1], by_one),
        ([5, 1], [10, 2, 2, 10, i64::MIN | 2]),
        ([0, 63], [0; 5]),
        ([-1, 63], [i64::MIN, -1, 1, -1, -1]),
    ] {
        for (opcode, result) in (0x86..=0x8a).zip(expected) {
            assert_i64_result(opcode, &operands, result);
        }
    }
}

fn numeric_ops() -> impl Iterator<Item = (u8, usize)> {
    (0x50..=0x5a).chain(0x79..=0x8a).map(|opcode| {
        (
            opcode,
            if matches!(opcode, 0x50 | 0x79..=0x7b) {
                1
            } else {
                2
            },
        )
    })
}

#[test]
fn i64_numeric_ops_check_live_and_dead_concrete_inputs_at_the_opcode() {
    for (op, arity) in numeric_ops() {
        let inputs = b"\x42\0".repeat(arity);
        for head in [&b"\0"[..], b"\0\0"] {
            for operand in 0..arity {
                let mut body = [head, &inputs, &[op, 0x0b]].concat();
                body[head.len() + 2 * operand] = 0x41;
                bad_body(&body, head.len() + inputs.len(), NEED_I64);
            }
        }

        // Frozen operands cannot make up a missing input in a reachable block.
        let inside = &inputs[2..];
        let body = [&b"\0"[..], &inputs, b"\x02\x40", inside, &[op, 0x0b]].concat();
        bad_body(
            &body,
            body.len() - 2,
            ErrorKind::StackUnderflow { depth: arity - 1 },
        );
        if arity == 2 {
            // A missing lhs may be polymorphic; a concrete rhs is still checked.
            bad_body(&[0, 0x00, 0x41, 0, op, 0x0b], 4, NEED_I64);
        }
    }
}

#[test]
fn i64_numeric_ops_check_concrete_results_even_with_polymorphic_inputs() {
    for (op, arity) in numeric_ops() {
        let (consumer, wrong_consumer, mismatch) = if op >= 0x79 {
            (0xa7, 0x45, NEED_I32)
        } else {
            (0x45, 0xa7, NEED_I64)
        };
        let mut live = vec![0];
        live.extend(b"\x42\0".repeat(arity));
        live.extend([op, wrong_consumer, 0x0b]);
        bad_body(&live, live.len() - 2, mismatch);

        for concrete in 0..=arity {
            // Skip a dead tail whose i64 inputs can be concrete or implicit.
            let mut body = b"\0\x02\x7f\x41\x2a\x0c\0".to_vec();
            body.extend(b"\x42\0".repeat(concrete));
            body.extend([op, consumer, 0x0b, 0x0b]);
            assert_body_result(&body, 42);
            let offset = body.len() - 3;
            body[offset] = wrong_consumer;
            bad_body(&body, offset, mismatch);
        }
    }
}

#[test]
fn other_i64_numeric_and_memory_opcodes_remain_unsupported() {
    // i64.load, i64.store, and post-Core-1.0 i64 sign extensions.
    for opcode in [0x29, 0x37, 0xc2, 0xc3, 0xc4] {
        bad_body(
            &[0, 0x00, opcode, 0x0b],
            2,
            ErrorKind::UnsupportedOpcode { opcode },
        );
    }
}

#[test]
fn sleb64_rejects_every_bad_tenth_byte_and_stays_inside_nested_frames() {
    let field = IntegerField::I64Const;
    let overflow = ErrorKind::IntegerOverflow { field };
    let truncated = ErrorKind::TruncatedInteger { field };
    let mut body = [0x80; 15];
    body[..3].copy_from_slice(&[0, 0x00, 0x42]); // Decode even after unreachable.
    body[13..].copy_from_slice(&[0x1a, 0x0b]);
    for terminal in 0..=u8::MAX {
        if matches!(terminal, 0 | 0x7f) {
            continue;
        }
        body[12] = terminal;
        bad_body(&body, 12, overflow);
    }
    for end in 3..=12 {
        bad_body(&body[..end], end, truncated); // Body ends before a custom section.
    }
    // The next bytes lie in the same code section, but belong to another body.
    let first = &body[..12];
    let code = code_bodies(&[first, &[0, 0x41, 0, 0x0b]]);
    let mut bytes = module(&[(1, TYPE), (3, TWO_FUNCTIONS)]);
    let start = push_section(&mut bytes, 10, &code);
    expect_error(&bytes, start + 2 + first.len(), truncated);
}

#[test]
fn typed_consumers_reject_wrong_concrete_values_in_live_and_dead_code() {
    for (pushes, consumer, kind) in [
        (&b"\x41\0"[..], &b"\xa7"[..], NEED_I64),
        (b"\x42\0", b"\xac", NEED_I32),
        (b"\x42\0", b"\xad", NEED_I32),
        (b"\x42\0", b"\x45", NEED_I32),
        (b"\x42\0\x41\0", b"\x6a", NEED_I32),
        (b"\x42\0\x41\0\x41\0", b"\x1b", NEED_I32), // First value.
        (b"\x41\0\x42\0\x41\0", b"\x1b", NEED_I32), // Second value.
        (b"\x41\0\x41\0\x42\0", b"\x1b", NEED_I32), // Condition.
        (b"\x42\0", b"\x21\0", NEED_I32),
        (b"\x42\0", b"\x22\0", NEED_I32),
        (b"\x42\0", b"\x04\x40", NEED_I32),
        (b"\x42\0", b"\x0f", NEED_I32),
        (b"\x42\0", b"\x0c\0", NEED_I32),
        (b"\x42\0\x41\0", b"\x0d\0", NEED_I32), // Label result below condition.
        (b"\x42\0\x41\0", b"\x0e\0\0", NEED_I32), // Label result below selector.
        (b"\x41\0\x42\0", b"\x0e\0\0", NEED_I32), // Selector.
    ] {
        for head in [&[1, 1, 0x7f][..], &[1, 1, 0x7f, 0x00]] {
            let body = [head, pushes, consumer, &[0x0b]].concat();
            bad_body(&body, head.len() + pushes.len(), kind);
        }
    }
    // The concrete operand is still checked even if the other input is implicit.
    bad_body(b"\0\0\x42\0\x6a\x0b", 4, NEED_I32);
}

#[test]
fn joins_and_branches_use_only_the_active_floor_and_declared_result() {
    for (head, delimiter) in [
        (&b"\0"[..], 0x0b),                      // Function.
        (b"\0\x02\x7f", 0x0b),                   // Block.
        (b"\0\x41\0\x04\x7f", 0x05),             // Untaken then.
        (b"\0\x41\x01\x04\x7f\x41\0\x05", 0x0b), // Untaken else.
    ] {
        for reachability in [&[][..], &[0x00]] {
            let body = [head, reachability, &[0x42, 0, delimiter, 0x0b]].concat();
            bad_body(&body, head.len() + reachability.len() + 2, NEED_I32);
        }
    }
    bad_body(
        b"\0\x42\x2a\x02\x40\x0f\x0b\xa7\x0b",
        5,
        ErrorKind::StackUnderflow { depth: 0 },
    );
    // New frames/else arms do not inherit the parent's polymorphic floor.
    bad_body(
        b"\0\0\x02\x40\xa7\x0b\x0b",
        4,
        ErrorKind::StackUnderflow { depth: 0 },
    );
    bad_body(
        b"\0\0\x04\x40\0\x05\xa7\x0b\x0b",
        6,
        ErrorKind::StackUnderflow { depth: 0 },
    );
    // An implicit child result and a restored br_if result are concrete i32s.
    bad_body(b"\0\0\x02\x7f\0\x0b\xa7\x0b", 6, NEED_I64);
    bad_body(b"\0\0\x0d\0\xa7\x0b", 4, NEED_I64);
    // Implicit operands may satisfy either conversion, but each push is typed.
    bad_body(b"\0\0\xa7\xa7\x0b", 3, NEED_I64);
    bad_body(b"\0\0\xac\x0b", 3, NEED_I32);
    // The branch discards its inner i64, not the i64 below the target floor.
    // The unreachable tail can still use implicit i32 and i64 at that floor.
    assert_body_result(
        &[
            &b"\0\x42\x28\x02\x7f\x42\x07\x41\x02\x0c\0"[..],
            b"\xac\xa7\x1a\xa7\x1a\x0b\x1a\xa7\x41\x02\x6a\x0b",
        ]
        .concat(),
        42,
    );
}

const TABLE: &[u8] = &[1, 0x70, 0, 1];
const ELEMENT: &[u8] = &[1, 0, 0x41, 0, 0x0b, 1, 1];
const HELPER: &[u8] = b"\0\x20\0\x0b"; // i32 identity.

fn calls_module(caller: &[u8]) -> (Vec<u8>, usize) {
    let code = code_bodies(&[caller, HELPER]);
    let mut bytes = module(&[
        (1, &i32_types(&[0, 1])),
        (3, &[2, 0, 1]),
        (4, TABLE),
        (7, EXPORT),
        (9, ELEMENT),
    ]);
    let start = push_section(&mut bytes, 10, &code);
    (bytes, start + 2) // Count and caller length, one byte each.
}

#[test]
fn calls_check_i32_args_and_selector_but_protect_the_caller_i64_prefix() {
    for (pushes, call) in [
        (&b"\x42\0"[..], &b"\x10\x01"[..]),
        (b"\x42\0\x41\0", b"\x11\x01\0"), // Wrong argument.
        (b"\x41\0\x42\0", b"\x11\x01\0"), // Wrong selector.
    ] {
        for head in [&b"\0"[..], b"\0\0"] {
            let (bytes, start) = calls_module(&[head, pushes, call, &[0x0b]].concat());
            expect_error(&bytes, start + head.len() + pushes.len(), NEED_I32);
        }
    }
    for call in [&b"\x10\x01"[..], b"\x41\0\x11\x01\0"] {
        let (bytes, _) =
            calls_module(&[&b"\0\x42\x2a\x41\x07"[..], call, b"\x1a\xa7\x0b"].concat());
        assert_eq!(
            validate_module(&bytes)
                .unwrap()
                .instantiate()
                .unwrap()
                .invoke_i32("answer"),
            Ok(42)
        );
    }
}

#[test]
fn memory_and_globals_do_not_accept_i64_operands_even_in_dead_code() {
    for (pushes, op) in [
        (&b"\x42\0"[..], &b"\x28\0\0"[..]),
        (b"\x42\0\x41\0", b"\x36\0\0"), // Store address.
        (b"\x41\0\x42\0", b"\x36\0\0"), // Store value.
        (b"\x42\0", b"\x40\0"),         // Grow delta.
        (b"\x42\0", b"\x24\0"),         // Mutable global.
    ] {
        for head in [&b"\0"[..], b"\0\0"] {
            let body = [head, pushes, op, &[0x0b]].concat();
            let mut bytes = module(&[
                (1, TYPE),
                (3, FUNCTION),
                (5, &[1, 0, 1]),
                (6, &[1, 0x7f, 1, 0x41, 0, 0x0b]),
            ]);
            let start = push_section(&mut bytes, 10, &code_body(&body));
            expect_error(&bytes, start + 2 + head.len() + pushes.len(), NEED_I32);
        }
    }
}

#[test]
fn i64_remains_unsupported_in_signatures_globals_and_control_labels() {
    for (id, payload, offset, expected) in [
        (1, &b"\x01\x60\x01\x7e\0"[..], 3, 0x7f), // Parameter.
        (1, b"\x01\x60\0\x01\x7e", 4, 0x7f),      // Result.
        (6, b"\x01\x7e\0\x42\0\x0b", 1, 0x7f),    // Global.
        (6, b"\x01\x7f\0\x42\0\x0b", 3, 0x41),    // Initializer.
    ] {
        bad_payload(
            id,
            payload,
            offset,
            ErrorKind::UnexpectedByte {
                expected,
                found: payload[offset],
            },
        );
    }
    for (body, offset, expected) in [
        (&b"\0\x02\x7e\x0b"[..], 2, 0x7f),  // Block result.
        (b"\0\x03\x7e\x0b", 2, 0x40),       // Loop result.
        (b"\0\x41\0\x04\x7e\x0b", 4, 0x7f), // If result.
    ] {
        bad_body(
            body,
            offset,
            ErrorKind::UnexpectedByte {
                expected,
                found: 0x7e,
            },
        );
    }
}

#[test]
fn mixed_operands_share_the_exact_stack_cap_including_frozen_prefixes() {
    let limit = Limit::OperandStackDepth;
    assert_eq!(limit.maximum(), 32);
    let mut body = b"\0\x42\x2a".to_vec();
    body.extend(b"\x42\0\x41\0".repeat(15));
    body.extend(b"\x02\x40\x42\0");
    let peak = body.len();
    body.extend(b"\xa7\xac\x1a\x0b");
    body.extend([0x1a; 30]);
    body.extend(b"\xa7\x0b");
    assert_body_result(&body, 42);
    body.splice(peak..peak, [0x42, 0]);
    bad_body(&body, peak, ErrorKind::LimitExceeded { limit });
    let dead = [&b"\0\0"[..], &b"\x42\0".repeat(33)].concat();
    bad_body(&dead, 2 + 2 * 32, ErrorKind::LimitExceeded { limit });
    // Even if the child synthesizes a missing result at its floor, the
    // parent's resulting push must fit alongside all the frozen operands.
    let join = [&b"\0"[..], &b"\x42\0".repeat(32), b"\x02\x7f\0\x0b\x0b"].concat();
    bad_body(&join, 1 + 2 * 32 + 3, ErrorKind::LimitExceeded { limit });
}

#[test]
fn counts_i64_instruction_slots_and_all_padded_body_bytes() {
    let mut body = vec![0];
    body.extend([0x01; 123]);
    body.extend(b"\x42\0\x42\0\x7e\x50\x0b");
    assert_body_result(&body, 1);
    let extra = body.len() - 1;
    body.insert(extra, 0xa7);
    bad_body(
        &body,
        extra,
        ErrorKind::LimitExceeded {
            limit: Limit::BodyInstructions,
        },
    );

    let mut body = vec![120];
    body.extend([0, 0x7f].repeat(120));
    body.extend(b"\x42\x80\x80\x80\x80\x80\x80\x80\x80\x80\0\x1a\x41\x2a\x0b");
    let limit = Limit::CodeBodyBytes;
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 42);
    body.insert(body.len() - 1, 0x01);
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn i64_numeric_ops_share_the_exact_stack_limit_even_at_control_floors() {
    let limit = Limit::OperandStackDepth;
    assert_eq!(limit.maximum(), 32);
    for (op, arity) in numeric_ops() {
        let prefix = limit.maximum() - arity;
        let mut body = b"\0\x41\x2a".to_vec();
        for slot in 1..prefix {
            body.extend([if slot % 2 == 0 { 0x41 } else { 0x42 }, 0]);
        }
        body.extend(b"\x02\x40");
        body.extend(b"\x42\x01".repeat(arity));
        let peak = body.len();
        body.extend([op, 0x1a, 0x0b]);
        body.extend(std::iter::repeat_n(0x1a, prefix - 1));
        body.push(0x0b);
        assert_body_result(&body, 42);
        body.splice(peak..peak, [0x42, 0]);
        bad_body(&body, peak, ErrorKind::LimitExceeded { limit });

        // Implicit inputs consume no slots, but the concrete result needs a slot.
        let mut dead = vec![0];
        dead.extend(b"\x42\0".repeat(limit.maximum()));
        dead.extend([0x02, 0x40, 0x00, op, 0x0b]);
        bad_body(&dead, dead.len() - 2, ErrorKind::LimitExceeded { limit });
    }
}

#[test]
fn every_i64_numeric_op_spends_exactly_one_dispatch() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    for (op, arity) in numeric_ops() {
        let mut base = b"\0\x41\x2a\x03\x40".to_vec();
        base.extend(b"\x42\x01".repeat(arity));
        base.extend([op, 0x1a]);
        base.extend(b"\x20\0\x41\x01\x6b\x22\0\x0d\0\x0b");
        // One dispatch before the loop, seven plus arity per iteration.
        let per_iteration = 7 + arity;
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
                "opcode {op:#x} with {nops} trailing nops"
            );
        }
    }
}

#[test]
fn i64_const_and_conversions_spend_one_dispatch_each() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let base = b"\0\x41\x2a\x03\x40\x42\0\x1a\x41\x7f\xac\xa7\xad\xa7\x1a\x20\0\x41\x01\x6b\x22\0\x0d\0\x0b";
    // 1 before the loop, 13 per iteration, then 9 nops = 1024 dispatches.
    for (nops, expected) in [(9, Ok(42)), (10, Err(InvokeError::LimitExceeded { limit }))] {
        let body = [&base[..], &vec![0x01; nops], &[0x0b]].concat();
        let module = exported_function(&i32_type(1), &code_body(&body));
        assert_eq!(
            module
                .instantiate()
                .unwrap()
                .invoke_i32_with_args("answer", &[78]),
            expected
        );
    }
}
