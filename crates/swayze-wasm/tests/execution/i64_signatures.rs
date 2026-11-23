use super::{i64_operands::push_i64, *};
use swayze_wasm::{ResultKind, ValType};

const I32: u8 = 0x7f;
const I64: u8 = 0x7e;
const MIXED: [u8; 4] = [I64, I32, I64, I32];
const LEFT: i64 = 0x7123_4567_89ab_cdef;
const RIGHT: i64 = -0x1234_5678_7654_3211; // Same low word, different high word.
const TRAP_BODY: &[u8] = b"\0\0\x0b";
const NEED_I32: ErrorKind = ErrorKind::TypeMismatch {
    expected: ValType::I32,
    actual: ValType::I64,
};
const NEED_I64: ErrorKind = ErrorKind::TypeMismatch {
    expected: ValType::I64,
    actual: ValType::I32,
};

fn types(signatures: &[(&[u8], Option<u8>)]) -> Vec<u8> {
    let mut bytes = vec![signatures.len() as u8];
    for &(params, result) in signatures {
        bytes.extend([0x60, params.len() as u8]);
        bytes.extend(params);
        bytes.push(u8::from(result.is_some()));
        bytes.extend(result);
    }
    bytes
}

fn push_mixed_args(body: &mut Vec<u8>) {
    push_i64(body, LEFT);
    body.extend(b"\x41\x0b"); // i32.const 11
    push_i64(body, RIGHT);
    body.extend(b"\x41\x09"); // i32.const 9
}

#[test]
fn mixed_calls_preserve_parameter_order_full_width_results_and_the_caller_prefix() {
    let ty = types(&[
        (&[I32], Some(I32)),
        (&MIXED, Some(I64)),
        (&MIXED, Some(I64)),
    ]);
    let result = LEFT.wrapping_sub(RIGHT).wrapping_add(2);
    for indirect in [false, true] {
        let mut entry = b"\0\x42\x13\x02\x7e".to_vec(); // i64 prefix (19); block i64
        push_mixed_args(&mut entry);
        if indirect {
            entry.extend(b"\x20\0\x11\x01\0"); // Selector from host; type 1.
        } else {
            entry.extend(b"\x10\x01"); // Direct target function 1.
        }
        entry.extend(b"\x0b\x7c"); // end block; i64.add to the prefix
        push_i64(&mut entry, result.wrapping_add(19));
        entry.extend(b"\x51\x0b"); // Compare all 64 bits, return i32.

        // One i64 local at index 4. Save RHS there before subtracting it, then
        // incorporate the position-dependent difference of the i32 arguments.
        let compute = b"\x20\x02\x21\x04\x20\0\x20\x04\x7d\x20\x01\x20\x03\x6b\xac\x7c";
        for (nested, exit) in [
            (false, &[][..]),                     // Normal fallthrough.
            (false, &b"\x41\0\x0d\0"[..]),        // br_if not taken, leaving the result.
            (true, &b"\x0f"[..]),                 // return
            (true, &b"\x0c\x01"[..]),             // br 1
            (true, &b"\x41\x01\x0d\x01\x1a"[..]), // br_if 1, taken; drop on the validated fallthrough.
            (true, &b"\x41\0\x0e\0\x01"[..]),     // br_table with just a default to the function.
        ] {
            let mut callee = vec![1, 1, I64];
            if nested {
                callee.extend(b"\x41\x63\x02\x40"); // Junk below a void block's floor.
            }
            callee.extend(compute);
            callee.extend(exit);
            if nested {
                callee.extend(b"\x0b\0"); // end block; unreachable on the skipped path.
            }
            callee.push(0x0b);
            // Even for the indirect call the target declares type 2, not 1.
            let bytes = module(&[
                (1, &ty),
                (3, &[2, 0, 2]),
                (4, &[1, 0x70, 0, 1]),
                (7, EXPORT),
                (9, &[1, 0, 0x41, 0, 0x0b, 1, 1]),
                (10, &code_bodies(&[&entry, &callee])),
            ]);
            let validated = validate_module(&bytes).unwrap();
            let instance = validated.instantiate().unwrap();
            assert_eq!(instance.invoke_i32_with_args("answer", &[0]), Ok(1));
        }
    }
}

