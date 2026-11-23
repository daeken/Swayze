use super::{CODE, FUNCTION, HEADER, TYPE, code_bodies, module, push_section, push_u32};
use swayze_wasm::{
    Error, ErrorKind::*, GlobalImportBinding, GlobalImportInstantiationError::*, Instance,
    InstantiationError, IntegerField::*, LengthField::*, Limit, ModuleWithGlobalImports,
    ResultKind, Trap, ValType, Value, validate_module,
    validate_module_with_global_imports as validate,
};

fn binding<'a>(module: &'a str, field: &'a str, value: Value) -> GlobalImportBinding<'a> {
    GlobalImportBinding {
        module,
        field,
        value,
    }
}

fn import_section(entries: &[(&str, &str, u8)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_u32(&mut bytes, entries.len());
    for &(module, field, ty) in entries {
        for name in [module, field] {
            push_u32(&mut bytes, name.len());
            bytes.extend_from_slice(name.as_bytes());
        }
        bytes.extend_from_slice(&[3, ty, 0]);
    }
    bytes
}

const I32_IMPORT: &[u8] = b"\x01\x01m\x01f\x03\x7f\0";

#[test]
fn import_frame_order_and_legacy_counts_still_take_precedence() {
    for prior in [2, 3] {
        let mut prefix = module(&[(prior, &[0]), (0, &[0])]);
        let offset = prefix.len();
        for (tail, relative, kind) in [
            (&[2, 0x80][..], 2, TruncatedLength { field: SectionSize }),
            (
                &[2, 0xff, 0xff, 0xff, 0xff, 0x0f][..],
                1,
                LimitExceeded {
                    limit: Limit::SectionBytes,
                },
            ),
            (
                &[2, 2, 1][..],
                3,
                TruncatedSection {
                    declared: 2,
                    available: 1,
                },
            ),
            (&[2, 0][..], 0, SectionOutOfOrder { id: 2 }),
        ] {
            prefix.extend_from_slice(tail);
            let expected = Error {
                offset: offset + relative,
                kind,
            };
            assert_eq!(validate_module(&prefix).unwrap_err(), expected);
            assert_eq!(validate(&prefix).unwrap_err(), expected);
            prefix.truncate(offset);
        }
    }
    let mut bytes = module(&[(0, &[0])]);
    let offset = push_section(&mut bytes, 2, &[0x81, 0x80, 0x80, 0x80, 0, 1, 0xff]);
    assert_eq!(
        validate_module(&bytes).unwrap_err(),
        Error {
            offset,
            kind: UnsupportedCount {
                field: ImportCount,
                count: 1
            },
        }
    );
    assert_eq!(
        validate(&bytes).unwrap_err(),
        Error {
            offset: offset + 6,
            kind: InvalidUtf8Name
        }
    );
    for payload in [&[0x81][..], &[0x81, 0x80, 0x80, 0x80, 0x10]] {
        let bytes = module(&[(2, payload), (0, &[0])]);
        assert_eq!(
            validate(&bytes).unwrap_err(),
            validate_module(&bytes).unwrap_err()
        );
    }
    let offset = push_section(&mut HEADER.to_vec(), 2, &[0]);
    assert_eq!(
        validate(&module(&[(2, &[0x89, 0, 0xff])])).unwrap_err(),
        Error {
            offset,
            kind: UnsupportedCount {
                field: ImportCount,
                count: 9
            },
        }
    );
    assert_eq!(
        validate(&module(&[(2, &[0x80, 0, 0xff])])).unwrap_err(),
        Error {
            offset: offset + 2,
            kind: TrailingSectionBytes { id: 2 },
        }
    );
}

#[test]
fn both_names_have_bounded_framed_lengths_and_whole_name_utf8_precedence() {
    for (field, prior) in [
        (ImportModuleNameSize, &[1][..]),
        (ImportFieldNameSize, &[1, 0][..]),
    ] {
        let check = |name: &[u8], relative, kind| {
            let mut bytes = HEADER.to_vec();
            let begin = push_section(&mut bytes, 2, &[prior, name].concat());
            push_section(&mut bytes, 0, &[0]);
            assert_eq!(
                validate(&bytes).unwrap_err(),
                Error {
                    offset: begin + prior.len() + relative,
                    kind
                }
            );
        };
        check(&[], 0, TruncatedLength { field });
        check(&[0x80], 1, TruncatedLength { field });
        check(&[0x80, 0x80, 0x80, 0x80, 0x10], 4, LengthOverflow { field });
        check(
            &[0x81, 0x20, 0xff],
            0,
            ImportNameTooLong {
                field,
                declared: 4097,
            },
        );
        check(
            &[5, 0xff, 0],
            3,
            TruncatedName {
                declared: 5,
                available: 2,
            },
        );
        check(&[4, b'a', 0xff, 0xe2, 0x82, 0x00], 2, InvalidUtf8Name);
        // The next byte would complete the sequence but lies outside the name.
        check(&[3, b'a', 0xe2, 0x82, 0xac], 2, InvalidUtf8Name);
    }
    let bytes = module(&[(2, b"\x81\0\x80\x80\x80\x80\0\x81\x80\x80\x80\0z\x03\x7f\0")]);
    let validated = validate(&bytes).unwrap();
    assert_eq!(validated.global_imports()[0].module(), "");
    assert_eq!(validated.global_imports()[0].field(), "z");
    validated
        .instantiate_with_globals(&[binding("", "z", Value::I32(3))])
        .unwrap();
}

#[test]
fn descriptor_kind_numeric_type_mutability_and_finish_have_exact_offsets() {
    for (tail, relative, kind) in [
        (&[][..], 0, TruncatedByte { expected: 3 }),
        (&[0, 0xff][..], 0, UnsupportedImportKind { kind: 0 }),
        (&[1][..], 0, UnsupportedImportKind { kind: 1 }),
        (&[2][..], 0, UnsupportedImportKind { kind: 2 }),
        (&[4][..], 0, UnsupportedImportKind { kind: 4 }),
        (&[0x83, 0][..], 0, UnsupportedImportKind { kind: 0x83 }),
        (&[3][..], 1, TruncatedByte { expected: 0x7f }),
        (
            &[3, 0x70, 1][..],
            1,
            UnexpectedByte {
                expected: 0x7f,
                found: 0x70,
            },
        ),
        (
            &[3, 0x7b, 1][..],
            1,
            UnexpectedByte {
                expected: 0x7f,
                found: 0x7b,
            },
        ),
        (&[3, 0x7f][..], 2, TruncatedGlobalMutability),
        (
            &[3, 0x7c, 1, 0xff][..],
            2,
            MutableGlobalImport { import_index: 1 },
        ),
        (
            &[3, 0x7f, 0x80, 0][..],
            2,
            InvalidGlobalMutability { byte: 0x80 },
        ),
        (&[3, 0x7f, 0, 0xff][..], 3, TrailingSectionBytes { id: 2 }),
    ] {
        let prefix = b"\x02\x01m\x01f\x03\x7f\0\x01a\x01b";
        let mut bytes = HEADER.to_vec();
        let start = push_section(&mut bytes, 2, &[prefix, tail].concat());
        push_section(&mut bytes, 0, &[0]);
        assert_eq!(
            validate(&bytes).unwrap_err(),
            Error {
                offset: start + prefix.len() + relative,
                kind
            }
        );
    }
}

#[test]
fn exact_aggregate_name_payload_and_combined_slot_caps() {
    let module_name = "é".repeat(2048);
    let field_name = "b".repeat(4096);
    let imports = import_section(&[(module_name.as_str(), field_name.as_str(), 0x7f); 8]);
    assert!(imports.len() > 64 * 1024); // Lengths/descriptors do not count as name payload.
    let exports = b"\x02\x01a\x03\0\x01b\x03\x87\0";
    let validated = validate(&module(&[(2, &imports), (6, &[0x80, 0]), (7, exports)])).unwrap();
    let manifest = validated.global_imports();
    assert_eq!(manifest.len(), 8);
    assert_eq!(
        manifest
            .iter()
            .map(|entry| entry.module().len() + entry.field().len())
            .sum::<usize>(),
        64 * 1024
    );
    let bindings: [_; 8] =
        std::array::from_fn(|i| binding(&module_name, &field_name, Value::I32(i as i32)));
    let instance = validated.instantiate_with_globals(&bindings).unwrap();
    assert_eq!(instance.read_global("a"), Ok(Value::I32(0)));
    assert_eq!(instance.read_global("b"), Ok(Value::I32(7)));

    let mut bytes = module(&[(2, &imports)]);
    let offset = push_section(&mut bytes, 6, &[0x81, 0, 0xff]);
    assert_eq!(
        validate(&bytes).unwrap_err(),
        Error {
            offset,
            kind: UnsupportedCount {
                field: GlobalCount,
                count: 1
            },
        }
    );
    let mut bytes = module(&[(2, I32_IMPORT)]);
    let offset = push_section(&mut bytes, 6, &[8]);
    assert_eq!(
        validate(&bytes).unwrap_err(),
        Error {
            offset,
            kind: UnsupportedCount {
                field: GlobalCount,
                count: 8
            },
        }
    );
    let mut bytes = module(&[(2, I32_IMPORT)]);
    let offset = push_section(&mut bytes, 7, b"\x01\x01x\x03\x81\0");
    assert_eq!(
        validate(&bytes).unwrap_err(),
        Error {
            offset: offset + 4,
            kind: InvalidIndex {
                field: GlobalIndex,
                index: 1
            },
        }
    );
}

#[test]
fn repeated_heterogeneous_slots_copy_exact_bits_into_code_start_and_reexports() {
    const MODULE: &str = "é\0sensitive-module";
    const FIELD: &str = "雪-sensitive-field";
    let validated = {
        let imports = import_section(&[0x7f, 0x7e, 0x7d, 0x7c].map(|ty| (MODULE, FIELD, ty)));
        let code = code_bodies(&[
            b"\0\x23\x04\x23\0\x6a\x24\x04\x0b", // Start adds slot 0 to the initialized slot 4.
            b"\0\x23\x07\x0b",                   // Return the f64 copy in the last slot.
        ]);
        validate(&module(&[
            (1, b"\x02\x60\0\0\x60\0\x01\x7c"),
            (2, &imports), (3, &[2, 0, 1]),
            (6, b"\x04\x7f\x01\x23\x80\0\x0b\x7e\0\x23\x01\x0b\x7d\0\x23\x02\x0b\x7c\0\x23\x03\x0b"),
            (7, b"\x08\x01a\x03\0\x01b\x03\x01\x01c\x03\x02\x01d\x03\x03\x01A\x03\x04\x01B\x03\x05\x01C\x03\x06\x01v\0\x01"),
            (8, &[0]), (10, &code),
        ])).unwrap()
    }; // Manifest names must not borrow the binary.
    for (entry, ty) in validated.global_imports().iter().zip([
        ValType::I32,
        ValType::I64,
        ValType::F32,
        ValType::F64,
    ]) {
        assert_eq!(
            (entry.module(), entry.field(), entry.value_type()),
            (MODULE, FIELD, ty)
        );
    }
    fn instantiate<'a>(
        validated: &'a ModuleWithGlobalImports,
        values: [Value; 4],
        module: &str,
        field: &str,
    ) -> Instance<'a> {
        let bindings = values.map(|value| binding(module, field, value));
        validated.instantiate_with_globals(&bindings).unwrap()
    }
    let first_values = [
        Value::I32(-3),
        Value::I64(-0x0100_0000_0000_007f),
        Value::F32(0xff80_1234),
        Value::F64(0x8000_0000_0000_0000),
    ];
    let second_values = [
        Value::I32(12),
        Value::I64(i64::MIN),
        Value::F32(0x8000_0000),
        Value::F64(0x7ff0_0000_0000_0001),
    ];
    // Both the binding slice and its name storage may die before instance use.
    let first = {
        let module_name = String::from(MODULE);
        let field_name = String::from(FIELD);
        instantiate(&validated, first_values, &module_name, &field_name)
    };
    let second = instantiate(&validated, second_values, MODULE, FIELD);
    for (instance, values, start_result) in
        [(&first, first_values, -6), (&second, second_values, 24)]
    {
        for (name, value) in ["a", "b", "c", "d"].into_iter().zip(values) {
            assert_eq!(instance.read_global(name), Ok(value));
        }
        assert_eq!(instance.read_global("A"), Ok(Value::I32(start_result)));
        assert_eq!(instance.read_global("B"), Ok(values[1]));
        assert_eq!(instance.read_global("C"), Ok(values[2]));
        assert_eq!(
            instance.invoke_with_values("v", &[], ResultKind::F64),
            Ok(Some(values[3]))
        );
    }
    assert_eq!(format!("{first:?}"), format!("{second:?}"));
    assert_eq!(format!("{first:#?}"), format!("{second:#?}"));
    for formatted in [
        format!("{validated:?}"),
        format!("{validated:#?}"),
        format!("{:#?}", validated.global_imports()),
        format!("{first:#?}"),
    ] {
        assert!(!formatted.contains("sensitive-"));
        assert!(!formatted.contains("4286583348")); // f32 payload, if exposed as decimal.
    }
}

