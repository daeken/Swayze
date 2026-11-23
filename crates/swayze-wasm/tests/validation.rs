use swayze_wasm::{Error, ErrorKind, LengthField, Limit, Unsupported, prepare, validate_module};

const HEADER: &[u8; 8] = b"\0asm\x01\0\0\0";

fn module_with_suffix(suffix: &[u8]) -> Vec<u8> {
    [HEADER.as_slice(), suffix].concat()
}

fn expect_error(bytes: &[u8], offset: usize, kind: ErrorKind) {
    assert_eq!(validate_module(bytes).unwrap_err(), Error { offset, kind });
}

fn push_length(bytes: &mut Vec<u8>, length: usize) {
    let mut length = u32::try_from(length).unwrap();
    while length >= 128 {
        bytes.push((length & 0x7f) as u8 | 0x80);
        length >>= 7;
    }
    bytes.push(length as u8);
}

fn push_section(bytes: &mut Vec<u8>, payload: &[u8]) {
    bytes.push(0);
    push_length(bytes, payload.len());
    bytes.extend_from_slice(payload);
}

fn push_custom(bytes: &mut Vec<u8>, name: &[u8], body: &[u8]) {
    let mut payload = Vec::new();
    push_length(&mut payload, name.len());
    payload.extend_from_slice(name);
    payload.extend_from_slice(body);
    push_section(bytes, &payload);
}

#[test]
fn validates_empty_module_without_enabling_prepare() {
    let bytes = *HEADER;
    let _module = validate_module(&bytes).unwrap();
    assert_eq!(prepare(&bytes), Err(Unsupported));
}

#[test]
fn accepts_repeated_custom_names_and_ignores_opaque_payloads() {
    let mut bytes = HEADER.to_vec();
    push_custom(&mut bytes, b"", b"");
    push_custom(&mut bytes, "\0é𝄞".as_bytes(), b"\xff\x0b\x00");
    // Even a well-known custom section name does not select a decoder.
    push_custom(&mut bytes, b"name", b"\xff");
    push_custom(&mut bytes, b"name", b"\x80");
    let _module = validate_module(&bytes).unwrap();
    drop(bytes); // Module does not retain the input.
}

#[test]
fn accepts_five_byte_padded_sizes() {
    let bytes = module_with_suffix(&[
        0, 0x86, 0x80, 0x80, 0x80, 0, // size 6
        0x81, 0x80, 0x80, 0x80, 0, b'x', // one-byte name
    ]);
    validate_module(&bytes).unwrap();
}

#[test]
fn distinguishes_bad_or_incomplete_magic_from_unsupported_version() {
    for end in 0..HEADER.len() {
        expect_error(&HEADER[..end], end, ErrorKind::TruncatedHeader);
    }
    for offset in 0..4 {
        let mut header = *HEADER;
        header[offset] ^= 0xff;
        expect_error(&header, offset, ErrorKind::InvalidMagic);
    }
    for version in [0u32, 2, 0x0100_0000, u32::MAX] {
        let mut header = *HEADER;
        header[4..].copy_from_slice(&version.to_le_bytes());
        expect_error(&header, 4, ErrorKind::UnsupportedVersion { version });
    }
}

#[test]
fn rejects_even_empty_vectors_in_unsupported_sections() {
    for id in [2, 8, 12, 13] {
        let bytes = module_with_suffix(&[0, 1, 0, id, 1, 0]);
        expect_error(&bytes, 11, ErrorKind::UnsupportedSection { id });
    }
}

#[test]
fn rejects_unknown_ids_before_size() {
    for id in [14, 0x80, 0xff] {
        let bytes = module_with_suffix(&[0, 1, 0, id]);
        expect_error(&bytes, 11, ErrorKind::UnknownSectionId { id });
    }
}

#[test]
fn reports_incomplete_lengths_at_their_frame_boundary() {
    for count in 0..=4 {
        let unfinished = &[0x80; 4][..count];
        let mut section = module_with_suffix(&[0]);
        section.extend_from_slice(unfinished);
        expect_error(
            &section,
            9 + count,
            ErrorKind::TruncatedLength {
                field: LengthField::SectionSize,
            },
        );

        let mut custom = HEADER.to_vec();
        push_section(&mut custom, unfinished);
        let next_section = custom.len();
        push_custom(&mut custom, b"", b"");
        expect_error(
            &custom,
            next_section,
            ErrorKind::TruncatedLength {
                field: LengthField::CustomNameSize,
            },
        );
    }
}

#[test]
fn rejects_overflow_or_continuation_in_the_fifth_length_byte() {
    for fifth in [0x10, 0x7f, 0x80, 0x8f, 0xff] {
        let leb = [0x80, 0x80, 0x80, 0x80, fifth, 0];
        let mut section = module_with_suffix(&[0]);
        section.extend_from_slice(&leb);
        expect_error(
            &section,
            13,
            ErrorKind::LengthOverflow {
                field: LengthField::SectionSize,
            },
        );

        let mut custom = HEADER.to_vec();
        push_section(&mut custom, &leb);
        expect_error(
            &custom,
            14,
            ErrorKind::LengthOverflow {
                field: LengthField::CustomNameSize,
            },
        );
    }
}