#[test]
fn start_can_call_a_mixed_void_helper_and_an_export_can_call_a_mixed_i32_helper() {
    let ty = types(&[
        (&[], Some(I32)),
        (&[], None),
        (&[I64, I32], None),
        (&[I32, I64], Some(I32)),
    ]);
    let mut start = vec![0];
    push_i64(&mut start, LEFT);
    start.extend(b"\x41\x0b\x10\x02\x0b"); // Add 11 to LEFT and store in global 0.
    let write = b"\0\x20\0\x20\x01\xac\x7c\x24\0\x0b";
    let mut entry = b"\0\x41\x03".to_vec();
    push_i64(&mut entry, LEFT + 8);
    entry.extend(b"\x41\0\x11\x03\0\x0b"); // Ask the indirect helper to test the global.
    let compare = b"\0\x20\x01\x20\0\xac\x7c\x23\0\x51\x0b";
    let bytes = module(&[
        (1, &ty),
        (3, &[4, 0, 1, 2, 3]),
        (4, &[1, 0x70, 0, 1]),
        (6, &[1, I64, 1, 0x42, 0, 0x0b]),
        (7, EXPORT),
        (8, &[1]),
        (9, &[1, 0, 0x41, 0, 0x0b, 1, 3]),
        (10, &code_bodies(&[&entry, &start, write, compare])),
    ]);
    let validated = validate_module(&bytes).unwrap();
    assert_eq!(validated.instantiate().unwrap().invoke_i32("answer"), Ok(1));
}

#[test]
fn indirect_match_compares_each_position_and_the_result_type() {
    let mut params = [MIXED; 4];
    for (position, changed) in params.iter_mut().enumerate() {
        changed[position] = if changed[position] == I32 { I64 } else { I32 };
    }
    let mut signatures: Vec<(&[u8], Option<u8>)> = vec![(&[I32], Some(I32)), (&MIXED, Some(I64))];
    signatures.extend(params.iter().map(|p| (p.as_slice(), Some(I64))));
    let reordered = [I32, I64, I32, I64];
    signatures.push((&reordered, Some(I64))); // Same types and arity, different order.
    signatures.push((&MIXED, Some(I32)));
    let ty = types(&signatures);
    let mut entry = vec![0];
    push_mixed_args(&mut entry);
    entry.extend(b"\x20\0\x11\x01\0\x50\x0b"); // call_indirect type 1; i64.eqz.
    let bodies = [
        &entry[..],
        TRAP_BODY,
        TRAP_BODY,
        TRAP_BODY,
        TRAP_BODY,
        TRAP_BODY,
        TRAP_BODY,
    ];
    let bytes = module(&[
        (1, &ty),
        (3, &[7, 0, 2, 3, 4, 5, 6, 7]),
        (4, &[1, 0x70, 0, 6]),
        (7, EXPORT),
        (9, &[1, 0, 0x41, 0, 0x0b, 6, 1, 2, 3, 4, 5, 6]),
        (10, &code_bodies(&bodies)),
    ]);
    let validated = validate_module(&bytes).unwrap();
    let instance = validated.instantiate().unwrap();
    for selector in 0..6 {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[selector]),
            Err(InvokeError::Trap(Trap::IndirectCallTypeMismatch)),
            "selected target {selector} must fail before executing its body"
        );
    }
}

