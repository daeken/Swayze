use super::*;

const TYPES: &[u8] = &[
    7, // Type count.
    0x60, 1, 0x7f, 1, 0x7f, // 0: (i32) -> i32
    0x60, 2, 0x7f, 0x7f, 1, 0x7f, // 1: (i32, i32) -> i32
    0x60, 2, 0x7f, 0x7f, 1, 0x7f, // 2: duplicate of type 1
    0x60, 2, 0x7f, 0x7f, 0, // 3: (i32, i32) -> ()
    0x60, 0, 0, // 4: () -> ()
    0x60, 0, 1, 0x7f, // 5: () -> i32
    0x60, 1, 0x7f, 0, // 6: (i32) -> ()
];
const ZERO_ENTRY_TABLE: &[u8] = &[1, 0x70, 0, 0];
const EMPTY_BODY: &[u8] = &[0, 0x0b];

fn declarations(type_indices: &[u8], table: Option<&[u8]>) -> Vec<u8> {
    let functions = [&[type_indices.len() as u8], type_indices].concat();
    let mut bytes = module(&[(1, TYPES), (3, &functions)]);
    if let Some(table) = table {
        push_section(&mut bytes, 4, table);
    }
    bytes
}

fn body_error(table: Option<&[u8]>, body: &[u8], offset: usize, kind: ErrorKind) {
    let mut bytes = declarations(&[4, 4], table);
    let code = code_bodies(&[body, EMPTY_BODY]);
    let start = push_section(&mut bytes, 10, &code);
    let first_body = code.len() - EMPTY_BODY.len() - 1 - body.len();
    // Immediate decoding must not consume the next body's size or contents.
    expect_error(&bytes, start + first_body + offset, kind);
}

#[test]
fn structural_match_preserves_argument_order_and_the_callers_prefix() {
    // Save 40 below the block floor. Push 11, 9, and the selector; ask for type
    // 1 with a padded LEB32. On success, add 40 to the callee's subtraction.
    let entry = [
        0, 0x41, 40, 0x02, 0x7f, 0x41, 11, 0x41, 9, 0x20, 0, 0x11, 0x81, 0x80, 0x80, 0x80, 0, 0,
        0x0b, 0x6a, 0x0b,
    ];
    let subtract = [0, 0x20, 0, 0x20, 1, 0x6b, 0x0b];
    let identity = [0, 0x20, 0, 0x0b];
    let mut bytes = declarations(&[0, 2, 0, 3], Some(&[1, 0x70, 0, 4]));
    push_section(&mut bytes, 7, &function_exports(&[(b"entry", 0)]));
    push_section(&mut bytes, 9, &[1, 0, 0x41, 0, 0x0b, 3, 1, 2, 3]);
    push_section(
        &mut bytes,
        10,
        &code_bodies(&[&entry, &subtract, &identity, EMPTY_BODY]),
    );
    let module = validate_module(&bytes).unwrap();
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32_with_args("entry", &[0]), Ok(42));
    for (selector, trap) in [
        (1, Trap::IndirectCallTypeMismatch), // (i32) -> i32, wrong parameter list.
        (2, Trap::IndirectCallTypeMismatch), // (i32, i32) -> (), wrong result.
        (3, Trap::UninitializedElement),
        (4, Trap::TableOutOfBounds), // Within backing storage, past the logical length.
        (-1, Trap::TableOutOfBounds),
        (i32::MIN, Trap::TableOutOfBounds),
    ] {
        assert_eq!(
            instance.invoke_i32_with_args("entry", &[selector]),
            Err(InvokeError::Trap(trap)),
            "selector {selector}"
        );
    }
    assert_eq!(instance.invoke_i32_with_args("entry", &[0]), Ok(42));
}

#[test]
fn requires_a_defined_table_even_in_dead_code_but_accepts_zero_entries() {
    let reachable = [0, 0x41, 0, 0x11, 4, 0, 0x0b];
    let dead = [0, 0x00, 0x11, 4, 0, 0x0b];
    for table in [None, Some(&[0][..])] {
        body_error(table, &reachable, 3, ErrorKind::MissingTable);
        body_error(table, &dead, 2, ErrorKind::MissingTable);
    }
    let mut bytes = declarations(&[4, 4], Some(ZERO_ENTRY_TABLE));
    push_section(&mut bytes, 7, &function_exports(&[(b"run", 0)]));
    push_section(&mut bytes, 10, &code_bodies(&[&reachable, &dead]));
    let module = validate_module(&bytes).unwrap();
    assert_eq!(
        module.instantiate().unwrap().invoke_void("run"),
        Err(InvokeError::Trap(Trap::TableOutOfBounds))
    );
}

