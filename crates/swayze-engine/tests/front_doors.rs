use swayze_engine::{WasmInvokeError, parse_script, validate_module};

const EMPTY_MODULE: &[u8] = b"\0asm\x01\0\0\0";
// Custom section: payload size 5, UTF-8 name "é", two opaque bytes.
const NAMED_CUSTOM: &[u8] = b"\0\x05\x02\xc3\xa9\xff\x00";
// Custom section: payload size 1, empty name, no opaque bytes.
const EMPTY_CUSTOM: &[u8] = b"\0\x01\0";

#[test]
fn script_admits_supported_arithmetic() {
    for source in ["0", "42;", "1+2*3-4", " (12 - 3) * (4 + 5) ; \n"] {
        assert!(parse_script(source).is_ok(), "rejected {source:?}");
    }
}

#[test]
fn script_prefixes_distinguish_complete_and_incomplete_expressions() {
    let source = "(12+3)*45;";
    // A closing paren or any digit of the last literal can end a script.
    for end in 0..=source.len() {
        let prefix = &source[..end];
        assert_eq!(
            parse_script(prefix).is_ok(),
            matches!(prefix, "(12+3)" | "(12+3)*4" | "(12+3)*45" | "(12+3)*45;"),
            "unexpected admission for prefix {prefix:?}",
        );
    }
}

#[test]
fn script_rejects_malformed_arithmetic_and_invalid_suffixes() {
    for source in [
        "()",
        "1 + * 2",
        "(1 + )",
        "1 2",
        "1 + 2 )",
        "1 + 2; * 3",
        "1;\0",
    ] {
        assert!(parse_script(source).is_err(), "accepted {source:?}");
    }
}

#[test]
fn script_handles_bounded_nesting() {
    let opening = "(".repeat(16);
    let closing = ")".repeat(16);
    let source = format!("{opening}1+2{closing}");
    assert!(parse_script(&source).is_ok());
    assert!(parse_script(&source[..source.len() - 1]).is_err());
}

#[test]
fn module_admits_empty_and_custom_only_modules() {
    assert!(validate_module(EMPTY_MODULE).is_ok());

    let repeated_custom = [EMPTY_MODULE, NAMED_CUSTOM, EMPTY_CUSTOM, NAMED_CUSTOM].concat();
    assert!(validate_module(&repeated_custom).is_ok());

    let mut two_byte_size = EMPTY_MODULE.to_vec();
    // A payload of 128 bytes: one empty-name length and 127 opaque bytes.
    two_byte_size.extend_from_slice(&[0, 0x80, 0x01, 0]);
    two_byte_size.extend_from_slice(&[0xff; 127]);
    assert!(validate_module(&two_byte_size).is_ok());
}

#[test]
fn module_prefixes_are_admitted_only_at_section_boundaries() {
    let bytes = [EMPTY_MODULE, NAMED_CUSTOM, EMPTY_CUSTOM].concat();
    let boundaries = [
        EMPTY_MODULE.len(),
        EMPTY_MODULE.len() + NAMED_CUSTOM.len(),
        bytes.len(),
    ];
    for end in 0..=bytes.len() {
        assert_eq!(
            validate_module(&bytes[..end]).is_ok(),
            boundaries.contains(&end),
            "unexpected admission for prefix of {end} bytes",
        );
    }
}

#[test]
fn module_invokes_a_validated_constant_export() {
    let bytes = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7f\x03\x02\x01\0\
                  \x07\x0a\x01\x06answer\0\0\x0a\x06\x01\x04\0\x41\x2a\x0b";
    let module = validate_module(bytes).unwrap();
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("answer"), Ok(42));
    assert_eq!(
        instance.invoke_i32("other"),
        Err(WasmInvokeError::ExportNotFound)
    );
    assert!(validate_module(&bytes[..bytes.len() - 1]).is_err());
}

#[test]
fn module_rejects_malformed_custom_sections_even_after_a_valid_section() {
    assert!(validate_module(b"\0bad\x01\0\0\0").is_err());
    let malformed: &[(&str, &[u8])] = &[
        ("missing section size", b"\0"),
        ("unterminated section size", b"\0\x80"),
        ("missing name length", b"\0\0"),
        ("name crosses section boundary", b"\0\x02\x02x\0\x01\0"),
        ("invalid name UTF-8", b"\0\x02\x01\xff"),
        ("declared payload exceeds remaining bytes", b"\0\x80\x01\0"),
    ];
    for &(description, tail) in malformed {
        let bytes = [EMPTY_MODULE, NAMED_CUSTOM, tail].concat();
        assert!(validate_module(&bytes).is_err(), "accepted {description}");
    }
}
