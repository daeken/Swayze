use super::{i64_signatures::types, *};
use swayze_wasm::{ResultKind, ValType, Value};

const F32: u8 = 0x7d;
const F64: u8 = 0x7c;
const I32_TO_F32: u8 = 0xbe;
const I64_TO_F64: u8 = 0xbf;
const F32_TO_I32: u8 = 0xbc;
const F64_TO_I64: u8 = 0xbd;

#[test]
fn interleaved_float_and_integer_locals_move_exact_bits_after_integer_parameters() {
    let ty = types(&[(&[0x7f, 0x7e], Some(0x7f))]);
    #[rustfmt::skip]
    let body = [
        6, 1, F32, 1, 0x7e, 0, F64, 2, F64, 1, 0x7f, 1, F32,
        // Slots 0,1 are the integer parameters; 2..7 are declarations.
        0x20, 2, F32_TO_I32, 0x45, // Reinterpret before eqz: only +0 counts.
        0x20, 3, 0x50, 0x71,
        0x20, 4, F64_TO_I64, 0x50, 0x71,
        0x20, 5, F64_TO_I64, 0x50, 0x71,
        0x20, 6, 0x45, 0x71,
        0x20, 7, F32_TO_I32, 0x45, 0x71,
        0x20, 0, 0x21, 6, // Copy i32 parameter to i32 local.
        0x20, 1, 0x21, 3, // Copy i64 parameter to i64 local.
        0x20, 6, I32_TO_F32, 0x21, 2,
        0x20, 3, I64_TO_F64, 0x21, 4,
        0x20, 2, 0x22, 7, F32_TO_I32, 0x20, 0, 0x46, 0x71, // Tee's result.
        0x20, 7, F32_TO_I32, 0x20, 0, 0x46, 0x71, // Tee's write.
        0x20, 4, 0x22, 5, F64_TO_I64, 0x20, 1, 0x51, 0x71,
        0x20, 5, F64_TO_I64, 0x20, 1, 0x51, 0x71,
        0x0b,
    ];
    let module = exported_function(&ty, &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (f32_bits, f64_bits) in [
        (0x8000_0000_u32, 0x8000_0000_0000_0000_u64), // Negative zero.
        (0xff80_0b12, 0x7ff0_000b_89ab_cdef),         // Signaling NaNs, varied payload bits.
        (0x7fc1_2345, 0xfff8_7654_3210_0b01),         // Quiet NaNs.
        (1, 0x0000_abcd_0000_0000),                   // Subnormals.
        (0, 0),
    ] {
        let args = [Value::I32(f32_bits as i32), Value::I64(f64_bits as i64)];
        assert_eq!(
            instance.invoke_with_values("answer", &args, ResultKind::I32),
            Ok(Some(Value::I32(1))),
            "f32 bits {f32_bits:08x}, f64 bits {f64_bits:016x}",
        );
    }
}

#[test]
fn tee_preserves_signaling_nan_bits_and_the_next_invocation_reads_positive_zero() {
    for (float, int, to_float, to_int, ne, result, snan, zero) in [
        (
            F32,
            0x7f,
            I32_TO_F32,
            F32_TO_I32,
            0x47,
            ResultKind::I32,
            Value::I32(0xff80_0b12_u32 as i32),
            Value::I32(0),
        ),
        (
            F64,
            0x7e,
            I64_TO_F64,
            F64_TO_I64,
            0x52,
            ResultKind::I64,
            Value::I64(0x7ff0_000b_89ab_cdef),
            Value::I64(0),
        ),
    ] {
        let ty = types(&[(&[int, 0x7f], Some(int))]);
        #[rustfmt::skip]
        let body = [
            1, 1, float, // Float local 2 after two integer parameters.
            0x20, 1, 0x04, int, // if write (result int)
                0x20, 0, to_float, 0x22, 2, to_int, // Return the value left by tee as raw bits.
                0x20, 2, to_int, 0x20, 0, ne, // Check the saved bits independently.
                0x04, 0x40, 0x00, 0x0b, // Trap if the stored bits changed.
            0x05, // else
                0x20, 2, to_int, // Read the same slot without writing.
            0x0b, 0x0b,
        ];
        let module = exported_function(&ty, &code_body(&body));
        let instance = module.instantiate().unwrap();
        assert_eq!(
            instance.invoke_with_values("answer", &[snan, Value::I32(1)], result),
            Ok(Some(snan)),
        );
        assert_eq!(
            instance.invoke_with_values("answer", &[snan, Value::I32(0)], result),
            Ok(Some(zero)),
        );
    }
}

#[test]
fn recursive_calls_preserve_caller_locals_and_clear_reused_frames() {
    for (ty, to_float, to_int, eqz, eq, extend) in [
        (F32, I32_TO_F32, F32_TO_I32, 0x45, 0x46, &[][..]),
        (F64, I64_TO_F64, F64_TO_I64, 0x50, 0x51, &[0xac][..]),
    ] {
        let depth_plus_one = [0x20, 0, 0x41, 1, 0x6a];
        let mut body = vec![1, 1, ty, 0x20, 1, to_int, eqz]; // Was local 1 exact +0?
        body.extend(depth_plus_one);
        body.extend(extend);
        body.extend([to_float, 0x21, 1]); // Save different nonzero bits at each depth.
        body.extend([0x20, 0, 0x04, 0x7f]); // if depth != 0 (result i32)
        for _ in 0..2 {
            body.extend([0x20, 0, 0x41, 1, 0x6b, 0x10, 0]); // Reuse the callee slot.
        }
        body.extend([0x71, 0x05, 0x41, 1, 0x0b, 0x71]); // Combine initial and child checks.
        body.extend([0x20, 1, to_int]);
        body.extend(depth_plus_one);
        body.extend(extend);
        body.extend([eq, 0x71, 0x0b]);
        let module = exported_function(&i32_type(1), &code_body(&body));
        let instance = module.instantiate().unwrap();
        for depth in [0, 3, 2, 3] {
            assert_eq!(instance.invoke_i32_with_args("answer", &[depth]), Ok(1));
        }
    }
}

#[test]
fn interleaved_groups_share_the_sixteen_local_cap_and_parameter_first_index_bound() {
    let ty = i32_type(4);
    // Padded group count, final nonempty count, and final index. With the four
    // parameters, slot 19 is the sixteenth declaration. The empty group adds no slot.
    let locals = [0x85, 0, 4, F32, 5, 0x7e, 6, 0x7f, 0x81, 0, F64, 0, F32];
    let ops = [
        0x20, 3, 0xac, I64_TO_F64, 0x21, 0x93, 0, 0x20, 0x93, 0, F64_TO_I64, 0xa7, 0x0b,
    ];
    let body = [&locals[..], &ops].concat();
    let module = exported_function(&ty, &code_body(&body));
    assert_eq!(
        module
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[1, 2, 3, -42]),
        Ok(-42),
    );
    let mut body = locals.to_vec();
    body[11] = 1; // Seventeenth declaration, even without reading its type.
    body.truncate(12);
    bad_typed_body(
        &ty,
        &body,
        11,
        ErrorKind::UnsupportedCount {
            field: IntegerField::LocalCount,
            count: 1,
        },
    );
    for tail in [&[][..], &[0x00][..]] {
        for op in [0x20, 0x21, 0x22] {
            let body = [&locals[..], tail, &[op, 0x94, 0, 0x0b]].concat();
            bad_typed_body(
                &ty,
                &body,
                locals.len() + tail.len() + 1,
                ErrorKind::InvalidIndex {
                    field: IntegerField::LocalIndex,
                    index: 20,
                },
            );
        }
    }
}