#[test]
fn differentiates_maximum_u32_from_leb_overflow() {
    let max_u32 = [0xff, 0xff, 0xff, 0xff, 0x0f];
    let mut section = module_with_suffix(&[0]);
    section.extend_from_slice(&max_u32);
    expect_error(
        &section,
        9,
        ErrorKind::LimitExceeded {
            limit: Limit::SectionBytes,
        },
    );

    let mut custom = HEADER.to_vec();
    push_section(&mut custom, &max_u32);
    expect_error(
        &custom,
        10,
        ErrorKind::LimitExceeded {
            limit: Limit::CustomNameBytes,
        },
    );
}

#[test]
fn checks_size_and_availability_before_reporting_recognized_unsupported_sections() {
    for id in [0, 1, 11, 12, 13] {
        let bytes = module_with_suffix(&[id, 2, 0]);
        expect_error(
            &bytes,
            11,
            ErrorKind::TruncatedSection {
                declared: 2,
                available: 1,
            },
        );
    }
    let bytes = module_with_suffix(&[1, 0x80]);
    expect_error(
        &bytes,
        10,
        ErrorKind::TruncatedLength {
            field: LengthField::SectionSize,
        },
    );
}

#[test]
fn name_cannot_reach_into_the_next_section() {
    let bytes = module_with_suffix(&[0, 2, 3, b'x', 0, 1, 0]);
    expect_error(
        &bytes,
        12,
        ErrorKind::TruncatedName {
            declared: 3,
            available: 1,
        },
    );
}

#[test]
fn checks_utf8_to_the_end_of_the_name_but_not_beyond() {
    let cases: &[&[u8]] = &[
        b"x\xff",             // invalid lead byte
        b"x\xc0\x80",         // overlong NUL
        b"x\xed\xa0\x80",     // surrogate
        b"x\xf4\x90\x80\x80", // out of scalar range
        b"x\xe2\x82",         // would complete if the decoder read into the body
    ];
    for name in cases {
        let mut bytes = HEADER.to_vec();
        push_custom(&mut bytes, name, b"\xac");
        expect_error(&bytes, 12, ErrorKind::InvalidUtf8Name);
    }
}

#[test]
fn bounds_custom_name_bytes_inclusively() {
    let limit = Limit::CustomNameBytes;
    let mut bytes = HEADER.to_vec();
    push_custom(&mut bytes, &vec![b'a'; limit.maximum()], b"");
    validate_module(&bytes).unwrap();

    let mut bytes = HEADER.to_vec();
    push_custom(&mut bytes, &vec![b'a'; limit.maximum() + 1], b"");
    expect_error(&bytes, 11, ErrorKind::LimitExceeded { limit });
}

#[test]
fn bounds_section_payload_bytes_inclusively() {
    let limit = Limit::SectionBytes;
    let mut bytes = HEADER.to_vec();
    push_custom(&mut bytes, b"", &vec![0xff; limit.maximum() - 1]);
    validate_module(&bytes).unwrap();

    let mut bytes = HEADER.to_vec();
    push_custom(&mut bytes, b"", &vec![0xff; limit.maximum()]);
    expect_error(&bytes, 9, ErrorKind::LimitExceeded { limit });
}

#[test]
fn bounds_section_count_inclusively() {
    let limit = Limit::SectionCount;
    let mut bytes = HEADER.to_vec();
    for _ in 0..limit.maximum() {
        push_custom(&mut bytes, b"", b"");
    }
    validate_module(&bytes).unwrap();

    let extra = bytes.len();
    push_custom(&mut bytes, b"", b"");
    expect_error(&bytes, extra, ErrorKind::LimitExceeded { limit });
}

#[test]
fn bounds_total_input_bytes_inclusively() {
    let limit = Limit::ModuleBytes;
    let mut bytes = HEADER.to_vec();
    for _ in 0..3 {
        push_custom(&mut bytes, b"", &vec![0; Limit::SectionBytes.maximum() - 1]);
    }
    let room = limit.maximum() - bytes.len();
    // 1 ID byte, 3 LEB32 size bytes, and 1 byte for the empty name length.
    push_custom(&mut bytes, b"", &vec![0; room - 5]);
    assert_eq!(bytes.len(), limit.maximum());
    validate_module(&bytes).unwrap();

    // This remains a well-formed module; enlarge the last payload by one byte.
    bytes.truncate(limit.maximum() - room);
    push_custom(&mut bytes, b"", &vec![0; room - 4]);
    assert_eq!(bytes.len(), limit.maximum() + 1);
    expect_error(&bytes, limit.maximum(), ErrorKind::LimitExceeded { limit });
}
