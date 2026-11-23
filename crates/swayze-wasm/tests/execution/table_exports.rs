use super::*;
use swayze_wasm::{ReadGlobalError, ReadMemoryError, ResultKind, TableSizeError, Value};

const TABLE_ZERO: &[u8] = &[1, 0x70, 0, 0];
const TABLE_THREE: &[u8] = &[1, 0x70, 1, 3, 9];
const MEMORY: &[u8] = &[1, 0, 1];
const GLOBAL: &[u8] = &[1, 0x7f, 0, 0x41, 42, 0x0b];
const EXPORT_TABLE: &[u8] = b"\x01\x01t\x01\0";
const PADDED_ZERO: &[u8] = &[0x80, 0x80, 0x80, 0x80, 0];

fn exports(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_u32(&mut bytes, entries.len());
    for &(name, kind, index) in entries {
        push_u32(&mut bytes, name.len());
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(kind);
        bytes.extend_from_slice(index);
    }
    bytes
}

#[test]
fn table_only_exports_observe_fixed_slot_count_and_exact_names() {
    let aliases = exports(&[("", 1, &[0]), ("\0é𝄞", 1, PADDED_ZERO)]);
    let mut debug = None;
    for (table, size) in [(TABLE_ZERO, 0), (TABLE_THREE, 3), (&[1, 0x70, 0, 16], 16)] {
        let validated = validate_module(&module(&[(4, table), (7, &aliases)])).unwrap();
        let instance = validated.instantiate().unwrap();
        assert_eq!(instance.table_size(""), Ok(size));
        assert_eq!(instance.table_size("\0é𝄞"), Ok(size));
        assert_eq!(
            instance.table_size("\0e\u{301}𝄞"),
            Err(TableSizeError::ExportNotFound)
        );
        let formats = [
            format!("{validated:?}"),
            format!("{validated:#?}"),
            format!("{instance:?}"),
            format!("{instance:#?}"),
        ];
        assert_eq!(
            formats[0],
            "Module { function_count: 0, export_count: 2, data_segment_count: 0, element_segment_count: 0, .. }"
        );
        assert_eq!(
            formats[2],
            "Instance { function_count: 0, export_count: 2, data_segment_count: 0, element_segment_count: 0, .. }"
        );
        assert!(debug.get_or_insert_with(|| formats.clone()) == &formats);
    }

    // Definition alone does not make the table visible to the host.
    for sections in [
        &[][..],
        &[(4, TABLE_THREE)],
        &[(4, TABLE_THREE), (7, PADDED_ZERO)],
    ] {
        let validated = validate_module(&module(sections)).unwrap();
        assert_eq!(
            validated.instantiate().unwrap().table_size("t"),
            Err(TableSizeError::ExportNotFound)
        );
    }
}

