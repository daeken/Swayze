use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

struct FloatCase {
    block_type: u8,
    ty: ValType,
    reinterpret: u8,
    negative_zero: Value,
    snan: Value,
}

const CASES: [FloatCase; 2] = [
    FloatCase {
        block_type: 0x7d,
        ty: ValType::F32,
        reinterpret: 0xbc,
        negative_zero: Value::I32(i32::MIN),
        snan: Value::I32(0x7f80_0b12),
    },
    FloatCase {
        block_type: 0x7c,
        ty: ValType::F64,
        reinterpret: 0xbd,
        negative_zero: Value::I64(i64::MIN),
        snan: Value::I64(0xfff0_000b_89ab_cdef_u64 as i64),
    },
];

fn float_const(bits: Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    match bits {
        Value::I32(bits) => {
            bytes.push(0x43);
            bytes.extend(bits.to_le_bytes());
        }
        Value::I64(bits) => {
            bytes.push(0x44);
            bytes.extend(bits.to_le_bytes());
        }
    }
    bytes
}

fn bad_suffix(prefix: &[u8], suffix: &[u8], kind: ErrorKind) {
    bad_body(&[prefix, suffix].concat(), prefix.len(), kind);
}

fn mismatch(expected: ValType, actual: ValType) -> ErrorKind {
    ErrorKind::TypeMismatch { expected, actual }
}

impl FloatCase {
    fn invoke_bits(&self, body: &[u8], args: &[i32]) -> Result<Option<Value>, InvokeError> {
        let (result_byte, result_kind) = match self.snan {
            Value::I32(_) => (0x7f, ResultKind::I32),
            Value::I64(_) => (0x7e, ResultKind::I64),
        };
        let parameters = vec![0x7f; args.len()];
        let ty = types(&[(&parameters, Some(result_byte))]);
        let body = [body, &[self.reinterpret, 0x0b]].concat();
        let module = exported_function(&ty, &code_body(&body));
        let args: Vec<_> = args.iter().copied().map(Value::I32).collect();
        module
            .instantiate()
            .unwrap()
            .invoke_with_values("answer", &args, result_kind)
    }
}

#[test]
fn float_if_arms_and_blocks_fall_through_with_exact_bits() {
    for case in &CASES {
        let ty = case.block_type;
        let mut body = vec![0, 0x02, ty, 0x20, 0, 0x04, ty];
        body.extend(float_const(case.snan));
        body.push(0x05);
        body.extend(float_const(case.negative_zero));
        body.extend(b"\x0b\x0b");
        for (condition, expected) in [(0, case.negative_zero), (1, case.snan), (-1, case.snan)] {
            assert_eq!(case.invoke_bits(&body, &[condition]), Ok(Some(expected)));
        }
    }
}

#[test]
fn br_and_br_if_keep_bits_and_restore_the_outer_label_floor() {
    for case in &CASES {
        for (payload, prefix) in [
            (case.snan, case.negative_zero),
            (case.negative_zero, case.snan),
        ] {
            let mut body = vec![0];
            body.extend(float_const(prefix));
            // Outer float block; i64 junk; inner void block.
            body.extend([0x02, case.block_type, 0x42, 3, 0x02, 0x40]);
            body.extend(float_const(payload));
            body.extend(b"\x20\0\x0d\x01"); // br_if 1
            // Only fallthrough can reach this optional trap, with the payload
            // still live. After the void if, br 1 carries the same payload.
            body.extend(b"\x20\x01\x04\x40\0\x0b\x0c\x01");
            // End inner block; drop junk; unreachable supplies outer result.
            body.extend(b"\x0b\x1a\0\x0b");
            body.extend(b"\x20\x02\x1b"); // Choose the prefix or the result.
            for (condition, trap_if_fallthrough) in [(0, 0), (-1, 1)] {
                for (choose_prefix, expected) in [(0, payload), (1, prefix)] {
                    assert_eq!(
                        case.invoke_bits(&body, &[condition, trap_if_fallthrough, choose_prefix]),
                        Ok(Some(expected)),
                    );
                }
            }
            assert_eq!(
                case.invoke_bits(&body, &[0, 1, 0]),
                Err(InvokeError::Trap(Trap::Unreachable)),
            );
        }
    }
}

