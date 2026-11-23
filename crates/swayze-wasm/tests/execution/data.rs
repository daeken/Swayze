use super::*;
use swayze_wasm::InstantiationError;

const DATA: u8 = 11;
const ZERO_PAGES: &[u8] = &[1, 0, 0];
const EMPTY_SEGMENT: &[u8] = &[1, 0, 0x41, 0, 0x0b, 0];

fn push_segment(payload: &mut Vec<u8>, mut offset: i32, contents: &[u8]) -> usize {
    payload.extend([0, 0x41]); // memory index zero, i32.const
    loop {
        let low = (offset & 0x7f) as u8;
        offset >>= 7;
        if (offset == 0 && low & 0x40 == 0) || (offset == -1 && low & 0x40 != 0) {
            payload.push(low);
            break;
        }
        payload.push(low | 0x80);
    }
    payload.push(0x0b);
    let length_offset = payload.len();
    push_u32(payload, contents.len());
    payload.extend_from_slice(contents);
    length_offset
}

fn data_payload(segments: &[(i32, &[u8])]) -> Vec<u8> {
    let mut payload = Vec::new();
    push_u32(&mut payload, segments.len());
    for &(offset, contents) in segments {
        push_segment(&mut payload, offset, contents);
    }
    payload
}

fn data_module(payload: &[u8]) -> Module {
    let bytes = module(&[
        (1, &i32_types(&[1, 2])),
        (3, &[2, 0, 1]),
        (5, ONE_PAGE),
        (7, &function_exports(&[(b"read", 0), (b"write", 1)])),
        (10, &code_bodies(&[READ, WRITE])),
        (0, &[0]),
        (DATA, payload),
        (0, &[0]),
    ]);
    validate_module(&bytes).unwrap()
}

fn bad_data(payload: &[u8], position: usize, kind: ErrorKind) {
    let mut bytes = module(&[(5, ONE_PAGE)]);
    let start = push_section(&mut bytes, DATA, payload);
    // Decoding must stay inside the frame even if more bytes follow it.
    push_section(&mut bytes, 0, &[0]);
    expect_error(&bytes, start + position, kind);
}

#[test]
fn accepts_data_in_memory_only_modules_and_enforces_section_order() {
    for sections in [
        &[(DATA, &[0][..])][..],
        &[
            (5, ONE_PAGE),
            (0, &[0]),
            (DATA, &[1, 0, 0x41, 0, 0x0b, 1, 0xff]),
        ],
        &[(5, ZERO_PAGES), (DATA, EMPTY_SEGMENT)],
    ] {
        let module = validate_module(&module(sections)).unwrap();
        assert_eq!(
            module.instantiate().unwrap().invoke_i32(""),
            Err(InvokeError::ExportNotFound)
        );
    }

    for (prefix, late_id, late_payload) in [
        (
            &[(5, ONE_PAGE), (DATA, &[0][..]), (0, &[0][..])][..],
            DATA,
            &[0][..],
        ),
        (&[(DATA, &[0][..])][..], 5, ONE_PAGE),
        (
            &[
                (1, TYPE),
                (3, FUNCTION),
                (5, ONE_PAGE),
                (DATA, EMPTY_SEGMENT),
            ][..],
            10,
            CODE,
        ),
    ] {
        let mut bytes = module(prefix);
        let id_offset = bytes.len();
        push_section(&mut bytes, late_id, late_payload);
        expect_error(
            &bytes,
            id_offset,
            ErrorKind::SectionOutOfOrder { id: late_id },
        );
    }
    let bytes = module(&[
        (1, TYPE),
        (3, FUNCTION),
        (5, ONE_PAGE),
        (DATA, EMPTY_SEGMENT),
    ]);
    expect_error(&bytes, bytes.len(), ErrorKind::MissingSection { id: 10 });
}

