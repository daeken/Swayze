use swayze_wasm::{
    Error, ErrorKind, IntegerField, InvokeError, LengthField, Limit, Module, validate_module,
};

const HEADER: &[u8] = b"\0asm\x01\0\0\0";
const TYPE: &[u8] = &[1, 0x60, 0, 1, 0x7f];
const FUNCTION: &[u8] = &[1, 0];
const EXPORT: &[u8] = b"\x01\x06answer\0\0";
const CODE: &[u8] = &[1, 4, 0, 0x41, 42, 0x0b];
const SECTIONS: [(u8, &[u8]); 4] = [(1, TYPE), (3, FUNCTION), (7, EXPORT), (10, CODE)];

fn push_u32(bytes: &mut Vec<u8>, n: usize) {
    let mut n = u32::try_from(n).unwrap();
    while n >= 128 {
        bytes.push((n & 0x7f) as u8 | 0x80);
        n >>= 7;
    }
    bytes.push(n as u8);
}

fn push_section(bytes: &mut Vec<u8>, id: u8, payload: &[u8]) -> usize {
    bytes.push(id);
    push_u32(bytes, payload.len());
    let payload_start = bytes.len();
    bytes.extend_from_slice(payload);
    payload_start
}

fn module(sections: &[(u8, &[u8])]) -> Vec<u8> {
    let mut bytes = HEADER.to_vec();
    for &(id, payload) in sections {
        push_section(&mut bytes, id, payload);
    }
    bytes
}

fn expect_error(bytes: &[u8], offset: usize, kind: ErrorKind) {
    assert_eq!(validate_module(bytes).unwrap_err(), Error { offset, kind });
}

fn bad_payload(id: u8, payload: &[u8], index: usize, kind: ErrorKind) {
    let mut bytes = HEADER.to_vec();
    let start = push_section(&mut bytes, id, payload);
    // A malformed section must not read through its boundary into the next ID.
    push_section(&mut bytes, 0, &[0]);
    expect_error(&bytes, start + index, kind);
}

#[test]
fn instantiates_and_invokes_only_the_exact_owned_export() {
    let validated = {
        let mut bytes = HEADER.to_vec();
        for (id, payload) in SECTIONS {
            push_section(&mut bytes, 0, b"\x04name\xff");
            push_section(&mut bytes, id, payload);
        }
        push_section(&mut bytes, 0, &[0]);
        validate_module(&bytes).unwrap()
    };
    let instance = validated.instantiate();
    assert_eq!(instance.invoke_i32("answer"), Ok(42));
    assert_eq!(validated.instantiate().invoke_i32("answer"), Ok(42));
    for name in ["", "Answer", "answe", "answer\0"] {
        assert_eq!(instance.invoke_i32(name), Err(InvokeError::ExportNotFound));
    }
}

#[test]
fn accepts_empty_and_custom_only_modules_and_functions_without_exports() {
    for sections in [
        &[][..],
        &[(0, &[0][..])],
        &[(1, TYPE), (3, FUNCTION), (10, CODE)],
        &[(1, TYPE), (3, FUNCTION), (7, &[0]), (10, CODE)],
    ] {
        let validated = validate_module(&module(sections)).unwrap();
        assert_eq!(
            validated.instantiate().invoke_i32(""),
            Err(InvokeError::ExportNotFound)
        );
        assert_eq!(
            validated.instantiate().invoke_i32("answer"),
            Err(InvokeError::ExportNotFound)
        );
    }
}