#[test]
fn checks_framed_type_indices_and_the_literal_reserved_byte_in_dead_code() {
    for (immediates, offset, kind) in [
        (
            &[][..],
            0,
            ErrorKind::TruncatedInteger {
                field: IntegerField::TypeIndex,
            },
        ),
        (
            &[0x80, 0x80],
            2,
            ErrorKind::TruncatedInteger {
                field: IntegerField::TypeIndex,
            },
        ),
        (
            &[0x80, 0x80, 0x80, 0x80, 0x10, 0],
            4,
            ErrorKind::IntegerOverflow {
                field: IntegerField::TypeIndex,
            },
        ),
        (
            &[0x87, 0, 0],
            0,
            ErrorKind::InvalidIndex {
                field: IntegerField::TypeIndex,
                index: 7,
            },
        ),
        (&[4], 1, ErrorKind::TruncatedByte { expected: 0 }),
        (
            &[4, 1],
            1,
            ErrorKind::UnexpectedByte {
                expected: 0,
                found: 1,
            },
        ),
        (
            &[4, 0x80, 0],
            1,
            ErrorKind::UnexpectedByte {
                expected: 0,
                found: 0x80,
            },
        ),
    ] {
        let body = [&[0, 0x00, 0x11][..], immediates].concat();
        body_error(Some(ZERO_ENTRY_TABLE), &body, 3 + offset, kind);
    }
}

#[test]
fn pops_arguments_and_selector_only_above_the_active_floor() {
    let table = Some(ZERO_ENTRY_TABLE);
    let prefix = &[0, 0x41, 10, 0x41, 20, 0x41, 0, 0x02, 0x40][..];
    // Three operands below the block floor cannot supply even the selector.
    for (operands, signature, depth) in [
        (&[][..], 4, 0),
        (&[0x41, 0], 3, 1),
        (&[0x41, 9, 0x41, 0], 3, 2),
    ] {
        let body = [prefix, operands, &[0x11, signature, 0, 0x0b]].concat();
        body_error(
            table,
            &body,
            prefix.len() + operands.len(),
            ErrorKind::StackUnderflow { depth },
        );
    }
    // A dead frame supplies missing arguments and selector, but the expected
    // i32 result is explicit and must be dropped for this void block to close.
    let dead_call = [prefix, &[0x00, 0x11, 1, 0]].concat();
    let no_drop = [&dead_call[..], &[0x0b, 0x1a, 0x1a, 0x1a, 0x0b]].concat();
    body_error(
        table,
        &no_drop,
        dead_call.len(),
        ErrorKind::InvalidResultDepth { depth: 1 },
    );
    let body = [&dead_call[..], &[0x1a, 0x0b, 0x1a, 0x1a, 0x1a, 0x0b]].concat();
    let mut bytes = declarations(&[4], table);
    push_section(&mut bytes, 10, &code_body(&body));
    validate_module(&bytes).unwrap();
}

#[test]
fn indirect_calls_count_toward_instruction_and_operand_caps() {
    let table = Some(ZERO_ENTRY_TABLE);
    let limit = Limit::BodyInstructions;
    let mut body = vec![0];
    body.extend(std::iter::repeat_n(0x01, limit.maximum() - 2));
    body.extend([0x41, 0, 0x11, 4, 0, 0x0b]);
    let mut bytes = declarations(&[4], table);
    push_section(&mut bytes, 10, &code_body(&body));
    validate_module(&bytes).unwrap();
    body.insert(1, 0x01);
    body_error(
        table,
        &body,
        body.len() - 4,
        ErrorKind::LimitExceeded { limit },
    );

    let limit = Limit::OperandStackDepth;
    let mut body = vec![0];
    for _ in 0..limit.maximum() {
        body.extend([0x41, 0]);
    }
    body.extend([0x02, 0x40, 0x00, 0x11]);
    let call = body.len() - 1;
    // Even a dead call produces an explicit result; the frozen prefix still
    // counts toward the cap.
    body.extend([1, 0, 0x1a, 0x0b]);
    body.extend(std::iter::repeat_n(0x1a, limit.maximum() - 1));
    body.push(0x0b);
    body_error(table, &body, call, ErrorKind::LimitExceeded { limit });
    body.drain(1..3); // One fewer saved operand leaves room for the call's result.
    let mut bytes = declarations(&[4], table);
    push_section(&mut bytes, 10, &code_body(&body));
    validate_module(&bytes).unwrap();
}

fn recursive_module(leaf_nops: usize) -> Module {
    // Type 3: (remaining descents, terminal selector) -> (). Table slots are
    // this function, a matching () -> () leaf, and null. The recursion uses
    // slot 0 with type 3; the terminal call uses the host selector with type 4.
    let mut recur = vec![0, 0x20, 0, 0x04, 0x40];
    recur.extend(std::iter::repeat_n(0x01, 60));
    recur.extend([0x20, 0, 0x41, 1, 0x6b, 0x20, 1, 0x41, 0, 0x11, 3, 0]);
    recur.extend([0x05, 0x20, 1]);
    recur.extend(std::iter::repeat_n(0x01, leaf_nops));
    recur.extend([0x11, 4, 0, 0x0b, 0x0b]);
    let mut bytes = declarations(&[3, 4], Some(&[1, 0x70, 0, 3]));
    push_section(&mut bytes, 7, &function_exports(&[(b"recur", 0)]));
    push_section(&mut bytes, 9, &[1, 0, 0x41, 0, 0x0b, 2, 0, 1]);
    push_section(&mut bytes, 10, &code_bodies(&[&recur, EMPTY_BODY]));
    validate_module(&bytes).unwrap()
}