#[test]
fn each_segment_requires_defined_memory_and_only_index_zero_is_supported() {
    for prefix in [&[][..], &[(5, &[0][..])][..]] {
        let mut bytes = module(prefix);
        let start = push_section(&mut bytes, DATA, EMPTY_SEGMENT);
        expect_error(&bytes, start + 1, ErrorKind::MissingMemory);
    }
    // Passive (1) and explicit active (2) bulk-memory flags are not Core 1.0.
    for index in [1, 2] {
        bad_data(
            &[1, index, 0, 0x41, 0, 0x0b, 0],
            1,
            ErrorKind::InvalidIndex {
                field: IntegerField::MemoryIndex,
                index: u32::from(index),
            },
        );
    }
    bad_data(
        &[1, 0xff, 0xff, 0xff, 0xff, 0x0f],
        1,
        ErrorKind::InvalidIndex {
            field: IntegerField::MemoryIndex,
            index: u32::MAX,
        },
    );
    let module = data_module(&[
        0x81, 0x80, 0x80, 0x80, 0, // padded count 1
        0x80, 0x80, 0x80, 0x80, 0, // padded uLEB32 memory index zero
        0x41, 0x81, 0x80, 0x80, 0x80, 0, 0x0b, // padded sLEB32 offset 1
        0x81, 0x80, 0x80, 0x80, 0, 0xff, // padded byte length 1 and non-UTF-8 byte
    ]);
    assert_eq!(
        module
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("read", &[0]),
        Ok(0xff00)
    );
}

#[test]
fn checks_exact_offset_expression_and_bounded_leb32s() {
    for (prefix, field, invalid_last) in [
        (&[][..], IntegerField::DataCount, 0x10),
        (&[1][..], IntegerField::MemoryIndex, 0x10),
        (&[1, 0, 0x41][..], IntegerField::I32Const, 0x08),
    ] {
        for n in 0..=4 {
            let truncated = [prefix, &[0x80; 4][..n]].concat();
            bad_data(
                &truncated,
                truncated.len(),
                ErrorKind::TruncatedInteger { field },
            );
        }
        for fifth in [invalid_last, 0x80] {
            let overflow = [prefix, &[0x80; 4], &[fifth, 0]].concat();
            bad_data(
                &overflow,
                prefix.len() + 4,
                ErrorKind::IntegerOverflow { field },
            );
        }
    }
    bad_data(&[1, 0], 2, ErrorKind::TruncatedByte { expected: 0x41 });
    for other_opcode in [0x0b, 0x42] {
        // empty, i64.const
        bad_data(
            &[1, 0, other_opcode, 0, 0x0b, 0],
            2,
            ErrorKind::UnexpectedByte {
                expected: 0x41,
                found: other_opcode,
            },
        );
    }
    bad_data(
        &[1, 0, 0x41, 0],
        4,
        ErrorKind::TruncatedByte { expected: 0x0b },
    );
    bad_data(
        &[1, 0, 0x41, 0, 0x01, 0x0b, 0],
        4,
        ErrorKind::UnexpectedByte {
            expected: 0x0b,
            found: 0x01,
        },
    );
}

#[test]
fn checks_declared_data_length_and_rejects_trailing_bytes() {
    let prefix = &[1, 0, 0x41, 0, 0x0b];
    let field = LengthField::DataSize;
    for n in 0..=4 {
        let truncated = [prefix.as_slice(), &[0x80; 4][..n]].concat();
        bad_data(
            &truncated,
            truncated.len(),
            ErrorKind::TruncatedLength { field },
        );
    }
    for fifth in [0x10, 0x80] {
        let overflow = [prefix.as_slice(), &[0x80; 4], &[fifth, 0]].concat();
        bad_data(
            &overflow,
            prefix.len() + 4,
            ErrorKind::LengthOverflow { field },
        );
    }
    let oversized = [prefix.as_slice(), &[0xff, 0xff, 0xff, 0xff, 0x0f]].concat();
    bad_data(
        &oversized,
        prefix.len(),
        ErrorKind::LimitExceeded {
            limit: Limit::DataBytes,
        },
    );
    // A well-formed but out-of-bounds first segment must not prevent
    // validation of later segments.
    let mut data = data_payload(&[(PAGE, &[0xff]), (1, &[2, 3, 4])]);
    data.pop();
    bad_data(
        &data,
        data.len(),
        ErrorKind::TruncatedData {
            declared: 3,
            available: 2,
        },
    );
    for payload in [&[0][..], EMPTY_SEGMENT] {
        let trailing = [payload, &[0xff]].concat();
        bad_data(
            &trailing,
            payload.len(),
            ErrorKind::TrailingSectionBytes { id: DATA },
        );
    }
}

