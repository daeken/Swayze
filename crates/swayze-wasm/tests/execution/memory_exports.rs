use super::*;
use swayze_wasm::{InstantiationError, ReadMemoryError, ResultKind, Value};

const ONE_PAGE: &[u8] = &[1, 0, 1];
const ZERO_PAGES: &[u8] = &[1, 1, 0, 1];
const PAGE: u32 = 65_536;
const MEM: &[u8] = b"\x01\x03mem\x02\0";

fn exports(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
    let mut payload = Vec::new();
    push_u32(&mut payload, entries.len());
    for &(name, kind, index) in entries {
        push_u32(&mut payload, name.len());
        payload.extend_from_slice(name.as_bytes());
        payload.push(kind);
        payload.extend_from_slice(index);
    }
    payload
}

#[test]
fn memory_only_exports_are_owned_exact_utf8_aliases_and_copies() {
    let composed = "caf\u{e9}";
    let decomposed = "cafe\u{301}";
    let validated = {
        let names = exports(&[
            (composed, 2, &[0x80, 0x80, 0x80, 0x80, 0]),
            (decomposed, 2, &[0]),
            ("nul\0name", 2, &[0x80, 0]),
            ("touch\u{e9}", 2, &[0]),
            ("", 2, &[0]),
        ]);
        let data = b"\x02\0\x41\0\x0b\x04ABCD\0\x41\x01\x0b\x02xy";
        validate_module(&module(&[(5, ONE_PAGE), (7, &names), (11, data)])).unwrap()
    };
    let instance = validated.instantiate().unwrap();
    let mut dest = [0xff; 5];
    for name in [composed, decomposed, "nul\0name", "touch\u{e9}", ""] {
        instance.read_memory(name, 0, &mut dest).unwrap();
        assert_eq!(&dest, b"AxyD\0");
        dest.fill(0x55); // A host write affects only the detached caller buffer.
    }
    for name in ["Caf\u{e9}", "cafe", "nul", "nul\0name\0", "touche\u{301}"] {
        assert_eq!(
            instance.read_memory(name, u32::MAX, &mut dest),
            Err(ReadMemoryError::ExportNotFound)
        );
        assert_eq!(dest, [0x55; 5]);
    }
}

#[test]
fn a_read_validates_the_whole_current_range_before_copying() {
    // Initialize the last byte, so an incorrect partial copy of that byte is observable.
    let data = b"\x01\0\x41\xff\xff\x03\x0b\x01\xab";
    let validated = validate_module(&module(&[(5, ONE_PAGE), (7, MEM), (11, data)])).unwrap();
    let instance = validated.instantiate().unwrap();
    let mut dest = [0xcc; 2];
    instance
        .read_memory("mem", PAGE - 1, &mut dest[1..])
        .unwrap();
    assert_eq!(dest, [0xcc, 0xab]);
    let mut full = vec![0xcc; PAGE as usize];
    instance.read_memory("mem", 0, &mut full).unwrap();
    assert_eq!(full.last(), Some(&0xab));
    assert!(full[..full.len() - 1].iter().all(|&b| b == 0));

    assert_eq!(instance.read_memory("mem", PAGE, &mut []), Ok(()));
    for offset in [PAGE - 1, PAGE, PAGE + 1, 0x8000_0000, u32::MAX] {
        assert_eq!(
            instance.read_memory("mem", offset, &mut dest),
            Err(ReadMemoryError::OutOfBounds)
        );
        assert_eq!(dest, [0xcc, 0xab]);
    }
    assert_eq!(
        instance.read_memory("mem", PAGE + 1, &mut []),
        Err(ReadMemoryError::OutOfBounds)
    );
    full.push(0xcc);
    full.fill(0xcc);
    assert_eq!(
        instance.read_memory("mem", 0, &mut full),
        Err(ReadMemoryError::OutOfBounds)
    );
    assert!(full.iter().all(|&b| b == 0xcc));
}

