use super::*;
use ErrorKind::*;
use IntegerField::*;

const ZERO: &[u8] = &[0];
const PADDED_ZERO: &[u8] = &[0x80, 0x80, 0x80, 0x80, 0];
const VECTORS: [(u8, IntegerField); 4] = [
    (1, TypeCount),
    (3, FunctionCount),
    (7, ExportCount),
    (10, CodeCount),
];
const CUSTOM: &[u8] = b"\x04name\xff\x80";

#[test]
fn omitted_and_explicit_empty_vectors_have_no_entries() {
    for empty in [ZERO, PADDED_ZERO] {
        for mask in 0..1 << VECTORS.len() {
            let mut bytes = module(&[(0, CUSTOM)]);
            for (bit, (id, _)) in VECTORS.into_iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    push_section(&mut bytes, id, empty);
                    push_section(&mut bytes, 0, CUSTOM);
                }
            }
            let validated = validate_module(&bytes).unwrap();
            assert_eq!(
                validated.instantiate().unwrap().invoke_i32("answer"),
                Err(InvokeError::ExportNotFound)
            );
        }
    }
    // Unused types need neither declarations nor code.
    for declarations in [None, Some(PADDED_ZERO)] {
        for code in [None, Some(PADDED_ZERO)] {
            let mut bytes = module(&[(1, TYPE)]);
            for (id, vector) in [(3, declarations), (10, code)] {
                if let Some(vector) = vector {
                    push_section(&mut bytes, id, vector);
                }
            }
            validate_module(&bytes).unwrap();
        }
    }
}

#[test]
fn empty_counts_still_obey_leb_and_section_frames() {
    for (id, field) in VECTORS {
        for end in 0..PADDED_ZERO.len() {
            bad_payload(id, &PADDED_ZERO[..end], end, TruncatedInteger { field });
        }
        for fifth in [0x10, 0x80] {
            let overflow = [0x80, 0x80, 0x80, 0x80, fifth, 0];
            bad_payload(id, &overflow, 4, IntegerOverflow { field });
        }
        for zero in [ZERO, PADDED_ZERO] {
            let trailing = [zero, &[0xff]].concat();
            bad_payload(id, &trailing, zero.len(), TrailingSectionBytes { id });

            let bytes = [HEADER, &[id, (zero.len() + 1) as u8], zero].concat();
            expect_error(
                &bytes,
                bytes.len(),
                TruncatedSection {
                    declared: zero.len() + 1,
                    available: zero.len(),
                },
            );
        }
    }
}

#[test]
fn empty_sections_remain_ordered_and_unique_across_custom_sections() {
    for (i, (id, _)) in VECTORS.into_iter().enumerate() {
        for &(earlier, _) in &VECTORS[i..] {
            let mut bytes = module(&[(earlier, ZERO), (0, CUSTOM)]);
            let at = bytes.len();
            // Ordering wins over count decoding (the section frame is valid).
            push_section(&mut bytes, id, &[]);
            expect_error(&bytes, at, SectionOutOfOrder { id });
        }
    }
}

#[test]
fn distinguishes_missing_types_from_an_empty_type_index_space() {
    let missing = module(&[(3, FUNCTION), (10, CODE), (0, CUSTOM)]);
    expect_error(&missing, missing.len(), MissingSection { id: 1 });

    let mut empty = module(&[(1, PADDED_ZERO), (0, CUSTOM)]);
    let at = push_section(&mut empty, 3, &[1, 0x80, 0]);
    push_section(&mut empty, 10, CODE);
    expect_error(
        &empty,
        at + 1,
        InvalidIndex {
            field: TypeIndex,
            index: 0,
        },
    );

    let mut fragment = module(&[(1, ZERO)]);
    let at = push_section(&mut fragment, 3, &[1, 0x80]);
    push_section(&mut fragment, 0, CUSTOM);
    expect_error(&fragment, at + 2, TruncatedInteger { field: TypeIndex });
}

