use super::super::{
    bad_payload, code_bodies, expect_error, function_exports, module, push_section, validate_module,
};
use super::{bad_global_body, with_global_body};
use swayze_wasm::ValType::{F32, F64, I32, I64};
use swayze_wasm::{
    ErrorKind, ExecutionLimit, IntegerField, InvokeError, Limit, ResultKind, Trap, Value,
};

const F32_ZERO: &[u8] = b"\x7d\x01\x43\0\0\0\0\x0b";
const F64_ZERO: &[u8] = b"\x7c\x01\x44\0\0\0\0\0\0\0\0\x0b";
const F32_NEG_ZERO: &[u8] = b"\x7d\0\x43\0\0\0\x80\x0b";
const F64_NEG_ZERO: &[u8] = b"\x7c\0\x44\0\0\0\0\0\0\0\x80\x0b";
const F32_SNAN: &[u8] = b"\x7d\0\x43\x0b\x34\x92\xff\x0b";
const F64_SNAN: &[u8] = b"\x7c\0\x44\xef\xcd\xab\x89\x67\x45\xf3\x7f\x0b";
const SNAN32_BITS: i32 = 0xff92_340b_u32 as i32;
const SNAN64_BITS: i64 = 0x7ff3_4567_89ab_cdef;

#[test]
fn eight_mixed_globals_share_the_cap_and_initialize_exact_float_bits() {
    let mut globals = [
        &b"\x88\0\x7f\0\x41\x7f\x0b"[..], // Padded count 8; slot 0 = -1.
        F32_ZERO,
        F64_NEG_ZERO,
        b"\x7e\x01\x42\x2a\x0b",
        F32_NEG_ZERO,
        F64_ZERO,
        F32_SNAN,
        F64_SNAN,
    ]
    .concat();
    // Check the bits through integer results, not float equality (+0 == -0,
    // NaNs compare unordered). The last get is padded to exercise the top slot.
    let expected = [
        (Value::I32(-1), None),
        (Value::I32(0), Some(0xbc)),
        (Value::I64(i64::MIN), Some(0xbd)),
        (Value::I64(42), None),
        (Value::I32(i32::MIN), Some(0xbc)),
        (Value::I64(0), Some(0xbd)),
        (Value::I32(SNAN32_BITS), Some(0xbc)),
        (Value::I64(SNAN64_BITS), Some(0xbd)),
    ];
    let mut bodies = Vec::new();
    let mut functions = vec![8];
    let mut exports: Vec<(&[u8], usize)> = Vec::new();
    let names = b"abcdefgh";
    for (index, (value, reinterpret)) in expected.into_iter().enumerate() {
        let mut body = vec![0, 0x23, 0x80 | index as u8, 0];
        body.extend(reinterpret);
        body.push(0x0b);
        bodies.push(body);
        functions.push(u8::from(matches!(value, Value::I64(_))));
        exports.push((&names[index..=index], index));
    }
    let body_refs: Vec<&[u8]> = bodies.iter().map(Vec::as_slice).collect();
    let validated = validate_module(&module(&[
        (1, b"\x02\x60\0\x01\x7f\x60\0\x01\x7e"),
        (3, &functions),
        (6, &globals),
        (7, &function_exports(&exports)),
        (10, &code_bodies(&body_refs)),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    for ((expected, _), (name, _)) in expected.into_iter().zip(exports) {
        let kind = match expected {
            Value::I32(_) => ResultKind::I32,
            Value::I64(_) => ResultKind::I64,
        };
        let name = std::str::from_utf8(name).unwrap();
        assert_eq!(
            instance.invoke_with_values(name, &[], kind),
            Ok(Some(expected)),
            "{name}"
        );
    }

    for opcode in [0x23, 0x24] {
        bad_global_body(
            Some(&globals),
            &[0, 0, opcode, 8, 0x0b],
            3,
            ErrorKind::InvalidIndex {
                field: IntegerField::GlobalIndex,
                index: 8,
            },
        );
    }
    globals[0] = 0x89; // Reject nine even without a ninth declaration.
    bad_payload(
        6,
        &globals,
        0,
        ErrorKind::UnsupportedCount {
            field: IntegerField::GlobalCount,
            count: 9,
        },
    );
}

#[test]
fn start_and_invocations_move_exact_bits_into_independent_instance_slots() {
    let globals = [&[4][..], F32_ZERO, F64_ZERO, F32_SNAN, F64_SNAN].concat();
    #[rustfmt::skip]
    let start = [
        0,
        0x23, 0, 0xbc, 0x45, // Was slot 0 exact +0?
        0x23, 1, 0xbd, 0x50, // Was slot 1 exact +0?
        0x71, 0x45, 0x04, 0x40, 0x00, 0x0b, // Trap unless both are newly initialized.
        0x23, 2, 0x24, 0, 0x23, 3, 0x24, 1, 0x0b, // Copy both initial sNaNs.
    ];
    let code = code_bodies(&[
        b"\0\x23\0\xbc\x0b",   // read32 (as i32)
        b"\0\x23\x01\xbd\x0b", // read64 (as i64)
        &start,
        b"\0\x20\0\xbe\x24\x80\0\x20\x01\xbf\x24\x01\x0b", // set from integer args
        b"\0\x20\0\x20\x01\x10\x03\0\x23\x02\x24\0\x23\x03\x24\x01\x0b", // call set, trap, skip restoring sNaNs
    ]);
    let validated = validate_module(&module(&[
        (
            1,
            b"\x04\x60\0\x01\x7f\x60\0\x01\x7e\x60\0\0\x60\x02\x7f\x7e\0",
        ),
        (3, &[5, 0, 1, 2, 3, 3]),
        (6, &globals),
        (
            7,
            &function_exports(&[(b"read32", 0), (b"read64", 1), (b"set", 3), (b"trap", 4)]),
        ),
        (8, &[2]),
        (10, &code),
    ]))
    .unwrap();
    let read = |instance: &swayze_wasm::Instance<'_>, bits32, bits64| {
        assert_eq!(instance.invoke_i32("read32"), Ok(bits32));
        assert_eq!(
            instance.invoke_with_values("read64", &[], ResultKind::I64),
            Ok(Some(Value::I64(bits64)))
        );
    };
    let a = validated.instantiate().unwrap();
    let b = validated.instantiate().unwrap();
    read(&a, SNAN32_BITS, SNAN64_BITS);
    for (f32_bits, f64_bits) in [(0, i64::MIN), (i32::MIN, 0), (SNAN32_BITS, SNAN64_BITS)] {
        assert_eq!(
            a.invoke_with_values(
                "set",
                &[Value::I32(f32_bits), Value::I64(f64_bits)],
                ResultKind::Void
            ),
            Ok(None),
        );
        read(&a, f32_bits, f64_bits);
        read(&b, SNAN32_BITS, SNAN64_BITS);
    }
    // New signaling NaN payloads persist; the restore after the trap does not run.
    let (trap32, trap64) = (SNAN32_BITS ^ 0x1234, SNAN64_BITS ^ 0x4567_89ab);
    assert_eq!(
        a.invoke_with_values(
            "trap",
            &[Value::I32(trap32), Value::I64(trap64)],
            ResultKind::Void
        ),
        Err(InvokeError::Trap(Trap::Unreachable)),
    );
    read(&a, trap32, trap64);
    read(&b, SNAN32_BITS, SNAN64_BITS);

    // Starting a new instance writes the original sNaNs to its own slots,
    // without restoring those bits over the already-live instance's writes.
    let fresh = validated.instantiate().unwrap();
    read(&fresh, SNAN32_BITS, SNAN64_BITS);
    read(&a, trap32, trap64);
}

#[test]
fn float_initializers_check_mutability_opcode_width_and_end_in_order() {
    use ErrorKind::*;
    for (decl, ty, wrong_width, same_width_int) in [
        (F32_SNAN, F32, F64_SNAN, 0x41),
        (F64_SNAN, F64, F32_SNAN, 0x42),
    ] {
        let opcode = decl[2];
        let initializer = &decl[..decl.len() - 1];
        for (declaration, offset, kind) in [
            (&decl[..1], 2, TruncatedGlobalMutability),
            (
                &[decl[0], 2, wrong_width[2]],
                2,
                InvalidGlobalMutability { byte: 2 },
            ),
            (&decl[..2], 3, TruncatedByte { expected: opcode }),
            (initializer, decl.len(), TruncatedByte { expected: 0x0b }),
            (
                &[initializer, &[opcode]].concat(),
                decl.len(),
                UnexpectedByte {
                    expected: 0x0b,
                    found: opcode,
                },
            ),
            (
                &[decl, &[0]].concat(),
                1 + decl.len(),
                TrailingSectionBytes { id: 6 },
            ),
        ] {
            bad_payload(6, &[&[1][..], declaration].concat(), offset, kind);
        }
        // Even a complete other-width constant and end fail at the opcode.
        let payload = [&[1][..], &decl[..2], &wrong_width[2..]].concat();
        bad_payload(
            6,
            &payload,
            3,
            UnexpectedByte {
                expected: opcode,
                found: wrong_width[2],
            },
        );
        for found in [same_width_int, 0x23] {
            // global.get is not allowed, even referring to an earlier same-type immutable global.
            let payload = [&[2][..], decl, &decl[..2], &[found, 0, 0x0b]].concat();
            bad_payload(
                6,
                &payload,
                3 + decl.len(),
                UnexpectedByte {
                    expected: opcode,
                    found,
                },
            );
        }
        for len in 3..initializer.len() {
            let mut bytes = module(&[]);
            push_section(&mut bytes, 6, &[&[1][..], &initializer[..len]].concat());
            let global_end = bytes.len();
            // There are enough following bytes to fill even an empty immediate,
            // but all of them belong to the next section.
            push_section(&mut bytes, 0, &[0; 9]);
            expect_error(&bytes, global_end, TruncatedFloatConst { ty });
        }
    }
}

#[test]
fn float_globals_stay_private_and_data_offsets_stay_i32_const() {
    let globals = [&[1][..], F32_SNAN].concat();
    let bytes = module(&[(2, b"\x01\x01m\x01g\x03\x7d\0")]);
    expect_error(
        &bytes,
        super::HEADER.len(),
        ErrorKind::UnsupportedSection { id: 2 },
    );
    let mut bytes = module(&[(6, &globals)]);
    let start = push_section(&mut bytes, 7, b"\x01\x01g\x03\0");
    expect_error(
        &bytes,
        start + 3,
        ErrorKind::UnexpectedByte {
            expected: 0,
            found: 3,
        },
    );
    for opcode in [0x23, 0x43, 0x44] {
        let mut bytes = module(&[(5, &[1, 0, 1]), (6, &globals)]);
        let start = push_section(&mut bytes, 11, &[1, 0, opcode]);
        expect_error(
            &bytes,
            start + 2,
            ErrorKind::UnexpectedByte {
                expected: 0x41,
                found: opcode,
            },
        );
    }
}

#[test]
fn float_global_index_mutability_and_operand_errors_keep_precedence_even_in_dead_code() {
    use ErrorKind::*;
    let globals = [
        &[6][..],
        F32_ZERO,
        F64_ZERO,
        F32_SNAN,
        F64_SNAN,
        &super::MUTABLE[1..],
        &super::MUTABLE_I64[1..],
    ]
    .concat();
    let field = IntegerField::GlobalIndex;
    for head in [&b"\0"[..], b"\0\0"] {
        for (from, to, expected, actual) in [(1, 0, F32, F64), (0, 1, F64, F32)] {
            let opcode = head.len() + 2;
            for (index, offset, kind) in [
                (6, opcode + 1, InvalidIndex { field, index: 6 }),
                (
                    to + 2,
                    opcode + 1,
                    ImmutableGlobal {
                        index: u32::from(to + 2),
                    },
                ),
                (to, opcode, TypeMismatch { expected, actual }),
            ] {
                let body = [head, &[0x23, from, 0x24, index | 0x80, 0, 0x0b]].concat();
                bad_global_body(Some(&globals), &body, offset, kind);
            }
            // A get produces a concrete float even when in a polymorphic tail.
            let eqz = if actual == F32 { 0x45 } else { 0x50 };
            let expected = if actual == F32 { I32 } else { I64 };
            let body = [head, &[0x23, from, eqz, 0x0b]].concat();
            bad_global_body(
                Some(&globals),
                &body,
                opcode,
                TypeMismatch { expected, actual },
            );
        }
        for (from, to, expected, actual) in [
            (4, 0, F32, I32),
            (5, 1, F64, I64),
            (0, 4, I32, F32),
            (1, 5, I64, F64),
        ] {
            let body = [head, &[0x23, from, 0x24, to, 0x0b]].concat();
            bad_global_body(
                Some(&globals),
                &body,
                head.len() + 2,
                TypeMismatch { expected, actual },
            );
        }
        for index in [2, 3] {
            let body = [head, &[0x24, index, 0x0b]].concat();
            bad_global_body(
                Some(&globals),
                &body,
                head.len() + 1,
                ImmutableGlobal {
                    index: u32::from(index),
                },
            );
        }
    }
}

#[test]
fn float_globals_respect_shared_stack_and_dispatch_caps() {
    let stack_limit = Limit::OperandStackDepth;
    let dispatch_limit = ExecutionLimit::InstructionDispatches;
    for (decl, neg, reinterpret, eqz) in
        [(F32_ZERO, 0x8c, 0xbc, 0x45), (F64_ZERO, 0x9a, 0xbd, 0x50)]
    {
        let globals = [&[1][..], decl].concat();
        // The initial get fills a slot below a fresh block floor, so the set underflows.
        bad_global_body(
            Some(&globals),
            b"\0\x23\0\x02\x40\x24\0\x0b",
            5,
            ErrorKind::StackUnderflow { depth: 0 },
        );

        let mut full = vec![0, 0x41, 42];
        full.extend([0x23, 0].repeat(stack_limit.maximum() - 1));
        full.extend([0x02, 0x40, 0x0c, 0]); // Branch past a dead store at the full floor.
        let dead = full.len();
        full.extend([0x24, 0, 0x0b]); // May consume an implicit operand (no new slot).
        full.extend(vec![0x1a; stack_limit.maximum() - 1]);
        full.push(0x0b);
        assert_eq!(
            with_global_body(&globals, &full)
                .instantiate()
                .unwrap()
                .invoke_i32("answer"),
            Ok(42)
        );
        full[dead] = 0x23; // A concrete get still needs a slot.
        bad_global_body(
            Some(&globals),
            &full,
            dead,
            ErrorKind::LimitExceeded { limit: stack_limit },
        );

        let read = [0, 0x23, 0, reinterpret, eqz, 0x0b];
        let loop_body = [
            0, 0x03, 0x40, 0x23, 0, neg, 0x01, 0x24, 0, 0x41, 1, 0x0d, 0, 0x0b, 0x41, 0, 0x0b,
        ];
        let validated = validate_module(&module(&[
            (1, super::TYPE),
            (3, super::TWO_FUNCTIONS),
            (6, &globals),
            (7, &function_exports(&[(b"read", 0), (b"loop", 1)])),
            (10, &code_bodies(&[&read, &loop_body])),
        ]))
        .unwrap();
        let instance = validated.instantiate().unwrap();
        assert_eq!(instance.invoke_i32("read"), Ok(1)); // Exact +0.
        assert_eq!(
            instance.invoke_i32("loop"),
            Err(InvokeError::LimitExceeded {
                limit: dispatch_limit
            })
        );
        // 1024 dispatches complete 170 six-op iterations and the first four of
        // the next one, so the 171st sign flip is stored on the final dispatch.
        assert_eq!(instance.invoke_i32("read"), Ok(0)); // Exact -0.
    }
}