#[test]
fn mixed_nonfunction_exports_need_no_function_sections() {
    let names = b"\x03\x01t\x01\0\x01m\x02\0\x01g\x03\0";
    let validated = validate_module(&module(&[
        (4, TABLE_ZERO),
        (5, MEMORY),
        (6, GLOBAL),
        (7, names),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.table_size("t"), Ok(0));
    for name in ["m", "g"] {
        assert_eq!(
            instance.table_size(name),
            Err(TableSizeError::ExportNotTable)
        );
    }
    let mut byte = [0xff];
    assert_eq!(
        instance.read_memory("t", u32::MAX, &mut byte),
        Err(ReadMemoryError::ExportNotMemory)
    );
    assert_eq!(byte, [0xff]);
    assert_eq!(instance.read_memory("m", 0, &mut byte), Ok(()));
    assert_eq!(byte, [0]);
    assert_eq!(
        instance.read_global("t"),
        Err(ReadGlobalError::ExportNotGlobal)
    );
    assert_eq!(instance.read_global("g"), Ok(Value::I32(42)));
    assert_eq!(
        instance.invoke_i32("t"),
        Err(InvokeError::ExportNotFunction)
    );
    assert_eq!(
        instance.invoke_void_with_args("t", &[0]),
        Err(InvokeError::ExportNotFunction)
    );
    assert_eq!(
        instance.invoke_with_values("t", &[Value::I64(0)], ResultKind::F64),
        Err(InvokeError::ExportNotFunction)
    );
}

#[test]
fn exported_guest_wrapper_can_call_through_table_but_private_callee_is_not_host_callable() {
    let names = b"\x04\x01t\x01\0\x01m\x02\0\x01g\x03\0\x03run\0\0";
    let code = code_bodies(&[
        &[0, 0x41, 1, 0x11, 0, 0, 0x0b], // Run call_indirect at slot 1.
        &[0, 0x41, 42, 0x0b],            // Private callee.
    ]);
    let validated = validate_module(&module(&[
        (0, b"\x04name\x01\x0a\x01\x01\x07private"), // Name function 1, without exporting it.
        (1, TYPE),
        (3, TWO_FUNCTIONS),
        (4, TABLE_THREE),
        (5, MEMORY),
        (6, GLOBAL),
        (7, names),
        (9, &[1, 0, 0x41, 1, 0x0b, 1, 1]), // Set slot 1 to function 1; 0 and 2 remain null.
        (10, &code),
    ]))
    .unwrap();
    for instance in [
        validated.instantiate().unwrap(),
        validated.instantiate().unwrap(),
    ] {
        assert_eq!(instance.table_size("t"), Ok(3)); // Neither occupied count (1) nor maximum (9).
        assert_eq!(
            instance.table_size("run"),
            Err(TableSizeError::ExportNotTable)
        );
        assert_eq!(instance.invoke_i32("run"), Ok(42));
        assert_eq!(
            instance.invoke_i32("private"),
            Err(InvokeError::ExportNotFound)
        );
        assert_eq!(
            instance.invoke_i32("t"),
            Err(InvokeError::ExportNotFunction)
        );
        assert_eq!(instance.table_size("t"), Ok(3));
    }
}

#[test]
fn table_indices_are_framed_and_checked_before_presence() {
    use ErrorKind::*;
    let field = IntegerField::TableIndex;
    for table in [None, Some(&[0][..]), Some(TABLE_ZERO)] {
        let mut prefix = HEADER.to_vec();
        if let Some(payload) = table {
            push_section(&mut prefix, 4, payload);
        }
        for (index, offset, kind) in [
            (&[][..], 0, TruncatedInteger { field }),
            (&PADDED_ZERO[..4], 4, TruncatedInteger { field }),
            (
                &[0x80, 0x80, 0x80, 0x80, 0x10, 0],
                4,
                IntegerOverflow { field },
            ),
            (
                &[0x80, 0x80, 0x80, 0x80, 0x80, 0],
                4,
                IntegerOverflow { field },
            ),
            (&[0x81, 0], 0, InvalidIndex { field, index: 1 }),
            (
                &[0xff, 0xff, 0xff, 0xff, 0x0f],
                0,
                InvalidIndex {
                    field,
                    index: u32::MAX,
                },
            ),
        ] {
            let mut bytes = prefix.clone();
            let at = push_section(&mut bytes, 7, &exports(&[("t", 1, index)]));
            push_section(&mut bytes, 0, &[0]); // Cannot supply a terminal index byte.
            expect_error(&bytes, at + 4 + offset, kind);
        }
        for index in [&[0][..], PADDED_ZERO] {
            let mut bytes = prefix.clone();
            let mut payload = exports(&[("t", 1, index)]);
            let at = push_section(&mut bytes, 7, &payload);
            push_section(&mut bytes, 0, &[0]);
            if table == Some(TABLE_ZERO) {
                let validated = validate_module(&bytes).unwrap();
                assert_eq!(validated.instantiate().unwrap().table_size("t"), Ok(0));
            } else {
                expect_error(&bytes, at + 4, MissingTable);
            }
            payload.push(0);
            let mut bytes = prefix.clone();
            let at = push_section(&mut bytes, 7, &payload);
            if table == Some(TABLE_ZERO) {
                expect_error(&bytes, at + 4 + index.len(), TrailingSectionBytes { id: 7 });
            } else {
                expect_error(&bytes, at + 4, MissingTable);
            }
        }
    }
}

#[test]
fn table_exports_share_name_checks_and_budget_across_kinds() {
    let declarations = [
        (1, TYPE),
        (3, FUNCTION),
        (4, TABLE_ZERO),
        (5, MEMORY),
        (6, GLOBAL),
    ];
    for (first, second) in [(0, 1), (1, 1), (1, 2), (3, 1)] {
        let payload = exports(&[("a", first, &[0]), ("a", second, &[0])]);
        for names in [&payload[..], &payload[..payload.len() - 2]] {
            let mut bytes = module(&declarations);
            let at = push_section(&mut bytes, 7, names);
            expect_error(&bytes, at + 5, ErrorKind::DuplicateExportName);
        }
    }
    bad_payload(7, b"\x01\x02t\xff\x01\x80", 3, ErrorKind::InvalidUtf8Name);

    let entries = [
        ("a", 0),
        ("b", 1),
        ("c", 2),
        ("d", 3),
        ("e", 0),
        ("f", 1),
        ("g", 2),
        ("h", 3),
        ("i", 1),
    ]
    .map(|(name, kind)| (name, kind, &[0][..]));
    let mut bytes = module(&declarations);
    push_section(&mut bytes, 7, &exports(&entries[..8]));
    push_section(&mut bytes, 10, CODE);
    let validated = validate_module(&bytes).unwrap();
    assert_eq!(validated.instantiate().unwrap().table_size("f"), Ok(0));
    let mut bytes = module(&declarations);
    let at = push_section(&mut bytes, 7, &exports(&entries));
    expect_error(
        &bytes,
        at,
        ErrorKind::UnsupportedCount {
            field: IntegerField::ExportCount,
            count: 9,
        },
    );

    let limit = Limit::ExportNameBytes;
    let max = "é".repeat(limit.maximum() / 2);
    let names = exports(&[(&max, 1, &[0])]);
    let validated = validate_module(&module(&[(4, TABLE_ZERO), (7, &names)])).unwrap();
    assert_eq!(validated.instantiate().unwrap().table_size(&max), Ok(0));
    let oversized = exports(&[(&(max + "a"), 1, &[0])]);
    bad_payload(7, &oversized, 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn table_exports_preserve_missing_and_empty_function_section_distinctions() {
    let names = b"\x02\x01t\x01\0\x01f\0\0";
    let all = [
        (1, TYPE),
        (3, FUNCTION),
        (4, TABLE_THREE),
        (7, names),
        (10, CODE),
    ];
    assert!(validate_module(&module(&all)).is_ok());
    for missing in [1, 3, 10] {
        let bytes = module(
            &all.into_iter()
                .filter(|&(id, _)| id != missing)
                .collect::<Vec<_>>(),
        );
        expect_error(
            &bytes,
            bytes.len(),
            ErrorKind::MissingSection { id: missing },
        );
    }
    let mut bytes = module(&[(1, TYPE), (3, PADDED_ZERO), (4, TABLE_THREE)]);
    let at = push_section(&mut bytes, 7, names);
    expect_error(
        &bytes,
        at + 8,
        ErrorKind::InvalidIndex {
            field: IntegerField::FunctionIndex,
            index: 0,
        },
    );

    let bytes = module(&[
        (1, PADDED_ZERO),
        (3, PADDED_ZERO),
        (4, TABLE_ZERO),
        (7, EXPORT_TABLE),
        (10, PADDED_ZERO),
    ]);
    let validated = validate_module(&bytes).unwrap();
    assert_eq!(validated.instantiate().unwrap().table_size("t"), Ok(0));
}