#[test]
fn group_types_and_indices_cannot_read_past_the_body_frame() {
    for (body, suffix, kind) in [
        (
            &[2, 1, F32, 0][..],
            [F32, 0x0b],
            ErrorKind::TruncatedByte { expected: 0x7f },
        ),
        (
            &[1, 1, F64, 0x22, 0x80],
            [0, 0x0b],
            ErrorKind::TruncatedInteger {
                field: IntegerField::LocalIndex,
            },
        ),
    ] {
        let mut code = code_body(body);
        let end = code.len();
        code.extend(suffix); // Could complete the immediate, but outside the body.
        bad_code(&code, end, kind);
    }
}

#[test]
fn local_types_are_checked_in_live_and_dead_code() {
    use ValType::{F32 as Float32, F64 as Float64, I32, I64};
    // Slots 0/1/2/3 are f32/f64/i32/i64.
    let locals = [4, 1, F32, 1, F64, 1, 0x7f, 1, 0x7e];
    for dead in [&[][..], &[0x00][..]] {
        for (from, to, expected, actual) in [
            (0, 1, Float64, Float32),
            (1, 0, Float32, Float64),
            (0, 2, I32, Float32),
            (3, 1, Float64, I64),
        ] {
            for op in [0x21, 0x22] {
                let body = [&locals[..], dead, &[0x20, from, op, to, 0x0b]].concat();
                bad_body(
                    &body,
                    locals.len() + dead.len() + 2,
                    ErrorKind::TypeMismatch { expected, actual },
                );
            }
        }
    }
}