#[test]
fn validates_export_name_utf8_and_its_inclusive_limit() {
    let limit = Limit::ExportNameBytes;
    for name in [
        "".to_owned(),
        "\0é𝄞".to_owned(),
        "x".repeat(limit.maximum()),
    ] {
        let mut export = vec![1];
        push_u32(&mut export, name.len());
        export.extend_from_slice(name.as_bytes());
        export.extend([0, 0]);
        let bytes = module(&[(1, TYPE), (3, FUNCTION), (7, &export), (10, CODE)]);
        assert_eq!(
            validate_module(&bytes)
                .unwrap()
                .instantiate()
                .invoke_i32(&name),
            Ok(42)
        );
    }
    let mut too_long = vec![1];
    push_u32(&mut too_long, limit.maximum() + 1);
    bad_payload(7, &too_long, 1, ErrorKind::LimitExceeded { limit });
    bad_payload(7, b"\x01\x03x\xe2\x82\xac\0", 3, ErrorKind::InvalidUtf8Name);
    bad_payload(
        7,
        b"\x01\x03x",
        3,
        ErrorKind::TruncatedName {
            declared: 3,
            available: 1,
        },
    );
}

fn code_body(body: &[u8]) -> Vec<u8> {
    let mut code = vec![1];
    push_u32(&mut code, body.len());
    code.extend_from_slice(body);
    code
}

fn i32_type(parameter_count: u8) -> Vec<u8> {
    let mut ty = vec![1, 0x60, parameter_count];
    ty.extend(std::iter::repeat_n(0x7f, usize::from(parameter_count)));
    ty.extend([1, 0x7f]);
    ty
}

fn exported_function(ty: &[u8], code: &[u8]) -> Module {
    // Drop the original input before instantiation or invocation.
    validate_module(&module(&[(1, ty), (3, FUNCTION), (7, EXPORT), (10, code)])).unwrap()
}

fn assert_body_result(body: &[u8], expected: i32) {
    let validated = exported_function(TYPE, &code_body(body));
    let instance = validated.instantiate();
    assert_eq!(instance.invoke_i32("answer"), Ok(expected));
    assert_eq!(instance.invoke_i32("answer"), Ok(expected));
}

fn bad_typed_code(ty: &[u8], code: &[u8], offset: usize, kind: ErrorKind) {
    let mut bytes = module(&[(1, ty), (3, FUNCTION), (7, EXPORT)]);
    let start = push_section(&mut bytes, 10, code);
    push_section(&mut bytes, 0, &[0]);
    expect_error(&bytes, start + offset, kind);
}

fn bad_code(code: &[u8], offset: usize, kind: ErrorKind) {
    bad_typed_code(TYPE, code, offset, kind);
}

fn bad_typed_body(ty: &[u8], body: &[u8], offset: usize, kind: ErrorKind) {
    let code = code_body(body);
    bad_typed_code(ty, &code, code.len() - body.len() + offset, kind);
}

fn bad_body(body: &[u8], offset: usize, kind: ErrorKind) {
    bad_typed_body(TYPE, body, offset, kind);
}

#[test]
fn reads_four_parameters_in_declared_order_on_each_invocation() {
    // (p2 - p0) * p3 + p1
    let code = code_body(&[
        0, 0x20, 2, 0x20, 0, 0x6b, 0x20, 3, 0x6c, 0x20, 1, 0x6a, 0x0b,
    ]);
    let validated = exported_function(&i32_type(4), &code);
    let instance = validated.instantiate();
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[5, 7, 9, 11]),
        Ok(51)
    );
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[-5, 7, 9, -2]),
        Ok(-21)
    );
}

#[test]
fn checks_export_name_then_exact_declared_arity_even_for_unused_parameters() {
    let validated = exported_function(&i32_type(2), CODE);
    let instance = validated.instantiate();
    assert_eq!(
        instance.invoke_i32_with_args("Answer", &[]),
        Err(InvokeError::ExportNotFound)
    );
    assert_eq!(
        instance.invoke_i32("answer"),
        Err(InvokeError::ArityMismatch {
            expected: 2,
            actual: 0
        })
    );
    for args in [&[][..], &[1], &[1, 2, 3], &[1, 2, 3, 4, 5]] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", args),
            Err(InvokeError::ArityMismatch {
                expected: 2,
                actual: args.len()
            })
        );
    }
    assert_eq!(instance.invoke_i32_with_args("answer", &[1, 2]), Ok(42));

    let zero = validate_module(&module(&SECTIONS)).unwrap();
    assert_eq!(
        zero.instantiate().invoke_i32_with_args("answer", &[]),
        Ok(42)
    );
    assert_eq!(
        zero.instantiate().invoke_i32_with_args("answer", &[1]),
        Err(InvokeError::ArityMismatch {
            expected: 0,
            actual: 1
        })
    );
}