#[test]
fn distinguishes_missing_declarations_from_an_explicit_zero_count() {
    let void = &[1, 0x60, 0, 0][..];
    let void_code = &[1, 2, 0, 0x0b][..];
    for (types, code, missing_id) in [
        (&[][..], CODE, 1),
        (&[(1, void)][..], void_code, 3),
        (&[(1, ZERO)][..], CODE, 3),
    ] {
        let mut bytes = module(types);
        push_section(&mut bytes, 10, code);
        push_section(&mut bytes, 0, CUSTOM);
        expect_error(&bytes, bytes.len(), MissingSection { id: missing_id });
    }
    // Provisional body checking uses the first type, if any (including void
    // above), but provides no direct call targets.
    let mut bytes = module(&[(1, TYPE)]);
    let at = push_section(&mut bytes, 10, &[1, 4, 0, 0x10, 0, 0x0b]);
    expect_error(
        &bytes,
        at + 4,
        InvalidIndex {
            field: FunctionIndex,
            index: 0,
        },
    );

    for code in [CODE, &[0x81, 0]] {
        // The second payload has count 1 but no body.
        let mut bytes = module(&[(1, TYPE), (3, ZERO), (0, CUSTOM)]);
        let at = push_section(&mut bytes, 10, code);
        expect_error(
            &bytes,
            at,
            UnsupportedCount {
                field: CodeCount,
                count: 1,
            },
        );
    }

    let mut bytes = module(&[(1, TYPE)]);
    let at = push_section(&mut bytes, 10, &[0x81, 0]); // Provisional decoding reaches the body.
    push_section(&mut bytes, 0, CUSTOM);
    expect_error(
        &bytes,
        at + 2,
        TruncatedLength {
            field: LengthField::CodeBodySize,
        },
    );
}

#[test]
fn distinguishes_missing_code_from_explicit_count_mismatch() {
    let declarations = [(1, TYPE), (3, FUNCTION), (0, CUSTOM)];
    let mut bytes = module(&declarations);
    expect_error(&bytes, bytes.len(), MissingSection { id: 10 });
    let at = push_section(&mut bytes, 10, PADDED_ZERO);
    expect_error(
        &bytes,
        at,
        UnsupportedCount {
            field: CodeCount,
            count: 0,
        },
    );

    for (declarations, count) in [(FUNCTION, 2), (TWO_FUNCTIONS, 1)] {
        let mut bytes = module(&[(1, TYPE), (3, declarations)]);
        let at = push_section(&mut bytes, 10, &[count]);
        expect_error(
            &bytes,
            at,
            UnsupportedCount {
                field: CodeCount,
                count: count.into(),
            },
        );
    }
}

#[test]
fn zero_declarations_allow_no_start_element_or_export_function_targets() {
    const TABLE: &[u8] = &[1, 0x70, 0, 1];
    const ELEMENT: &[u8] = &[1, 0, 0x41, 0, 0x0b, 1, 0];
    let invalid = InvalidIndex {
        field: FunctionIndex,
        index: 0,
    };
    for functions in [None, Some(PADDED_ZERO)] {
        let mut prefix = module(&[(1, ZERO)]);
        if let Some(functions) = functions {
            push_section(&mut prefix, 3, functions);
        }
        for (id, payload, index) in [(8, PADDED_ZERO, 0), (9, ELEMENT, 6), (7, EXPORT, 9)] {
            let mut bytes = prefix.clone();
            push_section(&mut bytes, 4, TABLE);
            let at = push_section(&mut bytes, id, payload);
            push_section(&mut bytes, 0, CUSTOM);
            if id == 7 && functions.is_none() {
                // An omitted #3 alone retains the legacy export deferral.
                expect_error(&bytes, bytes.len(), MissingSection { id: 3 });
            } else {
                expect_error(&bytes, at + index, invalid);
            }
        }
    }
    let mut bytes = module(&[(3, ZERO)]);
    let at = push_section(&mut bytes, 7, b"\x01\x01f\0\x80");
    push_section(&mut bytes, 0, CUSTOM);
    expect_error(
        &bytes,
        at + 5,
        TruncatedInteger {
            field: FunctionIndex,
        },
    );

    let mut bytes = module(&[(1, TYPE), (3, FUNCTION)]);
    let at = push_section(&mut bytes, 10, &[1, 4, 0, 0x10, 1, 0x0b]);
    expect_error(
        &bytes,
        at + 4,
        InvalidIndex {
            field: FunctionIndex,
            index: 1,
        },
    );
}
