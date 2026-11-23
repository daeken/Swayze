use super::{
    I32, I64, InvokeError, ResultKind, ValType, Value, code_bodies, function_exports, module,
    types, validate_module,
};

const F32: u8 = 0x7d;
const F64: u8 = 0x7c;

#[test]
fn float_arguments_and_results_preserve_exact_ieee_bits_in_each_direction() {
    const BITS32: &[u64] = &[
        0,
        0x8000_0000, // Signed zeros.
        0x0000_0001,
        0x807f_ffff, // Subnormals.
        0x3f80_0001,
        0xbf80_0001, // Finite normal values.
        0x7f80_0000,
        0xff80_0000, // Infinities.
        0x7fc0_ab12,
        0xffc0_4321, // Quiet NaNs with distinct payloads.
        0x7f80_0b12,
        0xff80_1234, // Signaling NaNs with distinct payloads.
    ];
    const BITS64: &[u64] = &[
        0,
        0x8000_0000_0000_0000,
        0x0000_0000_0000_0001,
        0x800f_ffff_ffff_ffff,
        0x3ff0_0000_0000_0001,
        0xbff0_0000_0000_0001,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_000b_89ab_cdef,
        0xfff8_4321_7654_3210,
        0x7ff0_000b_89ab_cdef,
        0xfff0_4321_7654_3210,
    ];
    // The alias uses a padded function index referring to the same body as echo.
    let exports = b"\x04\x04echo\0\0\x05alias\0\x80\0\x07observe\0\x01\x04emit\0\x02";
    for (float, integer, float_kind, int_kind, patterns) in [
        (F32, I32, ResultKind::F32, ResultKind::I32, BITS32),
        (F64, I64, ResultKind::F64, ResultKind::I64, BITS64),
    ] {
        let ty = types(&[
            (&[float], Some(float)),
            (&[float], Some(integer)),
            (&[], Some(float)),
        ]);
        for &bits in patterns {
            let mut emit = vec![0];
            let (float_value, int_value, reinterpret) = if float == F32 {
                let bits = bits as u32;
                emit.push(0x43);
                emit.extend(bits.to_le_bytes());
                (Value::F32(bits), Value::I32(bits as i32), 0xbc)
            } else {
                emit.push(0x44);
                emit.extend(bits.to_le_bytes());
                (Value::F64(bits), Value::I64(bits as i64), 0xbd)
            };
            emit.push(0x0b);
            let observe = [0, 0x20, 0, reinterpret, 0x0b]; // local.get 0; integer reinterpretation
            let validated = validate_module(&module(&[
                (1, &ty),
                (3, &[3, 0, 1, 2]),
                (7, exports),
                (10, &code_bodies(&[b"\0\x20\0\x0b", &observe, &emit])),
            ]))
            .unwrap();
            let instance = validated.instantiate().unwrap();
            // Test both directions independently, so a round trip cannot hide re-encoding.
            assert_eq!(
                instance.invoke_with_values("observe", &[float_value], int_kind),
                Ok(Some(int_value))
            );
            assert_eq!(
                instance.invoke_with_values("emit", &[], float_kind),
                Ok(Some(float_value))
            );
            for name in ["echo", "alias"] {
                assert_eq!(
                    instance.invoke_with_values(name, &[float_value], float_kind),
                    Ok(Some(float_value))
                );
            }
        }
    }
}

