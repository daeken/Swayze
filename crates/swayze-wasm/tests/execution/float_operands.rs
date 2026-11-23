use super::{i64_signatures::types, *};
use ValType::{F32, F64, I32, I64};
use swayze_wasm::{ResultKind, ValType, Value};

const F32_CONST: u8 = 0x43;
const F64_CONST: u8 = 0x44;
const I32_FROM_F32: u8 = 0xbc;
const I64_FROM_F64: u8 = 0xbd;
const F32_FROM_I32: u8 = 0xbe;
const F64_FROM_I64: u8 = 0xbf;
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
fn signaling_nan_bits_survive_reinterpret_select_and_reinterpret_back() {
    let i32_signature = i32_type(2); // (bits, condition) -> bits
    let i64_signature = types(&[(&[0x7e, 0x7f], Some(0x7e))]);
    for nan_first in [false, true] {
        let mut f32_body = vec![0];
        let mut f64_body = vec![0];
        for nan in [nan_first, !nan_first] {
            if nan {
                // Input comes from integer host arguments, never host floats.
                f32_body.extend([0x20, 0, F32_FROM_I32]);
                f64_body.extend([0x20, 0, F64_FROM_I64]);
            } else {
                push_f32(&mut f32_body, 0x8000_0000);
                push_f64(&mut f64_body, 1 << 63);
            }
        }
        f32_body.extend([0x20, 1, 0x1b, I32_FROM_F32, 0x0b]);
        f64_body.extend([0x20, 1, 0x1b, I64_FROM_F64, 0x0b]);
        let nan32 = Value::I32(F32_SNAN as i32);
        let nan64 = Value::I64(F64_SNAN as i64);
        for cond in [0, 1, -1] {
            let selects_nan = (cond != 0) == nan_first;
            assert_bits(
                &i32_signature,
                &f32_body,
                &[nan32, Value::I32(cond)],
                ResultKind::I32,
                if selects_nan {
                    nan32
                } else {
                    Value::I32(i32::MIN)
                },
            );
            assert_bits(
                &i64_signature,
                &f64_body,
                &[nan64, Value::I32(cond)],
                ResultKind::I64,
                if selects_nan {
                    nan64
                } else {
                    Value::I64(i64::MIN)
                },
            );
        }
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
            // The last body ends with the code section, but a following custom
            // section contains enough bytes for the entire immediate.
            let mut bytes = module(&[(1, TYPE), (3, FUNCTION)]);
            push_section(&mut bytes, 10, &code_body(&body));
            let end = bytes.len();
            push_section(&mut bytes, 0, &vec![0; width + 1]); // Empty name and opaque bytes.
            expect_error(&bytes, end, kind);

            // This time the code section still has enough bytes: the first
            // body must not borrow them from the next body's frame.
            let mut bytes = module(&[(1, TYPE), (3, TWO_FUNCTIONS)]);
            let next = b"\0\x01\x01\x01\x01\x01\x41\x2a\x0b";
            let code = code_bodies(&[&body, next]);
            let start = push_section(&mut bytes, 10, &code);
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
fn all_six_opcodes_charge_the_shared_dispatch_budget() {
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