#[test]
fn assertions_win_over_segment_preflights_and_start_in_ordinal_order() {
    let imports = import_section(&[("é", "secret-field", 0x7f), ("b", "y", 0x7e)]);
    let trap = b"\x01\x03\0\0\x0b";
    let make_module = |element_offset, data_offset| {
        validate(&module(&[
            (1, b"\x01\x60\0\0"),
            (2, &imports),
            (3, &[1, 0]),
            (4, &[1, 0x70, 0, 0]),
            (5, &[1, 0, 0]),
            (8, &[0]),
            (9, &[1, 0, 0x41, element_offset, 0x0b, 0]),
            (10, trap),
            (11, &[1, 0, 0x41, data_offset, 0x0b, 0]),
        ]))
        .unwrap()
    };
    let broken = make_module(1, 1);
    let good = || {
        [
            binding("é", "secret-field", Value::I32(0)),
            binding("b", "y", Value::I64(0)),
        ]
    };
    let error = |bindings: &[GlobalImportBinding<'_>]| {
        broken.instantiate_with_globals(bindings).unwrap_err()
    };
    assert_eq!(
        error(&[]),
        BindingCountMismatch {
            expected: 2,
            actual: 0
        }
    );
    assert_eq!(
        error(&[binding("wrong", "wrong", Value::F64(0))]),
        BindingCountMismatch {
            expected: 2,
            actual: 1
        }
    );
    assert_eq!(
        error(&[
            binding("wrong", "wrong", Value::F32(0)),
            binding("wrong", "wrong", Value::F32(0)),
            binding("", "", Value::I64(0))
        ]),
        BindingCountMismatch {
            expected: 2,
            actual: 3
        }
    );
    let large = "x".repeat(1 << 16);
    for wrong_module in ["e\u{301}", "ê", &large] {
        let mut entries = good();
        entries[0] = binding(wrong_module, "secret-ghost", Value::I64(987_654_321));
        entries[1] = binding("wrong", "wrong", Value::I32(0));
        let why = error(&entries);
        assert_eq!(why, ModuleNameMismatch { import_index: 0 });
        for text in [format!("{why:#?}"), why.to_string()] {
            assert!(!text.contains("secret-"));
            assert!(!text.contains("987654321"));
        }
        entries[0].module = "é";
        assert_eq!(error(&entries), FieldNameMismatch { import_index: 0 });
        entries[0].field = "secret-field";
        assert_eq!(
            error(&entries),
            ValueTypeMismatch {
                import_index: 0,
                expected: ValType::I32,
                actual: ValType::I64
            }
        );
        entries[0].value = Value::I32(0);
        assert_eq!(error(&entries), ModuleNameMismatch { import_index: 1 });
        entries[1].module = "b";
        assert_eq!(error(&entries), FieldNameMismatch { import_index: 1 });
        entries[1].field = "y";
        assert_eq!(
            error(&entries),
            ValueTypeMismatch {
                import_index: 1,
                expected: ValType::I64,
                actual: ValType::I32
            }
        );
    }
    let element = InstantiationError::ElementSegmentOutOfBounds { segment_index: 0 };
    assert_eq!(error(&good()), Instantiation(element));
    assert_eq!(
        make_module(0, 1)
            .instantiate_with_globals(&good())
            .unwrap_err(),
        Instantiation(InstantiationError::DataSegmentOutOfBounds { segment_index: 0 })
    );
    let start = InstantiationError::StartTrap(Trap::Unreachable);
    let wrapped = make_module(0, 0)
        .instantiate_with_globals(&good())
        .unwrap_err();
    assert_eq!(wrapped, Instantiation(start));
    assert_eq!(
        std::error::Error::source(&wrapped)
            .unwrap()
            .downcast_ref::<InstantiationError>(),
        Some(&start)
    );
}

