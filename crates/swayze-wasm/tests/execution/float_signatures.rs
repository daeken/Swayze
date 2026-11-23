use super::{i64_operands::push_i64, i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

const I32: u8 = 0x7f;
const I64: u8 = 0x7e;
const F32: u8 = 0x7d;
const F64: u8 = 0x7c;
const MIXED: [u8; 4] = [F32, I64, I32, F64];
const TRAP_BODY: &[u8] = b"\0\0\x0b";
const WIDE: i64 = 0x7123_4567_89ab_cdef;
const F32_SNAN: i32 = 0xff80_1234_u32 as i32;
const F64_SNAN: i64 = 0x7ff0_1234_89ab_cdef;

fn push_f32(body: &mut Vec<u8>, bits: i32) {
    body.push(0x43);
    body.extend(bits.to_le_bytes());
}

fn push_f64(body: &mut Vec<u8>, bits: i64) {
    body.push(0x44);
    body.extend(bits.to_le_bytes());
}

#[test]
fn direct_and_indirect_calls_preserve_mixed_arguments_and_exact_float_exits() {
    for (float, param, host_local, reinterpret, eq) in
        [(F32, 0, 0, 0xbc, 0x46), (F64, 3, 3, 0xbd, 0x51)]
    {
        let ty = types(&[
            (&[I32, I64, I32, I64], Some(I32)),
            (&MIXED, Some(float)),
            (&MIXED, Some(float)),
        ]);
        for (call, type_index) in [(&b"\x10\x81\0"[..], 1), (b"\x41\0\x11\x81\0\0", 2)] {
            // Preserve a caller prefix across the call; reinterpret the two float
            // arguments from borrowed host integers without changing their bits.
            let mut entry = b"\0\x20\x02\x20\0\xbe\x20\x01\x20\x02\x20\x03\xbf".to_vec();
            entry.extend(call);
            entry.extend([reinterpret, 0x20, host_local, eq, 0x71, 0x0b]);
            for exit in [
                &b""[..],                // Fallthrough.
                b"\x41\0\x0d\0",         // br_if fallthrough.
                b"\x0f",                 // return from nested block.
                b"\x0c\x01",             // br to function label.
                b"\x41\x01\x0d\x01\x1a", // br_if taken (drop in dead-at-runtime arm).
                b"\x41\0\x0e\0\x01",     // br_table with default to function.
            ] {
                let mut callee = vec![1, 1, float]; // Declared local starts after all four params.
                callee.extend(b"\x20\x01");
                push_i64(&mut callee, WIDE);
                callee.extend(b"\x52\x04\x40\0\x0b"); // i64.ne; if different, trap.
                callee.extend(b"\x20\x02\x41\x25\x47\x04\x40\0\x0b"); // Same for i32(37).
                let nested = !exit.is_empty() && exit != b"\x41\0\x0d\0";
                if nested {
                    callee.extend(b"\x41\x63\x02\x40"); // Junk below the block floor.
                }
                callee.extend([0x20, param, 0x21, 4, 0x20, 4]);
                callee.extend(exit);
                if nested {
                    callee.extend(b"\x0b\0"); // end block; no fallthrough at runtime.
                }
                callee.push(0x0b);
                let validated = validate_module(&module(&[
                    (1, &ty),
                    (3, &[2, 0, type_index]),
                    (4, &[1, 0x70, 0, 1]),
                    (7, EXPORT),
                    (9, &[1, 0, 0x41, 0, 0x0b, 1, 1]),
                    (10, &code_bodies(&[&entry, &callee])),
                ]))
                .unwrap();
                let instance = validated.instantiate().unwrap();
                for (narrow, wide) in [(F32_SNAN, F64_SNAN), (i32::MIN, i64::MIN), (0, 0)] {
                    let args = [
                        Value::I32(narrow),
                        Value::I64(WIDE),
                        Value::I32(37),
                        Value::I64(wide),
                    ];
                    assert_eq!(
                        instance.invoke_with_values("answer", &args, ResultKind::I32),
                        Ok(Some(Value::I32(1))),
                    );
                    assert_eq!(
                        instance.invoke_with_values("answer", &args[..3], ResultKind::I32),
                        Err(InvokeError::ArityMismatch {
                            expected: 4,
                            actual: 3
                        }),
                    );
                    let mut wrongly_tagged = args;
                    wrongly_tagged[3] = Value::I32(wide as i32);
                    assert_eq!(
                        instance.invoke_with_values("answer", &wrongly_tagged, ResultKind::I32),
                        Err(InvokeError::ArgumentTypeMismatch {
                            index: 3,
                            expected: ValType::I64,
                            actual: ValType::I32,
                        }),
                    );
                }
            }
        }
    }
}

#[test]
fn start_calls_a_float_to_void_helper_and_integer_exports_wrap_float_parameters() {
    let ty = types(&[
        (&[], None),
        (&[F32, F64], None),
        (&[], Some(I32)),
        (&[], Some(I64)),
        (&[F32], Some(I32)),
        (&[F64], Some(I64)),
    ]);
    let mut start = vec![0];
    push_f32(&mut start, F32_SNAN);
    push_f64(&mut start, i64::MIN);
    start.extend(b"\x10\x01\x0b");
    let bytes = module(&[
        (1, &ty),
        (3, &[6, 0, 1, 2, 3, 4, 5]),
        (
            6,
            &[
                2, F32, 1, 0x43, 0, 0, 0, 0, 0x0b, F64, 1, 0x44, 0, 0, 0, 0, 0, 0, 0, 0, 0x0b,
            ],
        ),
        (7, &function_exports(&[(b"narrow", 2), (b"wide", 3)])),
        (8, &[0x80, 0]),
        (
            10,
            &code_bodies(&[
                &start,
                b"\0\x20\0\x24\0\x20\x01\x24\x01\x0b", // void helper writes both globals.
                b"\0\x23\0\x10\x04\x0b",
                b"\0\x23\x01\x10\x05\x0b",
                b"\0\x20\0\xbc\x0b",
                b"\0\x20\0\xbd\x0b",
            ]),
        ),
    ]);
    let validated = validate_module(&bytes).unwrap();
    drop(bytes);
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("narrow"), Ok(F32_SNAN));
    assert_eq!(
        instance.invoke_with_values("wide", &[], ResultKind::I64),
        Ok(Some(Value::I64(i64::MIN)))
    );
}