#[test]
fn call_validation_checks_positional_operands_above_the_active_floor_even_in_dead_code() {
    let ty = types(&[(&[], Some(I32)), (&MIXED, Some(I64))]);
    // A caller cannot consume the prefix through a reachable block floor.
    // In a dead block it may supply missing inputs implicitly, but an explicitly
    // stacked suffix must match the final parameters (and selector, if any).
    let block = b"\0\x41\x01\x02\x40";
    for (call, selector) in [(&b"\x10\x01"[..], &b""[..]), (b"\x11\x01\0", b"\x41\0")] {
        let check = |inputs: &[u8], after: &[u8], error| {
            let body = [block, inputs, call, after].concat();
            let code = code_bodies(&[&body, TRAP_BODY]);
            let mut bytes = module(&[(1, &ty), (3, &[2, 0, 1]), (4, &[1, 0x70, 0, 0])]);
            let first_body = push_section(&mut bytes, 10, &code) + 2;
            if let Some((offset, kind)) = error {
                expect_error(
                    &bytes,
                    first_body + block.len() + inputs.len() + offset,
                    kind,
                );
            } else {
                validate_module(&bytes).unwrap();
            }
        };
        check(
            b"\x42\0",
            b"",
            Some((0, ErrorKind::StackUnderflow { depth: 1 })),
        );
        let wrong_live_args = [b"\x42\0\x41\0\x41\0\x41\0", selector].concat();
        check(&wrong_live_args, b"", Some((0, NEED_I64)));
        let wrong_dead_suffix = [b"\0\x42\0", selector].concat();
        check(&wrong_dead_suffix, b"", Some((0, NEED_I32)));
        let good_dead_suffix = [b"\0\x41\0", selector].concat();
        check(&good_dead_suffix, b"\x50\x1a\x0b\x0b", None);
        check(&good_dead_suffix, b"\x45", Some((call.len(), NEED_I32)));
        check(
            &good_dead_suffix,
            b"\x0b",
            Some((call.len(), ErrorKind::InvalidResultDepth { depth: 1 })),
        );
        if !selector.is_empty() {
            let wrong_selector = [b"\x42\0\x41\0\x42\0\x41\0", b"\x42\0".as_slice()].concat();
            check(&wrong_selector, b"", Some((0, NEED_I32)));
        }
    }
}

#[test]
fn i64_function_results_and_parameter_writes_are_checked_in_private_bodies() {
    let ty = types(&[(&[I64], Some(I64))]);
    for (body, offset, kind) in [
        (&b"\0\x41\0\x0b"[..], 3, NEED_I64),
        (b"\0\x41\0\x0f", 3, NEED_I64),
        (b"\0\0\x41\0\x0c\0", 4, NEED_I64),
        (b"\0\0\x41\0\x21\0", 4, NEED_I64),
        (
            b"\0\x02\x7f\0\x0e\x01\0\x01",
            7,
            ErrorKind::BranchTableTypeMismatch {
                expected: ValType::I32,
                actual: ValType::I64,
            },
        ),
    ] {
        let mut bytes = module(&[(1, &ty), (3, FUNCTION)]);
        let start = push_section(&mut bytes, 10, &code_body(body)) + 2;
        expect_error(&bytes, start + offset, kind);
    }
}

#[test]
fn rejects_i64_parameters_or_results_at_the_export_index() {
    for (params, result) in [
        (&MIXED[..], None),
        (&[I64], Some(I32)),
        (&[], Some(I64)),
        (&MIXED, Some(I64)),
    ] {
        let ty = types(&[(&[], None), (params, result)]);
        let prefix = module(&[(1, &ty), (3, &[2, 0, 1])]);
        let code = code_bodies(&[b"\0\x0b", TRAP_BODY]);
        let mut private = prefix.clone();
        push_section(&mut private, 10, &code);
        validate_module(&private).unwrap();

        let mut exported = prefix;
        let at = push_section(&mut exported, 7, b"\x01\x01x\0\x81\0") + 4;
        push_section(&mut exported, 10, &code);
        expect_error(
            &exported,
            at,
            ErrorKind::UnsupportedExportSignature { function: 1 },
        );
    }
}

