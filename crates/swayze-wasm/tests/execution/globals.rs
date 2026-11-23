use super::{
    EXPORT, ErrorKind, ExecutionLimit, FUNCTION, HEADER, IntegerField, InvokeError, Limit, Module,
    TWO_FUNCTIONS, TYPE, Trap, bad_payload, code_bodies, code_body, expect_error, function_exports,
    i32_types, module, push_section, validate_module,
};
use swayze_wasm::ValType::{I32, I64};

#[path = "globals/float.rs"]
mod float;

const MUTABLE: &[u8] = &[1, 0x7f, 1, 0x41, 42, 0x0b];
const IMMUTABLE: &[u8] = &[1, 0x7f, 0, 0x41, 42, 0x0b];
const MUTABLE_I64: &[u8] = &[1, 0x7e, 1, 0x42, 42, 0x0b];
const IMMUTABLE_I64: &[u8] = &[1, 0x7e, 0, 0x42, 42, 0x0b];

fn with_global_body(globals: &[u8], body: &[u8]) -> Module {
    validate_module(&module(&[
        (1, TYPE),
        (3, FUNCTION),
        (6, globals),
        (7, EXPORT),
        (10, &code_body(body)),
    ]))
    .unwrap()
}

fn bad_global_body(globals: Option<&[u8]>, body: &[u8], offset: usize, kind: ErrorKind) {
    let mut bytes = module(&[(1, TYPE), (3, TWO_FUNCTIONS)]);
    if let Some(globals) = globals {
        push_section(&mut bytes, 6, globals);
    }
    push_section(&mut bytes, 7, EXPORT);
    // The tested body is unexported and followed by another section, so the
    // validator must inspect it and must not read its immediates past its frame.
    let first = &[0, 0x41, 0, 0x0b];
    let code = code_bodies(&[first, body]);
    let start = push_section(&mut bytes, 10, &code);
    push_section(&mut bytes, 0, &[0]);
    expect_error(&bytes, start + code.len() - body.len() + offset, kind);
}

#[test]
fn accepts_globals_only_and_empty_global_vectors() {
    for sections in [
        &[(6, MUTABLE)][..],
        &[(6, IMMUTABLE)],
        &[(6, MUTABLE_I64)],
        &[(6, IMMUTABLE_I64)],
        &[(6, &[0])],
        &[(5, &[0]), (6, &[0x80, 0]), (11, &[0])],
    ] {
        let validated = validate_module(&module(sections)).unwrap();
        assert_eq!(
            validated.instantiate().unwrap().invoke_i32("answer"),
            Err(InvokeError::ExportNotFound),
        );
    }
    let validated = with_global_body(&[0], &[0, 0x41, 42, 0x0b]);
    assert_eq!(
        validated.instantiate().unwrap().invoke_i32("answer"),
        Ok(42)
    );
}

#[test]
fn validates_global_section_order_and_rejects_unsupported_scope() {
    for preceding_sections in [
        &[(6, &[0][..]), (0, &[0])][..],
        &[(1, TYPE), (3, FUNCTION), (7, EXPORT)],
        &[
            (1, TYPE),
            (3, FUNCTION),
            (10, &code_body(&[0, 0x41, 0, 0x0b])),
        ],
        &[(11, &[0])],
    ] {
        let mut bytes = module(preceding_sections);
        let offset = bytes.len();
        push_section(&mut bytes, 6, &[0]);
        expect_error(&bytes, offset, ErrorKind::SectionOutOfOrder { id: 6 });
    }
    let mut bytes = module(&[(6, MUTABLE)]);
    let offset = bytes.len();
    push_section(&mut bytes, 5, &[1, 0, 1]);
    expect_error(&bytes, offset, ErrorKind::SectionOutOfOrder { id: 5 });

    let mut bytes = HEADER.to_vec();
    let count = push_section(&mut bytes, 2, b"\x01\x01m\x01g\x03\x7f\0");
    push_section(&mut bytes, 6, IMMUTABLE);
    expect_error(
        &bytes,
        count,
        ErrorKind::UnsupportedCount {
            field: IntegerField::ImportCount,
            count: 1,
        },
    );
    let mut bytes = module(&[(5, &[1, 0, 1]), (6, IMMUTABLE)]);
    let start = push_section(&mut bytes, 11, &[1, 0, 0x23, 0, 0x0b, 0]);
    expect_error(
        &bytes,
        start + 2,
        ErrorKind::UnexpectedByte {
            expected: 0x41,
            found: 0x23,
        },
    );
}

