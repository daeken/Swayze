use swayze_wasm::{
    Error, ErrorKind, IntegerField, InvokeError, LengthField, Limit, validate_module,
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
        (1, &[1, 0x60], ParameterCount, &[1]),
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
        (10, &[1, 4, 0, 0x42, 42, 0x0b], 3, 0x41),
        (10, &[1, 4, 0, 0x41, 42, 0x01], 5, 0x0b),
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
    for count in 0..=4 {
        let body = [&[0, 0x41][..], &[0x80; 4][..count]].concat();
        // The apparent terminating LEB byte is in the section, but not in this body.
        let mut code = vec![1, u8::try_from(body.len()).unwrap()];
        code.extend(body);
        let boundary = code.len();
        code.extend([0, 0x0b]);
        bad_payload(
            10,
            &code,
            boundary,
            ErrorKind::TruncatedInteger { field: I32Const },
        );
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