#[test]
fn indirect_match_checks_positional_tags_and_result_even_with_same_arity() {
    let ty = types(&[
        (&[I32], Some(I32)),
        (&MIXED, Some(F64)),
        (&[F64, I64, I32, F32], Some(F64)), // Same tags in a different order.
        (&[I32, I64, I32, F64], Some(F64)), // Same-width integer in float position.
        (&MIXED, Some(F32)),
        (&MIXED, Some(I64)),
        (&MIXED, None),
    ]);
    let mut entry = vec![0];
    push_f32(&mut entry, F32_SNAN);
    entry.extend(b"\x42\0\x41\0");
    push_f64(&mut entry, F64_SNAN);
    entry.extend(b"\x20\0\x11\x01\0\xbd\x50\x0b");
    let bytes = module(&[
        (1, &ty),
        (3, &[6, 0, 2, 3, 4, 5, 6]),
        (4, &[1, 0x70, 0, 5]),
        (7, EXPORT),
        (9, &[1, 0, 0x41, 0, 0x0b, 5, 1, 2, 3, 4, 5]),
        (
            10,
            &code_bodies(&[
                &entry, TRAP_BODY, TRAP_BODY, TRAP_BODY, TRAP_BODY, TRAP_BODY,
            ]),
        ),
    ]);
    let validated = validate_module(&bytes).unwrap();
    let instance = validated.instantiate().unwrap();
    for selector in 0..5 {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[selector]),
            Err(InvokeError::Trap(Trap::IndirectCallTypeMismatch)),
            "target {selector} must fail before executing its trap body"
        );
    }
}