#[test]
fn rejects_malformed_global_declarations_and_counts_at_the_section_boundary() {
    use ErrorKind::*;
    use IntegerField::{GlobalCount, I32Const};

    for (payload, offset, kind) in [
        (&[][..], 0, TruncatedInteger { field: GlobalCount }),
        (&[0x80], 1, TruncatedInteger { field: GlobalCount }),
        (
            &[0x80, 0x80, 0x80, 0x80, 0x10],
            4,
            IntegerOverflow { field: GlobalCount },
        ),
        (
            &[9],
            0,
            UnsupportedCount {
                field: GlobalCount,
                count: 9,
            },
        ),
        (&[1], 1, TruncatedByte { expected: 0x7f }),
        (
            &[1, 0x70],
            1,
            UnexpectedByte {
                expected: 0x7f,
                found: 0x70,
            },
        ),
        (&[1, 0x7f], 2, TruncatedGlobalMutability),
        (&[1, 0x7f, 2], 2, InvalidGlobalMutability { byte: 2 }),
        (
            &[1, 0x7f, 0x80, 0],
            2,
            InvalidGlobalMutability { byte: 0x80 },
        ),
        (&[1, 0x7f, 0], 3, TruncatedByte { expected: 0x41 }),
        (
            &[1, 0x7f, 0, 0x23, 0, 0x0b],
            3,
            UnexpectedByte {
                expected: 0x41,
                found: 0x23,
            },
        ),
        (
            &[1, 0x7f, 1, 0x42, 0, 0x0b],
            3,
            UnexpectedByte {
                expected: 0x41,
                found: 0x42,
            },
        ),
        (
            &[1, 0x7f, 1, 0x41, 0x80],
            5,
            TruncatedInteger { field: I32Const },
        ),
        (
            &[1, 0x7f, 1, 0x41, 0x80, 0x80, 0x80, 0x80, 8],
            8,
            IntegerOverflow { field: I32Const },
        ),
        (&[1, 0x7f, 0, 0x41, 0], 5, TruncatedByte { expected: 0x0b }),
        (
            &[1, 0x7f, 0, 0x41, 0, 0x41, 0, 0x0b],
            5,
            UnexpectedByte {
                expected: 0x0b,
                found: 0x41,
            },
        ),
        (
            &[1, 0x7f, 0, 0x41, 0, 0x0b, 0],
            6,
            TrailingSectionBytes { id: 6 },
        ),
        (
            &[2, 0x7f, 0, 0x41, 0, 0x0b],
            6,
            TruncatedByte { expected: 0x7f },
        ),
    ] {
        bad_payload(6, payload, offset, kind);
    }
    // Even reads of earlier immutable declarations are not an initializer form.
    bad_payload(
        6,
        &[2, 0x7f, 0, 0x41, 42, 0x0b, 0x7f, 0, 0x23, 0, 0x0b],
        8,
        UnexpectedByte {
            expected: 0x41,
            found: 0x23,
        },
    );
}

#[test]
fn requires_an_exact_i64_initializer_in_the_global_section_frame() {
    use ErrorKind::*;
    let field = IntegerField::I64Const;
    for (payload, offset, kind) in [
        (&b"\x01\x7e"[..], 2, TruncatedGlobalMutability),
        (b"\x01\x7e\x02\x41", 2, InvalidGlobalMutability { byte: 2 }),
        (b"\x01\x7e\x01", 3, TruncatedByte { expected: 0x42 }),
        (
            b"\x01\x7e\x01\x41", // Reject the opcode, not its missing immediate.
            3,
            UnexpectedByte {
                expected: 0x42,
                found: 0x41,
            },
        ),
        (b"\x01\x7e\0\x42", 4, TruncatedInteger { field }),
        (b"\x01\x7e\0\x42\x80", 5, TruncatedInteger { field }),
        (b"\x01\x7e\0\x42\0", 5, TruncatedByte { expected: 0x0b }),
        (
            b"\x01\x7e\0\x42\0\x42\0\x0b",
            5,
            UnexpectedByte {
                expected: 0x0b,
                found: 0x42,
            },
        ),
        (b"\x01\x7e\0\x42\0\x0b\0", 6, TrailingSectionBytes { id: 6 }),
        (
            b"\x02\x7e\0\x42\x2a\x0b\x7e\0\x23\0\x0b", // Even a prior immutable i64.
            8,
            UnexpectedByte {
                expected: 0x42,
                found: 0x23,
            },
        ),
    ] {
        bad_payload(6, payload, offset, kind);
    }
    let mut payload = b"\x01\x7e\0\x42".to_vec();
    payload.extend([0x80; 9]);
    bad_payload(6, &payload, 13, TruncatedInteger { field });
    for invalid_tenth_byte in [0x01, 0x7e, 0x80] {
        let payload = [&payload[..], &[invalid_tenth_byte, 0, 0x0b]].concat();
        bad_payload(6, &payload, 13, IntegerOverflow { field });
    }
}