#[test]
fn code_rejects_setting_an_import_even_in_dead_code_before_operand_checks() {
    let mut bytes = module(&[(1, b"\x01\x60\0\0"), (2, I32_IMPORT), (3, &[1, 0])]);
    let code = code_bodies(&[b"\0\0\x44\0\0\0\0\0\0\0\0\x24\x80\0\x0b"]);
    let offset = push_section(&mut bytes, 10, &code);
    assert_eq!(
        validate(&bytes).unwrap_err(),
        Error {
            offset: offset + 14,
            kind: ImmutableGlobal { index: 0 }
        }
    );
}

#[test]
fn initializer_c_prime_sees_only_imports_and_keeps_exact_type_and_frame_checks() {
    // Slot 1 is immutable and statically initialized, but not in C′.
    let prefix = b"\x02\x7f\0\x41\0\x0b\x7f\0";
    for (expr, relative, kind) in [
        (
            &[0x23, 0x80][..],
            2,
            TruncatedInteger { field: GlobalIndex },
        ),
        (
            &[0x23, 0x80, 0x80, 0x80, 0x80, 0x10][..],
            5,
            IntegerOverflow { field: GlobalIndex },
        ),
        (
            &[0x23, 0x81, 0, 0xff][..],
            1,
            InvalidIndex {
                field: GlobalIndex,
                index: 1,
            },
        ),
        (&[0x23, 0][..], 2, TruncatedByte { expected: 0x0b }),
        (
            &[0x23, 0, 0x41][..],
            2,
            UnexpectedByte {
                expected: 0x0b,
                found: 0x41,
            },
        ),
        (
            &[0x42][..],
            0,
            UnexpectedByte {
                expected: 0x41,
                found: 0x42,
            },
        ),
    ] {
        let mut bytes = module(&[(2, I32_IMPORT)]);
        let offset = push_section(&mut bytes, 6, &[prefix, expr].concat());
        push_section(&mut bytes, 0, &[0]);
        assert_eq!(
            validate(&bytes).unwrap_err(),
            Error {
                offset: offset + prefix.len() + relative,
                kind
            }
        );
    }
    let mut bytes = module(&[(2, I32_IMPORT)]);
    let offset = push_section(&mut bytes, 6, b"\x01\x7d\0\x23\0\xff");
    assert_eq!(
        validate(&bytes).unwrap_err(),
        Error {
            offset: offset + 3,
            kind: TypeMismatch {
                expected: ValType::F32,
                actual: ValType::I32
            },
        }
    );
    let mut bytes = HEADER.to_vec();
    let offset = push_section(&mut bytes, 6, b"\x01\x7f\0\x23\0\x0b");
    assert_eq!(
        validate(&bytes).unwrap_err(),
        Error {
            offset: offset + 4,
            kind: InvalidIndex {
                field: GlobalIndex,
                index: 0
            }
        }
    );
    assert_eq!(
        validate_module(&bytes).unwrap_err(),
        Error {
            offset: offset + 3,
            kind: UnexpectedByte {
                expected: 0x41,
                found: 0x23
            }
        }
    );
}