#[test]
fn float_local_writes_respect_the_active_floor_even_in_dead_tails() {
    for (ty, actual, to_int) in [
        (F32, ValType::F32, F32_TO_I32),
        (F64, ValType::F64, F64_TO_I64),
    ] {
        let head = [1, 1, ty, 0x20, 0, 0x02, 0x40]; // Same-type prefix below the block floor.
        for op in [0x21, 0x22] {
            for state in [&[][..], &[0x00, 0x02, 0x40][..]] {
                // An inner frame starts reachable even when its parent is dead.
                let body = [&head[..], state, &[op, 0, 0x0b]].concat();
                bad_body(
                    &body,
                    head.len() + state.len(),
                    ErrorKind::StackUnderflow { depth: 0 },
                );
            }
        }
        // The taken branch preserves an i32 prefix. Dead set and tee may use
        // implicit inputs, and a polymorphic select may produce unknown input.
        let skip = [1, 1, ty, 0x41, 42, 0x02, 0x40, 0x0c, 0];
        let body = [
            &skip[..],
            &[
                0x21, 0, 0x22, 0, to_int, 0x1a, 0x1b, 0x22, 0, to_int, 0x1a, 0x0b, 0x0b,
            ],
        ]
        .concat();
        assert_body_result(&body, 42);
        for source in [&[][..], &[0x1b][..]] {
            // Tee always produces a concrete float even from an implicit or unknown input.
            let body = [&skip[..], source, &[0x22, 0, 0x45, 0x0b]].concat();
            bad_body(
                &body,
                skip.len() + source.len() + 2,
                ErrorKind::TypeMismatch {
                    expected: ValType::I32,
                    actual,
                },
            );
        }
    }
}

#[test]
fn dead_float_tee_still_needs_room_for_its_concrete_result() {
    let limit = Limit::OperandStackDepth;
    let mut body = vec![1, 1, F64];
    body.extend([0x20, 0].repeat(limit.maximum()));
    body.extend([0x02, 0x40, 0x00, 0x21, 0]); // Set at the full dead floor needs no slot.
    let offset = body.len();
    body.extend([0x22, 0, 0x0b, 0x0b]);
    for op in [0x20, 0x22] {
        body[offset] = op;
        bad_body(&body, offset, ErrorKind::LimitExceeded { limit });
    }
}

#[test]
fn each_float_local_opcode_spends_one_dispatch() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    for (ty, zero) in [
        (F32, &b"\x43\0\0\0\0"[..]),
        (F64, &b"\x44\0\0\0\0\0\0\0\0"[..]),
    ] {
        for (ops, dispatches) in [
            (vec![0x20, 1, 0x1a], 2),               // get, drop
            ([zero, &[0x21, 1]].concat(), 2),       // const, set
            ([zero, &[0x22, 1, 0x1a]].concat(), 3), // const, tee, drop
        ] {
            let mut body = vec![1, 1, ty, 0x41, 42, 0x03, 0x40];
            body.extend(&ops);
            body.extend([0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b]);
            // One dispatch before the loop and five more per iteration to count down.
            let per_iteration = dispatches + 5;
            let iterations = i32::try_from((limit.maximum() - 1) / per_iteration).unwrap();
            let nops = (limit.maximum() - 1) % per_iteration;
            for (nops, expected) in [
                (nops, Ok(42)),
                (nops + 1, Err(InvokeError::LimitExceeded { limit })),
            ] {
                let body = [&body[..], &vec![0x01; nops], &[0x0b]].concat();
                let module = exported_function(&i32_type(1), &code_body(&body));
                assert_eq!(
                    module
                        .instantiate()
                        .unwrap()
                        .invoke_i32_with_args("answer", &[iterations]),
                    expected,
                    "local type {ty:02x}, loop ops {ops:02x?}, {nops} trailing nops",
                );
            }
        }
    }
}
