use super::*;
use crate::{Error, ErrorKind, IntegerField, LengthField, Limit, Module, validate_module};

const HEADER: &[u8] = b"\0asm\x01\0\0\0";
const TABLE: u8 = 4;
const ELEMENT: u8 = 9;
const TABLE_0: &[u8] = &[1, 0x70, 0, 0];
const TABLE_16: &[u8] = &[1, 0x70, 1, 16, 16];
const NO_ENTRIES: &[u8] = &[1, 0, 0x41, 0, 0x0b, 0];
const TYPES: &[u8] = &[2, 0x60, 0, 0, 0x60, 0, 1, 0x7f];
const FUNCTIONS: &[u8] = &[8, 0, 0, 0, 0, 0, 0, 0, 1];
const CODE: &[u8] = &[
    8, 2, 0, 0x0b, 2, 0, 0x0b, 2, 0, 0x0b, 2, 0, 0x0b, 2, 0, 0x0b, 2, 0, 0x0b, 2, 0, 0x0b, 4, 0,
    0x41, 42, 0x0b,
];

fn push_section(bytes: &mut Vec<u8>, id: u8, payload: &[u8]) -> usize {
    bytes.push(id);
    let mut size = payload.len();
    while size >= 128 {
        bytes.push((size as u8 & 0x7f) | 0x80);
        size >>= 7;
    }
    bytes.push(size as u8);
    let start = bytes.len();
    bytes.extend_from_slice(payload);
    start
}

fn bytes(sections: &[(u8, &[u8])]) -> Vec<u8> {
    let mut bytes = HEADER.to_vec();
    for &(id, payload) in sections {
        push_section(&mut bytes, id, payload);
    }
    bytes
}

fn expect_error(bytes: &[u8], offset: usize, kind: ErrorKind) {
    assert_eq!(validate_module(bytes).unwrap_err(), Error { offset, kind });
}

fn bad_section(prefix: &[(u8, &[u8])], id: u8, payload: &[u8], position: usize, kind: ErrorKind) {
    let mut bytes = bytes(prefix);
    let start = push_section(&mut bytes, id, payload);
    // A following section cannot supply a missing byte in the current frame.
    push_section(&mut bytes, 0, &[0]);
    expect_error(&bytes, start + position, kind);
}

fn bad_table(payload: &[u8], position: usize, kind: ErrorKind) {
    bad_section(&[], TABLE, payload, position, kind);
}

fn bad_element(payload: &[u8], position: usize, kind: ErrorKind) {
    bad_section(
        &[(1, TYPES), (3, FUNCTIONS), (TABLE, TABLE_16)],
        ELEMENT,
        payload,
        position,
        kind,
    );
}

fn push_segment(payload: &mut Vec<u8>, offset: i32, indices: &[u8]) -> usize {
    payload.extend([0, 0x41]);
    // Emit a valid five-byte signed LEB32, including sign-extension bits.
    for shift in [0, 7, 14, 21] {
        payload.push(((offset >> shift) as u8 & 0x7f) | 0x80);
    }
    payload.extend([(offset >> 28) as u8 & 0x7f, 0x0b]);
    let count_offset = payload.len();
    assert!(indices.len() < 128);
    payload.push(indices.len() as u8);
    payload.extend_from_slice(indices);
    count_offset
}

fn element_payload(segments: &[(i32, &[u8])]) -> Vec<u8> {
    let mut payload = vec![segments.len() as u8];
    for &(offset, indices) in segments {
        push_segment(&mut payload, offset, indices);
    }
    payload
}

fn element_module(table: &[u8], elements: &[u8]) -> Module {
    validate_module(&bytes(&[
        (1, TYPES),
        (3, FUNCTIONS),
        (TABLE, table),
        (ELEMENT, elements),
        (10, CODE),
    ]))
    .unwrap()
}

fn live_entries(table: &FunctionTable) -> &[Option<usize>] {
    &table.entries[..table.len]
}