#[test]
fn full_c_offset_checks_precede_the_static_only_restriction_in_both_segment_kinds() {
    let imports = import_section(&[("m", "i", 0x7f), ("m", "d", 0x7c)]);
    let globals = b"\x03\x7f\0\x23\0\x0b\x7c\x01\x23\x01\x0b\x7f\0\x41\0\x0b";
    for (section, target, limits, target_field, missing) in [
        (9, 4, &[1, 0x70, 0, 0][..], TableIndex, MissingTable),
        (11, 5, &[1, 0, 0][..], MemoryIndex, MissingMemory),
    ] {
        let check = |presence: &[u8], payload: &[u8], relative, kind| {
            let mut bytes = module(&[(2, &imports), (target, presence), (6, globals)]);
            let offset = push_section(&mut bytes, section, payload);
            push_section(&mut bytes, 0, &[0]);
            assert_eq!(
                validate(&bytes).unwrap_err(),
                Error {
                    offset: offset + relative,
                    kind
                }
            );
        };
        check(
            &[0],
            &[1, 1, 0x23, 0, 0x0b],
            1,
            InvalidIndex {
                field: target_field,
                index: 1,
            },
        );
        check(&[0], &[1, 0, 0x23, 0, 0x0b], 1, missing);
        let read = |expr: &[u8], relative: usize, kind| {
            check(limits, &[&[1, 0][..], expr].concat(), 2 + relative, kind)
        };
        read(&[0x23, 0x80], 2, TruncatedInteger { field: GlobalIndex });
        read(
            &[0x23, 5],
            1,
            InvalidIndex {
                field: GlobalIndex,
                index: 5,
            },
        );
        read(&[0x23, 3], 1, MutableGlobalInSegmentOffset { index: 3 });
        read(
            &[0x23, 1],
            0,
            TypeMismatch {
                expected: ValType::I32,
                actual: ValType::F64,
            },
        );
        for index in [0, 2] {
            read(
                &[0x23, 0x80 | index, 0],
                3,
                TruncatedByte { expected: 0x0b },
            );
            read(
                &[0x23, 0x80 | index, 0, 0x41],
                3,
                UnexpectedByte {
                    expected: 0x0b,
                    found: 0x41,
                },
            );
            read(
                &[0x23, 0x80 | index, 0, 0x0b],
                1,
                BindingDependentSegmentOffset {
                    index: index.into(),
                },
            );
        }
        let static_offsets = b"\x02\0\x23\x84\0\x0b\0\0\x41\0\x0b\0";
        let validated = validate(&module(&[
            (2, &imports),
            (target, limits),
            (6, globals),
            (section, static_offsets),
        ]))
        .unwrap();
        validated
            .instantiate_with_globals(&[
                binding("m", "i", Value::I32(-1)),
                binding("m", "d", Value::F64(u64::MAX)),
            ])
            .unwrap();
    }
}