#[test]
fn reads_the_highest_declared_slot_and_preserves_signed_initial_values() {
    let mut globals = vec![0x88, 0]; // Eight globals, encoded as a padded uLEB32.
    for value in 0..7 {
        globals.extend([0x7f, 0, 0x41, value, 0x0b]);
    }
    for (encoding, value) in [
        (&[0x80, 0x80, 0x80, 0x80, 0x78][..], i32::MIN),
        (&[0xff, 0xff, 0xff, 0xff, 0x07], i32::MAX),
        (&[0xff, 0x7f], -1), // Padded negative sLEB32.
    ] {
        let mut declarations = globals.clone();
        declarations.extend([0x7f, 0, 0x41]);
        declarations.extend(encoding);
        declarations.push(0x0b);
        let body = [0, 0x23, 0x87, 0, 0x0b]; // Padded uLEB32 slot 7 from the first body.
        let validated = with_global_body(&declarations, &body);
        assert_eq!(
            validated.instantiate().unwrap().invoke_i32("answer"),
            Ok(value)
        );
    }
}

#[test]
fn mixed_globals_share_the_exact_cap_and_keep_both_words_of_i64_initializers() {
    let mut globals = vec![0x88, 0]; // Eight globals, encoded as a padded uLEB32.
    globals.extend([0x7f, 0, 0x41, 42, 0x0b].repeat(4));
    globals.extend([0x7e, 1, 0x42, 42, 0x0b].repeat(3));
    globals.extend([0x7e, 0, 0x42]);
    for (encoding, high, low) in [
        (
            &b"\x80\x80\x80\x80\x80\x80\x80\x80\x80\x7f"[..],
            i32::MIN,
            0,
        ),
        (b"\xff\xff\xff\xff\xff\xff\xff\xff\xff\0", i32::MAX, -1),
        (b"\xff\x7f", -1, -1), // Padded negative sLEB64.
    ] {
        let declarations = [&globals[..], encoding, &[0x0b]].concat();
        for (body, expected) in [
            (&b"\0\x23\x87\0\x42\x20\x88\xa7\x0b"[..], high), // high = (u64 >> 32) as i32
            (b"\0\x23\x87\0\xa7\x0b", low),
        ] {
            let validated = with_global_body(&declarations, body);
            assert_eq!(
                validated.instantiate().unwrap().invoke_i32("answer"),
                Ok(expected)
            );
        }
        for op in [0x23, 0x24] {
            bad_global_body(
                Some(&declarations),
                &[0, 0x00, op, 8, 0x0b],
                3,
                ErrorKind::InvalidIndex {
                    field: IntegerField::GlobalIndex,
                    index: 8,
                },
            );
        }
        let mut over_cap = declarations;
        over_cap[0] = 0x89; // Nine, with no ninth declaration.
        bad_payload(
            6,
            &over_cap,
            0,
            ErrorKind::UnsupportedCount {
                field: IntegerField::GlobalCount,
                count: 9,
            },
        );
    }
}

