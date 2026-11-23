use super::{i64_signatures::types, *};
use ValType::{F32, F64, I32, I64};
use swayze_wasm::{ResultKind, ValType, Value};

const F32_CONST: u8 = 0x43;
const F64_CONST: u8 = 0x44;
const I32_FROM_F32: u8 = 0xbc;
const I64_FROM_F64: u8 = 0xbd;
const F32_FROM_I32: u8 = 0xbe;
const F64_FROM_I64: u8 = 0xbf;
const F32_ABS: u8 = 0x8b;
const F32_NEG: u8 = 0x8c;
const F32_COPYSIGN: u8 = 0x98;
const F64_ABS: u8 = 0x99;
const F64_NEG: u8 = 0x9a;
const F64_COPYSIGN: u8 = 0xa6;
const I64_RESULT: &[u8] = &[1, 0x60, 0, 1, 0x7e];
const F32_SNAN: u32 = 0x7f80_0b12; // Includes a literal `end` byte.
const F64_SNAN: u64 = 0xfff0_000b_89ab_cdef;

fn push_f32(body: &mut Vec<u8>, bits: u32) {
    body.push(F32_CONST);
    body.extend(bits.to_le_bytes());
}

fn push_f64(body: &mut Vec<u8>, bits: u64) {
    body.push(F64_CONST);
    body.extend(bits.to_le_bytes());
}

fn assert_bits(ty: &[u8], body: &[u8], args: &[Value], result: ResultKind, expected: Value) {
    let module = exported_function(ty, &code_body(body));
    assert_eq!(
        module
            .instantiate()
            .unwrap()
            .invoke_with_values("answer", args, result),
        Ok(Some(expected)),
        "body {body:02x?} with {args:?}"
    );
}

#[test]
fn fixed_width_constants_and_all_reinterpretations_preserve_every_bit() {
    // Positive/negative zero, signaling/quiet NaNs with payloads, a subnormal,
    // and all ones. No floating-point value crosses the host boundary.
    let i32_param = i32_type(1);
    for bits in [0, 0x8000_0000, F32_SNAN, 0xffc1_2345, 1, u32::MAX] {
        let value = Value::I32(bits as i32);
        let mut body = vec![0];
        push_f32(&mut body, bits);
        body.extend([I32_FROM_F32, 0x0b]);
        assert_bits(TYPE, &body, &[], ResultKind::I32, value);
        let round_trip = [0, 0x20, 0, F32_FROM_I32, I32_FROM_F32, 0x0b];
        assert_bits(&i32_param, &round_trip, &[value], ResultKind::I32, value);
    }
    let i64_param = types(&[(&[0x7e], Some(0x7e))]);
    for bits in [0, 1 << 63, F64_SNAN, 0x7ff8_7654_3210_0b01, 1, u64::MAX] {
        let value = Value::I64(bits as i64);
        let mut body = vec![0];
        push_f64(&mut body, bits);
        body.extend([I64_FROM_F64, 0x0b]);
        assert_bits(I64_RESULT, &body, &[], ResultKind::I64, value);
        let round_trip = [0, 0x20, 0, F64_FROM_I64, I64_FROM_F64, 0x0b];
        assert_bits(&i64_param, &round_trip, &[value], ResultKind::I64, value);
    }
}