#[test]
fn absence_empty_vectors_and_defined_zero_entries_are_distinct() {
    for sections in [
        &[][..],
        &[(TABLE, &[0][..]), (ELEMENT, &[0][..])],
        &[(ELEMENT, &[0][..])],
    ] {
        assert!(
            validate_module(&bytes(sections))
                .unwrap()
                .instantiate()
                .unwrap()
                .table
                .is_none()
        );
    }
    let module = validate_module(&bytes(&[(TABLE, TABLE_0), (ELEMENT, NO_ENTRIES)])).unwrap();
    let instance = module.instantiate().unwrap();
    assert!(live_entries(instance.table.as_ref().unwrap()).is_empty());
    let module = validate_module(&bytes(&[(TABLE, &[1, 0x70, 1, 0, 16])])).unwrap();
    assert!(live_entries(module.instantiate().unwrap().table.as_ref().unwrap()).is_empty());

    for prefix in [&[][..], &[(TABLE, &[0][..])]] {
        bad_section(prefix, ELEMENT, NO_ENTRIES, 1, ErrorKind::MissingTable);
    }
    for (id, field) in [
        (TABLE, IntegerField::TableCount),
        (ELEMENT, IntegerField::ElementCount),
    ] {
        bad_section(&[], id, &[], 0, ErrorKind::TruncatedInteger { field });
    }
}

#[test]
fn table_and_element_have_core_section_order_and_exact_frames() {
    let module = validate_module(&bytes(&[
        (1, TYPES),
        (3, FUNCTIONS),
        (TABLE, TABLE_16),
        (0, &[0]),
        (5, &[0]),
        (6, &[0]),
        (7, &[0]),
        (ELEMENT, NO_ENTRIES),
        (0, &[0]),
        (10, CODE),
        (11, &[0]),
    ]))
    .unwrap();
    assert_eq!(
        live_entries(module.instantiate().unwrap().table.as_ref().unwrap()),
        &[None; 16]
    );

    for (prior, next) in [
        (4, 3),
        (4, 4),
        (5, 4),
        (6, 4),
        (7, 4),
        (9, 4),
        (9, 5),
        (9, 7),
        (9, 9),
        (10, 9),
        (11, 9),
    ] {
        let mut bytes = if prior == 10 {
            bytes(&[(1, TYPES), (3, FUNCTIONS), (10, CODE)])
        } else {
            bytes(&[(prior, &[0])])
        };
        push_section(&mut bytes, 0, &[0]);
        let offset = bytes.len();
        push_section(&mut bytes, next, &[0]);
        expect_error(&bytes, offset, ErrorKind::SectionOutOfOrder { id: next });
    }
    for (id, payload) in [(TABLE, TABLE_16), (ELEMENT, NO_ENTRIES)] {
        let mut bytes = bytes(&[]);
        bytes.extend([id, payload.len() as u8 + 1]);
        bytes.extend_from_slice(payload);
        expect_error(
            &bytes,
            bytes.len(),
            ErrorKind::TruncatedSection {
                declared: payload.len() + 1,
                available: payload.len(),
            },
        );
        bad_section(
            &[],
            id,
            &[0, 0xff],
            1,
            ErrorKind::TrailingSectionBytes { id },
        );
        let prefix: &[(u8, &[u8])] = if id == ELEMENT {
            &[(TABLE, TABLE_16)]
        } else {
            &[]
        };
        bad_section(
            prefix,
            id,
            &[payload, &[0xff]].concat(),
            payload.len(),
            ErrorKind::TrailingSectionBytes { id },
        );
    }
}

#[test]
fn rejects_bad_table_types_flags_counts_and_bounds() {
    use ErrorKind::*;
    use IntegerField::*;
    bad_table(
        &[2],
        0,
        UnsupportedCount {
            field: TableCount,
            count: 2,
        },
    );
    bad_table(&[1], 1, TruncatedByte { expected: 0x70 });
    bad_table(
        &[1, 0x6f, 0, 0],
        1,
        UnexpectedByte {
            expected: 0x70,
            found: 0x6f,
        },
    );
    bad_table(&[1, 0x70], 2, TruncatedTableLimitsFlags);
    for flags in [2, 3, 4, 0x80, 0x81] {
        bad_table(
            &[1, 0x70, flags, 0, 0],
            2,
            UnsupportedTableLimitsFlags { flags },
        );
    }
    bad_table(
        &[1, 0x70, 0, 17],
        3,
        UnsupportedCount {
            field: TableMinimum,
            count: 17,
        },
    );
    bad_table(
        &[1, 0x70, 1, 0, 17],
        4,
        UnsupportedCount {
            field: TableMaximum,
            count: 17,
        },
    );
    bad_table(
        &[1, 0x70, 1, 16, 0x8f, 0],
        4,
        InvalidTableLimits {
            minimum: 16,
            maximum: 15,
        },
    );
}