#[test]
fn validates_global_indices_and_mutability_in_all_bodies_and_arms() {
    use ErrorKind::*;
    let field = IntegerField::GlobalIndex;
    for opcode in [0x23, 0x24] {
        for globals in [None, Some(&[0][..])] {
            bad_global_body(
                globals,
                &[0, opcode, 0, 0x0b],
                2,
                InvalidIndex { field, index: 0 },
            );
        }
        bad_global_body(
            Some(MUTABLE),
            &[0, opcode, 1, 0x0b],
            2,
            InvalidIndex { field, index: 1 },
        );
        bad_global_body(
            Some(MUTABLE),
            &[0, opcode, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x0b],
            2,
            InvalidIndex {
                field,
                index: u32::MAX,
            },
        );
        bad_global_body(
            Some(MUTABLE),
            &[0, opcode, 0x80],
            3,
            TruncatedInteger { field },
        );
        bad_global_body(
            Some(MUTABLE),
            &[0, opcode, 0xff, 0xff, 0xff, 0xff, 0x10],
            6,
            IntegerOverflow { field },
        );
    }
    bad_global_body(
        Some(IMMUTABLE),
        &[0, 0x41, 1, 0x24, 0, 0x41, 0, 0x0b],
        4,
        ImmutableGlobal { index: 0 },
    );
    // False condition, so the attempted const store would never execute.
    bad_global_body(
        Some(IMMUTABLE),
        &[
            0, 0x41, 0, 0x04, 0x7f, 0x41, 1, 0x24, 0x80, 0, 0x41, 2, 0x05, 0x41, 3, 0x0b, 0x0b,
        ],
        8,
        ImmutableGlobal { index: 0 },
    );
}

#[test]
fn checks_global_types_and_mutability_in_live_and_dead_code() {
    use ErrorKind::*;
    let mixed = b"\x02\x7f\x01\x41\x2a\x0b\x7e\x01\x42\x2a\x0b";
    for head in [&b"\0"[..], b"\0\0"] {
        for (index, push, op, expected, actual) in
            [(0, 0x42, 0x50, I32, I64), (1, 0x41, 0x45, I64, I32)]
        {
            let set = [head, &[push, 0, 0x24, index, 0x0b]].concat();
            bad_global_body(
                Some(mixed),
                &set,
                head.len() + 2,
                TypeMismatch { expected, actual },
            );
            let get = [head, &[0x23, index, op, 0x0b]].concat();
            bad_global_body(
                Some(mixed),
                &get,
                head.len() + 2,
                TypeMismatch {
                    expected: actual,
                    actual: expected,
                },
            );
        }
        // Mutability wins over a missing or incorrect operand.
        for operand in [&b""[..], b"\x41\0"] {
            let body = [head, operand, b"\x24\x80\0\x0b"].concat();
            bad_global_body(
                Some(IMMUTABLE_I64),
                &body,
                head.len() + operand.len() + 1,
                ImmutableGlobal { index: 0 },
            );
        }
    }
}

#[test]
fn i64_global_set_respects_reachable_and_polymorphic_floors() {
    bad_global_body(
        Some(MUTABLE_I64),
        b"\0\x42\x2a\x02\x40\x24\0\x0b\xa7\x0b",
        5,
        ErrorKind::StackUnderflow { depth: 0 },
    );
    // The branch preserves an i64 prefix below the block floor. A skipped
    // set can consume an implicit (or unknown select) operand at that floor.
    let validated = with_global_body(
        MUTABLE_I64,
        b"\0\x42\x2a\x02\x40\x0c\0\x24\0\x1b\x24\0\x0b\xa7\x0b",
    );
    assert_eq!(
        validated.instantiate().unwrap().invoke_i32("answer"),
        Ok(42)
    );
}

#[test]
fn applies_global_stack_effects_at_the_active_floor_and_operand_cap() {
    use ErrorKind::*;
    bad_global_body(
        Some(MUTABLE),
        &[0, 0x24, 0, 0x0b],
        1,
        StackUnderflow { depth: 0 },
    );
    for (opener, ty) in [(0x02, 0x7f), (0x03, 0x40)] {
        bad_global_body(
            Some(MUTABLE),
            &[0, 0x41, 42, opener, ty, 0x24, 0, 0x0b, 0x0b],
            5,
            StackUnderflow { depth: 0 },
        );
    }
    bad_global_body(
        Some(MUTABLE),
        &[
            0, 0x41, 42, 0x41, 1, 0x04, 0x7f, 0x41, 0, 0x05, 0x24, 0, 0x41, 99, 0x0b, 0x0b,
        ],
        10,
        StackUnderflow { depth: 0 },
    );

    let limit = Limit::OperandStackDepth;
    for (globals, result_ops) in [(MUTABLE, &b"\x0b"[..]), (MUTABLE_I64, b"\xa7\x0b")] {
        let mut body = vec![0];
        body.extend([0x23, 0].repeat(limit.maximum()));
        let over_limit = body.len();
        let mut overflow = body.clone();
        overflow.extend([0x23, 0, 0x0b]);
        bad_global_body(
            Some(globals),
            &overflow,
            over_limit,
            LimitExceeded { limit },
        );
        body.extend([0x24, 0].repeat(limit.maximum() - 1));
        body.extend(result_ops);
        let validated = with_global_body(globals, &body);
        assert_eq!(
            validated.instantiate().unwrap().invoke_i32("answer"),
            Ok(42)
        );
    }
}