#[test]
fn sign_operations_preserve_every_non_sign_bit() {
    let f32_pairs = [
        (0x0000_0000, 0x8000_0000), // Zero.
        (0x7f80_0000, 0xff80_0000), // Infinity.
        (0x40ad_4321, 0xc0ad_4321), // Normal.
        (0x0012_3456, 0x8012_3456), // Subnormal.
        (0x7fc1_2345, 0xffc1_2345), // Quiet NaN.
        (F32_SNAN, 0xff80_0b12),    // Signaling NaN.
    ]
    .map(|(pos, neg): (u32, u32)| (Value::I32(pos as i32), Value::I32(neg as i32)));
    let f64_pairs = [
        (0x0000_0000_0000_0000, 0x8000_0000_0000_0000),
        (0x7ff0_0000_0000_0000, 0xfff0_0000_0000_0000),
        (0x43ad_cdef_789a_4321, 0xc3ad_cdef_789a_4321),
        (0x0001_2345_6789_abcd, 0x8001_2345_6789_abcd),
        (0x7ff8_7654_3210_0b01, 0xfff8_7654_3210_0b01),
        (0x7ff0_000b_89ab_cdef, F64_SNAN),
    ]
    .map(|(pos, neg): (u64, u64)| (Value::I64(pos as i64), Value::I64(neg as i64)));

    for (int, result, [to_float, to_int], [abs, neg, copysign], pairs) in [
        (
            0x7f,
            ResultKind::I32,
            [F32_FROM_I32, I32_FROM_F32],
            [F32_ABS, F32_NEG, F32_COPYSIGN],
            f32_pairs,
        ),
        (
            0x7e,
            ResultKind::I64,
            [F64_FROM_I64, I64_FROM_F64],
            [F64_ABS, F64_NEG, F64_COPYSIGN],
            f64_pairs,
        ),
    ] {
        let unary_type = types(&[(&[int], Some(int))]);
        let binary_type = types(&[(&[int; 2], Some(int))]);
        let body = [
            0, 0x20, 0, to_float, 0x20, 1, to_float, copysign, to_int, 0x0b,
        ];
        let [zero, _, _, _, quiet, signaling] = pairs;
        for (positive, negative) in pairs {
            for (input, opposite) in [(positive, negative), (negative, positive)] {
                for (opcode, expected) in [(abs, positive), (neg, opposite)] {
                    let body = [0, 0x20, 0, to_float, opcode, to_int, 0x0b];
                    assert_bits(&unary_type, &body, &[input], result, expected);
                }
                // Both zero signs and NaN signs are significant, but the rhs
                // magnitude (including NaN quiet bit and payload) is ignored.
                for (rhs, expected) in [
                    (zero.0, positive),
                    (zero.1, negative),
                    (quiet.0, positive),
                    (signaling.1, negative),
                ] {
                    assert_bits(&binary_type, &body, &[input, rhs], result, expected);
                }
            }
        }
    }
}

#[test]
fn select_chooses_exact_bits_of_either_same_width_candidate() {
    let i64_result = types(&[(&[0x7f], Some(0x7e))]);
    let mut f32_body = vec![0];
    push_f32(&mut f32_body, F32_SNAN);
    push_f32(&mut f32_body, 0x8000_0000);
    f32_body.extend([0x20, 0, 0x1b, I32_FROM_F32, 0x0b]);
    let mut f64_body = vec![0];
    push_f64(&mut f64_body, F64_SNAN);
    push_f64(&mut f64_body, 1 << 63);
    f64_body.extend([0x20, 0, 0x1b, I64_FROM_F64, 0x0b]);
    for cond in [0, 1, -1] {
        assert_bits(
            &i32_type(1),
            &f32_body,
            &[Value::I32(cond)],
            ResultKind::I32,
            Value::I32(if cond == 0 { i32::MIN } else { F32_SNAN as i32 }),
        );
        assert_bits(
            &i64_result,
            &f64_body,
            &[Value::I32(cond)],
            ResultKind::I64,
            Value::I64(if cond == 0 { i64::MIN } else { F64_SNAN as i64 }),
        );
    }
}

#[test]
fn float_prefix_survives_inner_control_floor_and_integer_call() {
    let mut caller = vec![0];
    push_f64(&mut caller, F64_SNAN);
    caller.extend([0x02, 0x7f, 0x10, 1, 0x0b, 0x1a, I64_FROM_F64, 0x0b]);
    let ty = types(&[(&[], Some(0x7e)), (&[], Some(0x7f))]);
    let bytes = module(&[
        (1, &ty),
        (3, &[2, 0, 1]),
        (7, EXPORT),
        (10, &code_bodies(&[&caller, b"\0\x41\x2a\x0b"])),
    ]);
    let validated = validate_module(&bytes).unwrap();
    assert_eq!(
        validated
            .instantiate()
            .unwrap()
            .invoke_with_values("answer", &[], ResultKind::I64),
        Ok(Some(Value::I64(F64_SNAN as i64)))
    );
}