#[test]
fn omitted_and_padded_empty_imports_keep_defined_only_execution() {
    for imports in [None, Some(&[0][..]), Some(&[0x80, 0x80, 0x80, 0x80, 0])] {
        let mut bytes = module(&[(1, TYPE)]);
        if let Some(imports) = imports {
            push_section(&mut bytes, 2, imports);
        }
        for (id, section) in [
            (3, FUNCTION),
            (6, &[1, 0x7f, 0, 0x41, 42, 0x0b]),
            (7, b"\x02\x01x\0\0\x01g\x03\0"),
            (10, CODE),
        ] {
            push_section(&mut bytes, id, section);
        }
        let legacy = validate_module(&bytes).unwrap();
        let wrapped = validate(&bytes).unwrap();
        assert!(wrapped.global_imports().is_empty());
        assert_eq!(
            wrapped
                .instantiate_with_globals(&[binding("", "", Value::I32(0))])
                .unwrap_err(),
            BindingCountMismatch {
                expected: 0,
                actual: 1
            }
        );
        for instance in [
            legacy.instantiate().unwrap(),
            wrapped.instantiate_with_globals(&[]).unwrap(),
        ] {
            assert_eq!(instance.invoke_i32("x"), Ok(42));
            assert_eq!(instance.read_global("g"), Ok(Value::I32(42)));
        }
    }
}