#[test]
fn both_global_instructions_count_toward_the_body_instruction_cap() {
    let limit = Limit::BodyInstructions;
    for (globals, eqz) in [(MUTABLE, 0x45), (MUTABLE_I64, 0x50)] {
        let mut body = vec![0, 0x23, 0];
        body.extend([0x23, 0, 0x24, 0].repeat((limit.maximum() - 3) / 2));
        body.extend([eqz, 0x45]);
        for opcode in [0x23, 0x24] {
            let mut overflow = body.clone();
            overflow.extend([opcode, 0, 0x0b]);
            bad_global_body(
                Some(globals),
                &overflow,
                body.len(),
                ErrorKind::LimitExceeded { limit },
            );
        }
        body.push(0x0b);
        let validated = with_global_body(globals, &body);
        assert_eq!(validated.instantiate().unwrap().invoke_i32("answer"), Ok(1));
    }
}

#[test]
fn shares_global_slots_across_calls_and_aliases_but_not_instances() {
    let mut globals = vec![8];
    globals.extend([0x7f, 0, 0x41, 42, 0x0b].repeat(7));
    globals.extend([0x7f, 1, 0x41, 3, 0x0b]);
    let exports = function_exports(&[(b"read", 0), (b"bump", 2), (b"alias", 2)]);
    let bodies = code_bodies(&[
        &[0, 0x23, 7, 0x0b],
        &[0, 0x23, 7, 0x20, 0, 0x6a, 0x24, 0x87, 0, 0x23, 7, 0x0b],
        &[0, 0x20, 0, 0x10, 1, 0x0b],
    ]);
    let validated = validate_module(&module(&[
        (1, &i32_types(&[0, 1])),
        (3, &[3, 0, 1, 1]),
        (6, &globals),
        (7, &exports),
        (10, &bodies),
    ]))
    .unwrap();
    let a = validated.instantiate().unwrap();
    let b = validated.instantiate().unwrap();
    assert_eq!(a.invoke_i32("read"), Ok(3));
    assert_eq!(a.invoke_i32_with_args("bump", &[5]), Ok(8));
    assert_eq!(a.invoke_i32("read"), Ok(8));
    assert_eq!(b.invoke_i32_with_args("alias", &[-7]), Ok(-4));
    assert_eq!(a.invoke_i32_with_args("alias", &[2]), Ok(10));
    assert_eq!(b.invoke_i32("read"), Ok(-4));
    assert_eq!(validated.instantiate().unwrap().invoke_i32("read"), Ok(3));
}

#[test]
fn full_width_global_writes_persist_through_start_calls_and_traps_only_in_that_instance() {
    let globals = [
        3, 0x7e, 0, 0x42, 0x80, 0x80, 0x80, 0x80, 0x10, 0x0b, // i64 1 << 32
        0x7f, 0, 0x41, 3, 0x0b, // i32 3
        0x7e, 1, 0x42, 5, 0x0b, // i64 5
    ];
    let types = b"\x03\x60\0\x01\x7f\x60\x01\x7f\0\x60\0\0";
    let exports = function_exports(&[(b"high", 0), (b"low", 1), (b"bump", 4), (b"trap", 5)]);
    let code = code_bodies(&[
        b"\0\x23\x02\x42\x20\x88\xa7\x0b", // Read high word.
        b"\0\x23\x02\xa7\x0b",             // Read low word.
        b"\0\x23\x02\x23\0\x7c\x23\x01\xac\x7c\x24\x02\x0b", // Start sets slot 2 = 5 + (1 << 32) + 3.
        b"\0\x23\x02\x20\0\xac\x42\x20\x86\x7c\x24\x82\0\x0b", // Callee adds (arg << 32), using a padded set index.
        b"\0\x20\0\x10\x03\x0b", // Forward the arg to the private callee.
        b"\0\x41\x7c\x10\x03\x00\x42\0\x24\x02\x0b", // Add (-4 << 32), trap, then skip the reset.
    ]);
    let validated = validate_module(&module(&[
        (1, types),
        (3, &[6, 0, 0, 2, 1, 1, 2]),
        (6, &globals),
        (7, &exports),
        (8, &[2]),
        (10, &code),
    ]))
    .unwrap();
    let a = validated.instantiate().unwrap();
    let b = validated.instantiate().unwrap();
    assert_eq!(a.invoke_i32("high"), Ok(1));
    assert_eq!(a.invoke_i32("low"), Ok(8));
    assert_eq!(a.invoke_void_with_args("bump", &[2]), Ok(()));
    assert_eq!(a.invoke_i32("high"), Ok(3));
    assert_eq!(
        a.invoke_void("trap"),
        Err(InvokeError::Trap(Trap::Unreachable))
    );
    assert_eq!(a.invoke_i32("high"), Ok(-1));
    assert_eq!(a.invoke_i32("low"), Ok(8));
    for independent in [b, validated.instantiate().unwrap()] {
        assert_eq!(independent.invoke_i32("high"), Ok(1));
        assert_eq!(independent.invoke_i32("low"), Ok(8));
    }
}

