use super::{Module, code_bodies, function_exports, i32_types, module, validate_module};

const FUNCTION_NAME: &str = "function_secret";
const EXPORT_NAME: &str = "export_secret";
const OTHER_EXPORT_NAME: &str = "alternate_export_secret";
const RUNTIME_VALUE: i32 = 90_172_635;

fn fixture(export: &str, marker: u8) -> (Module, i32, i32, i32) {
    let slot = marker % 4;
    let function = 296_320 + i32::from(marker);
    let global = 790_912 + i32::from(marker);
    let data = [marker, b'B', b'C', b'D'];
    let code = code_bodies(&[
        // () -> i32: a distinct i32.const immediate, called through the table.
        &[0, 0x41, 0x80 | marker, 0x8b, 0x12, 0x0b],
        // (i32) -> i32: store the argument to memory and the global, then return it.
        &[
            0, 0x41, marker, 0x20, 0, 0x36, 2, 0, 0x20, 0, 0x24, 0, 0x20, 0, 0x0b,
        ],
        // () -> i32: sum the indirect call result, global, and word from memory.
        &[
            0, 0x41, slot, 0x11, 0, 0, 0x23, 0, 0x6a, 0x41, marker, 0x28, 2, 0, 0x6a, 0x0b,
        ],
    ]);
    let validated = validate_module(&module(&[
        (0, b"\x04name\x01\x12\x01\0\x0ffunction_secret"),
        (1, &i32_types(&[0, 1])),
        (3, &[3, 0, 1, 0]),
        (4, &[1, 0x70, 0, 4]),
        (5, &[1, 0, 1]),
        (6, &[1, 0x7f, 1, 0x41, 0x80 | marker, 0xa3, 0x30, 0x0b]),
        (
            7,
            &function_exports(&[(b"write", 1), (export.as_bytes(), 2)]),
        ),
        (9, &[1, 0, 0x41, slot, 0x0b, 1, 0]),
        (10, &code),
        (11, &[&[1, 0, 0x41, marker, 0x0b, 4][..], &data].concat()),
    ]))
    .unwrap();
    (validated, function, global, i32::from_le_bytes(data))
}

fn debug_formats(value: &impl std::fmt::Debug) -> [String; 2] {
    [format!("{value:?}"), format!("{value:#?}")]
}

#[test]
fn module_and_instance_debug_report_metadata_not_guest_content() {
    let (validated, function, global, data) = fixture(EXPORT_NAME, 5);
    let (alternate, _, _, _) = fixture(OTHER_EXPORT_NAME, 10);
    let empty = validate_module(&module(&[])).unwrap();
    let instance = validated.instantiate().unwrap();
    let alternate_instance = alternate.instantiate().unwrap();
    assert_eq!(
        instance.invoke_i32(EXPORT_NAME),
        Ok(function + global + data)
    );

    // In both formatting modes, neither changing private instance state nor
    // changing guest content without changing counts should affect the output.
    let module_debug = debug_formats(&validated);
    let before = debug_formats(&instance);
    assert!(
        module_debug == debug_formats(&alternate),
        "module content leaked"
    );
    assert!(
        before == debug_formats(&alternate_instance),
        "instance content leaked"
    );
    assert_eq!(
        instance.invoke_i32_with_args("write", &[RUNTIME_VALUE]),
        Ok(RUNTIME_VALUE)
    );
    assert_eq!(
        instance.invoke_i32(EXPORT_NAME),
        Ok(function + 2 * RUNTIME_VALUE)
    );
    let after = debug_formats(&instance);
    assert!(before == after, "private state leaked");

    // Counts are useful: an empty module and instance render differently.
    for (populated, empty) in [
        (&module_debug, debug_formats(&empty)),
        (&before, debug_formats(&empty.instantiate().unwrap())),
    ] {
        for (populated, empty) in populated.iter().zip(&empty) {
            assert_ne!(populated, empty);
        }
    }

    let numbers = [function, global, data, RUNTIME_VALUE].map(|value| value.to_string());
    for output in module_debug.iter().chain(&before).chain(&after) {
        for sentinel in [EXPORT_NAME, FUNCTION_NAME]
            .into_iter()
            .chain(numbers.iter().map(String::as_str))
        {
            assert!(
                !output.contains(sentinel),
                "guest sentinel leaked: {sentinel}"
            );
        }
    }
}