#[test]
fn start_remains_void_and_parameterless_and_reports_the_actual_result() {
    for (params, result, kind, detail) in [
        (
            &MIXED[..],
            None,
            ErrorKind::InvalidStartSignature {
                parameters: 4,
                result: ResultKind::Void,
            },
            "4 parameters and void result",
        ),
        (
            &[],
            Some(I64),
            ErrorKind::InvalidStartI64Result { parameters: 0 },
            "0 parameters and i64 result",
        ),
        (
            &MIXED,
            Some(I64),
            ErrorKind::InvalidStartI64Result { parameters: 4 },
            "4 parameters and i64 result",
        ),
    ] {
        let ty = types(&[(params, result)]);
        let mut bytes = module(&[(1, &ty), (3, FUNCTION)]);
        let at = push_section(&mut bytes, 8, &[0x80, 0]);
        let code_start = bytes.len();
        push_section(&mut bytes, 10, &code_body(TRAP_BODY));
        expect_error(&bytes, at, kind);
        assert!(
            validate_module(&bytes)
                .unwrap_err()
                .to_string()
                .contains(detail)
        );
        bytes.truncate(code_start);
        let at = push_section(&mut bytes, 10, &code_body(b"\0\xff\x0b")) + 3;
        expect_error(&bytes, at, ErrorKind::UnsupportedOpcode { opcode: 0xff });
    }
}

#[test]
fn export_and_start_check_index_and_section_framing_before_signature() {
    use ErrorKind::*;
    const FIELD: IntegerField = IntegerField::FunctionIndex;
    let ty = types(&[(&[], Some(I64))]);
    let prefix = module(&[(1, &ty), (3, FUNCTION)]);
    for (id, before_index) in [(7, &b"\x01\x01x\0"[..]), (8, &b""[..])] {
        for (index, offset, kind) in [
            (&b"\x80"[..], 1, TruncatedInteger { field: FIELD }),
            (b"\x80\x80\x80\x80\x10", 4, IntegerOverflow { field: FIELD }),
            (
                b"\x81\0",
                0,
                InvalidIndex {
                    field: FIELD,
                    index: 1,
                },
            ),
        ] {
            let mut bytes = prefix.clone();
            let at = push_section(&mut bytes, id, &[before_index, index].concat());
            // A continuation cannot read the next section's ID as its terminal byte.
            push_section(&mut bytes, 0, &[0]);
            push_section(&mut bytes, 10, &code_body(TRAP_BODY));
            expect_error(&bytes, at + before_index.len() + offset, kind);
        }

        // Even a complete index cannot be validated until the declared section fits.
        let available = before_index.len() + 2;
        let declared = available + 1;
        let mut bytes = prefix.clone();
        bytes.push(id);
        push_u32(&mut bytes, declared);
        bytes.extend(before_index);
        bytes.extend(b"\x80\0");
        expect_error(
            &bytes,
            bytes.len(),
            TruncatedSection {
                declared,
                available,
            },
        );
    }

    // The start target is in bounds but has an i64 result. Its trailing byte
    // is reported before the deferred signature check, after the padded index.
    let mut bytes = prefix;
    let at = push_section(&mut bytes, 8, b"\x80\0\0");
    push_section(&mut bytes, 10, &code_body(TRAP_BODY));
    expect_error(&bytes, at + 2, TrailingSectionBytes { id: 8 });
}

#[test]
fn mixed_recursive_calls_use_the_existing_fuel_and_depth_budgets() {
    let ty = types(&[(&[I32], Some(I32)), (&[I32, I64], Some(I64))]);
    let mut entry = b"\0\x20\0".to_vec();
    push_i64(&mut entry, LEFT);
    entry.extend(b"\x10\x01");
    push_i64(&mut entry, LEFT);
    entry.extend(b"\x51\x0b");
    for (recur_nops, leaf_nops, depth, expected) in [
        (64, 8, 14, Ok(1)), // 5 entry + 14 × (8 + 64) recursive + (3 + 8) leaf = 1024.
        (
            64,
            9,
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
        let mut recur = b"\0\x20\0\x04\x7e".to_vec(); // if local.get 0, result i64
        recur.extend(std::iter::repeat_n(0x01, recur_nops));
        recur.extend(b"\x20\0\x41\x01\x6b\x20\x01\x10\x01\x05"); // recurse with n - 1; else
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
        assert_eq!(instance.invoke_i32_with_args("answer", &[depth]), expected);
        assert_eq!(instance.invoke_i32_with_args("answer", &[1]), Ok(1));
    }
}