#[test]
fn completed_writes_survive_a_callee_trap_but_subsequent_writes_do_not_run() {
    let exports = function_exports(&[(b"read", 0), (b"trap", 2)]);
    let bodies = code_bodies(&[
        &[0, 0x23, 0, 0x0b],
        &[
            0, 0x41, 7, 0x24, 0, 0x41, 1, 0x41, 0, 0x6d, 0x24, 0, 0x23, 0, 0x0b,
        ],
        &[0, 0x10, 1, 0x24, 0, 0x23, 0, 0x0b],
    ]);
    let validated = validate_module(&module(&[
        (1, TYPE),
        (3, &[3, 0, 0, 0]),
        (6, MUTABLE),
        (7, &exports),
        (10, &bodies),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("read"), Ok(42));
    assert_eq!(
        instance.invoke_i32("trap"),
        Err(InvokeError::Trap(Trap::IntegerDivideByZero))
    );
    assert_eq!(instance.invoke_i32("read"), Ok(7));
    assert_eq!(validated.instantiate().unwrap().invoke_i32("read"), Ok(42));
}

#[test]
fn counts_global_dispatches_and_keeps_the_final_permitted_store() {
    let exports = function_exports(&[(b"read", 0), (b"loop", 1)]);
    for (globals, increment, read, initial) in [
        (MUTABLE, &b"\x41\x01\x6a"[..], &b"\0\x23\0\x0b"[..], 42),
        // Add 1 << 32 to the i64 value each time and read back the high word.
        (
            MUTABLE_I64,
            b"\x42\x80\x80\x80\x80\x10\x7c",
            b"\0\x23\0\x42\x20\x88\xa7\x0b",
            0,
        ),
    ] {
        let body = [
            &b"\0\x03\x40\x23\0"[..],
            increment,
            b"\x24\0\x41\x01\x0d\0\x0b\x41\0\x0b",
        ]
        .concat();
        let bodies = code_bodies(&[read, &body]);
        let validated = validate_module(&module(&[
            (1, TYPE),
            (3, TWO_FUNCTIONS),
            (6, globals),
            (7, &exports),
            (10, &bodies),
        ]))
        .unwrap();
        let instance = validated.instantiate().unwrap();
        let limit = ExecutionLimit::InstructionDispatches;
        assert_eq!(
            instance.invoke_i32("loop"),
            Err(InvokeError::LimitExceeded { limit })
        );
        // 1024 dispatches complete 170 six-instruction iterations plus the first
        // four instructions of the next iteration, ending on global.set.
        assert_eq!(instance.invoke_i32("read"), Ok(initial + 171));
    }
}

#[test]
fn global_and_active_data_initialization_coexist_without_changing_data_order() {
    let globals = &[1, 0x7f, 0, 0x41, 42, 0x0b];
    let code = code_body(&[0, 0x23, 0, 0x2d, 0, 0, 0x0b]);
    let data = &[2, 0, 0x41, 42, 0x0b, 1, 11, 0, 0x41, 42, 0x0b, 1, 99];
    let validated = validate_module(&module(&[
        (1, TYPE),
        (3, FUNCTION),
        (5, &[1, 0, 1]),
        (6, globals),
        (7, EXPORT),
        (10, &code),
        (11, data),
    ]))
    .unwrap();
    assert_eq!(
        validated.instantiate().unwrap().invoke_i32("answer"),
        Ok(99)
    );
}