#[test]
fn checks_leb32_width_sign_and_frame_boundary_for_every_new_field() {
    use IntegerField::*;
    for (bad, prefix, field) in [
        (
            bad_table as fn(&[u8], usize, ErrorKind),
            &[][..],
            TableCount,
        ),
        (bad_table, &[1, 0x70, 0], TableMinimum),
        (bad_table, &[1, 0x70, 1, 0], TableMaximum),
        (bad_element, &[], ElementCount),
        (bad_element, &[1], TableIndex),
        (bad_element, &[1, 0, 0x41], I32Const),
        (bad_element, &[1, 0, 0x41, 0, 0x0b], ElementFunctionCount),
        (bad_element, &[1, 0, 0x41, 0, 0x0b, 2, 0], FunctionIndex),
    ] {
        for continuations in 0..=4 {
            let truncated = [prefix, &[0x80; 4][..continuations]].concat();
            bad(
                &truncated,
                truncated.len(),
                ErrorKind::TruncatedInteger { field },
            );
        }
        let invalid_fifths: &[u8] = if field == I32Const {
            &[0x08, 0x77, 0xff]
        } else {
            &[0x10, 0x80]
        };
        for &fifth in invalid_fifths {
            let overflow = [prefix, &[0x80; 4], &[fifth, 0]].concat();
            bad(
                &overflow,
                prefix.len() + 4,
                ErrorKind::IntegerOverflow { field },
            );
        }
    }
    let padded_table = [
        0x81, 0x80, 0x80, 0x80, 0, 0x70, 1, 0x90, 0x80, 0x80, 0x80, 0, 0x90, 0x80, 0x80, 0x80, 0,
    ];
    let padded_elements = [
        0x81, 0x80, 0x80, 0x80, 0, // one segment
        0x80, 0x80, 0x80, 0x80, 0, // Core 1.0 uLEB32 table index zero
        0x41, 0x8f, 0x80, 0x80, 0x80, 0, 0x0b, // signed offset 15
        0x81, 0x80, 0x80, 0x80, 0, // one index
        0x87, 0x80, 0x80, 0x80, 0, // last declared function
    ];
    let module = element_module(&padded_table, &padded_elements);
    let instance = module.instantiate().unwrap();
    let entries = live_entries(instance.table.as_ref().unwrap());
    assert_eq!(&entries[..15], &[None; 15]);
    assert_eq!(entries[15], Some(7));

    // Section sizes themselves use the same bounded, padded unsigned LEB32.
    let mut bytes = HEADER.to_vec();
    bytes.extend([TABLE, 0x84, 0x80, 0x80, 0x80, 0]);
    bytes.extend_from_slice(TABLE_0);
    bytes.extend([ELEMENT, 0x81, 0x80, 0x80, 0x80, 0, 0]);
    assert!(validate_module(&bytes).is_ok());
    bytes.pop();
    let fifth = bytes.len() - 1;
    bytes[fifth] = 0x10;
    expect_error(
        &bytes,
        fifth,
        ErrorKind::LengthOverflow {
            field: LengthField::SectionSize,
        },
    );
}

#[test]
fn rejects_post_mvp_segment_forms_and_wrong_offset_expressions() {
    use ErrorKind::*;
    for index in [1, 2, 3, 4, 5, 6, 7] {
        bad_element(
            &[1, index as u8],
            1,
            InvalidIndex {
                field: IntegerField::TableIndex,
                index,
            },
        );
    }
    bad_element(
        &[1, 0xff, 0xff, 0xff, 0xff, 0x0f],
        1,
        InvalidIndex {
            field: IntegerField::TableIndex,
            index: u32::MAX,
        },
    );
    bad_element(&[1, 0], 2, TruncatedByte { expected: 0x41 });
    for found in [0x0b, 0x23, 0x42, 0xd2] {
        bad_element(
            &[1, 0, found, 0, 0x0b, 0],
            2,
            UnexpectedByte {
                expected: 0x41,
                found,
            },
        );
    }
    bad_element(&[1, 0, 0x41, 0], 4, TruncatedByte { expected: 0x0b });
    bad_element(
        &[1, 0, 0x41, 0, 0x01, 0x0b, 0],
        4,
        UnexpectedByte {
            expected: 0x0b,
            found: 0x01,
        },
    );
}