#[test]
fn float_immediates_stop_at_the_body_and_section_bounds() {
    for (opcode, ty, width) in [(F32_CONST, F32, 4), (F64_CONST, F64, 8)] {
        let kind = ErrorKind::TruncatedFloatConst { ty };
        for available in 0..width {
            let mut body = vec![0, 0, opcode]; // Decode the immediate even in dead code.
            body.resize(body.len() + available, 0x0b); // Never terminate an immediate early.
            bad_body(&body, body.len(), kind); // The body ends with the code section.
            let mut bytes = module(&[(1, TYPE), (3, TWO_FUNCTIONS)]);
            let code = code_bodies(&[&body, &[0, 0x01, 0x01, 0x01, 0x01, 0x01, 0x41, 42, 0x0b]]);
            let start = push_section(&mut bytes, 10, &code);
            // Another body has enough bytes but must not satisfy the immediate.
            expect_error(&bytes, start + 2 + body.len(), kind);
        }
        // A byte that looks like `end` belongs to the immediate; no actual end follows.
        let mut body = vec![0, opcode];
        body.resize(2 + width, 0x0b);
        bad_body(
            &body,
            body.len(),
            ErrorKind::TruncatedByte { expected: 0x0b },
        );
        assert_eq!(
            Error { offset: 9, kind }.to_string(),
            format!("WebAssembly at byte 9: truncated fixed-width {ty}.const immediate")
        );
    }
}

fn mismatch(expected: ValType, actual: ValType) -> ErrorKind {
    ErrorKind::TypeMismatch { expected, actual }
}

#[test]
fn reinterpret_requires_its_exact_source_type_and_produces_a_concrete_type() {
    for (opcode, expected, actual, producer) in [
        (I32_FROM_F32, F32, F64, &b"\x44\0\0\0\0\0\0\0\0"[..]),
        (I64_FROM_F64, F64, I64, &b"\x42\0"[..]),
        (F32_FROM_I32, I32, F32, &b"\x43\0\0\0\0"[..]),
        (F64_FROM_I64, I64, I32, &b"\x41\0"[..]),
    ] {
        for prefix in [&[0][..], &[0, 0][..]] {
            let mut body = [prefix, producer].concat();
            let offset = body.len();
            body.extend([opcode, 0x1a, 0x41, 42, 0x0b]);
            bad_body(&body, offset, mismatch(expected, actual));
        }
        exported_function(TYPE, &code_body(&[0, 0, opcode, 0x1a, 0x0b]));
    }
    bad_body(&[0, 0, F32_FROM_I32, 0x45, 0x0b], 3, mismatch(I32, F32));
    bad_typed_body(
        I64_RESULT,
        &[0, 0, F64_FROM_I64, 0x0b],
        3,
        mismatch(I64, F64),
    );
    bad_typed_body(
        I64_RESULT,
        &[0, 0, I32_FROM_F32, 0x0b],
        3,
        mismatch(I64, I32),
    );
    bad_body(&[0, 0, I64_FROM_F64, 0x0b], 3, mismatch(I32, I64));
}

#[test]
fn sign_operations_require_same_width_float_operands_even_in_dead_code() {
    let zero32 = &b"\x43\0\0\0\0"[..];
    let zero64 = &b"\x44\0\0\0\0\0\0\0\0"[..];
    let int32 = &b"\x41\0"[..];
    let int64 = &b"\x42\0"[..];
    for (ty, other, int, zero, wrong_width, wrong_kind, ops) in [
        (
            F32,
            F64,
            I32,
            zero32,
            zero64,
            int32,
            [F32_ABS, F32_NEG, F32_COPYSIGN],
        ),
        (
            F64,
            F32,
            I64,
            zero64,
            zero32,
            int64,
            [F64_ABS, F64_NEG, F64_COPYSIGN],
        ),
    ] {
        for (index, opcode) in ops.into_iter().enumerate() {
            let binary = index == 2;
            for prefix in [&[0][..], &[0, 0][..]] {
                for (wrong, actual) in [(wrong_width, other), (wrong_kind, int)] {
                    let mut body = [prefix, if binary { zero } else { &[] }, wrong].concat();
                    let offset = body.len();
                    body.extend([opcode, 0x1a, 0x41, 42, 0x0b]);
                    bad_body(&body, offset, mismatch(ty, actual));
                    if binary {
                        // The lhs must also have the required type.
                        let mut body = [prefix, wrong, zero].concat();
                        let offset = body.len();
                        body.extend([opcode, 0x1a, 0x41, 42, 0x0b]);
                        bad_body(&body, offset, mismatch(ty, actual));
                    }
                }
            }
            // Even with entirely implicit inputs, the result is concrete.
            bad_body(&[0, 0, opcode, 0x0b], 3, mismatch(I32, ty));
        }
        let mut body = [&[0][..], wrong_kind, wrong_width].concat();
        let offset = body.len();
        body.extend([ops[2], 0x1a, 0x41, 42, 0x0b]);
        bad_body(&body, offset, mismatch(ty, other)); // Topmost mismatch first.

        let mut body = [&[0, 0][..], wrong_kind].concat();
        let offset = body.len();
        body.extend([ops[2], 0x1a, 0x0b]);
        bad_body(&body, offset, mismatch(ty, int)); // Dead floor supplies only the lhs.
    }
}