#[test]
fn br_table_explicit_and_default_exits_keep_bits_and_share_the_if_label() {
    for case in &CASES {
        let ty = case.block_type;
        for (payload, alternate) in [
            (case.snan, case.negative_zero),
            (case.negative_zero, case.snan),
        ] {
            for count in [0, 16] {
                let mut body = vec![0, 0x02, ty];
                body.extend(float_const(alternate)); // Prefix for the if only.
                body.extend([0x20, 0, 0x04, ty, 0x41, 42]); // i32 junk in then.
                body.extend(float_const(payload));
                body.extend(b"\x20\x01\x0e");
                let count_offset = body.len();
                body.push(count);
                body.extend(vec![0; count as usize]); // Explicit: leave if.
                body.extend([1, 0x1b, 0x05]); // Default: leave block. Dead select yields an unknown.
                body.extend(float_const(alternate));
                body.extend(b"\x0c\0\x0b"); // Else also branches to if; implicit dead result.
                body.extend(b"\x20\x02\x1b\x0b"); // Select after if, but not on default.
                for index in [16, -1] {
                    assert_eq!(case.invoke_bits(&body, &[1, index, 1]), Ok(Some(payload)));
                }
                assert_eq!(case.invoke_bits(&body, &[0, 0, 0]), Ok(Some(alternate)));
                if count == 16 {
                    assert_eq!(case.invoke_bits(&body, &[1, 15, 0]), Ok(Some(payload)));
                    assert_eq!(case.invoke_bits(&body, &[1, 15, 1]), Ok(Some(alternate)));
                    body[count_offset] = 17;
                    bad_typed_body(
                        &i32_type(3),
                        &body,
                        count_offset,
                        ErrorKind::UnsupportedCount {
                            field: IntegerField::BranchTableCount,
                            count: 17,
                        },
                    );
                } else {
                    assert_eq!(case.invoke_bits(&body, &[1, 0, 1]), Ok(Some(payload)));
                }
            }
        }
    }
}

#[test]
fn br_table_reports_incompatible_label_at_the_explicit_or_default_index() {
    use ErrorKind::{BranchTableArityMismatch, BranchTableTypeMismatch};
    use ValType::{F32, F64, I32, I64};
    // Labels: 0 void loop, 1 i64 block, 2 f64 block, 3 f32 block, 4 i32 function.
    let prefix = b"\0\x02\x7d\x02\x7c\x02\x7e\x03\x40\0\x0e";
    let wrong_type = |expected, actual| BranchTableTypeMismatch { expected, actual };
    let wrong_arity = |expected, actual| BranchTableArityMismatch { expected, actual };
    for (table, kind) in [
        (&[2, 3, 0x82, 0, 3][..], wrong_type(F32, F64)), // Padded explicit index.
        (&[1, 2, 3][..], wrong_type(F64, F32)),          // Default index.
        (&[2, 1, 3, 1][..], wrong_type(I64, F32)),
        (&[1, 2, 4][..], wrong_type(F64, I32)),
        (&[1, 0, 3][..], wrong_arity(0, 1)),
        (&[1, 2, 0][..], wrong_arity(1, 0)),
    ] {
        bad_body(&[prefix.as_slice(), table].concat(), prefix.len() + 2, kind);
    }
}

#[test]
fn float_delimiters_check_else_arity_type_and_live_arm_floors() {
    use ErrorKind::*;
    for (case, other) in [(&CASES[0], &CASES[1]), (&CASES[1], &CASES[0])] {
        let prefix = [0, 0x41, 0, 0x04, case.block_type];
        let value = float_const(case.snan);
        let wrong = float_const(other.snan);
        for arm in [value.as_slice(), b"\0"] {
            let body = [prefix.as_slice(), arm].concat();
            bad_body(&body, body.len(), TruncatedByte { expected: 0x05 });
            bad_suffix(&body, b"\x0b", MissingElse);
        }
        for arm in [&b""[..], b"\0"] {
            let kind = mismatch(case.ty, other.ty);
            let head = [prefix.as_slice(), arm, wrong.as_slice()].concat();
            bad_suffix(&head, b"\x05", kind);
            let head = [prefix.as_slice(), b"\0\x05", arm, wrong.as_slice()].concat();
            bad_suffix(&head, b"\x0b", kind);
        }
        // The float below the then floor cannot satisfy its result at else.
        let head = [&b"\0"[..], &value, &prefix[1..]].concat();
        bad_suffix(&head, b"\x05", InvalidResultDepth { depth: 0 });
        let head = [&[0, 0x02, case.block_type][..], &value, &value].concat();
        bad_suffix(&head, b"\x0b", InvalidResultDepth { depth: 2 });

        let selector = [&b"\0"[..], value.as_slice()].concat();
        bad_suffix(
            &selector,
            &[0x04, case.block_type],
            mismatch(ValType::I32, case.ty),
        );
        // Even a dead then arm must not lend a result or its flag to else.
        let head = [prefix.as_slice(), b"\0\x05"].concat();
        bad_suffix(&head, b"\x0c\0", StackUnderflow { depth: 0 });
    }
}