#[test]
fn rejects_table_import_and_unsupported_instructions() {
    let mut import = bytes(&[]);
    let count = push_section(&mut import, 2, &[1, 1, b'm', 1, b't', 1, 0x70, 0, 1]);
    push_section(&mut import, TABLE, TABLE_16);
    expect_error(
        &import,
        count,
        ErrorKind::UnsupportedCount {
            field: IntegerField::ImportCount,
            count: 1,
        },
    );

    for opcode in [0x25, 0x26, 0xfc] {
        // table.get, table.set, table.grow prefix
        bad_section(
            &[(1, TYPES), (3, &[1, 0]), (TABLE, TABLE_16)],
            10,
            &[
                1,
                8,
                0,
                0x41,
                0,
                opcode,
                if opcode == 0xfc { 0x0f } else { 0 },
                0,
                0x01,
                0x0b,
            ],
            5,
            ErrorKind::UnsupportedOpcode { opcode },
        );
    }
}

#[test]
fn checks_every_function_index_and_validates_referenced_and_unreferenced_bodies() {
    use IntegerField::FunctionIndex;
    for (encoded, index) in [(&[8][..], 8), (&[0xff, 0xff, 0xff, 0xff, 0x0f], u32::MAX)] {
        bad_element(
            &[&[1, 0, 0x41, 0, 0x0b, 2, 7], encoded].concat(),
            7,
            ErrorKind::InvalidIndex {
                field: FunctionIndex,
                index,
            },
        );
    }
    // An earlier range failure cannot hide a later invalid function index.
    let payload = element_payload(&[(16, &[0]), (0, &[8])]);
    bad_element(
        &payload,
        payload.len() - 1,
        ErrorKind::InvalidIndex {
            field: FunctionIndex,
            index: 8,
        },
    );
    let payload = &[1, 0, 0x41, 0, 0x0b, 1, 0];
    bad_section(
        &[(TABLE, TABLE_16)],
        ELEMENT,
        payload,
        6,
        ErrorKind::InvalidIndex {
            field: FunctionIndex,
            index: 0,
        },
    );
    let bytes = bytes(&[
        (1, TYPES),
        (3, FUNCTIONS),
        (TABLE, TABLE_16),
        (ELEMENT, payload),
    ]);
    expect_error(&bytes, bytes.len(), ErrorKind::MissingSection { id: 10 });

    for position in [3, CODE.len() - 3] {
        let mut code = CODE.to_vec();
        code[position] = 0xff;
        bad_section(
            &[
                (1, TYPES),
                (3, FUNCTIONS),
                (TABLE, TABLE_16),
                (ELEMENT, payload),
            ],
            10,
            &code,
            position,
            ErrorKind::UnsupportedOpcode { opcode: 0xff },
        );
    }
}

#[test]
fn exact_segment_and_aggregate_index_caps_count_overlapping_entries() {
    let limit = Limit::ElementFunctionIndices;
    assert_eq!(limit.maximum(), 64);
    let mut payload = vec![8];
    for index in [0, 1, 2, 7] {
        push_segment(&mut payload, 0, &[index; 16]);
    }
    let mut over_budget = payload.clone();
    for _ in 0..4 {
        push_segment(&mut payload, 16, &[]);
    }
    let module = element_module(TABLE_16, &payload);
    assert_eq!(
        live_entries(module.instantiate().unwrap().table.as_ref().unwrap()),
        &[Some(7); 16]
    );
    payload[0] = 9;
    bad_element(
        &payload,
        0,
        ErrorKind::UnsupportedCount {
            field: IntegerField::ElementCount,
            count: 9,
        },
    );

    over_budget[0] = 5;
    let count_offset = push_segment(&mut over_budget, 0, &[0]);
    bad_element(
        &over_budget,
        count_offset,
        ErrorKind::LimitExceeded { limit },
    );
    bad_element(
        &[1, 0, 0x41, 0, 0x0b, 0xff, 0xff, 0xff, 0xff, 0x0f],
        5,
        ErrorKind::LimitExceeded { limit },
    );
}

#[test]
fn empty_segment_fits_at_but_not_beyond_the_initial_length_including_zero() {
    for len in [0, 15] {
        // The declared maximum is greater than the initial length in both cases.
        let table = [1, 0x70, 1, len, 16];
        let offset = i32::from(len);
        let module = element_module(&table, &element_payload(&[(offset, &[])]));
        let instance = module.instantiate().unwrap();
        assert_eq!(
            live_entries(instance.table.as_ref().unwrap()),
            &([None; 16][..usize::from(len)])
        );

        let module = element_module(&table, &element_payload(&[(offset + 1, &[])]));
        assert_eq!(
            module.instantiate().unwrap_err(),
            InstantiationError::ElementSegmentOutOfBounds { segment_index: 0 }
        );
    }
}