#[test]
fn sign_operations_preserve_underflow_and_dead_floor_diagnostics() {
    for opcode in [
        F32_ABS,
        F32_NEG,
        F32_COPYSIGN,
        F64_ABS,
        F64_NEG,
        F64_COPYSIGN,
    ] {
        // The i32 prefix is below an inner void block's floor.
        let mut body = vec![0, 0x41, 42, 0x02, 0x40, opcode, 0x1a, 0x0b, 0x0b];
        bad_body(&body, 5, ErrorKind::StackUnderflow { depth: 0 });
        body.insert(5, 0x00);
        let validated = exported_function(TYPE, &code_body(&body));
        assert_eq!(
            validated.instantiate().unwrap().invoke_i32("answer"),
            Err(InvokeError::Trap(Trap::Unreachable))
        );
        // An unknown slot above the dead floor is also a valid input.
        body.insert(6, 0x1b);
        exported_function(TYPE, &code_body(&body));
    }
    for opcode in [F32_COPYSIGN, F64_COPYSIGN] {
        // With one wrong-typed operand above the live floor, report underflow
        // before a type error, without counting the i32 below the floor.
        let body = [0, 0x41, 42, 0x02, 0x40, 0x41, 0, opcode, 0x1a, 0x0b, 0x0b];
        bad_body(&body, 7, ErrorKind::StackUnderflow { depth: 1 });
    }
}

#[test]
fn dead_floors_synthesize_missing_operands_but_never_look_below_the_floor() {
    let mut body = vec![0];
    push_f64(&mut body, F64_SNAN);
    body.extend([0x02, 0x40]); // The f64 is below the active void block's floor.
    let offset = body.len();
    body.extend([I32_FROM_F32, 0x1a, 0x0b, I64_FROM_F64, 0x0b]);
    bad_typed_body(
        I64_RESULT,
        &body,
        offset,
        ErrorKind::StackUnderflow { depth: 0 },
    );
    body.insert(offset, 0); // The dead block synthesizes an f32, not the actual f64 below it.
    let validated = exported_function(I64_RESULT, &code_body(&body));
    assert_eq!(
        validated
            .instantiate()
            .unwrap()
            .invoke_with_values("answer", &[], ResultKind::I64),
        Err(InvokeError::Trap(Trap::Unreachable))
    );
}

#[test]
fn select_checks_known_types_and_defers_unknowns_in_dead_code() {
    for prefix in [&[0][..], &[0, 0][..]] {
        let mut body = prefix.to_vec();
        push_f32(&mut body, F32_SNAN);
        push_f64(&mut body, F64_SNAN);
        body.extend([0x41, 0, 0x1b, 0x0b]);
        bad_body(&body, body.len() - 2, mismatch(F32, F64));
        body.truncate(prefix.len());
        push_f32(&mut body, F32_SNAN);
        push_f32(&mut body, 0);
        push_f64(&mut body, F64_SNAN);
        body.extend([0x1b, 0x0b]);
        bad_body(&body, body.len() - 2, mismatch(I32, F64));
    }
    // A fully polymorphic select can supply an f32 to reinterpretation.
    exported_function(TYPE, &code_body(&[0, 0, 0x1b, I32_FROM_F32, 0x0b]));
    let mut body = vec![0, 0, 0x1b]; // Unknown first candidate from an earlier select.
    push_f64(&mut body, F64_SNAN);
    body.extend([0x41, 0, 0x1b, I64_FROM_F64, 0x0b]);
    exported_function(I64_RESULT, &code_body(&body));
    let offset = body.len() - 2;
    body[offset] = I32_FROM_F32;
    bad_body(&body, offset, mismatch(F32, F64));
}

