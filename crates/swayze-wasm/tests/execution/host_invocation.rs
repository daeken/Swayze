use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

#[path = "host_invocation/float.rs"]
mod float;

const I32: u8 = 0x7f;
const I64: u8 = 0x7e;

#[test]
fn tagged_invocation_preserves_mixed_argument_order_and_exact_results() {
    let mixed = &[I64, I32, I64, I32];
    let types = types(&[
        (mixed, Some(I64)),
        (mixed, Some(I32)),
        (mixed, None),
        (&[I64], Some(I64)),
    ]);
    let exports = function_exports(&[(b"wide", 0), (b"narrow", 1), (b"void", 2), (b"echo", 3)]);
    let code = code_bodies(&[
        // (param 0 - param 2) + extend_s(param 1) - extend_s(param 3).
        b"\0\x20\0\x20\x02\x7d\x20\x01\xac\x7c\x20\x03\xac\x7d\x0b",
        b"\0\x20\x01\x20\x03\x6b\x0b", // param 1 - param 3
        b"\0\x0b",
        b"\0\x20\0\x0b",
    ]);
    let validated = validate_module(&module(&[
        (1, &types),
        (3, &[4, 0, 1, 2, 3]),
        (7, &exports),
        (10, &code),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    let (a, b, c, d) = (
        0x7123_4567_89ab_cdef_i64,
        i32::MIN,
        -0x1234_5678_7654_3211_i64,
        -7,
    );
    let args = [Value::I64(a), Value::I32(b), Value::I64(c), Value::I32(d)];
    assert_eq!(
        instance.invoke_with_values("wide", &args, ResultKind::I64),
        Ok(Some(Value::I64(
            a.wrapping_sub(c)
                .wrapping_add(i64::from(b))
                .wrapping_sub(i64::from(d))
        )))
    );
    assert_eq!(
        instance.invoke_with_values("narrow", &args, ResultKind::I32),
        Ok(Some(Value::I32(b.wrapping_sub(d))))
    );
    assert_eq!(
        instance.invoke_with_values("void", &args, ResultKind::Void),
        Ok(None)
    );
    for value in [i64::MIN, i64::MAX, -1, 0x1_0000_0000] {
        assert_eq!(
            instance.invoke_with_values("echo", &[Value::I64(value)], ResultKind::I64),
            Ok(Some(Value::I64(value)))
        );
    }
    assert_eq!(
        instance.invoke_i32_with_args("echo", &[0]),
        Err(InvokeError::ResultMismatch {
            expected: ResultKind::I32,
            actual: ResultKind::I64
        })
    );
    assert_eq!(
        instance.invoke_void_with_args("echo", &[0]),
        Err(InvokeError::ResultMismatch {
            expected: ResultKind::Void,
            actual: ResultKind::I64
        })
    );
}

#[test]
fn old_and_new_preflight_share_order_and_never_enter_a_rejected_body() {
    let mixed = &[I32, I64, I32, I64];
    let types = types(&[
        (mixed, Some(I32)),
        (mixed, None),
        (&[], Some(I64)),
        (&[], Some(I32)),
    ]);
    let exports = function_exports(&[(b"change", 0), (b"reset", 1), (b"global", 2), (b"size", 3)]);
    // Both rejected bodies would overwrite the global, grow memory, and then trap.
    let change = b"\0\x42\x2a\x24\0\x41\x01\x40\0\x1a\0\x0b";
    let code = code_bodies(&[change, change, b"\0\x23\0\x0b", b"\0\x3f\0\x0b"]);
    let validated = validate_module(&module(&[
        (1, &types),
        (3, &[4, 0, 1, 2, 3]),
        (5, &[1, 0, 0]), // Zero initial pages; a memory.grow could install the first.
        (6, &[1, I64, 1, 0x42, 0, 0x0b]),
        (7, &exports),
        (10, &code),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    let too_many = InvokeError::ArityMismatch {
        expected: 4,
        actual: 5,
    };
    let no_args = InvokeError::ArityMismatch {
        expected: 4,
        actual: 0,
    };
    let wrong_result = InvokeError::ResultMismatch {
        expected: ResultKind::Void,
        actual: ResultKind::I32,
    };
    let wrong_legacy_arg = InvokeError::ArgumentTypeMismatch {
        index: 1,
        expected: ValType::I64,
        actual: ValType::I32,
    };

    assert_eq!(
        instance.invoke_void_with_args("Change", &[0; 5]),
        Err(InvokeError::ExportNotFound)
    );
    assert_eq!(
        instance.invoke_void_with_args("change", &[0; 5]),
        Err(too_many)
    );
    assert_eq!(instance.invoke_i32("reset"), Err(no_args));
    assert_eq!(instance.invoke_void("change"), Err(no_args));
    assert_eq!(
        instance.invoke_void_with_args("change", &[0; 4]),
        Err(wrong_result)
    );
    assert_eq!(
        instance.invoke_void_with_args("reset", &[0; 4]),
        Err(wrong_legacy_arg)
    );
    assert_eq!(
        instance.invoke_i32_with_args("change", &[0; 4]),
        Err(wrong_legacy_arg)
    );

    let overlong = [Value::I64(0); 5];
    assert_eq!(
        instance.invoke_with_values("Change", &overlong, ResultKind::Void),
        Err(InvokeError::ExportNotFound)
    );
    assert_eq!(
        instance.invoke_with_values("change", &overlong, ResultKind::Void),
        Err(too_many)
    );
    let first_two_wrong = [Value::I64(0), Value::I32(0), Value::I32(0), Value::I64(0)];
    assert_eq!(
        instance.invoke_with_values("change", &first_two_wrong, ResultKind::Void),
        Err(wrong_result)
    );
    assert_eq!(
        instance.invoke_with_values("change", &first_two_wrong, ResultKind::I32),
        Err(InvokeError::ArgumentTypeMismatch {
            index: 0,
            expected: ValType::I32,
            actual: ValType::I64
        })
    );
    for (index, wrong, expected, actual) in [
        (0, Value::I64(0), ValType::I32, ValType::I64),
        (1, Value::I32(0), ValType::I64, ValType::I32),
        (2, Value::I64(0), ValType::I32, ValType::I64),
        (3, Value::I32(0), ValType::I64, ValType::I32),
    ] {
        let mut args = [Value::I32(0), Value::I64(0), Value::I32(0), Value::I64(0)];
        args[index] = wrong;
        assert_eq!(
            instance.invoke_with_values("reset", &args, ResultKind::Void),
            Err(InvokeError::ArgumentTypeMismatch {
                index,
                expected,
                actual
            })
        );
    }
    assert_eq!(
        instance.invoke_with_values("global", &[], ResultKind::I64),
        Ok(Some(Value::I64(0)))
    );
    assert_eq!(instance.invoke_i32("size"), Ok(0));

    // A valid call to the same body really does reach its writes and trap.
    let args = [Value::I32(0), Value::I64(0), Value::I32(0), Value::I64(0)];
    assert_eq!(
        instance.invoke_with_values("change", &args, ResultKind::I32),
        Err(InvokeError::Trap(Trap::Unreachable))
    );
    assert_eq!(
        instance.invoke_with_values("global", &[], ResultKind::I64),
        Ok(Some(Value::I64(42)))
    );
    assert_eq!(instance.invoke_i32("size"), Ok(1));
}

#[test]
fn exact_export_aliases_share_state_after_success_traps_and_host_limits() {
    let types = types(&[(&[I32, I64], None), (&[], Some(I64))]);
    let longest = "n".repeat(Limit::ExportNameBytes.maximum());
    let exports = function_exports(&[
        (b"", 0),
        ("\0é".as_bytes(), 0),
        (longest.as_bytes(), 0),
        (b"global", 1),
        (b"memory", 2),
    ]);
    let code = code_bodies(&[
        b"\0\x20\x01\x24\0\x41\x03\x20\x01\x37\x03\0\x20\0\x41\x01\x46\x04\x40\0\x0b\
          \x20\0\x41\x02\x46\x04\x40\x03\x40\x0c\0\x0b\x0b\
          \x20\0\x41\x03\x46\x04\x40\x20\0\x20\x01\x10\0\x0b\x0b",
        b"\0\x23\0\x0b",             // global.get 0
        b"\0\x41\x03\x29\x03\0\x0b", // i64.load at address 3
    ]);
    // The writer stores its i64 in both the global and an unaligned memory slot.
    // Its i32 selector then chooses success, trap, infinite loop, or recursion.
    let validated = validate_module(&module(&[
        (1, &types),
        (3, &[3, 0, 1, 1]),
        (5, &[1, 0, 1]),
        (6, &[1, I64, 1, 0x42, 0, 0x0b]),
        (7, &exports),
        (10, &code),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    let args = [Value::I32(0), Value::I64(1)];
    for name in [
        "é",
        "\0É",
        "\0e\u{301}",
        "\0é\0",
        &longest[..longest.len() - 1],
    ] {
        assert_eq!(
            instance.invoke_with_values(name, &args, ResultKind::Void),
            Err(InvokeError::ExportNotFound)
        );
    }
    for (name, mode, value, expected) in [
        ("", 0, i64::MAX, Ok(None)),
        (
            "\0é",
            1,
            i64::MIN,
            Err(InvokeError::Trap(Trap::Unreachable)),
        ),
        (
            &longest,
            2,
            0x1_0000_0000,
            Err(InvokeError::LimitExceeded {
                limit: ExecutionLimit::InstructionDispatches,
            }),
        ),
        (
            "",
            3,
            -0x1_0000_0001,
            Err(InvokeError::LimitExceeded {
                limit: ExecutionLimit::CallDepth,
            }),
        ),
        ("\0é", 0, -1, Ok(None)),
    ] {
        assert_eq!(
            instance.invoke_with_values(
                name,
                &[Value::I32(mode), Value::I64(value)],
                ResultKind::Void
            ),
            expected
        );
        for reader in ["global", "memory"] {
            assert_eq!(
                instance.invoke_with_values(reader, &[], ResultKind::I64),
                Ok(Some(Value::I64(value)))
            );
        }
    }
    let fresh = validated.instantiate().unwrap();
    for reader in ["global", "memory"] {
        assert_eq!(
            fresh.invoke_with_values(reader, &[], ResultKind::I64),
            Ok(Some(Value::I64(0)))
        );
    }
}