#[test]
fn wrong_kind_and_missing_precede_range_or_signature_checks_without_effects() {
    let ty = &[2, 0x60, 1, 0x7d, 1, 0x7d, 0x60, 0, 1, 0x7f];
    let code = code_bodies(&[
        &[0, 0x41, 0, 0x20, 0, 0x38, 2, 0, 0x20, 0, 0x0b], // Store and return f32.
        &[0, 0x41, 42, 0x0b],
    ]);
    let names = exports(&[("mix", 0, &[0]), ("mem", 2, &[0]), ("answer", 0, &[1])]);
    let validated = validate_module(&module(&[
        (1, ty),
        (3, &[2, 0, 1]),
        (5, ONE_PAGE),
        (7, &names),
        (10, &code),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    let mut dest = [0x55; 4];
    for (name, read, invoke) in [
        ("mix", ReadMemoryError::ExportNotMemory, None),
        (
            "Mem",
            ReadMemoryError::ExportNotFound,
            Some(InvokeError::ExportNotFound),
        ),
        (
            "mem",
            ReadMemoryError::OutOfBounds,
            Some(InvokeError::ExportNotFunction),
        ),
    ] {
        assert_eq!(instance.read_memory(name, u32::MAX, &mut dest), Err(read));
        assert_eq!(dest, [0x55; 4]);
        if let Some(error) = invoke {
            assert_eq!(instance.invoke_i32(name), Err(error));
            assert_eq!(instance.invoke_i32_with_args(name, &[0; 5]), Err(error));
            assert_eq!(instance.invoke_void(name), Err(error));
            assert_eq!(instance.invoke_void_with_args(name, &[0]), Err(error));
            assert_eq!(
                instance.invoke_with_values(name, &[Value::F32(1); 5], ResultKind::F32),
                Err(error)
            );
        }
    }
    assert_eq!(
        instance.read_memory("mix", 0, &mut []),
        Err(ReadMemoryError::ExportNotMemory)
    );
    assert_eq!(
        instance.read_memory("Mem", 0, &mut []),
        Err(ReadMemoryError::ExportNotFound)
    );
    assert_eq!(instance.invoke_i32("answer"), Ok(42));
    assert_eq!(
        instance.invoke_i32_with_args("mix", &[]),
        Err(InvokeError::ArityMismatch {
            expected: 1,
            actual: 0
        })
    );
    assert_eq!(
        instance.invoke_i32_with_args("mix", &[1]),
        Err(InvokeError::ResultMismatch {
            expected: ResultKind::I32,
            actual: ResultKind::F32
        })
    );
    // Function and memory both have index zero in their own index spaces.
    // Isolate each signature error and also try the exact function signature:
    // the memory name must never reach signature preflight or guest dispatch.
    let f32_arg = [Value::F32(0x7f80_1234)];
    for (args, result, expected) in [
        (
            &[][..],
            ResultKind::F32,
            InvokeError::ArityMismatch {
                expected: 1,
                actual: 0,
            },
        ),
        (
            &f32_arg[..],
            ResultKind::I32,
            InvokeError::ResultMismatch {
                expected: ResultKind::I32,
                actual: ResultKind::F32,
            },
        ),
        (
            &[Value::I32(1)],
            ResultKind::F32,
            InvokeError::ArgumentTypeMismatch {
                index: 0,
                expected: swayze_wasm::ValType::F32,
                actual: swayze_wasm::ValType::I32,
            },
        ),
    ] {
        assert_eq!(
            instance.invoke_with_values("mix", args, result),
            Err(expected)
        );
        assert_eq!(
            instance.invoke_with_values("mem", args, result),
            Err(InvokeError::ExportNotFunction)
        );
    }
    assert_eq!(
        instance.invoke_with_values("mem", &f32_arg, ResultKind::F32),
        Err(InvokeError::ExportNotFunction)
    );
    instance.read_memory("mem", 0, &mut dest).unwrap();
    assert_eq!(dest, [0; 4]);
    let bits = 0x7f80_1234_u32;
    let arg = Value::F32(bits);
    assert_eq!(
        instance.invoke_with_values("mix", &[arg], ResultKind::F32),
        Ok(Some(arg))
    );
    instance.read_memory("mem", 0, &mut dest).unwrap();
    assert_eq!(dest, bits.to_le_bytes());
}

#[test]
fn binary_index_and_name_diagnostics_are_section_framed() {
    use ErrorKind::*;
    let field = IntegerField::MemoryIndex;
    for memory in [None, Some(&[0][..]), Some(ONE_PAGE)] {
        let cases = [
            (&b"\x01\x01m\x02"[..], 4, TruncatedInteger { field }),
            (b"\x01\x01m\x02\x80", 5, TruncatedInteger { field }),
            (
                b"\x01\x01m\x02\x80\x80\x80\x80\x10",
                8,
                IntegerOverflow { field },
            ),
            (b"\x01\x01m\x02\x81\0", 4, InvalidIndex { field, index: 1 }),
            (b"\x01\x02x\xff\x02\0", 3, InvalidUtf8Name),
            (
                b"\x01\x02x",
                3,
                TruncatedName {
                    declared: 2,
                    available: 1,
                },
            ),
            (b"\x01\x01m", 3, TruncatedByte { expected: 0 }),
            (
                b"\x01\x01m\x01\0",
                3,
                UnexpectedByte {
                    expected: 0,
                    found: 1,
                },
            ),
            (
                b"\x01\x01m\x04\0",
                3,
                UnexpectedByte {
                    expected: 0,
                    found: 4,
                },
            ),
            (
                b"\x01\x01m\x82\0",
                3,
                UnexpectedByte {
                    expected: 0,
                    found: 0x82,
                },
            ),
        ];
        for (payload, index, kind) in cases.into_iter().chain([if memory == Some(ONE_PAGE) {
            (
                &b"\x01\x01m\x02\0\x00"[..],
                5,
                TrailingSectionBytes { id: 7 },
            )
        } else {
            (&b"\x01\x01m\x02\x80\0"[..], 4, MissingMemory)
        }]) {
            let mut bytes = HEADER.to_vec();
            if let Some(memory) = memory {
                push_section(&mut bytes, 5, memory);
            }
            let start = push_section(&mut bytes, 7, payload);
            push_section(&mut bytes, 0, &[0]); // Must not supply the missing terminal byte.
            expect_error(&bytes, start + index, kind);
        }
    }
}

#[test]
fn names_share_count_length_and_duplicate_checks_across_kinds() {
    let prefix = [(1, TYPE), (3, FUNCTION), (5, ONE_PAGE)];
    for (first, second) in [(0, 2), (2, 0), (2, 2)] {
        let names = exports(&[("a", first, &[0]), ("a", second, &[0])]);
        // A duplicate is rejected across kinds, even without its descriptor.
        for payload in [&names[..], &names[..names.len() - 2]] {
            let mut bytes = module(&prefix);
            let start = push_section(&mut bytes, 7, payload);
            expect_error(&bytes, start + 5, ErrorKind::DuplicateExportName);
        }
    }

    let entries = ["a", "b", "c", "d", "e", "f", "g", "h", "i"].map(|name| {
        let kind = if name == "a" { 0 } else { 2 };
        (name, kind, &[0][..])
    });
    let mut bytes = module(&prefix);
    let mut accepted = bytes.clone();
    push_section(&mut accepted, 7, &exports(&entries[..8]));
    push_section(&mut accepted, 10, CODE);
    let validated = validate_module(&accepted).unwrap();
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("a"), Ok(42));
    for &(name, _, _) in &entries[1..8] {
        assert_eq!(instance.read_memory(name, PAGE, &mut []), Ok(()));
    }
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
    let max_name = "\u{e9}".repeat(limit.maximum() / 2);
    let valid = exports(&[(&max_name, 2, &[0])]);
    let validated = validate_module(&module(&[(5, ONE_PAGE), (7, &valid)])).unwrap();
    assert_eq!(
        validated
            .instantiate()
            .unwrap()
            .read_memory(&max_name, 0, &mut []),
        Ok(())
    );
    let over = exports(&[(&(max_name + "a"), 2, &[0])]);
    bad_payload(7, &over, 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn memory_exports_do_not_change_legacy_function_section_precedence() {
    let valid = [
        (1, TYPE),
        (3, FUNCTION),
        (5, ONE_PAGE),
        (7, b"\x02\x03mem\x02\0\x06answer\0\0"),
        (10, CODE),
    ];
    for names in [valid[3].1, EXPORT, &[0]] {
        let mut sections = valid;
        sections[3].1 = names;
        assert!(validate_module(&module(&sections)).is_ok());
        for missing in [1, 3, 10] {
            let present: Vec<_> = sections
                .into_iter()
                .filter(|&(id, _)| id != missing)
                .collect();
            let bytes = module(&present);
            expect_error(
                &bytes,
                bytes.len(),
                ErrorKind::MissingSection { id: missing },
            );
        }
    }
    for sections in [
        &[(7, &[0][..])][..], // Legacy empty export vector.
        &[(5, ONE_PAGE), (7, b"\x02\x03mem\x02\0\x01f\0\x7f")], // Unbound function index.
        &[(3, FUNCTION), (5, ONE_PAGE), (7, MEM)], // Explicit function section still opts in.
    ] {
        let bytes = module(sections);
        expect_error(&bytes, bytes.len(), ErrorKind::MissingSection { id: 1 });
    }
    let bytes = module(&[(1, TYPE), (5, ONE_PAGE), (7, MEM)]);
    expect_error(&bytes, bytes.len(), ErrorKind::MissingSection { id: 3 });
    let mut bytes = module(&[(1, TYPE), (5, ONE_PAGE)]);
    let start = push_section(&mut bytes, 7, b"\x02\x03mem\x02\0\x01f\0\x80");
    push_section(&mut bytes, 0, &[0]);
    expect_error(
        &bytes,
        start + 11,
        ErrorKind::TruncatedInteger {
            field: IntegerField::FunctionIndex,
        },
    );
    let mut bytes = module(&[(5, ONE_PAGE), (7, b"\x02\x03mem\x02\0\x01f\0\x7f")]);
    let duplicate = bytes.len();
    push_section(&mut bytes, 5, ONE_PAGE);
    expect_error(&bytes, duplicate, ErrorKind::SectionOutOfOrder { id: 5 });
    let mut bytes = module(&valid[..3]);
    let start = push_section(&mut bytes, 7, b"\x02\x03mem\x02\0\x01f\0\x01");
    expect_error(
        &bytes,
        start + 10,
        ErrorKind::InvalidIndex {
            field: IntegerField::FunctionIndex,
            index: 1,
        },
    );
}

#[test]
fn unexported_memory_stays_hidden_even_when_a_function_can_load_it() {
    let body = &[0, 0x41, 0, 0x2d, 0, 0, 0x0b];
    let data = b"\x01\0\x41\0\x0b\x01\x34";
    let validated = validate_module(&module(&[
        (1, TYPE),
        (3, FUNCTION),
        (5, ONE_PAGE),
        (7, EXPORT),
        (10, &code_body(body)),
        (11, data),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("answer"), Ok(0x34));
    let mut dest = [0xaa];
    assert_eq!(
        instance.read_memory("mem", 0, &mut dest),
        Err(ReadMemoryError::ExportNotFound)
    );
    assert_eq!(
        instance.read_memory("answer", 0, &mut dest),
        Err(ReadMemoryError::ExportNotMemory)
    );
    assert_eq!(dest, [0xaa]);
}

#[test]
fn reads_see_start_after_data_and_do_not_bypass_failed_instantiation() {
    let with_start = |memory, body, data| {
        validate_module(&module(&[
            (1, &[1, 0x60, 0, 0]),
            (3, FUNCTION),
            (5, memory),
            (7, MEM),
            (8, &[0]),
            (10, &code_body(body)),
            (11, data),
        ]))
        .unwrap()
    };
    let data = b"\x01\0\x41\0\x0b\x02AB";
    // Increment the data byte at offset zero.
    let start = &[
        0, 0x41, 0, 0x41, 0, 0x2d, 0, 0, 0x41, 1, 0x6a, 0x3a, 0, 0, 0x0b,
    ];
    let initialized = with_start(ONE_PAGE, start, data);
    let mut dest = [0; 2];
    initialized
        .instantiate()
        .unwrap()
        .read_memory("mem", 0, &mut dest)
        .unwrap();
    assert_eq!(&dest, b"BB");

    // Grow from zero and write at offset one.
    let start = &[
        0, 0x41, 1, 0x40, 0, 0x1a, 0x41, 1, 0x41, 42, 0x3a, 0, 0, 0x0b,
    ];
    let grown = with_start(ZERO_PAGES, start, &[0]);
    grown
        .instantiate()
        .unwrap()
        .read_memory("mem", 0, &mut dest)
        .unwrap();
    assert_eq!(dest, [0, 42]);
    assert_eq!(
        with_start(ZERO_PAGES, start, data)
            .instantiate()
            .unwrap_err(),
        InstantiationError::DataSegmentOutOfBounds { segment_index: 0 }
    );

    let trapped = [&start[..start.len() - 1], &[0x00, 0x0b]].concat();
    assert_eq!(
        with_start(ZERO_PAGES, &trapped, &[0])
            .instantiate()
            .unwrap_err(),
        InstantiationError::StartTrap(Trap::Unreachable)
    );
}

#[test]
fn invocation_growth_is_visible_without_changing_fuel_or_debug() {
    let limit = ExecutionLimit::InstructionDispatches;
    let body = |extra| {
        let mut code = vec![
            0, 0x03, 0x40, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b,
        ];
        // Five dispatches per loop iteration plus const and grow. The optional
        // nop makes grow the first instruction over the execution limit.
        code.extend(std::iter::repeat_n(0x01, (limit.maximum() - 2) % 5 + extra));
        code.extend([0x41, 1, 0x40, 0, 0x0b]);
        code
    };
    let store_then_trap = &[0, 0x41, 0, 0x41, 42, 0x3a, 0, 0, 0x00, 0x0b];
    let code = code_bodies(&[&body(0), &body(1), store_then_trap]);
    let names = exports(&[
        ("mem", 2, &[0]),
        ("grow", 0, &[0]),
        ("limit", 0, &[1]),
        ("trap", 0, &[2]),
        ("alias", 2, &[0]),
    ]);
    let validated = validate_module(&module(&[
        (1, &[2, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 0, 0]),
        (3, &[3, 0, 0, 1]),
        (5, ZERO_PAGES),
        (7, &names),
        (10, &code),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    let other = validated.instantiate().unwrap();
    let mut dest = [0xaa];
    for offset in [0, 1, PAGE] {
        assert_eq!(
            instance.read_memory("mem", offset, &mut dest),
            Err(ReadMemoryError::OutOfBounds)
        );
        assert_eq!(dest, [0xaa]);
    }
    assert_eq!(instance.read_memory("mem", 0, &mut []), Ok(()));
    assert_eq!(
        instance.read_memory("mem", 1, &mut []),
        Err(ReadMemoryError::OutOfBounds)
    );
    let args = &[i32::try_from((limit.maximum() - 2) / 5).unwrap()];
    assert_eq!(
        instance.invoke_i32_with_args("limit", args),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(
        instance.read_memory("alias", 0, &mut dest),
        Err(ReadMemoryError::OutOfBounds)
    );
    assert_eq!(instance.invoke_i32_with_args("grow", args), Ok(0));
    assert_eq!(instance.read_memory("mem", PAGE, &mut []), Ok(()));
    instance.read_memory("alias", 0, &mut dest).unwrap();
    assert_eq!(dest, [0]);
    assert_eq!(
        instance.invoke_void("trap"),
        Err(InvokeError::Trap(Trap::Unreachable))
    );
    for name in ["mem", "alias"] {
        instance.read_memory(name, 0, &mut dest).unwrap();
        assert_eq!(dest, [42]);
    }
    assert_eq!(
        other.read_memory("mem", 0, &mut dest),
        Err(ReadMemoryError::OutOfBounds)
    );
    assert_eq!(dest, [42]);
    // The same at-cap path still has the same budget and respects the page cap.
    assert_eq!(instance.invoke_i32_with_args("grow", args), Ok(-1));
    assert_eq!(other.invoke_i32_with_args("grow", args), Ok(0));
    other.read_memory("mem", 0, &mut dest).unwrap();
    assert_eq!(dest, [0]);

    assert_eq!(
        format!("{instance:?}"),
        "Instance { function_count: 3, export_count: 5, data_segment_count: 0, element_segment_count: 0, .. }"
    );
    assert_eq!(format!("{instance:#?}"), format!("{other:#?}"));
    let fresh = validated.instantiate().unwrap();
    assert_eq!(format!("{fresh:#?}"), format!("{instance:#?}"));
    assert_eq!(
        format!("{validated:?}"),
        "Module { function_count: 3, export_count: 5, data_segment_count: 0, element_segment_count: 0, .. }"
    );
}