#[test]
fn transfers_check_active_floors_and_concrete_inputs_in_dead_code() {
    use ErrorKind::*;
    for (case, other) in [(&CASES[0], &CASES[1]), (&CASES[1], &CASES[0])] {
        let value = float_const(case.snan);
        let wrong = float_const(other.snan);
        let head = [&[0, 0x02, case.block_type], value.as_slice(), b"\x02\x40"].concat();
        for (transfer, selector) in [
            (&b"\x0c\x01"[..], &b""[..]),
            (b"\x0d\x01", b"\x41\0"),
            (b"\x0e\0\x01", b"\x41\0"),
        ] {
            // The outer float is frozen, and underflow takes precedence
            // over the type of the only supplied operand (the selector).
            let supplied = if selector.is_empty() {
                &[]
            } else {
                value.as_slice()
            };
            let prefix = [head.as_slice(), supplied].concat();
            bad_suffix(
                &prefix,
                transfer,
                StackUnderflow {
                    depth: usize::from(!selector.is_empty()),
                },
            );
            for dead in [&b""[..], b"\0"] {
                for (payload, actual) in [(wrong.as_slice(), other.ty), (b"\x41\0", ValType::I32)] {
                    let prefix = [head.as_slice(), dead, payload, selector].concat();
                    bad_suffix(&prefix, transfer, mismatch(case.ty, actual));
                }
                if !selector.is_empty() {
                    let prefix =
                        [head.as_slice(), dead, value.as_slice(), wrong.as_slice()].concat();
                    bad_suffix(&prefix, transfer, mismatch(ValType::I32, other.ty));
                }
            }
        }
        // Target the integer function, not the float block.
        for transfer in [
            &b"\x0f"[..],
            b"\x0c\x01",
            b"\x41\0\x0d\x01",
            b"\x41\0\x0e\0\x01",
        ] {
            let body = [&[0, 0x02, case.block_type], value.as_slice(), transfer].concat();
            let at = 3 + value.len() + if transfer[0] == 0x41 { 2 } else { 0 };
            bad_body(&body, at, mismatch(ValType::I32, case.ty));
        }
    }
}

#[test]
fn dead_joins_and_br_if_restore_a_counted_concrete_float() {
    use ErrorKind::*;
    for case in &CASES {
        for input in [&b""[..], b"\x1b\x41\0"] {
            // No concrete branch input is required here, but the fallthrough
            // of br_if must still have a concrete result above the dead floor.
            let prefix = [&[0, 0x02, case.block_type, 0], input, b"\x0d\0"].concat();
            bad_suffix(&prefix, b"\x45", mismatch(ValType::I32, case.ty));
            assert_eq!(
                case.invoke_bits(&[prefix.as_slice(), b"\x0b"].concat(), &[]),
                Err(InvokeError::Trap(Trap::Unreachable))
            );
        }
        for child in [
            vec![0x02, case.block_type, 0, 0x1b],
            vec![0x04, case.block_type, 0, 0x05, 0],
        ] {
            // A dead parent stays dead across a child join but sees its type.
            let prefix = [&b"\0\0"[..], &child, b"\x0b"].concat();
            bad_suffix(&prefix, b"\x45", mismatch(ValType::I32, case.ty));
            assert_eq!(
                case.invoke_bits(&prefix, &[]),
                Err(InvokeError::Trap(Trap::Unreachable))
            );
        }
        // A nested frame starts live, even in a dead parent.
        bad_body(
            &[0, 0, 0x02, case.block_type, 0x0c, 0],
            4,
            StackUnderflow { depth: 0 },
        );
    }
}