#[test]
fn both_call_validators_check_dead_suffixes_and_never_consume_below_the_floor() {
    use ValType::*;
    let ty = types(&[(&[0x7d], None), (&MIXED, Some(0x7d))]);
    // The f32 argument beneath the block's floor cannot be borrowed by a live call.
    let prefix = b"\0\x20\0\x02\x40";
    for (call, selector) in [(&b"\x10\x01"[..], &b""[..]), (b"\x11\x01\0", b"\x41\0")] {
        let check = |args: &[u8], tail: &[u8], error| {
            let body = [prefix, args, selector, call, tail].concat();
            let code = code_bodies(&[&body, TRAP_BODY]);
            let mut bytes = module(&[(1, &ty), (3, &[2, 0, 1]), (4, &[1, 0x70, 0, 0])]);
            let start = push_section(&mut bytes, 10, &code) + 2;
            if let Some((offset, kind)) = error {
                expect_error(
                    &bytes,
                    start + prefix.len() + args.len() + selector.len() + offset,
                    kind,
                );
            } else {
                validate_module(&bytes).unwrap();
            }
        };
        check(
            b"\x42\0\x41\0\x44\0\0\0\0\0\0\0\0",
            b"",
            Some((
                0,
                ErrorKind::StackUnderflow {
                    depth: 3 + usize::from(!selector.is_empty()),
                },
            )),
        );
        check(
            b"\0\x42\0",
            b"",
            Some((
                0,
                ErrorKind::TypeMismatch {
                    expected: F64,
                    actual: I64,
                },
            )),
        );
        let suffix = b"\0\x44\0\0\0\0\0\0\0\0";
        check(suffix, b"\xbc\x1a\x0b\x1a\x0b", None);
        check(
            suffix,
            b"\x45",
            Some((
                call.len(),
                ErrorKind::TypeMismatch {
                    expected: I32,
                    actual: F32,
                },
            )),
        );
    }
}

#[test]
fn private_float_results_and_parameter_writes_check_explicit_wrong_tags() {
    let ty = types(&[(&[F64], Some(F64))]);
    let needs_f64 = ErrorKind::TypeMismatch {
        expected: ValType::F64,
        actual: ValType::I64,
    };
    for (body, offset, kind) in [
        (&b"\0\x42\0\x0b"[..], 3, needs_f64), // live fallthrough
        (b"\0\0\x42\0\x0f", 4, needs_f64),    // dead return
        (b"\0\0\x42\0\x0c\0", 4, needs_f64),  // dead branch to the function
        (b"\0\0\x42\0\x21\0", 4, needs_f64),  // dead parameter write
        (
            b"\0\x02\x7d\0\x0e\x01\0\x01",
            7,
            ErrorKind::BranchTableTypeMismatch {
                expected: ValType::F32,
                actual: ValType::F64,
            },
        ),
    ] {
        let mut bytes = module(&[(1, &ty), (3, FUNCTION)]);
        let start = push_section(&mut bytes, 10, &code_body(body)) + 2;
        expect_error(&bytes, start + offset, kind);
    }
}

#[test]
fn float_exports_report_the_first_padded_function_index_after_other_checks() {
    use ErrorKind::*;
    let ty = types(&[(&[], Some(I32)), (&[], Some(F32)), (&MIXED, None)]);
    let prefix = module(&[(1, &ty), (3, &[3, 0, 1, 2])]);
    let first = b"\x01x\0\x81\0";
    for (exports, offset, kind) in [
        (
            [b"\x02".as_slice(), first, b"\x01y\0\x82\0"].concat(),
            4,
            UnsupportedExportSignature { function: 1 },
        ),
        (
            b"\x01\x01y\0\x82\0".to_vec(),
            4,
            UnsupportedExportSignature { function: 2 },
        ),
        (
            [b"\x02".as_slice(), first, b"\x01y\0\x83\0"].concat(),
            9,
            InvalidIndex {
                field: IntegerField::FunctionIndex,
                index: 3,
            },
        ),
        (
            [b"\x02".as_slice(), first, b"\x01y\0\x82"].concat(),
            10,
            TruncatedInteger {
                field: IntegerField::FunctionIndex,
            },
        ),
        (
            [b"\x02".as_slice(), first, b"\x01x\0\x82\0"].concat(),
            6,
            DuplicateExportName,
        ),
        (
            [b"\x01".as_slice(), first, b"\0"].concat(),
            6,
            TrailingSectionBytes { id: 7 },
        ),
    ] {
        let mut bytes = prefix.clone();
        let at = push_section(&mut bytes, 7, &exports);
        push_section(&mut bytes, 0, &[0]); // May not terminate a truncated index.
        push_section(&mut bytes, 8, &[1]); // Also has an unsupported start signature.
        push_section(&mut bytes, 10, &code_bodies(&[TRAP_BODY; 3]));
        expect_error(&bytes, at + offset, kind);
    }

    let mut bytes = prefix.clone();
    bytes.extend(b"\x07\x07\x01\x01x\0\x81\0");
    expect_error(
        &bytes,
        bytes.len(),
        TruncatedSection {
            declared: 7,
            available: 6,
        },
    );
    let mut bytes = prefix;
    push_section(&mut bytes, 7, &[b"\x01".as_slice(), first].concat());
    expect_error(&bytes, bytes.len(), MissingSection { id: 10 });
    let body = b"\0\xff\x0b";
    let at = push_section(&mut bytes, 10, &code_bodies(&[body, TRAP_BODY, TRAP_BODY]));
    expect_error(&bytes, at + 3, UnsupportedOpcode { opcode: 0xff });
}