#[test]
fn known_floats_cannot_cross_integer_storage_and_call_boundaries() {
    for consumer in [
        &[0x21, 0][..],    // local.set
        &[0x22, 0][..],    // local.tee
        &[0x24, 0][..],    // global.set
        &[0x36, 0, 0][..], // i32.store
        &[0x10, 0][..],    // call (i32) -> i32
    ] {
        let mut body = vec![0, 0]; // An implicit address, if needed, is available below the value.
        push_f32(&mut body, F32_SNAN);
        let offset = body.len();
        body.extend(consumer);
        body.push(0x0b);
        let mut bytes = module(&[
            (1, &i32_type(1)),
            (3, FUNCTION),
            (5, &[1, 0, 1]),
            (6, &[1, 0x7f, 1, 0x41, 0, 0x0b]),
        ]);
        let start = push_section(&mut bytes, 10, &code_body(&body));
        expect_error(&bytes, start + 2 + offset, mismatch(I32, F32));
    }
}

#[test]
fn floating_point_declarations_labels_memory_and_numeric_ops_stay_unsupported() {
    let unexpected = |expected, found| ErrorKind::UnexpectedByte { expected, found };
    for ty in [0x7d, 0x7c] {
        bad_payload(1, &[1, 0x60, 1, ty, 0], 3, unexpected(0x7f, ty));
        let mut exported = HEADER.to_vec();
        let start = push_section(&mut exported, 1, &types(&[(&[], Some(ty))]));
        for (id, payload) in [(3, FUNCTION), (7, EXPORT), (10, CODE)] {
            push_section(&mut exported, id, payload);
        }
        expect_error(&exported, start + 4, unexpected(0x7f, ty));
        bad_payload(6, &[1, ty, 0], 1, unexpected(0x7f, ty));
        bad_body(&[1, 0, ty, 0x41, 42, 0x0b], 2, unexpected(0x7f, ty));
        bad_body(&[0, 0x02, ty, 0x0b, 0x0b], 2, unexpected(0x7f, ty));
        bad_body(&[0, 0x03, ty, 0x0b, 0x0b], 2, unexpected(0x40, ty));
        bad_body(&[0, 0x41, 0, 0x04, ty, 0x0b, 0x0b], 4, unexpected(0x7f, ty));
    }
    bad_payload(6, &[1, 0x7f, 0, F32_CONST], 3, unexpected(0x41, F32_CONST));
    // Test with a defined memory, and in dead code so underflow is irrelevant.
    for opcode in [0x2a, 0x2b, 0x38, 0x39, 0x5b, 0x92, 0xa0, 0xa8, 0xb2, 0x1c] {
        let mut bytes = module(&[(1, TYPE), (3, FUNCTION), (5, &[1, 0, 1])]);
        let start = push_section(&mut bytes, 10, &code_body(&[0, 0, opcode, 0, 0, 0x0b]));
        expect_error(&bytes, start + 4, ErrorKind::UnsupportedOpcode { opcode });
    }
}

#[test]
fn constants_and_reinterprets_share_existing_body_and_stack_caps() {
    let mut body = vec![0];
    for _ in 0..8 {
        push_f32(&mut body, F32_SNAN);
        push_f64(&mut body, F64_SNAN);
        body.extend([0x41, 0, 0x42, 0]);
    }
    let excess = body.len();
    body.extend([0x1a; 32]);
    body.extend([0x41, 42, 0x0b]);
    assert_body_result(&body, 42);
    body.splice(excess..excess, [F32_CONST, 0, 0, 0, 0, 0x1a]);
    bad_body(
        &body,
        excess,
        ErrorKind::LimitExceeded {
            limit: Limit::OperandStackDepth,
        },
    );

    let mut body = vec![0];
    push_f32(&mut body, F32_SNAN);
    body.extend([I32_FROM_F32, F32_FROM_I32, 0x1a]); // Four dispatches.
    push_f64(&mut body, F64_SNAN);
    body.extend([I64_FROM_F64, F64_FROM_I64, 0x1a]); // Four dispatches.
    body.extend([0x01; 118]);
    let excess = body.len();
    body.extend([0x41, 42, 0x0b]); // 127 instructions.
    assert_body_result(&body, 42);
    body.insert(excess, 0x01);
    bad_body(
        &body,
        excess + 1,
        ErrorKind::LimitExceeded {
            limit: Limit::BodyInstructions,
        },
    );

    let mut body = vec![0];
    for _ in 0..25 {
        push_f64(&mut body, F64_SNAN);
        body.push(0x1a);
    }
    body.extend([0x01, 0x01, 0x41, 42, 0x0b]);
    assert_eq!(body.len(), 256);
    assert_body_result(&body, 42);
    body.insert(1, 0x01);
    bad_code(
        &code_body(&body),
        1,
        ErrorKind::LimitExceeded {
            limit: Limit::CodeBodyBytes,
        },
    );
}