#[test]
fn parameter_arithmetic_wraps() {
    for (lhs, rhs, opcode, expected) in [
        (i32::MAX, 1, 0x6a, i32::MIN),
        (i32::MIN, 1, 0x6b, i32::MAX),
        (i32::MIN, -1, 0x6c, i32::MIN),
    ] {
        let code = code_body(&[0, 0x20, 0, 0x20, 1, opcode, 0x0b]);
        let validated = exported_function(&i32_type(2), &code);
        assert_eq!(
            validated
                .instantiate()
                .invoke_i32_with_args("answer", &[lhs, rhs]),
            Ok(expected)
        );
    }
}

#[test]
fn accepts_a_maximally_padded_local_index() {
    let code = code_body(&[0, 0x20, 0x82, 0x80, 0x80, 0x80, 0, 0x0b]);
    let validated = exported_function(&i32_type(3), &code);
    assert_eq!(
        validated
            .instantiate()
            .invoke_i32_with_args("answer", &[9, 4, -13]),
        Ok(-13)
    );
}

#[test]
fn rejects_local_indices_outside_the_parameter_range() {
    for (parameter_count, leb, index) in [
        (0, &[0][..], 0),
        (1, &[1], 1),
        (4, &[4], 4),
        (4, &[0x7f], 127),
        (4, &[0xff, 0xff, 0xff, 0xff, 0x0f], u32::MAX),
    ] {
        let body = [&[0, 0x20][..], leb, &[0x0b]].concat();
        bad_typed_body(
            &i32_type(parameter_count),
            &body,
            2,
            ErrorKind::InvalidIndex {
                field: IntegerField::LocalIndex,
                index,
            },
        );
    }
}

#[test]
fn rejects_local_index_leb_overflow() {
    for fifth in [0x10, 0x7f, 0x80] {
        let body = [0, 0x20, 0x80, 0x80, 0x80, 0x80, fifth, 0, 0x0b];
        bad_typed_body(
            &i32_type(1),
            &body,
            6,
            ErrorKind::IntegerOverflow {
                field: IntegerField::LocalIndex,
            },
        );
    }
}

#[test]
fn parameters_reach_the_operand_stack_only_through_local_get() {
    let ty = i32_type(2);
    for (body, offset, kind) in [
        (
            &[0, 0x6a, 0x0b][..],
            1,
            ErrorKind::StackUnderflow { depth: 0 },
        ),
        (
            &[0, 0x20, 0, 0x6a, 0x0b],
            3,
            ErrorKind::StackUnderflow { depth: 1 },
        ),
        (
            &[0, 0x20, 0, 0x20, 1, 0x0b],
            5,
            ErrorKind::InvalidResultDepth { depth: 2 },
        ),
    ] {
        bad_typed_body(&ty, body, offset, kind);
    }
}

#[test]
fn executes_mixed_arithmetic_in_stack_order() {
    // 7 * (3 - 9) + 11. The immediate 11 is itself spelled 0x0b, like `end`.
    assert_body_result(
        &[
            0, 0x41, 7, 0x41, 3, 0x41, 9, 0x6b, 0x6c, 0x41, 0x0b, 0x6a, 0x0b,
        ],
        -31,
    );
}