#[test]
fn mixed_float_exports_preflight_before_effects_and_keep_integer_entry_points() {
    use InvokeError::{ArgumentTypeMismatch, ArityMismatch, ExportNotFound, ResultMismatch};

    let ty = types(&[
        (&[I32, F32, I64, F64], Some(F64)),
        (&[], Some(I32)),
        (&[], Some(F32)),
        (&[], Some(I64)),
        (&[], Some(F64)),
        (&[F32], None),
        (&[F64], Some(I32)),
    ]);
    let exports = function_exports(&[
        (b"write", 0),
        (b"i32", 1),
        (b"f32", 2),
        (b"i64", 3),
        (b"f64", 4),
        (b"void", 5),
        (b"narrow", 6),
    ]);
    let code = code_bodies(&[
        // Store all parameters in their respective globals; return the last parameter.
        b"\0\x20\0\x24\0\x20\x01\x24\x01\x20\x02\x24\x02\x20\x03\x24\x03\x20\x03\x0b",
        b"\0\x23\0\x0b",
        b"\0\x23\x01\x0b",
        b"\0\x23\x02\x0b",
        b"\0\x23\x03\x0b",
        b"\0\x20\0\x24\x01\x0b",       // Store f32 and return nothing.
        b"\0\x20\0\x24\x03\x23\0\x0b", // Store f64 and return the i32 global.
    ]);
    let validated = validate_module(&module(&[
        (1, &ty),
        (3, &[7, 0, 1, 2, 3, 4, 5, 6]),
        (
            6,
            b"\x04\x7f\x01\x41\0\x0b\x7d\x01\x43\0\0\0\0\x0b\
                 \x7e\x01\x42\0\x0b\x7c\x01\x44\0\0\0\0\0\0\0\0\x0b",
        ),
        (7, &exports),
        (10, &code),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    let seeded = [
        Value::I32(-37),
        Value::F32(0xff80_1234),
        Value::I64(i64::MIN + 13),
        Value::F64(0x8000_0000_0000_0000),
    ];
    let snapshot = || {
        [
            ("i32", ResultKind::I32),
            ("f32", ResultKind::F32),
            ("i64", ResultKind::I64),
            ("f64", ResultKind::F64),
        ]
        .map(|(name, kind)| {
            instance
                .invoke_with_values(name, &[], kind)
                .unwrap()
                .unwrap()
        })
    };
    assert_eq!(
        instance.invoke_with_values("write", &seeded, ResultKind::F64),
        Ok(Some(seeded[3]))
    );
    assert_eq!(snapshot(), seeded);

    let overlong = [Value::F64(0); 5];
    let wrong = [Value::I32(0), Value::I32(0), Value::I64(0), Value::I64(0)];
    let argument = |index, expected, actual| ArgumentTypeMismatch {
        index,
        expected,
        actual,
    };
    let result = |expected, actual| ResultMismatch { expected, actual };
    let arity = |actual| ArityMismatch {
        expected: 4,
        actual,
    };
    for (name, args, kind, expected) in [
        ("Write", &overlong[..], ResultKind::F32, ExportNotFound),
        ("write", &overlong, ResultKind::F32, arity(5)),
        (
            "write",
            &wrong,
            ResultKind::F32,
            result(ResultKind::F32, ResultKind::F64),
        ),
        (
            "write",
            &wrong,
            ResultKind::F64,
            argument(1, ValType::F32, ValType::I32),
        ),
        (
            "write",
            &[Value::I32(0), Value::F32(0), Value::I64(0), Value::I64(0)],
            ResultKind::F64,
            argument(3, ValType::F64, ValType::I64),
        ),
        (
            "write",
            &[Value::I32(0), Value::F64(0), Value::I64(0), Value::F32(0)],
            ResultKind::F64,
            argument(1, ValType::F32, ValType::F64),
        ),
        (
            "write",
            &[Value::F32(0), Value::F32(0), Value::F64(0), Value::F64(0)],
            ResultKind::F64,
            argument(0, ValType::I32, ValType::F32),
        ),
        (
            "write",
            &[Value::I32(0), Value::F32(0), Value::F64(0), Value::F64(0)],
            ResultKind::F64,
            argument(2, ValType::I64, ValType::F64),
        ),
    ] {
        assert_eq!(instance.invoke_with_values(name, args, kind), Err(expected));
        assert_eq!(snapshot(), seeded);
    }

    assert_eq!(instance.invoke_i32("write"), Err(arity(0)));
    assert_eq!(instance.invoke_void("write"), Err(arity(0)));
    for (actual, expected) in [
        (
            instance.invoke_i32("f32").unwrap_err(),
            result(ResultKind::I32, ResultKind::F32),
        ),
        (
            instance.invoke_void("f64").unwrap_err(),
            result(ResultKind::Void, ResultKind::F64),
        ),
        (
            instance.invoke_i32_with_args("write", &[0; 4]).unwrap_err(),
            result(ResultKind::I32, ResultKind::F64),
        ),
        (
            instance
                .invoke_void_with_args("write", &[0; 4])
                .unwrap_err(),
            result(ResultKind::Void, ResultKind::F64),
        ),
        (
            instance.invoke_i32_with_args("narrow", &[0]).unwrap_err(),
            argument(0, ValType::F64, ValType::I32),
        ),
        (
            instance.invoke_void_with_args("void", &[0]).unwrap_err(),
            argument(0, ValType::F32, ValType::I32),
        ),
    ] {
        assert_eq!(actual, expected);
    }
    assert_eq!(snapshot(), seeded);
    assert_eq!(instance.invoke_i32("i32"), Ok(-37));

    assert_eq!(
        instance.invoke_with_values("void", &[Value::F32(0)], ResultKind::Void),
        Ok(None)
    );
    let snan64 = Value::F64(0xfff0_000b_89ab_cdef);
    assert_eq!(
        instance.invoke_with_values("narrow", &[snan64], ResultKind::I32),
        Ok(Some(seeded[0]))
    );
    assert_eq!(snapshot(), [seeded[0], Value::F32(0), seeded[2], snan64]);
}
