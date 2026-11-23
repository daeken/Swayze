use super::{
    CODE, ErrorKind, FUNCTION, HEADER, IntegerField, InvokeError, Limit, TYPE, Trap, code_bodies,
    code_body, expect_error, module, push_section, push_u32, validate_module,
};
use swayze_wasm::{
    InstantiationError, ReadGlobalError, ReadMemoryError, ResultKind, ValType, Value,
};

const ONE_PAGE: &[u8] = b"\x01\0\x01";
const GLOBALS: &[u8] = b"\x01\x7f\0\x41\x7e\x0b"; // (global i32 (i32.const -2))
const GLOBAL_EXPORT: &[u8] = b"\x01\x01g\x03\0";
const F32_NEG_ZERO: u32 = 0x8000_0000;
const F32_SNAN: u32 = 0x7f80_1234;
const F64_NEG_ZERO: u64 = 0x8000_0000_0000_0000;
const F64_SNAN: u64 = 0xfff0_0000_0000_4321;

fn exports(entries: &[(&str, u8, u8)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_u32(&mut bytes, entries.len());
    for &(name, kind, index) in entries {
        push_u32(&mut bytes, name.len());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend([kind, index]);
    }
    bytes
}

#[test]
fn global_only_exports_all_numeric_tags_without_function_sections() {
    let mut globals = b"\x06\x7f\0\x41\x7e\x0b\x7e\x01\x42\x80\x80\x80\x80\x20\x0b".to_vec();
    for (kind, mutable, opcode, bits) in [
        (0x7d, 0, 0x43, &F32_NEG_ZERO.to_le_bytes()[..]),
        (0x7d, 1, 0x43, &F32_SNAN.to_le_bytes()),
        (0x7c, 1, 0x44, &F64_NEG_ZERO.to_le_bytes()),
        (0x7c, 0, 0x44, &F64_SNAN.to_le_bytes()),
    ] {
        globals.extend([kind, mutable, opcode]);
        globals.extend_from_slice(bits);
        globals.push(0x0b);
    }
    let names = exports(&[
        ("", 3, 0),
        ("é\0", 3, 1),
        ("f32z", 3, 2),
        ("f32n", 3, 3),
        ("f64z", 3, 4),
        ("f64n", 3, 5),
        ("alias", 3, 1),
        ("e\u{301}\0", 3, 0), // Distinct from the precomposed name.
    ]);
    let validated = validate_module(&module(&[(6, &globals), (7, &names)])).unwrap();
    let instance = validated.instantiate().unwrap();
    for (name, expected) in [
        ("", Value::I32(-2)),
        ("é\0", Value::I64(0x2_0000_0000)),
        ("f32z", Value::F32(F32_NEG_ZERO)),
        ("f32n", Value::F32(F32_SNAN)),
        ("f64z", Value::F64(F64_NEG_ZERO)),
        ("f64n", Value::F64(F64_SNAN)),
        ("alias", Value::I64(0x2_0000_0000)),
        ("e\u{301}\0", Value::I32(-2)),
    ] {
        assert_eq!(instance.read_global(name), Ok(expected));
    }
    for name in ["é", "É\0", "e\u{301}", "Alias"] {
        assert_eq!(
            instance.read_global(name),
            Err(ReadGlobalError::ExportNotFound)
        );
    }
}

#[test]
fn memory_and_global_exports_need_no_functions_and_keep_data_semantics() {
    let names = exports(&[
        ("g", 3, 0),
        ("mem", 2, 0),
        ("g_alias", 3, 0),
        ("mem_alias", 2, 0),
    ]);
    let prefix = [(5, ONE_PAGE), (6, GLOBALS), (7, &names[..])];
    let mut bytes = module(&prefix);
    push_section(&mut bytes, 11, b"\x01\0\x41\0\x0b\x02hi");
    let validated = validate_module(&bytes).unwrap();
    let instance = validated.instantiate().unwrap();
    for name in ["g", "g_alias"] {
        assert_eq!(instance.read_global(name), Ok(Value::I32(-2)));
    }
    for name in ["mem", "mem_alias"] {
        let mut dest = [0; 2];
        assert_eq!(instance.read_memory(name, 0, &mut dest), Ok(()));
        assert_eq!(&dest, b"hi");
    }
    let mut bytes = module(&prefix);
    push_section(&mut bytes, 11, b"\x01\0\x41\x80\x80\x04\x0b\x01!"); // Offset 65536.
    let validated = validate_module(&bytes).unwrap();
    assert_eq!(
        validated.instantiate().unwrap_err(),
        InstantiationError::DataSegmentOutOfBounds { segment_index: 0 }
    );
}

#[test]
fn wrong_kind_precedes_range_and_signature_and_is_distinct_from_no_export() {
    let names = exports(&[("g", 3, 0), ("mem", 2, 0), ("set", 0, 0)]);
    let code = code_body(b"\0\x20\0\x24\0\x0b"); // (param i32) local.get 0; global.set 0
    let validated = validate_module(&module(&[
        (1, b"\x01\x60\x01\x7f\0"),
        (3, FUNCTION),
        (5, ONE_PAGE),
        (6, b"\x02\x7f\x01\x41\x2a\x0b\x7f\0\x41\x09\x0b"),
        (7, &names),
        (10, &code),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    for name in ["mem", "set"] {
        assert_eq!(
            instance.read_global(name),
            Err(ReadGlobalError::ExportNotGlobal)
        );
    }
    // The second slot is defined but unexported; names cannot serve as indices.
    for name in ["G", "1", ""] {
        assert_eq!(
            instance.read_global(name),
            Err(ReadGlobalError::ExportNotFound)
        );
        assert_eq!(instance.invoke_i32(name), Err(InvokeError::ExportNotFound));
    }
    let mut dest = [0xaa];
    assert_eq!(
        instance.read_memory("unknown", u32::MAX, &mut dest),
        Err(ReadMemoryError::ExportNotFound)
    );
    assert_eq!(
        instance.read_memory("g", u32::MAX, &mut dest),
        Err(ReadMemoryError::ExportNotMemory)
    );
    assert_eq!(dest, [0xaa]);

    // All targets have index zero in their own index spaces. Try each possible
    // signature error and the exact function signature; none dispatch via "g".
    let arg = [Value::I32(7)];
    for (args, result, expected) in [
        (
            &[][..],
            ResultKind::Void,
            Err(InvokeError::ArityMismatch {
                expected: 1,
                actual: 0,
            }),
        ),
        (
            &arg[..],
            ResultKind::I32,
            Err(InvokeError::ResultMismatch {
                expected: ResultKind::I32,
                actual: ResultKind::Void,
            }),
        ),
        (
            &[Value::I64(7)],
            ResultKind::Void,
            Err(InvokeError::ArgumentTypeMismatch {
                index: 0,
                expected: ValType::I32,
                actual: ValType::I64,
            }),
        ),
        (&arg[..], ResultKind::Void, Ok(None)),
    ] {
        assert_eq!(
            instance.invoke_with_values("g", args, result),
            Err(InvokeError::ExportNotFunction)
        );
        assert_eq!(instance.read_global("g"), Ok(Value::I32(42)));
        assert_eq!(instance.invoke_with_values("set", args, result), expected);
    }
    assert_eq!(instance.read_global("g"), Ok(Value::I32(7)));
}

#[test]
fn snapshots_observe_start_and_pretrap_stores_but_do_not_change_after_return() {
    let mut globals = vec![2];
    globals.extend(b"\x7d\x01\x43\0\0\0\0\x0b\x7c\x01\x44\0\0\0\0\0\0\0\0\x0b");
    let writes = |f32_bits: u32, f64_bits: u64, trap: bool| {
        let mut body = b"\0\x43".to_vec();
        body.extend(f32_bits.to_le_bytes());
        body.extend(b"\x24\0\x44");
        body.extend(f64_bits.to_le_bytes());
        body.extend(b"\x24\x01");
        if trap {
            body.push(0x00);
        }
        body.push(0x0b);
        body
    };
    let code = code_bodies(&[
        &writes(F32_NEG_ZERO, F64_SNAN, false),
        &writes(F32_SNAN, F64_NEG_ZERO, true),
    ]);
    let names = exports(&[
        ("private_marker32", 3, 0),
        ("private_marker64", 3, 1),
        ("alias", 3, 0),
        ("trap", 0, 1),
    ]);
    let validated = validate_module(&module(&[
        (1, b"\x01\x60\0\0"),
        (3, b"\x02\0\0"),
        (6, &globals),
        (7, &names),
        (8, &[0]),
        (10, &code),
    ]))
    .unwrap();
    let snapshots = |instance: &swayze_wasm::Instance<'_>| {
        let f32_snapshot = instance.read_global("private_marker32").unwrap();
        assert_eq!(instance.read_global("alias"), Ok(f32_snapshot));
        [
            f32_snapshot,
            instance.read_global("private_marker64").unwrap(),
        ]
    };
    let instance = validated.instantiate().unwrap();
    let before = snapshots(&instance);
    let debug_before = [format!("{instance:?}"), format!("{instance:#?}")];
    assert_eq!(
        instance.invoke_void("trap"),
        Err(InvokeError::Trap(Trap::Unreachable))
    );
    assert_eq!(
        snapshots(&instance),
        [Value::F32(F32_SNAN), Value::F64(F64_NEG_ZERO)]
    );
    let initial_after_start = [Value::F32(F32_NEG_ZERO), Value::F64(F64_SNAN)];
    assert_eq!(before, initial_after_start); // Already returned Values did not change.
    assert_eq!(
        snapshots(&validated.instantiate().unwrap()),
        initial_after_start
    );

    for (short, pretty, kind) in [
        (
            format!("{validated:?}"),
            format!("{validated:#?}"),
            "Module",
        ),
        (
            format!("{instance:?}"),
            format!("{instance:#?}"),
            "Instance",
        ),
    ] {
        assert_eq!(
            short,
            format!(
                "{kind} {{ function_count: 2, export_count: 4, data_segment_count: 0, element_segment_count: 0, .. }}"
            )
        );
        assert_eq!(
            pretty,
            format!(
                "{kind} {{\n    function_count: 2,\n    export_count: 4,\n    data_segment_count: 0,\n    element_segment_count: 0,\n    ..\n}}"
            )
        );
    }
    assert_eq!(
        [format!("{instance:?}"), format!("{instance:#?}")],
        debug_before
    );
}

#[test]
fn global_export_index_is_framed_and_bounded_by_the_defined_count() {
    use ErrorKind::*;
    let field = IntegerField::GlobalIndex;
    for globals in [None, Some(&[0][..]), Some(GLOBALS)] {
        let prefix: Vec<_> = globals.into_iter().map(|payload| (6, payload)).collect();
        let mut cases = vec![
            (&b"\x01\x01g\x03"[..], 4, TruncatedInteger { field }),
            (&b"\x01\x01g\x03\x80"[..], 5, TruncatedInteger { field }),
            (
                &b"\x01\x01g\x03\x80\x80\x80\x80\x10"[..],
                8,
                IntegerOverflow { field },
            ),
            (
                &b"\x01\x01g\x03\x81\0"[..],
                4,
                InvalidIndex { field, index: 1 },
            ),
            (
                &b"\x01\x01g\x03\xff\xff\xff\xff\x0f"[..],
                4,
                InvalidIndex {
                    field,
                    index: u32::MAX,
                },
            ),
        ];
        if globals != Some(GLOBALS) {
            cases.push((b"\x01\x01g\x03\x80\0", 4, InvalidIndex { field, index: 0 }));
        } else {
            cases.push((b"\x01\x01g\x03\0\xff", 5, TrailingSectionBytes { id: 7 }));
        }
        for (payload, offset, kind) in cases {
            let mut bytes = module(&prefix);
            let start = push_section(&mut bytes, 7, payload);
            push_section(&mut bytes, 0, &[0]); // Next section cannot supply the missing byte.
            expect_error(&bytes, start + offset, kind);
        }
    }

    let mut globals = vec![8];
    for n in 0..8 {
        globals.extend([0x7f, 0, 0x41, n, 0x0b]);
    }
    let padded = b"\x01\x01g\x03\x87\x80\x80\x80\0";
    let validated = validate_module(&module(&[(6, &globals), (7, padded)])).unwrap();
    assert_eq!(
        validated.instantiate().unwrap().read_global("g"),
        Ok(Value::I32(7))
    );
    let mut bytes = module(&[(6, &globals)]);
    let start = push_section(&mut bytes, 7, b"\x01\x01g\x03\x08");
    expect_error(&bytes, start + 4, InvalidIndex { field, index: 8 });
    globals[0] = 9;
    let mut bytes = HEADER.to_vec();
    let start = push_section(&mut bytes, 6, &globals);
    push_section(&mut bytes, 7, GLOBAL_EXPORT);
    expect_error(
        &bytes,
        start,
        UnsupportedCount {
            field: IntegerField::GlobalCount,
            count: 9,
        },
    );
}

#[test]
fn global_export_names_share_count_utf8_budget_and_duplicate_precedence() {
    let prefix = [(1, TYPE), (3, FUNCTION), (5, ONE_PAGE), (6, GLOBALS)];
    for (first, second) in [(0, 3), (3, 0), (2, 3), (3, 2), (3, 3)] {
        let names = exports(&[("g", first, 0), ("g", second, 0)]);
        for payload in [&names[..], &names[..names.len() - 2]] {
            let mut bytes = module(&prefix);
            let start = push_section(&mut bytes, 7, payload);
            expect_error(&bytes, start + 5, ErrorKind::DuplicateExportName);
        }
    }
    let mut bytes = module(&prefix);
    // Bad UTF-8 wins even over a missing descriptor for the second entry.
    let start = push_section(&mut bytes, 7, b"\x02\x01g\x03\0\x02g\xff");
    expect_error(&bytes, start + 7, ErrorKind::InvalidUtf8Name);

    let entries = [
        ("f", 0, 0),
        ("mem", 2, 0),
        ("a", 3, 0),
        ("b", 3, 0),
        ("c", 3, 0),
        ("d", 3, 0),
        ("e", 3, 0),
        ("g", 3, 0),
        ("h", 3, 0),
    ];
    let mut bytes = module(&prefix);
    let mut valid = bytes.clone();
    push_section(&mut valid, 7, &exports(&entries[..8]));
    push_section(&mut valid, 10, CODE);
    let validated = validate_module(&valid).unwrap();
    assert_eq!(
        validated.instantiate().unwrap().read_global("g"),
        Ok(Value::I32(-2))
    );
    let start = push_section(&mut bytes, 7, &exports(&entries));
    expect_error(
        &bytes,
        start,
        ErrorKind::UnsupportedCount {
            field: IntegerField::ExportCount,
            count: 9,
        },
    );

    let limit = Limit::ExportNameBytes;
    let name = "é".repeat(limit.maximum() / 2);
    let valid = exports(&[(&name, 3, 0)]);
    let validated = validate_module(&module(&[(6, GLOBALS), (7, &valid)])).unwrap();
    assert_eq!(
        validated.instantiate().unwrap().read_global(&name),
        Ok(Value::I32(-2))
    );
    let mut bytes = module(&[(6, GLOBALS)]);
    let start = push_section(&mut bytes, 7, &exports(&[(&(name + "x"), 3, 0)]));
    expect_error(&bytes, start + 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn globals_keep_legacy_function_section_requirements_and_error_order() {
    let mixed = b"\x03\x01m\x02\0\x01g\x03\0\x01f\0\0";
    // Nonempty declarations and bodies still require the other function
    // sections, even when the exports do not reference any functions.
    for names in [&mixed[..], &[0], GLOBAL_EXPORT] {
        let valid = [
            (1, TYPE),
            (3, FUNCTION),
            (5, ONE_PAGE),
            (6, GLOBALS),
            (7, names),
            (10, CODE),
        ];
        assert!(validate_module(&module(&valid)).is_ok());
        for missing in [1, 3, 10] {
            let present: Vec<_> = valid.into_iter().filter(|&(id, _)| id != missing).collect();
            let bytes = module(&present);
            expect_error(
                &bytes,
                bytes.len(),
                ErrorKind::MissingSection { id: missing },
            );
        }
    }
    let bytes = module(&[(6, GLOBALS), (7, &[0])]);
    validate_module(&bytes).unwrap().instantiate().unwrap();
    let exports = b"\x02\x01g\x03\0\x01f\0\x7f";
    let mut bytes = module(&[(6, GLOBALS), (7, exports)]);
    // Without #3, a decodable function index is checked at the final section
    // presence check. Out-of-order sections still take precedence.
    expect_error(&bytes, bytes.len(), ErrorKind::MissingSection { id: 1 });
    let duplicate = bytes.len();
    push_section(&mut bytes, 6, GLOBALS);
    expect_error(&bytes, duplicate, ErrorKind::SectionOutOfOrder { id: 6 });
    let field = IntegerField::FunctionIndex;
    let mut bytes = module(&[(6, GLOBALS)]);
    let start = push_section(&mut bytes, 7, b"\x02\x01g\x03\0\x01f\0\x80");
    push_section(&mut bytes, 0, &[0]);
    expect_error(&bytes, start + 9, ErrorKind::TruncatedInteger { field });
    let mut bytes = module(&[(3, FUNCTION), (6, GLOBALS)]);
    let start = push_section(&mut bytes, 7, exports);
    expect_error(
        &bytes,
        start + 8,
        ErrorKind::InvalidIndex { field, index: 127 },
    );
}