#[test]
fn wraps_all_three_arithmetic_operations() {
    let min = [0x41, 0x80, 0x80, 0x80, 0x80, 0x78];
    let max = [0x41, 0xff, 0xff, 0xff, 0xff, 0x07];
    for (lhs, rhs, opcode, expected) in [
        (max, 1, 0x6a, i32::MIN),    // MAX + 1
        (min, 1, 0x6b, i32::MAX),    // MIN - 1
        (min, 0x7f, 0x6c, i32::MIN), // MIN * -1
    ] {
        assert_body_result(
            &[&[0][..], &lhs, &[0x41, rhs, opcode, 0x0b]].concat(),
            expected,
        );
    }
}

#[test]
fn rejects_every_opcode_outside_the_slice() {
    for opcode in 0..=u8::MAX {
        if ![0x0b, 0x20, 0x41, 0x6a, 0x6b, 0x6c].contains(&opcode) {
            bad_body(
                &[0, 0x41, 3, opcode, 0x0b],
                3,
                ErrorKind::UnsupportedOpcode { opcode },
            );
        }
    }
}

#[test]
fn requires_two_operands_for_each_binary_op_and_one_result_at_end() {
    for opcode in [0x6a, 0x6b, 0x6c] {
        bad_body(
            &[0, opcode, 0x0b],
            1,
            ErrorKind::StackUnderflow { depth: 0 },
        );
        bad_body(
            &[0, 0x41, 1, opcode, 0x0b],
            3,
            ErrorKind::StackUnderflow { depth: 1 },
        );
        // A reduction may leave only one operand, too.
        bad_body(
            &[0, 0x41, 1, 0x41, 2, 0x6a, opcode, 0x0b],
            6,
            ErrorKind::StackUnderflow { depth: 1 },
        );
    }
    bad_body(&[0, 0x0b], 1, ErrorKind::InvalidResultDepth { depth: 0 });
    bad_body(
        &[0, 0x41, 1, 0x41, 2, 0x0b],
        5,
        ErrorKind::InvalidResultDepth { depth: 2 },
    );
}