#[test]
fn bounds_count_and_aggregate_retained_bytes_inclusively_even_with_overlap() {
    let limit = Limit::DataBytes;
    assert_eq!(limit.maximum(), PAGE as usize);
    let span = limit.maximum() / 2;
    let first = vec![1; span];
    let second = vec![2; span];
    let mut payload = vec![8];
    push_segment(&mut payload, 0, &first);
    push_segment(&mut payload, 0, &second);
    for _ in 2..8 {
        push_segment(&mut payload, PAGE, &[]);
    }
    let validated = data_module(&payload);
    let instance = validated.instantiate().unwrap();
    assert_eq!(
        instance.invoke_i32_with_args("read", &[span as i32 - 4]),
        Ok(0x0202_0202)
    );
    assert_eq!(instance.invoke_i32_with_args("read", &[span as i32]), Ok(0));

    let mut oversized = data_payload(&[(0, &first), (0, &second)]);
    oversized[0] = 3;
    let last_length = push_segment(&mut oversized, 0, &[3]);
    // More initial memory does not increase the aggregate data allowance.
    for memory in [ONE_PAGE, TWO_PAGES] {
        let mut bytes = module(&[(5, memory)]);
        let start = push_section(&mut bytes, DATA, &oversized);
        push_section(&mut bytes, 0, &[0]);
        expect_error(
            &bytes,
            start + last_length,
            ErrorKind::LimitExceeded { limit },
        );
    }
    payload[0] = 9;
    bad_data(
        &payload,
        0,
        ErrorKind::UnsupportedCount {
            field: IntegerField::DataCount,
            count: 9,
        },
    );
}

#[test]
fn initializes_in_source_order_and_reuses_owned_data_for_independent_instances() {
    // Descending offsets distinguish source order from sorting by address.
    let module = data_module(&data_payload(&[
        (2, &[0x11, 0x22, 0x33, 0x44]),
        (1, &[0x55, 0x66, 0]),
    ]));
    let a = module.instantiate().unwrap();
    let b = module.instantiate().unwrap();
    assert_eq!(a.invoke_i32_with_args("read", &[0]), Ok(0x0066_5500));
    assert_eq!(a.invoke_i32_with_args("read", &[2]), Ok(0x4433_0066));
    assert_eq!(a.invoke_i32_with_args("read", &[6]), Ok(0));
    assert_eq!(a.invoke_i32_with_args("write", &[2, -1]), Ok(42));
    assert_eq!(a.invoke_i32_with_args("read", &[2]), Ok(-1));
    let c = module.instantiate().unwrap();
    for instance in [b, c] {
        assert_eq!(instance.invoke_i32_with_args("read", &[2]), Ok(0x4433_0066));
    }
}

#[test]
fn last_byte_and_empty_at_end_fit_but_full_out_of_bounds_ranges_fail_instantiation() {
    let at_end = data_module(&data_payload(&[(PAGE - 1, &[0x12]), (PAGE, &[])]));
    assert_eq!(
        at_end
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("read", &[PAGE - 4]),
        Ok(0x1200_0000)
    );
    for (offset, contents) in [
        (PAGE - 1, &[1, 2][..]),
        (PAGE, &[1]),
        (PAGE + 1, &[]),
        (-1, &[1]), // unsigned offset plus length crosses 2^32
        (-1, &[]),
        (i32::MIN, &[]),
    ] {
        let data = data_payload(&[(0, &[7]), (offset, contents), (PAGE + 1, &[])]);
        let module = data_module(&data); // bounds failure is not a validation error
        let expected = InstantiationError::DataSegmentOutOfBounds { segment_index: 1 };
        // No Instance is returned, and a retry does not change the failure.
        assert_eq!(module.instantiate().unwrap_err(), expected);
        assert_eq!(module.instantiate().unwrap_err(), expected);
    }
}

#[test]
fn zero_page_memory_only_fits_empty_at_zero() {
    for (offset, contents) in [(0, &[1][..]), (1, &[]), (-1, &[])] {
        let data = data_payload(&[(offset, contents)]);
        let module = validate_module(&module(&[(5, ZERO_PAGES), (DATA, &data)])).unwrap();
        assert_eq!(
            module.instantiate().unwrap_err(),
            InstantiationError::DataSegmentOutOfBounds { segment_index: 0 }
        );
    }
}