#[test]
fn float_labels_use_the_exact_shared_control_and_operand_caps() {
    for case in &CASES {
        let value = float_const(case.snan);
        let limit = Limit::ControlDepth;
        assert_eq!(limit.maximum(), 16);
        let mut body = vec![0];
        body.extend([0x02, case.block_type].repeat(16));
        body.extend(&value);
        body.extend([0x0b; 16]);
        assert_eq!(case.invoke_bits(&body, &[]), Ok(Some(case.snan)));
        body.splice(33..33, [0x02, case.block_type]);
        bad_body(&body, 33, ErrorKind::LimitExceeded { limit });

        let limit = Limit::OperandStackDepth;
        assert_eq!(limit.maximum(), 32);
        // One float result fits above 31 frozen integer operands.
        let mut body = [
            &b"\0"[..],
            &b"\x41\0".repeat(31),
            &[0x02, case.block_type],
            &value,
            b"\x0b",
        ]
        .concat();
        body.extend([0x1a; 32]);
        body.extend(b"\x41\x2a\x0b");
        assert_body_result(&body, 42);
        let full = [
            &b"\0"[..],
            &b"\x41\0".repeat(32),
            &[0x02, case.block_type, 0],
        ]
        .concat();
        for suffix in [&b"\x0b"[..], b"\x0d\0"] {
            bad_suffix(&full, suffix, ErrorKind::LimitExceeded { limit });
        }
    }
}

#[test]
fn float_labels_do_not_change_body_byte_instruction_or_dispatch_limits() {
    for case in &CASES {
        let value = float_const(case.snan);
        let tail = [value.as_slice(), b"\x0b"].concat();
        let limit = Limit::CodeBodyBytes;
        assert_eq!(limit.maximum(), 256);
        let mut body = vec![0, 0x02, case.block_type];
        let size_before_reinterpret_and_end = 254;
        while body.len() + value.len() + 1 + tail.len() <= size_before_reinterpret_and_end {
            body.extend(&value);
            body.push(0x1a);
        }
        body.resize(size_before_reinterpret_and_end - tail.len(), 0x01);
        body.extend(&tail);
        assert_eq!(case.invoke_bits(&body, &[]), Ok(Some(case.snan)));
        body.splice(3..3, [0x01]);
        body.extend([case.reinterpret, 0x0b]);
        bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });

        let limit = Limit::BodyInstructions;
        assert_eq!(limit.maximum(), 127);
        let mut body = vec![0, 0x02, case.block_type];
        body.extend([0x01; 125]);
        body.extend(&tail);
        assert_eq!(case.invoke_bits(&body, &[]), Ok(Some(case.snan)));
        body.extend([0x01, case.reinterpret, 0x0b]);
        bad_body(&body, body.len() - 2, ErrorKind::LimitExceeded { limit });

        let limit = ExecutionLimit::InstructionDispatches;
        assert_eq!(limit.maximum(), 1024);
        let mut body = b"\0\x03\x40\x20\0\x41\x01\x6b\x22\0\x04".to_vec();
        body.push(case.block_type);
        body.extend(&value);
        body.extend(b"\x0c\0\x05");
        body.extend(float_const(case.negative_zero));
        body.extend([0x0b, case.reinterpret, 0x1a]);
        body.extend(b"\x20\0\x0d\0\x0b\x01\x41\x2a\x0b");
        // 92 continuing iterations at 11 dispatches, one final at 10,
        // a nop, and an i32.const. The then branch skips the else jump.
        let mut overflow = body.clone();
        overflow.insert(body.len() - 3, 0x01);
        let module = exported_functions(1, &[(b"exact", 0), (b"overflow", 1)], &[&body, &overflow]);
        let instance = module.instantiate().unwrap();
        assert_eq!(
            instance.invoke_i32_with_args("overflow", &[93]),
            Err(InvokeError::LimitExceeded { limit })
        );
        assert_eq!(instance.invoke_i32_with_args("exact", &[93]), Ok(42));
    }
}
