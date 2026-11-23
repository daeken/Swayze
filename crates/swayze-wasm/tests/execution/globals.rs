use super::{
    EXPORT, ErrorKind, ExecutionLimit, FUNCTION, HEADER, IntegerField, InvokeError, Limit, Module,
    TWO_FUNCTIONS, TYPE, Trap, bad_payload, code_bodies, code_body, expect_error, function_exports,
    i32_types, module, push_section, validate_module,
};

const MUTABLE: &[u8] = &[1, 0x7f, 1, 0x41, 42, 0x0b];
const IMMUTABLE: &[u8] = &[1, 0x7f, 0, 0x41, 42, 0x0b];

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

    let bytes = module(&[(2, b"\x01\x01m\x01g\x03\x7f\0"), (6, IMMUTABLE)]);
    expect_error(
        &bytes,
        HEADER.len(),
        ErrorKind::UnsupportedSection { id: 2 },
    );
    let mut bytes = module(&[(6, IMMUTABLE)]);
    let start = push_section(&mut bytes, 7, b"\x01\x01g\x03\0");
    expect_error(
        &bytes,
        start + 3,
        ErrorKind::UnexpectedByte {
            expected: 0,
            found: 3,
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
            &[1, 0x7e],
            1,
            UnexpectedByte {
                expected: 0x7f,
                found: 0x7e,
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
    let mut body = vec![0];
    body.extend([0x23, 0].repeat(limit.maximum()));
    let over_limit = body.len();
    let mut overflow = body.clone();
    overflow.extend([0x23, 0, 0x0b]);
    bad_global_body(
        Some(IMMUTABLE),
        &overflow,
        over_limit,
        LimitExceeded { limit },
    );
    body.extend([0x24, 0].repeat(limit.maximum() - 1));
    body.push(0x0b);
    let validated = with_global_body(MUTABLE, &body);
    assert_eq!(
        validated.instantiate().unwrap().invoke_i32("answer"),
        Ok(42)
    );
}

#[test]
fn both_global_instructions_count_toward_the_body_instruction_cap() {
    let limit = Limit::BodyInstructions;
    let mut body = vec![0, 0x23, 0];
    body.extend([0x23, 0, 0x24, 0].repeat((limit.maximum() - 3) / 2));
    body.extend([0x45, 0x45]);
    for opcode in [0x23, 0x24] {
        let mut overflow = body.clone();
        overflow.extend([opcode, 0, 0x0b]);
        bad_global_body(
            Some(MUTABLE),
            &overflow,
            body.len(),
            ErrorKind::LimitExceeded { limit },
        );
    }
    body.push(0x0b);
    let validated = with_global_body(MUTABLE, &body);
    assert_eq!(validated.instantiate().unwrap().invoke_i32("answer"), Ok(1));
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
    let bodies = code_bodies(&[
        &[0, 0x23, 0, 0x0b],
        &[
            0, 0x03, 0x40, 0x23, 0, 0x41, 1, 0x6a, 0x24, 0, 0x41, 1, 0x0d, 0, 0x0b, 0x41, 0, 0x0b,
        ],
    ]);
    let validated = validate_module(&module(&[
        (1, TYPE),
        (3, TWO_FUNCTIONS),
        (6, MUTABLE),
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
    assert_eq!(instance.invoke_i32("read"), Ok(42 + 171));
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