#[test]
fn checks_full_element_ranges_with_unsigned_offsets() {
    let module = element_module(
        &[1, 0x70, 0, 16],
        &element_payload(&[(15, &[7]), (16, &[])]),
    );
    let instance = module.instantiate().unwrap();
    assert_eq!(live_entries(instance.table.as_ref().unwrap())[15], Some(7));
    for (offset, indices) in [
        (15, &[0, 1][..]),
        (16, &[0]),
        (17, &[]),
        (-1, &[0]),
        (-1, &[]),
        (i32::MIN, &[]),
    ] {
        let payload = element_payload(&[(0, &[7]), (offset, indices), (17, &[])]);
        let module = element_module(TABLE_16, &payload);
        let error = InstantiationError::ElementSegmentOutOfBounds { segment_index: 1 };
        assert_eq!(module.instantiate().unwrap_err(), error);
        assert_eq!(module.instantiate().unwrap_err(), error);
    }
    // The explicit maximum is not the initial length, even for a zero-entry table.
    for (offset, indices) in [(0, &[0][..]), (-1, &[])] {
        let module = element_module(&[1, 0x70, 1, 0, 16], &element_payload(&[(offset, indices)]));
        assert_eq!(
            module.instantiate().unwrap_err(),
            InstantiationError::ElementSegmentOutOfBounds { segment_index: 0 }
        );
    }
}

fn with_data(elements: &[u8], data: &[u8]) -> Module {
    let code = [&CODE[..CODE.len() - 5], &[7, 0, 0x41, 0, 0x28, 0, 0, 0x0b]].concat();
    validate_module(&bytes(&[
        (1, TYPES),
        (3, FUNCTIONS),
        (TABLE, TABLE_16),
        (5, &[1, 0, 1]),
        (7, &[1, 4, b'r', b'e', b'a', b'd', 0, 7]),
        (ELEMENT, elements),
        (10, &code),
        (11, data),
    ]))
    .unwrap()
}

#[test]
fn elements_initialize_null_slots_in_source_order_alongside_data_on_fresh_instances() {
    let elements = element_payload(&[(2, &[2, 3, 4]), (1, &[0, 7]), (16, &[])]);
    let module = with_data(
        &elements,
        &[
            2, 0, 0x41, 2, 0x0b, 2, 0x12, 0x34, 0, 0x41, 1, 0x0b, 2, 0x56, 0x78,
        ],
    );
    let mut first = module.instantiate().unwrap();
    let second = module.instantiate().unwrap();
    let mut expected = [None; 16];
    expected[1..5].copy_from_slice(&[Some(0), Some(7), Some(3), Some(4)]);
    for instance in [&first, &second] {
        assert_eq!(live_entries(instance.table.as_ref().unwrap()), &expected);
        assert_eq!(instance.invoke_i32("read"), Ok(0x3478_5600));
    }
    // Only this test can directly mutate the private slots. It must not affect
    // either a live sibling instance or a later instantiation of the same module.
    first.table.as_mut().unwrap().entries[2] = None;
    let third = module.instantiate().unwrap();
    for instance in [second, third] {
        assert_eq!(live_entries(instance.table.as_ref().unwrap()), &expected);
    }
}

#[test]
fn instantiation_reports_elements_before_data_and_first_failure_within_each_kind() {
    // The second element writes two slots; its bad offset puts the second
    // write out of bounds. The first and third elements fit either way.
    let elements = |offset| element_payload(&[(0, &[7]), (offset, &[0, 1]), (16, &[])]);
    let bad_data = &[
        4, // two valid segments followed by two segments at unsigned offset u32::MAX
        0, 0x41, 0, 0x0b, 1, 1, // one byte at 0
        0, 0x41, 1, 0x0b, 1, 2, // one byte at 1
        0, 0x41, 0x7f, 0x0b, 1, 3, // one byte at -1
        0, 0x41, 0x7f, 0x0b, 0, // empty at -1
    ];
    assert_eq!(
        with_data(&elements(15), bad_data)
            .instantiate()
            .unwrap_err(),
        InstantiationError::ElementSegmentOutOfBounds { segment_index: 1 }
    );
    // Only repair the element's offset; the exact same data must now fail
    // at its first out-of-bounds segment.
    assert_eq!(
        with_data(&elements(14), bad_data)
            .instantiate()
            .unwrap_err(),
        InstantiationError::DataSegmentOutOfBounds { segment_index: 2 }
    );
}