#[test]
fn bounds_body_bytes_inclusively() {
    let limit = Limit::CodeBodyBytes;
    let mut body = vec![0, 0x41, 0];
    // Each addition costs seven bytes with a maximally padded but legal signed LEB32.
    for _ in 0..36 {
        body.extend([0x41, 0x81, 0x80, 0x80, 0x80, 0, 0x6a]);
    }
    body.push(0x0b);
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 36);

    // Change only the encoding of the first immediate, not the instruction count or values.
    body.splice(2..3, [0x80, 0]);
    assert_eq!(body.len(), limit.maximum() + 1);
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });
    // The bound also applies before trying to frame an unavailable body.
    let mut length_only = vec![1];
    push_u32(&mut length_only, body.len());
    bad_code(&length_only, 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn bounds_instruction_count_inclusively() {
    let limit = Limit::BodyInstructions;
    let pairs = (limit.maximum() - 1) / 2;
    assert_eq!(1 + 2 * pairs, limit.maximum());
    let ty = i32_type(1);
    let mut body = vec![0, 0x41, 0];
    for _ in 0..pairs {
        body.extend([0x20, 0, 0x6a]);
    }
    body.push(0x0b);
    let validated = exported_function(&ty, &code_body(&body));
    assert_eq!(
        validated.instantiate().invoke_i32_with_args("answer", &[1]),
        Ok(pairs as i32)
    );

    // A balanced program needs an odd number of non-end instructions. A further
    // pair would preserve that shape but exceeds the budget at its first opcode.
    let extra = body.len() - 1;
    body.splice(extra..extra, [0x20, 0, 0x6a]);
    bad_typed_body(&ty, &body, extra, ErrorKind::LimitExceeded { limit });
}

#[test]
fn bounds_operand_depth_inclusively() {
    let limit = Limit::OperandStackDepth;
    let ty = i32_type(1);
    let body_at_depth = |depth: usize| {
        let mut body = vec![0];
        for _ in 0..depth {
            body.extend([0x20, 0]);
        }
        body.extend(std::iter::repeat_n(0x6a, depth - 1));
        body.push(0x0b);
        body
    };
    let validated = exported_function(&ty, &code_body(&body_at_depth(limit.maximum())));
    assert_eq!(
        validated.instantiate().invoke_i32_with_args("answer", &[1]),
        Ok(limit.maximum() as i32)
    );
    bad_typed_body(
        &ty,
        &body_at_depth(limit.maximum() + 1),
        1 + 2 * limit.maximum(),
        ErrorKind::LimitExceeded { limit },
    );
}

fn code_immediate(leb: &[u8]) -> Vec<u8> {
    let mut code = vec![1, u8::try_from(leb.len() + 3).unwrap(), 0, 0x41];
    code.extend_from_slice(leb);
    code.push(0x0b);
    code
}

#[test]
fn decodes_signed_boundaries_and_legal_padding() {
    let cases: &[(&[u8], i32)] = &[
        (&[0], 0),
        (&[0x3f], 63),
        (&[0x40], -64),
        (&[0x7f], -1),
        (&[0xc0, 0], 64),
        (&[0xbf, 0x7f], -65),
        (&[0x80, 0xc0, 0x00], 8192),
        (&[0xff, 0xff, 0xbf, 0x7f], -1_048_577),
        (&[0x81, 0x80, 0x80, 0x80, 0], 1),
        (&[0xff, 0xff, 0xff, 0xff, 0x7f], -1),
        (&[0x80, 0x80, 0x80, 0x80, 0x78], i32::MIN),
        (&[0xff, 0xff, 0xff, 0xff, 0x07], i32::MAX),
    ];
    for &(leb, expected) in cases {
        let bytes = module(&[
            (1, TYPE),
            (3, FUNCTION),
            (7, EXPORT),
            (10, &code_immediate(leb)),
        ]);
        assert_eq!(
            validate_module(&bytes)
                .unwrap()
                .instantiate()
                .invoke_i32("answer"),
            Ok(expected)
        );
    }
}

#[test]
fn accepts_legal_padding_in_counts_indices_and_nested_lengths() {
    let zero = [0x80, 0x80, 0x80, 0x80, 0];
    let one = [0x81, 0x80, 0x80, 0x80, 0];
    let ty = [&one[..], &[0x60], &zero, &one, &[0x7f]].concat();
    let function = [&one[..], &zero].concat();
    let export = [&one[..], &zero, &[0], &zero].concat();
    let code = [
        &one[..],
        &[0x88, 0x80, 0x80, 0x80, 0],
        &zero,
        &[0x41, 42, 0x0b],
    ]
    .concat();
    let bytes = module(&[(1, &ty), (3, &function), (7, &export), (10, &code)]);
    assert_eq!(
        validate_module(&bytes)
            .unwrap()
            .instantiate()
            .invoke_i32(""),
        Ok(42)
    );
}

#[test]
fn rejects_signed_leb_overflow_and_continuation_in_the_fifth_byte() {
    for fifth in [0x08, 0x0f, 0x10, 0x70, 0x77, 0x80, 0xf8, 0xff] {
        // Even with a terminating sixth byte, the error is at byte five.
        let code = code_immediate(&[0x80, 0x80, 0x80, 0x80, fifth, 0]);
        bad_payload(
            10,
            &code,
            8,
            ErrorKind::IntegerOverflow {
                field: IntegerField::I32Const,
            },
        );
    }
}

#[test]
fn rejects_unsigned_leb_overflow_and_large_in_range_indices() {
    for fifth in [0x10, 0x7f, 0x80] {
        let leb = [0x80, 0x80, 0x80, 0x80, fifth, 0];
        let index = [&[1][..], &leb].concat();
        let cases = [
            (1, &leb[..], 4, IntegerField::TypeCount),
            (3, &index[..], 5, IntegerField::TypeIndex),
        ];
        for (id, payload, offset, field) in cases {
            bad_payload(id, payload, offset, ErrorKind::IntegerOverflow { field });
        }
        bad_payload(
            10,
            &index,
            5,
            ErrorKind::LengthOverflow {
                field: LengthField::CodeBodySize,
            },
        );
    }
    bad_payload(
        3,
        &[1, 0xff, 0xff, 0xff, 0xff, 0x0f],
        1,
        ErrorKind::InvalidIndex {
            field: IntegerField::TypeIndex,
            index: u32::MAX,
        },
    );
    bad_payload(
        7,
        &[1, 0, 0, 1],
        3,
        ErrorKind::InvalidIndex {
            field: IntegerField::FunctionIndex,
            index: 1,
        },
    );
}

#[test]
fn requires_singleton_type_function_code_and_at_most_one_export() {
    use IntegerField::*;
    for (id, prefix, field, bad_counts) in [
        (1, &[][..], TypeCount, &[0, 2][..]),
        (1, &[1, 0x60], ParameterCount, &[5]),
        (1, &[1, 0x60, 0], ResultCount, &[0, 2]),
        (3, &[], FunctionCount, &[0, 2]),
        (7, &[], ExportCount, &[2]),
        (10, &[], CodeCount, &[0, 2]),
        (10, &[1, 3], LocalGroupCount, &[1]),
    ] {
        for &count in bad_counts {
            let mut payload = [prefix, &[count]].concat();
            if field == LocalGroupCount {
                payload.extend([0, 0x7f]); // Even a declared zero-count local group is outside the slice.
            }
            bad_payload(
                id,
                &payload,
                prefix.len(),
                ErrorKind::UnsupportedCount {
                    field,
                    count: u32::from(count),
                },
            );
        }
    }
    for (id, payload, index, expected) in [
        (1, &[1, 0x61, 0, 1, 0x7f][..], 1, 0x60),
        (1, &[1, 0x60, 0, 1, 0x7e], 4, 0x7f),
        (7, &[1, 0, 1, 0], 2, 0),
    ] {
        bad_payload(
            id,
            payload,
            index,
            ErrorKind::UnexpectedByte {
                expected,
                found: payload[index],
            },
        );
    }
}

#[test]
fn rejects_malformed_parameter_type_vectors() {
    use IntegerField::ParameterCount;
    let mut ty = vec![1, 0x60];
    push_u32(&mut ty, u32::MAX as usize);
    bad_payload(
        1,
        &ty,
        2,
        ErrorKind::UnsupportedCount {
            field: ParameterCount,
            count: u32::MAX,
        },
    );
    bad_payload(
        1,
        &[1, 0x60, 0x80],
        3,
        ErrorKind::TruncatedInteger {
            field: ParameterCount,
        },
    );
    bad_payload(
        1,
        &[1, 0x60, 0x80, 0x80, 0x80, 0x80, 0x10],
        6,
        ErrorKind::IntegerOverflow {
            field: ParameterCount,
        },
    );
    bad_payload(
        1,
        &[1, 0x60, 2, 0x7f],
        4,
        ErrorKind::TruncatedByte { expected: 0x7f },
    );
    bad_payload(
        1,
        &[1, 0x60, 2, 0x7f, 0x7e, 1, 0x7f],
        4,
        ErrorKind::UnexpectedByte {
            expected: 0x7f,
            found: 0x7e,
        },
    );
}

#[test]
fn requires_each_standard_section_once_in_order_except_optional_export() {
    for missing in [1, 3, 10] {
        let sections: Vec<_> = SECTIONS
            .into_iter()
            .filter(|&(id, _)| id != missing)
            .collect();
        let bytes = module(&sections);
        expect_error(
            &bytes,
            bytes.len(),
            ErrorKind::MissingSection { id: missing },
        );
    }
    for (i, (id, payload)) in SECTIONS.into_iter().enumerate() {
        let mut bytes = module(&SECTIONS[..=i]);
        push_section(&mut bytes, 0, &[0]);
        let duplicate = bytes.len();
        push_section(&mut bytes, id, payload);
        expect_error(&bytes, duplicate, ErrorKind::SectionOutOfOrder { id });
    }
    for i in 0..SECTIONS.len() - 1 {
        let mut sections = SECTIONS;
        sections.swap(i, i + 1);
        let offset = module(&sections[..=i]).len();
        expect_error(
            &module(&sections),
            offset,
            ErrorKind::SectionOutOfOrder {
                id: sections[i + 1].0,
            },
        );
    }
}

#[test]
fn rejects_every_proper_prefix_of_the_demo_except_the_valid_empty_module() {
    let bytes = module(&SECTIONS);
    for end in 0..bytes.len() {
        if end != HEADER.len() {
            assert!(
                validate_module(&bytes[..end]).is_err(),
                "accepted {end}-byte prefix"
            );
        }
    }
}

#[test]
fn respects_section_and_body_boundaries_and_rejects_leftover_bytes() {
    use IntegerField::*;
    for (opcode, field) in [(0x41, I32Const), (0x20, LocalIndex)] {
        for count in 0..=4 {
            let body = [&[0, opcode][..], &[0x80; 4][..count]].concat();
            let mut code = code_body(&body);
            // The apparent terminating LEB byte is in the section, but not in this body.
            let boundary = code.len();
            code.extend([0, 0x0b]);
            bad_typed_code(
                &i32_type(1),
                &code,
                boundary,
                ErrorKind::TruncatedInteger { field },
            );
        }
    }
    bad_payload(
        3,
        &[1, 0x80],
        2,
        ErrorKind::TruncatedInteger { field: TypeIndex },
    );
    bad_payload(
        10,
        &[1, 0x80],
        2,
        ErrorKind::TruncatedLength {
            field: LengthField::CodeBodySize,
        },
    );
    bad_payload(
        10,
        &[1, 5, 0, 0x41, 42, 0x0b],
        6,
        ErrorKind::TruncatedBody {
            declared: 5,
            available: 4,
        },
    );
    bad_payload(
        10,
        &[1, 3, 0, 0x41, 42, 0x0b],
        5,
        ErrorKind::TruncatedByte { expected: 0x0b },
    );
    bad_payload(
        10,
        &[1, 5, 0, 0x41, 42, 0x0b, 0x0b],
        6,
        ErrorKind::TrailingBodyBytes,
    );
    // A second balance-preserving operation cannot continue past the first end.
    bad_body(
        &[0, 0x41, 40, 0x41, 2, 0x6a, 0x0b, 0x41, 1, 0x6b, 0x0b],
        7,
        ErrorKind::TrailingBodyBytes,
    );
    for (id, payload) in SECTIONS {
        let trailing = [payload, &[0]].concat();
        bad_payload(
            id,
            &trailing,
            payload.len(),
            ErrorKind::TrailingSectionBytes { id },
        );
    }
}

#[test]
fn rejects_invalid_suffixes_even_after_a_complete_valid_function() {
    let complete = module(&SECTIONS);
    for id in [2, 5, 8] {
        // Imports, memory, and start never reach instantiation.
        let mut bytes = complete.clone();
        push_section(&mut bytes, id, &[0]);
        expect_error(&bytes, complete.len(), ErrorKind::UnsupportedSection { id });
    }
    let mut bytes = complete.clone();
    let custom = push_section(&mut bytes, 0, &[1, 0xff]);
    expect_error(&bytes, custom + 1, ErrorKind::InvalidUtf8Name);
    expect_error(
        &[complete.as_slice(), &[0xff]].concat(),
        complete.len(),
        ErrorKind::UnknownSectionId { id: 0xff },
    );
}