#[test]
fn checks_dispatch_then_target_then_call_depth() {
    let module = recursive_module(0);
    let instance = module.instantiate().unwrap();
    let full_depth = ExecutionLimit::CallDepth.maximum() as i32 - 1;
    assert_eq!(
        instance.invoke_void_with_args("recur", &[full_depth - 1, 1]),
        Ok(())
    );
    // At full depth, 15 descents cost 68 dispatches each, then local.get, if,
    // and local.get cost three more. The terminal call is dispatch 1024.
    let exhausted = recursive_module(1);
    let exhausted = exhausted.instantiate().unwrap();
    for (selector, reason) in [
        (0, InvokeError::Trap(Trap::IndirectCallTypeMismatch)),
        (
            1,
            InvokeError::LimitExceeded {
                limit: ExecutionLimit::CallDepth,
            },
        ),
        (2, InvokeError::Trap(Trap::UninitializedElement)),
        (3, InvokeError::Trap(Trap::TableOutOfBounds)),
    ] {
        assert_eq!(
            instance.invoke_void_with_args("recur", &[full_depth, selector]),
            Err(reason)
        );
        // A single extra nop uses the last dispatch before the terminal call.
        assert_eq!(
            exhausted.invoke_void_with_args("recur", &[full_depth, selector]),
            Err(InvokeError::LimitExceeded {
                limit: ExecutionLimit::InstructionDispatches
            })
        );
    }
}

#[test]
fn indirect_calls_and_resumed_callers_share_one_dispatch_budget() {
    // 14 descents cost 14 * 68, then 4 terminal dispatches and 14 executed
    // else jumps on resumption. Frame completion, including the empty leaf,
    // is free. Pad the terminal arm to hit exactly 1024 or exceed it by one.
    let limit = ExecutionLimit::InstructionDispatches;
    let padding = limit.maximum() - (14 * 68 + 4 + 14);
    for (nops, outcome) in [
        (padding, Ok(())),
        (padding + 1, Err(InvokeError::LimitExceeded { limit })),
    ] {
        let module = recursive_module(nops);
        assert_eq!(
            module
                .instantiate()
                .unwrap()
                .invoke_void_with_args("recur", &[14, 1]),
            outcome
        );
    }
}

#[test]
fn private_callees_share_memory_and_globals_and_preserve_completed_stores() {
    let mut bytes = declarations(&[6, 6, 6, 5, 5], Some(&[1, 0x70, 0, 2]));
    push_section(&mut bytes, 5, &[1, 0, 1]); // One memory page.
    push_section(&mut bytes, 6, &[1, 0x7f, 1, 0x41, 0, 0x0b]); // Mutable global initialized to zero.
    push_section(
        &mut bytes,
        7,
        &function_exports(&[(b"write", 0), (b"read", 3)]),
    );
    push_section(&mut bytes, 9, &[1, 0, 0x41, 0, 0x0b, 2, 1, 4]);
    let write = [
        0, 0x20, 0, 0x41, 0, 0x11, 6, 0, // Indirect call to f1(arg).
        0x23, 0, 0x41, 2, 0x6a, 0x24, 0, 0x0b, // Only on success: global += 2.
    ];
    let store = [
        0, 0x41, 0, 0x20, 0, 0x36, 2, 0, // memory[0] = arg.
        0x20, 0, 0x10, 2, // Direct call to f2(arg), storing the global.
        0x20, 0, 0x41, 1, 0x46, 0x04, 0x40, 0x00, 0x0b, 0x0b, // Trap if arg == 1.
    ];
    let set_global = [0, 0x20, 0, 0x24, 0, 0x0b];
    let read = [0, 0x41, 1, 0x11, 5, 0, 0x0b]; // Indirect call to f4().
    let load = [0, 0x41, 0, 0x28, 2, 0, 0x23, 0, 0x6a, 0x0b]; // memory[0] + global.
    push_section(
        &mut bytes,
        10,
        &code_bodies(&[&write, &store, &set_global, &read, &load]),
    );
    let module = validate_module(&bytes).unwrap();
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_void_with_args("write", &[20]), Ok(()));
    assert_eq!(instance.invoke_i32("read"), Ok(42));
    assert_eq!(
        instance.invoke_void_with_args("write", &[1]),
        Err(InvokeError::Trap(Trap::Unreachable))
    );
    assert_eq!(instance.invoke_i32("read"), Ok(2));
    assert_eq!(module.instantiate().unwrap().invoke_i32("read"), Ok(0));
}