#[test]
fn start_signatures_still_require_void_and_no_parameters() {
    use ErrorKind::*;
    for (params, result, kind, detail) in [
        (
            &[][..],
            Some(F32),
            InvalidStartFloatResult {
                parameters: 0,
                result: ValType::F32,
            },
            "0 parameters and f32 result",
        ),
        (
            &MIXED[..],
            Some(F64),
            InvalidStartFloatResult {
                parameters: 4,
                result: ValType::F64,
            },
            "4 parameters and f64 result",
        ),
        (
            &[F32],
            None,
            InvalidStartSignature {
                parameters: 1,
                result: ResultKind::Void,
            },
            "1 parameters and void result",
        ),
        (
            &[F64],
            Some(I32),
            InvalidStartSignature {
                parameters: 1,
                result: ResultKind::I32,
            },
            "1 parameters and i32 result",
        ),
        (
            &[F32],
            Some(I64),
            InvalidStartI64Result { parameters: 1 },
            "1 parameters and i64 result",
        ),
    ] {
        let ty = types(&[(params, result)]);
        let prefix = module(&[(1, &ty), (3, FUNCTION)]);
        let mut bytes = prefix.clone();
        let at = push_section(&mut bytes, 8, b"\x80\0");
        push_section(&mut bytes, 10, &code_body(TRAP_BODY));
        expect_error(&bytes, at, kind);
        assert!(
            validate_module(&bytes)
                .unwrap_err()
                .to_string()
                .contains(detail)
        );
        for (index, offset, error) in [
            (&b"\x80\0\xff"[..], 2, TrailingSectionBytes { id: 8 }),
            (
                b"\x81\0",
                0,
                InvalidIndex {
                    field: IntegerField::FunctionIndex,
                    index: 1,
                },
            ),
        ] {
            let mut bytes = prefix.clone();
            let at = push_section(&mut bytes, 8, index);
            push_section(&mut bytes, 10, &code_body(TRAP_BODY));
            expect_error(&bytes, at + offset, error);
        }
    }
}

#[test]
fn private_float_recursion_shares_the_existing_depth_and_dispatch_budgets() {
    let ty = types(&[(&[I32], Some(I64)), (&[I32, F64], Some(F64))]);
    let mut entry = b"\0\x20\0".to_vec();
    push_f64(&mut entry, F64_SNAN);
    entry.extend(b"\x10\x01\xbd\x0b");
    let success = Ok(Some(Value::I64(F64_SNAN)));
    for (recur_nops, leaf_nops, depth, expected) in [
        (64, 9, 14, success), // 4 entry + 14 × (64 + 8) recursive + (3 + 9) leaf = 1024.
        (
            64,
            10,
            14,
            Err(InvokeError::LimitExceeded {
                limit: ExecutionLimit::InstructionDispatches,
            }),
        ),
        (
            0,
            0,
            15,
            Err(InvokeError::LimitExceeded {
                limit: ExecutionLimit::CallDepth,
            }),
        ),
    ] {
        let mut recur = b"\0\x20\0\x04\x7c".to_vec();
        recur.extend(std::iter::repeat_n(0x01, recur_nops));
        recur.extend(b"\x20\0\x41\x01\x6b\x20\x01\x10\x01\x05");
        recur.extend(std::iter::repeat_n(0x01, leaf_nops));
        recur.extend(b"\x20\x01\x0b\x0b");
        let validated = validate_module(&module(&[
            (1, &ty),
            (3, &[2, 0, 1]),
            (7, EXPORT),
            (10, &code_bodies(&[&entry, &recur])),
        ]))
        .unwrap();
        let instance = validated.instantiate().unwrap();
        assert_eq!(
            instance.invoke_with_values("answer", &[Value::I32(depth)], ResultKind::I64),
            expected
        );
        assert_eq!(
            instance.invoke_with_values("answer", &[Value::I32(1)], ResultKind::I64),
            success
        );
    }
}