#[test]
fn sign_operations_share_the_existing_stack_and_instruction_caps() {
    let mut body = vec![0, 0x41, 42];
    for _ in 1..30 {
        body.extend([0x41, 0]);
    }
    push_f32(&mut body, F32_SNAN);
    push_f32(&mut body, 0x8000_0000);
    let full = body.len(); // All 32 operand slots occupied.
    body.extend([F32_ABS, F32_NEG, F32_COPYSIGN, 0x1a]);
    body.extend([0x1a; 29]); // Leave the first i32 as the result.
    body.extend([0x01; 62]);
    let end = body.len();
    body.push(0x0b); // 32 pushes + 3 sign ops + 30 drops + 62 nops = 127.
    assert_body_result(&body, 42);
    body.insert(end, 0x01);
    bad_body(
        &body,
        end,
        ErrorKind::LimitExceeded {
            limit: Limit::BodyInstructions,
        },
    );

    for opcode in [F32_ABS, F64_COPYSIGN] {
        // A dead block entered at full depth can synthesize its inputs but
        // still has no physical slot for the concrete float result.
        let mut at_floor = body[..full].to_vec();
        at_floor.extend([0x02, 0x40, 0x00, opcode, 0x1a, 0x0b, 0x0b]);
        bad_body(
            &at_floor,
            full + 3,
            ErrorKind::LimitExceeded {
                limit: Limit::OperandStackDepth,
            },
        );
    }
}

#[test]
fn each_sign_operation_uses_one_shared_dispatch() {
    let mut body = vec![0, 0x03, 0x40]; // Loop until --p0 == 0.
    push_f32(&mut body, F32_SNAN);
    body.extend([F32_ABS, F32_NEG]);
    push_f32(&mut body, 0);
    body.extend([F32_COPYSIGN, 0x1a]);
    push_f64(&mut body, F64_SNAN);
    body.extend([F64_ABS, F64_NEG]);
    push_f64(&mut body, 1 << 63);
    body.extend([F64_COPYSIGN, 0x1a]);
    body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]); // 17 per loop.
    body.push(0x01);
    push_f32(&mut body, F32_SNAN);
    body.push(F32_NEG);
    let last = body.len();
    body.extend([I32_FROM_F32, 0x0b]); // 17 * 60 + 4 = 1024.
    let ty = i32_type(1);
    assert_bits(
        &ty,
        &body,
        &[Value::I32(60)],
        ResultKind::I32,
        Value::I32(0xff80_0b12u32 as i32),
    );
    body.insert(last, F32_NEG);
    let validated = exported_function(&ty, &code_body(&body));
    assert_eq!(
        validated
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[60]),
        Err(InvokeError::LimitExceeded {
            limit: ExecutionLimit::InstructionDispatches
        })
    );
}

#[test]
fn constants_and_reinterprets_charge_the_shared_dispatch_budget() {
    let mut body = vec![0, 0x03, 0x40]; // Loop until --p0 == 0.
    push_f32(&mut body, F32_SNAN);
    body.extend([I32_FROM_F32, F32_FROM_I32, I32_FROM_F32, 0x1a]);
    push_f64(&mut body, F64_SNAN);
    body.extend([I64_FROM_F64, F64_FROM_I64, I64_FROM_F64, 0x1a]);
    body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]); // 15 dispatches per loop.
    body.extend([0x01, 0x01]);
    push_f32(&mut body, F32_SNAN);
    let last = body.len();
    body.extend([I32_FROM_F32, 0x0b]); // 15 * 68 + 4 = 1024 dispatches.
    let ty = i32_type(1);
    assert_bits(
        &ty,
        &body,
        &[Value::I32(68)],
        ResultKind::I32,
        Value::I32(F32_SNAN as i32),
    );
    body.insert(last, 0x01);
    let validated = exported_function(&ty, &code_body(&body));
    assert_eq!(
        validated
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[68]),
        Err(InvokeError::LimitExceeded {
            limit: ExecutionLimit::InstructionDispatches
        })
    );
}
