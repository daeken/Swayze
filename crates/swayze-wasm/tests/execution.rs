use swayze_wasm::{
    Error, ErrorKind, ExecutionLimit, IntegerField, InvokeError, LengthField, Limit, Module, Trap,
    validate_module,
};

#[path = "execution/bits.rs"]
mod bits;
#[path = "execution/branch.rs"]
mod branch;
#[path = "execution/branch_table.rs"]
mod branch_table;
#[path = "execution/call_indirect.rs"]
mod call_indirect;
#[path = "execution/float_control.rs"]
mod float_control;
#[path = "execution/float_locals.rs"]
mod float_locals;
#[path = "execution/float_operands.rs"]
mod float_operands;
#[path = "execution/float_rounding.rs"]
mod float_rounding;
#[path = "execution/float_signatures.rs"]
mod float_signatures;
#[path = "execution/function_exit.rs"]
mod function_exit;
#[path = "execution/globals.rs"]
mod globals;
#[path = "execution/host_invocation.rs"]
mod host_invocation;
#[path = "execution/i64_control.rs"]
mod i64_control;
#[path = "execution/i64_locals.rs"]
mod i64_locals;
#[path = "execution/i64_operands.rs"]
mod i64_operands;
#[path = "execution/i64_signatures.rs"]
mod i64_signatures;
#[path = "execution/if_branch.rs"]
mod if_branch;
#[path = "execution/memory.rs"]
mod memory;
#[path = "execution/stack.rs"]
mod stack;
#[path = "execution/start.rs"]
mod start;
#[path = "execution/unreachable.rs"]
mod unreachable;
#[path = "execution/void_control.rs"]
mod void_control;
#[path = "execution/void_functions.rs"]
mod void_functions;

const HEADER: &[u8] = b"\0asm\x01\0\0\0";
const TYPE: &[u8] = &[1, 0x60, 0, 1, 0x7f];
const FUNCTION: &[u8] = &[1, 0];
const TWO_FUNCTIONS: &[u8] = &[2, 0, 0];
const EXPORT: &[u8] = b"\x01\x06answer\0\0";
const CODE: &[u8] = &[1, 4, 0, 0x41, 42, 0x0b];
const SECTIONS: [(u8, &[u8]); 4] = [(1, TYPE), (3, FUNCTION), (7, EXPORT), (10, CODE)];

fn push_u32(bytes: &mut Vec<u8>, n: usize) {
    let mut n = u32::try_from(n).unwrap();
    while n >= 128 {
        bytes.push((n & 0x7f) as u8 | 0x80);
        n >>= 7;
    }
    bytes.push(n as u8);
}

fn push_section(bytes: &mut Vec<u8>, id: u8, payload: &[u8]) -> usize {
    bytes.push(id);
    push_u32(bytes, payload.len());
    let payload_start = bytes.len();
    bytes.extend_from_slice(payload);
    payload_start
}

fn module(sections: &[(u8, &[u8])]) -> Vec<u8> {
    let mut bytes = HEADER.to_vec();
    for &(id, payload) in sections {
        push_section(&mut bytes, id, payload);
    }
    bytes
}

fn expect_error(bytes: &[u8], offset: usize, kind: ErrorKind) {
    assert_eq!(validate_module(bytes).unwrap_err(), Error { offset, kind });
}

fn bad_payload(id: u8, payload: &[u8], index: usize, kind: ErrorKind) {
    let mut bytes = HEADER.to_vec();
    let start = push_section(&mut bytes, id, payload);
    // A malformed section must not read through its boundary into the next ID.
    push_section(&mut bytes, 0, &[0]);
    expect_error(&bytes, start + index, kind);
}

#[test]
fn instantiates_and_invokes_only_the_exact_owned_export() {
    let validated = {
        let mut bytes = HEADER.to_vec();
        for (id, payload) in SECTIONS {
            push_section(&mut bytes, 0, b"\x04name\xff");
            push_section(&mut bytes, id, payload);
        }
        push_section(&mut bytes, 0, &[0]);
        validate_module(&bytes).unwrap()
    };
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("answer"), Ok(42));
    assert_eq!(
        validated.instantiate().unwrap().invoke_i32("answer"),
        Ok(42)
    );
    for name in ["", "Answer", "answe", "answer\0"] {
        assert_eq!(instance.invoke_i32(name), Err(InvokeError::ExportNotFound));
    }
}

#[test]
fn accepts_empty_and_custom_only_modules_and_functions_without_exports() {
    for sections in [
        &[][..],
        &[(0, &[0][..])],
        &[(1, TYPE), (3, FUNCTION), (10, CODE)],
        &[(1, TYPE), (3, FUNCTION), (7, &[0]), (10, CODE)],
    ] {
        let validated = validate_module(&module(sections)).unwrap();
        assert_eq!(
            validated.instantiate().unwrap().invoke_i32(""),
            Err(InvokeError::ExportNotFound)
        );
        assert_eq!(
            validated.instantiate().unwrap().invoke_i32("answer"),
            Err(InvokeError::ExportNotFound)
        );
    }
}

#[test]
fn validates_export_name_utf8_and_its_inclusive_limit() {
    let limit = Limit::ExportNameBytes;
    for name in [
        "".to_owned(),
        "\0é𝄞".to_owned(),
        "x".repeat(limit.maximum()),
    ] {
        let export = function_exports(&[(name.as_bytes(), 0)]);
        let bytes = module(&[(1, TYPE), (3, FUNCTION), (7, &export), (10, CODE)]);
        assert_eq!(
            validate_module(&bytes)
                .unwrap()
                .instantiate()
                .unwrap()
                .invoke_i32(&name),
            Ok(42)
        );
    }
    let mut too_long = vec![1];
    push_u32(&mut too_long, limit.maximum() + 1);
    bad_payload(7, &too_long, 1, ErrorKind::LimitExceeded { limit });
    bad_payload(7, b"\x01\x03x\xe2\x82\xac\0", 3, ErrorKind::InvalidUtf8Name);
    let mut bytes = module(&[(1, TYPE), (3, FUNCTION)]);
    let exports = function_exports(&[(b"valid", 0), (b"x\xff", 0)]);
    let start = push_section(&mut bytes, 7, &exports);
    let invalid_byte = function_exports(&[(b"valid", 0)]).len() + 2;
    expect_error(&bytes, start + invalid_byte, ErrorKind::InvalidUtf8Name);
    bad_payload(
        7,
        b"\x01\x03x",
        3,
        ErrorKind::TruncatedName {
            declared: 3,
            available: 1,
        },
    );
}

fn code_bodies(bodies: &[&[u8]]) -> Vec<u8> {
    let mut code = Vec::new();
    push_u32(&mut code, bodies.len());
    for body in bodies {
        push_u32(&mut code, body.len());
        code.extend_from_slice(body);
    }
    code
}

fn code_body(body: &[u8]) -> Vec<u8> {
    code_bodies(&[body])
}

fn function_exports(entries: &[(&[u8], usize)]) -> Vec<u8> {
    let mut exports = Vec::new();
    push_u32(&mut exports, entries.len());
    for &(name, index) in entries {
        push_u32(&mut exports, name.len());
        exports.extend_from_slice(name);
        exports.push(0);
        push_u32(&mut exports, index);
    }
    exports
}

fn i32_types(arities: &[u8]) -> Vec<u8> {
    let mut types = vec![u8::try_from(arities.len()).unwrap()];
    for &arity in arities {
        types.extend([0x60, arity]);
        types.extend(std::iter::repeat_n(0x7f, usize::from(arity)));
        types.extend([1, 0x7f]);
    }
    types
}

fn i32_type(arity: u8) -> Vec<u8> {
    i32_types(&[arity])
}

fn exported_function(ty: &[u8], code: &[u8]) -> Module {
    // Drop the original input before instantiation or invocation.
    validate_module(&module(&[(1, ty), (3, FUNCTION), (7, EXPORT), (10, code)])).unwrap()
}

fn exported_functions(arity: u8, exports: &[(&[u8], usize)], bodies: &[&[u8]]) -> Module {
    exported_typed_functions(&[arity], &vec![0; bodies.len()], exports, bodies)
}

fn exported_typed_functions(
    arities: &[u8],
    type_indices: &[u8],
    exports: &[(&[u8], usize)],
    bodies: &[&[u8]],
) -> Module {
    let mut declarations = vec![u8::try_from(type_indices.len()).unwrap()];
    declarations.extend_from_slice(type_indices);
    validate_module(&module(&[
        (1, &i32_types(arities)),
        (3, &declarations),
        (7, &function_exports(exports)),
        (10, &code_bodies(bodies)),
    ]))
    .unwrap()
}

fn assert_body_result(body: &[u8], expected: i32) {
    let validated = exported_function(TYPE, &code_body(body));
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("answer"), Ok(expected));
    assert_eq!(instance.invoke_i32("answer"), Ok(expected));
}

fn bad_typed_code(ty: &[u8], code: &[u8], offset: usize, kind: ErrorKind) {
    let mut bytes = module(&[(1, ty), (3, FUNCTION), (7, EXPORT)]);
    let start = push_section(&mut bytes, 10, code);
    push_section(&mut bytes, 0, &[0]);
    expect_error(&bytes, start + offset, kind);
}

fn bad_code(code: &[u8], offset: usize, kind: ErrorKind) {
    bad_typed_code(TYPE, code, offset, kind);
}

fn bad_typed_body(ty: &[u8], body: &[u8], offset: usize, kind: ErrorKind) {
    let code = code_body(body);
    bad_typed_code(ty, &code, code.len() - body.len() + offset, kind);
}

fn bad_body(body: &[u8], offset: usize, kind: ErrorKind) {
    bad_typed_body(TYPE, body, offset, kind);
}

#[test]
fn dispatches_multiple_bodies_by_exact_owned_names_and_aliases() {
    let validated = {
        let exports = function_exports(&[
            (b"", 1),
            ("é".as_bytes(), 0),
            ("e\u{301}".as_bytes(), 1),
            ("\0é".as_bytes(), 1),
            (b"alias", 0),
        ]);
        let code = code_bodies(&[&[0, 0x41, 19, 0x0b], &[0, 0x41, 42, 0x0b]]);
        let bytes = module(&[(1, TYPE), (3, TWO_FUNCTIONS), (7, &exports), (10, &code)]);
        validate_module(&bytes).unwrap()
    };
    let instance = validated.instantiate().unwrap();
    for (name, expected) in [
        ("", 42),
        ("é", 19),
        ("e\u{301}", 42),
        ("\0é", 42),
        ("alias", 19),
    ] {
        assert_eq!(instance.invoke_i32(name), Ok(expected));
    }
    for name in ["É", "e", "é\0", "alia", "Alias"] {
        assert_eq!(instance.invoke_i32(name), Err(InvokeError::ExportNotFound));
    }
}

#[test]
fn shares_arity_and_keeps_traps_and_locals_call_local_across_exports() {
    let exports = function_exports(&[(b"accumulate", 0), (b"divide", 1)]);
    let code = code_bodies(&[
        // Add the parameter to a zero-initialized local and write back the result.
        &[1, 1, 0x7f, 0x20, 1, 0x20, 0, 0x6a, 0x22, 1, 0x0b],
        // Store 12 in the same local index, then divide by the parameter.
        &[1, 1, 0x7f, 0x41, 12, 0x22, 1, 0x20, 0, 0x6d, 0x0b],
    ]);
    let bytes = module(&[
        (1, &i32_type(1)),
        (3, TWO_FUNCTIONS),
        (7, &exports),
        (10, &code),
    ]);
    let validated = validate_module(&bytes).unwrap();
    let instance = validated.instantiate().unwrap();
    for name in ["accumulate", "divide"] {
        assert_eq!(
            instance.invoke_i32(name),
            Err(InvokeError::ArityMismatch {
                expected: 1,
                actual: 0
            })
        );
    }
    assert_eq!(instance.invoke_i32_with_args("accumulate", &[3]), Ok(3));
    assert_eq!(
        instance.invoke_i32_with_args("divide", &[0]),
        Err(InvokeError::Trap(Trap::IntegerDivideByZero))
    );
    assert_eq!(instance.invoke_i32_with_args("accumulate", &[5]), Ok(5));
    assert_eq!(instance.invoke_i32_with_args("divide", &[2]), Ok(6));
    assert_eq!(instance.invoke_i32_with_args("accumulate", &[1]), Ok(1));
}

#[test]
fn admits_eight_types_functions_and_exports_with_independent_body_budgets() {
    // Identical signatures are valid as separate type table entries.
    let types = i32_types(&[0; 8]);
    let functions = [8, 7, 6, 5, 4, 3, 2, 1, 0];
    let names = [b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"h"];
    let entries: Vec<_> = names
        .iter()
        .enumerate()
        .rev()
        .map(|(i, name)| (name.as_slice(), i))
        .collect();
    let exports = function_exports(&entries);
    // 127 decoded instructions per body, not per module.
    assert_eq!(Limit::BodyInstructions.maximum(), 127);
    let bodies: Vec<_> = (0..8)
        .map(|i| {
            let mut body = vec![0, 0x41, i];
            for _ in 0..63 {
                body.extend([0x41, 0, 0x6a]);
            }
            body.push(0x0b);
            body
        })
        .collect();
    let code = code_bodies(&bodies.iter().map(Vec::as_slice).collect::<Vec<_>>());
    let validated = validate_module(&module(&[
        (1, &types),
        (3, &functions),
        (7, &exports),
        (10, &code),
    ]))
    .unwrap();
    let instance = validated.instantiate().unwrap();
    for (i, name) in ["a", "b", "c", "d", "e", "f", "g", "h"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(instance.invoke_i32(name), Ok(i as i32));
    }
}

#[test]
fn admits_eight_types_with_one_function_referencing_the_last_type() {
    let types = i32_types(&[0, 1, 2, 3, 4, 0, 1, 3]);
    // One function declares type index 7 (with unsigned LEB32 padding).
    let declarations = [1, 0x87, 0x80, 0x80, 0x80, 0];
    let code = code_body(b"\0\x20\x02\x0b");
    let validated = validate_module(&module(&[
        (1, &types),
        (3, &declarations),
        (7, EXPORT),
        (10, &code),
    ]))
    .unwrap();
    assert_eq!(
        validated
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[11, 22, 33]),
        Ok(33)
    );
}

#[test]
fn rejects_malformed_type_entries_even_if_no_function_references_them() {
    use ErrorKind::*;
    use IntegerField::*;
    for (second, offset, kind) in [
        (
            &[0x61][..],
            0,
            UnexpectedByte {
                expected: 0x60,
                found: 0x61,
            },
        ),
        (
            &[0x60, 5],
            1,
            UnsupportedCount {
                field: ParameterCount,
                count: 5,
            },
        ),
        (
            &[0x60, 1, 0x70],
            2,
            UnexpectedByte {
                expected: 0x7f,
                found: 0x70,
            },
        ),
        (
            &[0x60, 0, 2],
            2,
            UnsupportedCount {
                field: ResultCount,
                count: 2,
            },
        ),
        (
            &[0x60, 0, 1, 0x7b],
            3,
            UnexpectedByte {
                expected: 0x7f,
                found: 0x7b,
            },
        ),
        (&[0x60, 0, 0x80], 3, TruncatedInteger { field: ResultCount }),
        (&[0x60, 0, 1], 3, TruncatedByte { expected: 0x7f }),
    ] {
        let types = [&[2][..], &TYPE[1..], second].concat();
        let mut bytes = HEADER.to_vec();
        let start = push_section(&mut bytes, 1, &types);
        for (id, payload) in SECTIONS.into_iter().skip(1) {
            push_section(&mut bytes, id, payload);
        }
        expect_error(&bytes, start + TYPE.len() + offset, kind);
    }
}

#[test]
fn rejects_duplicate_names_regardless_of_target_or_empty_name() {
    for (name, second_index) in [("é".as_bytes(), 1), (b"".as_slice(), 0)] {
        let export = function_exports(&[(name, 0), (b"other", 0), (name, second_index)]);
        let mut bytes = module(&[(1, TYPE), (3, TWO_FUNCTIONS)]);
        let start = push_section(&mut bytes, 7, &export);
        push_section(&mut bytes, 10, &code_bodies(&[&CODE[2..]; 2]));
        let duplicate_offset = function_exports(&[(name, 0), (b"other", 0)]).len();
        expect_error(
            &bytes,
            start + duplicate_offset,
            ErrorKind::DuplicateExportName,
        );
    }
}

#[test]
fn checks_every_declaration_and_export_index_against_its_actual_space() {
    let types = i32_types(&[1, 0]);
    for (id, prefix, payload, index, field) in [
        (
            3,
            &[(1, types.as_slice())][..],
            &[3, 1, 0, 2][..],
            2,
            IntegerField::TypeIndex,
        ),
        (
            7,
            &[(1, TYPE), (3, TWO_FUNCTIONS)],
            b"\x02\x01a\0\x01\x01b\0\x02",
            2,
            IntegerField::FunctionIndex,
        ),
    ] {
        let mut bytes = module(prefix);
        let start = push_section(&mut bytes, id, payload);
        expect_error(
            &bytes,
            start + payload.len() - 1,
            ErrorKind::InvalidIndex { field, index },
        );
    }
}

#[test]
fn composes_nested_and_repeated_zero_arg_calls_to_forward_and_earlier_targets() {
    let validated = exported_functions(
        0,
        &[(b"answer", 2)],
        &[
            b"\0\x41\x05\x0b",                           // 0: 5
            b"\0\x41\x09\x10\x87\x80\x80\x80\0\x6b\x0b", // 1: 9 - f7() (padded index)
            b"\0\x10\x01\x10\0\x6c\x10\x01\x6a\x0b",     // 2: f1() * f0() + f1()
            &CODE[2..],
            &CODE[2..],
            &CODE[2..],
            &CODE[2..],
            b"\0\x10\0\x0b", // 7: f0()
        ],
    );
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("answer"), Ok(24));
}

#[test]
fn uses_each_export_arity_and_passes_four_arguments_above_the_caller_arm_floor() {
    let validated = exported_typed_functions(
        &[4, 2],
        &[1, 0],
        &[(b"answer", 0), (b"pack", 1)],
        &[
            // Keep 30 below the arm floor and call pack(a, b, 3, 4).
            b"\0\x41\x1e\x41\x01\x04\x7f\x20\0\x20\x01\x41\x03\x41\x04\x10\x01\
              \x05\x41\0\x0b\x6a\x0b",
            // (((a * 10) + b) * 10 + c) * 10 + d
            b"\0\x20\0\x41\x0a\x6c\x20\x01\x6a\x41\x0a\x6c\x20\x02\x6a\
              \x41\x0a\x6c\x20\x03\x6a\x0b",
        ],
    );
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32_with_args("answer", &[1, 2]), Ok(1264));
    assert_eq!(instance.invoke_i32_with_args("answer", &[4, 3]), Ok(4364));
    assert_eq!(
        instance.invoke_i32_with_args("pack", &[4, 3, 2, 1]),
        Ok(4321)
    );
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[4, 3, 2, 1]),
        Err(InvokeError::ArityMismatch {
            expected: 2,
            actual: 4
        })
    );
    assert_eq!(
        instance.invoke_i32_with_args("pack", &[1, 2]),
        Err(InvokeError::ArityMismatch {
            expected: 4,
            actual: 2
        })
    );
}

#[test]
fn keeps_caller_locals_and_operands_and_resets_reused_callee_storage() {
    let validated = exported_typed_functions(
        &[1, 0],
        &[0, 0, 1],
        &[(b"answer", 0), (b"zero", 2)],
        &[
            // local1 = 9; return 50 + (f1(arg) + f2()) + f1(arg) + arg + local1
            b"\x01\x01\x7f\x41\x09\x21\x01\x41\x32\x20\0\x10\x01\x10\x02\x6a\
              \x20\0\x10\x01\x6a\x6a\x20\0\x6a\x20\x01\x6a\x0b",
            // Return the initial local plus the arg, but mutate both slots.
            b"\x01\x01\x7f\x20\x01\x20\0\x22\x01\x6a\x41\x03\x21\0\x0b",
            // Both previous slots (including the parameter) are now fresh locals.
            b"\x01\x02\x7f\x20\0\x20\x01\x6a\x0b",
        ],
    );
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32_with_args("answer", &[7]), Ok(80));
    assert_eq!(instance.invoke_i32_with_args("answer", &[-2]), Ok(53));
    assert_eq!(instance.invoke_i32("zero"), Ok(0));
    assert_eq!(
        instance.invoke_i32_with_args("zero", &[7]),
        Err(InvokeError::ArityMismatch {
            expected: 0,
            actual: 1
        })
    );
}

#[test]
fn validates_call_indices_as_unsigned_leb32_against_the_declaration_count() {
    let field = IntegerField::FunctionIndex;
    for (immediate, index) in [
        (&[1][..], 1),
        (&[0x7f], 127),
        (&[0xff, 0xff, 0xff, 0xff, 0x0f], u32::MAX),
    ] {
        let mut body = vec![0, 0x10];
        body.extend_from_slice(immediate);
        body.push(0x0b);
        bad_body(&body, 2, ErrorKind::InvalidIndex { field, index });
    }
    for continuation_bytes in 0..=4 {
        let mut body = vec![0, 0x10];
        body.extend(std::iter::repeat_n(0x80, continuation_bytes));
        bad_body(&body, body.len(), ErrorKind::TruncatedInteger { field });
        if continuation_bytes == 4 {
            for fifth in [0x10, 0x7f, 0x80] {
                body.push(fifth);
                bad_body(&body, 6, ErrorKind::IntegerOverflow { field });
                body.pop();
            }
        }
    }
    // The else arm is untaken, but index 1 is still invalid.
    bad_body(
        b"\0\x41\x01\x04\x7f\x41\x02\x05\x10\x01\x0b\x0b",
        9,
        ErrorKind::InvalidIndex { field, index: 1 },
    );
}

#[test]
fn validates_call_stack_effect_at_the_active_floor_including_untaken_arms() {
    let ty = i32_type(4);
    bad_typed_body(
        &ty,
        b"\0\x10\0\x0b",
        1,
        ErrorKind::StackUnderflow { depth: 0 },
    );
    // In the untaken outer else arm, the nested call has three operands above
    // its floor. It cannot borrow the fourth from below that floor.
    let body = b"\0\x41\x01\x04\x7f\x41\x2a\x05\x41\x09\x41\x01\x04\x7f\
                 \x41\x01\x41\x02\x41\x03\x10\0\x05\x41\0\x0b\x6a\x0b\x0b";
    bad_typed_body(&ty, body, 20, ErrorKind::StackUnderflow { depth: 3 });
    bad_body(
        b"\0\x10\0\x10\0\x0b",
        5,
        ErrorKind::InvalidResultDepth { depth: 2 },
    );
}

#[test]
fn mixed_arity_call_cannot_pop_past_the_function_or_either_arm_floor() {
    let types = i32_types(&[1, 2]);
    let declarations = [2, 0, 1];
    let invalid_call = [0x41, 1, 0x10, 1]; // Supply one, but f1 requires two.
    let check_caller = |caller: &[u8], call_offset| {
        let mut bytes = module(&[(1, &types), (3, &declarations)]);
        let start = push_section(&mut bytes, 10, &code_bodies(&[caller, &CODE[2..]]));
        let caller_start = code_body(caller).len() - caller.len();
        expect_error(
            &bytes,
            start + caller_start + call_offset,
            ErrorKind::StackUnderflow { depth: 1 },
        );
    };
    check_caller(b"\0\x41\x01\x10\x01\x0b", 3);
    for invalid_then in [true, false] {
        // Keep an operand below the floor. The invalid arm is always untaken.
        let mut caller = vec![0, 0x41, 9, 0x41, u8::from(!invalid_then), 0x04, 0x7f];
        let mut call_offset = 0;
        for then_arm in [true, false] {
            if !then_arm {
                caller.push(0x05);
            }
            if then_arm == invalid_then {
                call_offset = caller.len() + 2;
                caller.extend(invalid_call);
            } else {
                caller.extend([0x41, 42]);
            }
        }
        caller.extend([0x0b, 0x6a, 0x0b]);
        check_caller(&caller, call_offset);
    }
}

#[test]
fn zero_arg_result_obeys_the_operand_cap_without_sharing_caller_stack_space() {
    let limit = Limit::OperandStackDepth;
    let peak = limit.maximum();
    let caller = |prefix| {
        let mut body = vec![0];
        for _ in 0..prefix {
            body.extend([0x41, 1]);
        }
        body.extend([0x10, 1]);
        body.extend(std::iter::repeat_n(0x6a, prefix));
        body.push(0x0b);
        body
    };
    let mut callee = vec![0];
    for _ in 0..peak {
        callee.extend([0x41, 1]);
    }
    callee.extend(std::iter::repeat_n(0x6a, peak - 1));
    callee.push(0x0b);
    let validated = exported_typed_functions(
        &[1, 0],
        &[0, 1],
        &[(b"answer", 0)],
        &[&caller(peak - 1), &callee],
    );
    assert_eq!(
        validated
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[7]),
        Ok(63)
    );

    let too_deep = caller(peak);
    let code = code_bodies(&[&too_deep, &callee]);
    let mut bytes = module(&[(1, &i32_types(&[1, 0])), (3, &[2, 0, 1])]);
    let start = push_section(&mut bytes, 10, &code);
    let caller_start = code_body(&too_deep).len() - too_deep.len();
    expect_error(
        &bytes,
        start + caller_start + 1 + 2 * peak,
        ErrorKind::LimitExceeded { limit },
    );
}

#[test]
fn propagates_the_first_nested_trap_and_reuses_the_instance() {
    let validated = exported_typed_functions(
        &[2, 1, 3],
        &[1, 0, 2],
        &[(b"answer", 0)],
        &[
            // f1(arg, MIN) / (arg + 1) -- would divide by zero for -1 if reached.
            b"\0\x20\0\x41\x80\x80\x80\x80\x78\x10\x01\x20\0\x41\x01\x6a\x6d\x0b",
            b"\0\x20\x01\x20\0\x41\x01\x10\x02\x0b", // f2(b, a, 1)
            b"\0\x20\0\x20\x01\x20\x02\x6c\x6d\x0b", // a / (b * c)
        ],
    );
    let instance = validated.instantiate().unwrap();
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[-1]),
        Err(InvokeError::Trap(Trap::IntegerOverflow))
    );
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[0]),
        Err(InvokeError::Trap(Trap::IntegerDivideByZero))
    );
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[1]),
        Ok(i32::MIN / 2)
    );
}

#[test]
fn bounds_live_frames_including_the_export_and_resumes_after_finite_self_calls() {
    let validated = exported_functions(
        1,
        &[(b"answer", 0), (b"recur", 1)],
        &[
            // recur(arg) / (arg - 15) -- a depth failure must skip the divide.
            b"\0\x20\0\x10\x01\x20\0\x41\x0f\x6b\x6d\x0b",
            // arg != 0 ? 1 + recur(arg - 1) : 0
            b"\0\x20\0\x04\x7f\x41\x01\x20\0\x41\x01\x6b\x10\x01\x6a\x05\x41\0\x0b\x0b",
        ],
    );
    let instance = validated.instantiate().unwrap();
    let limit = ExecutionLimit::CallDepth;
    assert_eq!(limit.maximum(), 16);
    let failure = Err(InvokeError::LimitExceeded { limit });
    assert_eq!(instance.invoke_i32_with_args("recur", &[15]), Ok(15));
    assert_eq!(instance.invoke_i32_with_args("recur", &[16]), failure);
    assert_eq!(instance.invoke_i32_with_args("answer", &[15]), failure);
    assert_eq!(instance.invoke_i32_with_args("answer", &[14]), Ok(-14));
}

#[test]
fn spends_one_dispatch_budget_across_sequential_calls_and_final_returns_are_free() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    // Leaf: 126 dispatches (condition, if, const, 122 eqz, else jump).
    // The untaken else arm's const does not count.
    let mut leaf = vec![0, 0x41, 1, 0x04, 0x7f, 0x41, 1];
    leaf.extend(std::iter::repeat_n(0x45, 122));
    leaf.extend([0x05, 0x41, 0, 0x0b, 0x0b]);
    let entry = |tail| {
        let mut body = vec![1, 1, 0x7f];
        for _ in 0..7 {
            body.extend([0x10, 4, 0x21, 0]); // call leaf, local.set: 128 dispatches
        }
        body.extend([0x10, tail, 0x0b]);
        body
    };
    let validated = exported_functions(
        0,
        &[(b"exact", 0), (b"over", 1), (b"leaf", 4)],
        &[
            &entry(2),
            &entry(3),
            b"\0\x10\x04\x0b",     // 7 * 128 + 2 calls + 126 = 1024, then unwind
            b"\0\x10\x04\x45\x0b", // one further dispatch in the suspended caller
            &leaf,
        ],
    );
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("exact"), Ok(1));
    assert_eq!(
        instance.invoke_i32("over"),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(instance.invoke_i32("leaf"), Ok(1));
    assert_eq!(instance.invoke_i32("exact"), Ok(1));
}

#[test]
fn dispatch_limit_is_checked_before_attempting_a_call_at_maximum_depth() {
    let recur = |zero_tests| {
        let mut body = vec![1, 1, 0x7f, 0x41, 0]; // One local; i32.const 0.
        body.extend(std::iter::repeat_n(0x45, zero_tests));
        body.extend([0x21, 0, 0x10, 1, 0x0b]); // local.set 0; call 1; end.
        body
    };
    let module = exported_functions(0, &[(b"both", 0), (b"depth", 1)], &[&recur(62), &recur(61)]);
    let instance = module.instantiate().unwrap();
    // Each frame spends 64 dispatches through its call. At depth 16 the
    // attempted push on dispatch 1024 is within budget but exceeds depth.
    assert_eq!(
        instance.invoke_i32("depth"),
        Err(InvokeError::LimitExceeded {
            limit: ExecutionLimit::CallDepth,
        })
    );
    // The extra instruction in frame 0 uses up the budget just before that
    // same attempted push, so the call is never dispatched.
    assert_eq!(
        instance.invoke_i32("both"),
        Err(InvokeError::LimitExceeded {
            limit: ExecutionLimit::InstructionDispatches,
        })
    );
}

#[test]
fn requires_exact_function_and_code_count_agreement() {
    let types = i32_types(&[2, 0, 1]);
    for bodies in [1, 3] {
        let mut bytes = module(&[(1, &types), (3, &[2, 1, 0])]);
        let start = push_section(&mut bytes, 10, &code_bodies(&vec![&CODE[2..]; bodies]));
        expect_error(
            &bytes,
            start,
            ErrorKind::UnsupportedCount {
                field: IntegerField::CodeCount,
                count: bodies as u32,
            },
        );
    }
}

#[test]
fn validates_later_unexported_bodies_with_their_own_local_space_and_frames() {
    let first = &[1, 1, 0x7f, 0x20, 4, 0x0b];
    let limit = Limit::CodeBodyBytes;
    let oversized = vec![0; limit.maximum() + 1];
    for (second, offset, kind) in [
        (
            &[0, 0x20, 0, 0x0b][..],
            3,
            ErrorKind::InvalidIndex {
                field: IntegerField::LocalIndex,
                index: 0,
            },
        ),
        (
            &[0, 0x12, 0, 0x0b],
            2,
            ErrorKind::UnsupportedOpcode { opcode: 0x12 },
        ),
        (
            &[0, 0x10, 0x80],
            4,
            ErrorKind::TruncatedInteger {
                field: IntegerField::FunctionIndex,
            },
        ),
        (
            &[0, 0x41, 0x80],
            4,
            ErrorKind::TruncatedInteger {
                field: IntegerField::I32Const,
            },
        ),
        (&[0, 0x41, 42, 0x0b, 0x0b], 5, ErrorKind::TrailingBodyBytes),
        (&oversized, 0, ErrorKind::LimitExceeded { limit }),
    ] {
        let mut bytes = module(&[(1, &i32_types(&[4, 0])), (3, &[3, 0, 1, 1]), (7, EXPORT)]);
        let start = push_section(&mut bytes, 10, &code_bodies(&[first, second, &CODE[2..]]));
        // The next valid body's frame cannot supply the missing immediate byte.
        let second_size = code_body(first).len();
        expect_error(&bytes, start + second_size + offset, kind);
    }
}

#[test]
fn reads_four_parameters_in_declared_order_on_each_invocation() {
    // (p2 - p0) * p3 + p1
    let code = code_body(&[
        0, 0x20, 2, 0x20, 0, 0x6b, 0x20, 3, 0x6c, 0x20, 1, 0x6a, 0x0b,
    ]);
    let validated = exported_function(&i32_type(4), &code);
    let instance = validated.instantiate().unwrap();
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[5, 7, 9, 11]),
        Ok(51)
    );
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[-5, 7, 9, -2]),
        Ok(-21)
    );
}

#[test]
fn zeroes_declared_locals_on_every_invocation_of_the_same_instance() {
    // Increment the local before returning it. A retained local would return 2
    // on the second call.
    assert_body_result(&[1, 1, 0x7f, 0x20, 0, 0x41, 1, 0x6a, 0x22, 0, 0x0b], 1);
}

#[test]
fn reads_and_assigns_parameters_before_grouped_declared_locals() {
    let code = code_body(&[
        3, 2, 0x7f, 0, 0x7f, 1, 0x7f, // Local indices 2, 3, 4.
        0x20, 0, 0x21, 2, // Save the first argument (a) in local 2.
        0x20, 1, 0x22, 0, // Store the second argument (b) in parameter 0.
        0x41, 3, 0x6c, 0x21, 1, // Multiply the tee result; parameter 1 = 3*b.
        0x20, 0, 0x20, 2, 0x6b, 0x21, 4, // Local 4 = b - a.
        0x20, 1, 0x20, 4, 0x6b, 0x20, 3, 0x6a, // 3*b - (b - a) + 0.
        0x0b,
    ]);
    let validated = exported_function(&i32_type(2), &code);
    let instance = validated.instantiate().unwrap();
    let args = [9, 4];
    assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(17));
    assert_eq!(args, [9, 4]);
    assert_eq!(instance.invoke_i32_with_args("answer", &[-1, 7]), Ok(13));
}

#[test]
fn checks_export_name_then_exact_declared_arity_even_for_unused_parameters() {
    let validated = exported_function(&i32_type(2), CODE);
    let instance = validated.instantiate().unwrap();
    assert_eq!(
        instance.invoke_i32_with_args("Answer", &[]),
        Err(InvokeError::ExportNotFound)
    );
    assert_eq!(
        instance.invoke_i32("answer"),
        Err(InvokeError::ArityMismatch {
            expected: 2,
            actual: 0
        })
    );
    for args in [&[][..], &[1], &[1, 2, 3], &[1, 2, 3, 4, 5]] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", args),
            Err(InvokeError::ArityMismatch {
                expected: 2,
                actual: args.len()
            })
        );
    }
    assert_eq!(instance.invoke_i32_with_args("answer", &[1, 2]), Ok(42));

    let zero = validate_module(&module(&SECTIONS)).unwrap();
    assert_eq!(
        zero.instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[]),
        Ok(42)
    );
    assert_eq!(
        zero.instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[1]),
        Err(InvokeError::ArityMismatch {
            expected: 0,
            actual: 1
        })
    );
}

#[test]
fn parameter_arithmetic_wraps() {
    for (lhs, rhs, opcode, expected) in [
        (i32::MAX, 1, 0x6a, i32::MIN),
        (i32::MIN, 1, 0x6b, i32::MAX),
        (i32::MIN, -1, 0x6c, i32::MIN),
    ] {
        let code = code_body(&[0, 0x20, 0, 0x20, 1, opcode, 0x0b]);
        let validated = exported_function(&i32_type(2), &code);
        assert_eq!(
            validated
                .instantiate()
                .unwrap()
                .invoke_i32_with_args("answer", &[lhs, rhs]),
            Ok(expected)
        );
    }
}

#[test]
fn accepts_maximally_padded_local_counts_and_indices() {
    assert_body_result(
        &[
            0x81, 0x80, 0x80, 0x80, 0, // One group.
            0x82, 0x80, 0x80, 0x80, 0, 0x7f, // Two i32 locals.
            0x41, 9, // Push 9.
            0x21, 0x81, 0x80, 0x80, 0x80, 0, // Set local 1.
            0x20, 0x81, 0x80, 0x80, 0x80, 0, // Get local 1.
            0x22, 0x80, 0x80, 0x80, 0x80, 0, // Tee into local 0.
            0x20, 0, 0x6a, 0x0b, // 9 + local 0.
        ],
        18,
    );
}

#[test]
fn bounds_total_declared_locals_independently_of_parameters() {
    let ty = i32_type(4);
    let body = [2, 7, 0x7f, 9, 0x7f, 0x20, 3, 0x21, 19, 0x20, 19, 0x0b];
    let validated = exported_function(&ty, &code_body(&body));
    assert_eq!(
        validated
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[1, 2, 3, 4]),
        Ok(4)
    );
    let mut too_many = body;
    too_many[3] = 10;
    bad_typed_body(
        &ty,
        &too_many,
        3,
        ErrorKind::UnsupportedCount {
            field: IntegerField::LocalCount,
            count: 10,
        },
    );
    // A full u32 is decodable but far above the budget, even after just one local.
    // A naive u32 sum of these counts would wrap to zero.
    bad_body(
        &[
            2, 1, 0x7f, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x7f, 0x41, 1, 0x0b,
        ],
        3,
        ErrorKind::UnsupportedCount {
            field: IntegerField::LocalCount,
            count: u32::MAX,
        },
    );
}

#[test]
fn zero_count_groups_use_body_bytes_but_not_local_slots() {
    let mut body = vec![126];
    for ty in [0x7f, 0x7e, 0x7d, 0x7c].into_iter().cycle().take(126) {
        body.extend([0, ty]);
    }
    body.extend([0x41, 9, 0x0b]);
    assert_eq!(body.len(), Limit::CodeBodyBytes.maximum());
    assert_body_result(&body, 9);
}

#[test]
fn validates_all_local_group_types_even_for_zero_counts() {
    for count in [0, 1] {
        bad_body(
            &[2, 1, 0x7f, count, 0x7b, 0x41, 1, 0x0b],
            4,
            ErrorKind::UnexpectedByte {
                expected: 0x7f,
                found: 0x7b,
            },
        );
    }
}

#[test]
fn validates_local_count_leb_values_within_the_body_frame() {
    use IntegerField::{LocalCount, LocalGroupCount};
    for (prefix, field) in [(&[][..], LocalGroupCount), (&[2, 1, 0x7f], LocalCount)] {
        for count in 0..=4 {
            let body = [prefix, &[0x80; 4][..count]].concat();
            let mut code = code_body(&body);
            let boundary = code.len();
            code.extend([0, 0x7f, 0x41, 1, 0x0b]);
            bad_code(&code, boundary, ErrorKind::TruncatedInteger { field });
        }
        for fifth in [0x10, 0x7f, 0x80] {
            let body = [prefix, &[0x80, 0x80, 0x80, 0x80, fifth, 0]].concat();
            bad_body(
                &body,
                prefix.len() + 4,
                ErrorKind::IntegerOverflow { field },
            );
        }
    }
    // A huge but decodable group vector cannot read beyond its first missing entry.
    bad_body(
        &[0xff, 0xff, 0xff, 0xff, 0x0f, 0, 0x7f],
        7,
        ErrorKind::TruncatedInteger { field: LocalCount },
    );
    let mut code = code_body(&[1, 1]);
    let boundary = code.len();
    code.extend([0x7f, 0x41, 1, 0x0b]);
    bad_code(&code, boundary, ErrorKind::TruncatedByte { expected: 0x7f });
}

#[test]
fn rejects_local_indices_outside_the_combined_index_space() {
    for (parameter_count, locals, leb, index) in [
        (0, &[0][..], &[0][..], 0),
        (1, &[1, 0, 0x7f], &[1], 1),
        (4, &[2, 8, 0x7f, 8, 0x7f], &[20], 20),
        (4, &[1, 2, 0x7f], &[0x7f], 127),
        (4, &[1, 2, 0x7f], &[0xff, 0xff, 0xff, 0xff, 0x0f], u32::MAX),
    ] {
        for opcode in [0x20, 0x21, 0x22] {
            let body = [locals, &[0x41, 1, opcode], leb, &[0x0b]].concat();
            bad_typed_body(
                &i32_type(parameter_count),
                &body,
                locals.len() + 3,
                ErrorKind::InvalidIndex {
                    field: IntegerField::LocalIndex,
                    index,
                },
            );
        }
    }
}

#[test]
fn rejects_local_index_leb_overflow() {
    for opcode in [0x20, 0x21, 0x22] {
        for fifth in [0x10, 0x7f, 0x80] {
            let body = [0, 0x41, 1, opcode, 0x80, 0x80, 0x80, 0x80, fifth, 0, 0x0b];
            bad_typed_body(
                &i32_type(1),
                &body,
                8,
                ErrorKind::IntegerOverflow {
                    field: IntegerField::LocalIndex,
                },
            );
        }
    }
}

#[test]
fn parameters_reach_the_operand_stack_only_through_local_get() {
    let ty = i32_type(2);
    for (body, offset, kind) in [
        (
            &[0, 0x6a, 0x0b][..],
            1,
            ErrorKind::StackUnderflow { depth: 0 },
        ),
        (
            &[0, 0x20, 0, 0x6a, 0x0b],
            3,
            ErrorKind::StackUnderflow { depth: 1 },
        ),
        (
            &[0, 0x20, 0, 0x20, 1, 0x0b],
            5,
            ErrorKind::InvalidResultDepth { depth: 2 },
        ),
    ] {
        bad_typed_body(&ty, body, offset, kind);
    }
}

#[test]
fn executes_mixed_arithmetic_in_stack_order() {
    // 7 * (3 - 9) + 11. The immediate 11 is itself spelled 0x0b, like `end`.
    assert_body_result(
        &[
            0, 0x41, 7, 0x41, 3, 0x41, 9, 0x6b, 0x6c, 0x41, 0x0b, 0x6a, 0x0b,
        ],
        -31,
    );
}

#[test]
fn wraps_all_three_arithmetic_operations() {
    let min = [0x41, 0x80, 0x80, 0x80, 0x80, 0x78];
    let max = [0x41, 0xff, 0xff, 0xff, 0xff, 0x07];
    for (lhs, rhs, opcode, expected) in [
        (max, 1, 0x6a, i32::MIN),    // MAX + 1
        (min, 1, 0x6b, i32::MAX),    // MIN - 1
        (min, 0x7f, 0x6c, i32::MIN), // MIN * -1
    ] {
        assert_body_result(
            &[&[0][..], &lhs, &[0x41, rhs, opcode, 0x0b]].concat(),
            expected,
        );
    }
}

#[test]
fn executes_signed_and_unsigned_division_and_remainder() {
    for (opcode, cases) in [
        (
            0x6d,
            &[(-7, 3, -2), (7, -3, -2), (i32::MIN, 1, i32::MIN)][..],
        ), // div_s
        (
            0x6e,
            &[
                (-1, 2, i32::MAX),
                (-1, 1, -1),
                (-1, i32::MIN, 1),
                (1, -1, 0),
            ],
        ), // div_u
        (0x6f, &[(-7, 3, -1), (7, -3, 1), (i32::MIN, -1, 0)]), // rem_s
        (
            0x70,
            &[
                (-1, 2, 1),
                (-1, i32::MIN, i32::MAX),
                (i32::MIN, -1, i32::MIN),
            ],
        ), // rem_u
    ] {
        let code = code_body(&[0, 0x20, 0, 0x20, 1, opcode, 0x0b]);
        let validated = exported_function(&i32_type(2), &code);
        let instance = validated.instantiate().unwrap();
        for &(lhs, rhs, expected) in cases {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &[lhs, rhs]),
                Ok(expected),
                "opcode {opcode:#x} with ({lhs}, {rhs})",
            );
        }
    }
}

#[test]
fn eqz_returns_exactly_one_for_zero_and_zero_otherwise() {
    let code = code_body(&[0, 0x20, 0, 0x45, 0x0b]);
    let module = exported_function(&i32_type(1), &code);
    let instance = module.instantiate().unwrap();
    for (arg, expected) in [(0, 1), (1, 0), (-1, 0), (i32::MIN, 0), (i32::MAX, 0)] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[arg]),
            Ok(expected)
        );
    }
}

#[test]
fn compares_signed_and_unsigned_operands_in_stack_order() {
    // Expected results in opcode order: eq, ne, lt_s/u, gt_s/u, le_s/u, ge_s/u.
    let cases = [
        ([-1, 0], [0, 1, 1, 0, 0, 1, 1, 0, 0, 1]),
        ([0, -1], [0, 1, 0, 1, 1, 0, 0, 1, 1, 0]),
        ([i32::MIN, i32::MAX], [0, 1, 1, 0, 0, 1, 1, 0, 0, 1]),
        ([i32::MAX, i32::MIN], [0, 1, 0, 1, 1, 0, 0, 1, 1, 0]),
        ([i32::MIN, -1], [0, 1, 1, 1, 0, 0, 1, 1, 0, 0]),
        ([i32::MIN, i32::MIN], [1, 0, 0, 0, 0, 0, 1, 1, 1, 1]),
    ];
    for (index, opcode) in (0x46..=0x4f).enumerate() {
        let code = code_body(&[0, 0x20, 0, 0x20, 1, opcode, 0x0b]);
        let module = exported_function(&i32_type(2), &code);
        let instance = module.instantiate().unwrap();
        for (args, expected) in cases {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &args),
                Ok(expected[index]),
                "opcode {opcode:#x} with {args:?}",
            );
        }
    }
}

#[test]
fn division_and_remainder_by_zero_validate_but_trap_on_invocation() {
    for opcode in 0x6d..=0x70 {
        let code = code_body(&[0, 0x41, 0, 0x41, 0, opcode, 0x0b]);
        let validated = exported_function(TYPE, &code);
        let instance = validated.instantiate().unwrap();
        assert_eq!(
            instance.invoke_i32("answer"),
            Err(InvokeError::Trap(Trap::IntegerDivideByZero)),
            "opcode {opcode:#x}",
        );
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[0]),
            Err(InvokeError::ArityMismatch {
                expected: 0,
                actual: 1
            }),
        );
        assert_eq!(
            instance.invoke_i32_with_args("Answer", &[0]),
            Err(InvokeError::ExportNotFound),
        );
    }
}

#[test]
fn stops_at_first_guest_trap_and_can_reuse_the_instance() {
    let code = code_body(&[
        1, 1, 0x7f, // Declare local 3.
        0x20, 3, 0x41, 1, 0x6a, 0x21, 3, // Increment local 3 before a possible trap.
        0x20, 0, 0x20, 1, 0x6d, // First possible trap: div_s(p0, p1).
        0x41, 5, 0x20, 2, 0x70, // Later possible trap: rem_u(5, p2).
        0x6a, 0x20, 3, 0x6a, 0x0b, // Sum the results and the local.
    ]);
    let validated = exported_function(&i32_type(3), &code);
    let instance = validated.instantiate().unwrap();
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[i32::MIN, -1, 0]),
        Err(InvokeError::Trap(Trap::IntegerOverflow)),
    );
    assert_eq!(
        instance.invoke_i32_with_args("answer", &[8, 2, 0]),
        Err(InvokeError::Trap(Trap::IntegerDivideByZero)),
    );
    for _ in 0..2 {
        assert_eq!(instance.invoke_i32_with_args("answer", &[8, 2, 3]), Ok(7));
    }
}

#[test]
fn selects_one_arm_using_i32_truthiness_and_preserves_outer_operands() {
    let code = code_body(&[
        1, 1, 0x7f, // One declared local (index 1).
        0x41, 40, 0x20, 0, // Preserve 40 below the condition.
        0x04, 0x7f, 0x41, 2, 0x22, 1, // Then: return 2 and write it to local 1.
        0x05, 0x41, 3, 0x0b, // Else: return 3 without writing.
        0x6a, 0x20, 1, 0x6a, 0x0b, // Add the preserved value, arm result and local 1.
    ]);
    let module = exported_function(&i32_type(1), &code);
    let instance = module.instantiate().unwrap();
    for (condition, expected) in [(1, 44), (7, 44), (-1, 44), (i32::MIN, 44), (0, 43)] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[condition]),
            Ok(expected)
        );
    }
}

#[test]
fn traps_only_when_the_arm_is_taken() {
    let safe = &[0x41, 42][..];
    let trap = &[0x41, 1, 0x41, 0, 0x6d][..];
    for (then, otherwise, trapping_condition) in [(safe, trap, 0), (trap, safe, 1)] {
        let code = code_body(
            &[
                &[0, 0x20, 0, 0x04, 0x7f][..],
                then,
                &[0x05],
                otherwise,
                &[0x0b, 0x0b],
            ]
            .concat(),
        );
        let module = exported_function(&i32_type(1), &code);
        let instance = module.instantiate().unwrap();
        for _ in 0..2 {
            assert_eq!(
                instance.invoke_i32_with_args("answer", &[1 - trapping_condition]),
                Ok(42)
            );
            assert_eq!(
                instance.invoke_i32_with_args("answer", &[trapping_condition]),
                Err(InvokeError::Trap(Trap::IntegerDivideByZero))
            );
        }
    }
}

#[test]
fn joins_nested_arms_at_the_enclosing_else_or_function_exit() {
    let code = code_body(&[
        0, 0x20, 0, 0x04, 0x7f, // Outer if.
        0x20, 1, 0x04, 0x7f, 0x41, 1, 0x05, 0x41, 2, 0x0b, // Nested in then.
        0x05, 0x20, 1, 0x04, 0x7f, 0x41, 3, 0x05, 0x41, 4, 0x0b, // Nested in else.
        0x0b, 0x0b, // End of outer if and function, respectively.
    ]);
    let module = exported_function(&i32_type(2), &code);
    let instance = module.instantiate().unwrap();
    for (args, expected) in [([1, 1], 1), ([1, 0], 2), ([0, 1], 3), ([0, 0], 4)] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(expected));
    }
}

#[test]
fn zero_test_and_ordering_comparisons_select_conditional_arms() {
    let code = code_body(&[
        0, 0x41, 40, // Preserve 40 below each arm's operand floor.
        0x20, 0, 0x45, 0x04, 0x7f, // if eqz(p0)
        0x20, 1, 0x20, 2, 0x48, // then: p1 <_s p2
        0x04, 0x7f, 0x41, 1, 0x05, 0x41, 2, 0x0b, // selects 1 or 2
        0x05, 0x20, 1, 0x20, 2, 0x49, // else: p1 <_u p2
        0x04, 0x7f, 0x41, 3, 0x05, 0x41, 4, 0x0b, // selects 3 or 4
        0x0b, 0x6a, 0x0b,
    ]);
    let module = exported_function(&i32_type(3), &code);
    let instance = module.instantiate().unwrap();
    for (args, expected) in [
        ([0, -1, 0], 41),
        ([0, 0, -1], 42),
        ([1, 0, -1], 43),
        ([1, -1, 0], 44),
    ] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(expected));
    }
}

#[test]
fn rejects_bad_if_block_type_and_missing_condition() {
    for found in [0x7b, 0, 0xff] {
        bad_body(
            &[0, 0x41, 1, 0x04, found],
            4,
            ErrorKind::UnexpectedByte {
                expected: 0x7f,
                found,
            },
        );
    }
    let mut code = code_body(&[0, 0x41, 1, 0x04]);
    let boundary = code.len();
    code.extend([0x7f, 0x41, 1, 0x05, 0x41, 2, 0x0b, 0x0b]);
    bad_code(&code, boundary, ErrorKind::TruncatedByte { expected: 0x7f });
    bad_body(
        &[0, 0x04, 0x7f, 0x41, 1, 0x05, 0x41, 2, 0x0b, 0x0b],
        1,
        ErrorKind::StackUnderflow { depth: 0 },
    );
}

#[test]
fn both_arms_require_one_new_result_and_validate_untaken_code() {
    let good = &[0x41, 7][..];
    for (bad, depth) in [(&[][..], 0), (&[0x41, 2, 0x41, 3][..], 2)] {
        for (condition, then, otherwise, bad_end) in [
            (0, bad, good, 7 + bad.len()),
            (1, good, bad, 8 + good.len() + bad.len()),
        ] {
            let body = [
                &[0, 0x41, 10, 0x41, condition, 0x04, 0x7f][..],
                then,
                &[0x05],
                otherwise,
                &[0x0b, 0x6a, 0x0b],
            ]
            .concat();
            bad_body(&body, bad_end, ErrorKind::InvalidResultDepth { depth });
        }
    }
    bad_body(
        &[0, 0x41, 1, 0x04, 0x7f, 0x41, 7, 0x05, 0x20, 0, 0x0b, 0x0b],
        9,
        ErrorKind::InvalidIndex {
            field: IntegerField::LocalIndex,
            index: 0,
        },
    );
}

#[test]
fn cannot_use_operands_below_the_arm_floor_even_if_restored() {
    let good = &[0x41, 7][..];
    for (bad, bad_op, depth) in [
        (&[0x21, 0, 0x41, 0, 0x41, 7][..], 0, 0), // Pop below the floor then restore depth.
        (&[0x22, 0, 0x41, 7], 0, 0),              // Access a floor operand without changing depth.
        (&[0x41, 1, 0x6a, 0x41, 7], 2, 1),        // Binary op takes two, not just its net pop.
        (&[0x04, 0x7f, 0x41, 1, 0x05, 0x41, 2, 0x0b, 0x41, 7], 0, 0), // Nested if needs its own condition.
    ] {
        for (condition, then, otherwise, arm_start) in
            [(0, bad, good, 9), (1, good, bad, 10 + good.len())]
        {
            let body = [
                &[1, 1, 0x7f, 0x41, 10, 0x41, condition, 0x04, 0x7f][..],
                then,
                &[0x05],
                otherwise,
                &[0x0b, 0x6a, 0x0b],
            ]
            .concat();
            bad_body(
                &body,
                arm_start + bad_op,
                ErrorKind::StackUnderflow { depth },
            );
        }
    }
}

#[test]
fn comparisons_require_their_operands_above_the_active_floor() {
    let push = [0x41, 1];
    for opcode in 0x45..=0x4f {
        let arity = if opcode == 0x45 { 1 } else { 2 };
        for depth in 0..arity {
            let operands = push.repeat(depth);
            let underflow = ErrorKind::StackUnderflow { depth };
            let body = [&[0][..], &operands, &[opcode, 0x0b]].concat();
            bad_body(&body, 1 + operands.len(), underflow);

            // The function has two frozen operands. If a comparison illegally
            // used one or both, the rest of the arm would restore its depth.
            let restore = push.repeat(arity - depth);
            let bad = [&operands[..], &[opcode], &restore].concat();
            for (condition, then, otherwise, arm_start) in
                [(0, &bad[..], &push[..], 9), (1, &push[..], &bad[..], 12)]
            {
                // The invalid arm is always untaken.
                let body = [
                    &[0, 0x41, 10, 0x41, 20, 0x41, condition, 0x04, 0x7f][..],
                    then,
                    &[0x05],
                    otherwise,
                    &[0x0b, 0x6a, 0x6a, 0x0b],
                ]
                .concat();
                bad_body(&body, arm_start + operands.len(), underflow);
            }
        }
    }
}

#[test]
fn conditional_delimiters_respect_the_body_and_function_end() {
    let then = &[0, 0x41, 1, 0x04, 0x7f, 0x41, 7][..];
    for (tail, at, kind) in [
        (&[][..], 0, ErrorKind::TruncatedByte { expected: 0x05 }),
        (&[0x0b, 0x0b], 0, ErrorKind::MissingElse),
        (
            &[0x05, 0x41, 8, 0x05, 0x0b, 0x0b],
            3,
            ErrorKind::UnexpectedElse,
        ),
        (
            &[0x05, 0x41, 8],
            3,
            ErrorKind::TruncatedByte { expected: 0x0b },
        ),
        (
            &[0x05, 0x41, 8, 0x0b],
            4,
            ErrorKind::TruncatedByte { expected: 0x0b },
        ),
        (
            &[0x05, 0x41, 8, 0x0b, 0x0b, 0x0b],
            5,
            ErrorKind::TrailingBodyBytes,
        ),
    ] {
        bad_body(&[then, tail].concat(), then.len() + at, kind);
    }
    bad_body(
        &[0, 0x41, 7, 0x05, 0x41, 8, 0x0b],
        3,
        ErrorKind::UnexpectedElse,
    );
    // Joining the arms checks their results, not the function's extra operand.
    let body = [&[0, 0x41, 9][..], &then[1..], &[0x05, 0x41, 8, 0x0b, 0x0b]].concat();
    bad_body(
        &body,
        body.len() - 1,
        ErrorKind::InvalidResultDepth { depth: 2 },
    );

    // Even after a valid conditional function, the module suffix is checked.
    let code = code_body(&[then, &[0x05, 0x41, 8, 0x0b, 0x0b]].concat());
    let mut bytes = module(&[(1, TYPE), (3, FUNCTION), (10, &code)]);
    let invalid = bytes.len();
    bytes.push(0xff);
    expect_error(&bytes, invalid, ErrorKind::UnknownSectionId { id: 0xff });
}

#[test]
fn bounds_simultaneously_open_if_frames() {
    let limit = Limit::ControlDepth;
    let nested = |depth| {
        let mut body = vec![0];
        for _ in 0..depth {
            body.extend([0x41, 1, 0x04, 0x7f]);
        }
        body.extend([0x41, 9]);
        for _ in 0..depth {
            body.extend([0x05, 0x41, 8, 0x0b]);
        }
        body.push(0x0b);
        body
    };
    assert_body_result(&nested(limit.maximum()), 9);
    bad_body(
        &nested(limit.maximum() + 1),
        3 + 4 * limit.maximum(),
        ErrorKind::LimitExceeded { limit },
    );
}

#[test]
fn conditional_body_byte_budget_includes_both_ends() {
    let limit = Limit::CodeBodyBytes;
    let ops = [0x41, 0, 0x04, 0x7f, 0x41, 7, 0x05, 0x41, 8, 0x0b, 0x0b];
    let groups = (limit.maximum() - 1 - ops.len()) / 2;
    let mut body = vec![u8::try_from(groups).unwrap()];
    for _ in 0..groups {
        body.extend([0, 0x7f]);
    }
    body.extend(ops);
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 8);

    // Pad the condition's LEB32 by one byte without changing decoded operations.
    let condition = 2 + 2 * groups;
    body.splice(condition..condition + 1, [0x80, 0]);
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn counts_control_ops_and_both_arms_together_in_the_instruction_budget() {
    let limit = Limit::BodyInstructions;
    let ty = i32_type(1);
    let mut body = vec![0, 0x20, 0, 0x04, 0x7f, 0x41, 7, 0x05, 0x41, 1];
    let pairs = (limit.maximum() - 5) / 2;
    assert_eq!(5 + 2 * pairs, limit.maximum());
    for _ in 0..pairs {
        body.extend([0x41, 1, 0x46]); // Check equality with 1, leaving 1.
    }
    body.extend([0x0b, 0x0b]);
    let module = exported_function(&ty, &code_body(&body));
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32_with_args("answer", &[1]), Ok(7));
    assert_eq!(instance.invoke_i32_with_args("answer", &[0]), Ok(1));

    let extra = body.len() - 2;
    body.insert(extra, 0x45);
    bad_typed_body(&ty, &body, extra, ErrorKind::LimitExceeded { limit });
}

#[test]
fn arm_operand_limit_includes_values_below_its_floor() {
    let limit = Limit::OperandStackDepth;
    let base = limit.maximum() - 2;
    let mut body = vec![0];
    for _ in 0..base {
        body.extend([0x41, 1]);
    }
    body.extend([0x41, 0, 0x04, 0x7f, 0x41, 7, 0x05, 0x41, 1, 0x41, 2]);
    let at_limit = body.len();
    // eqz replaces the top operand at full depth, then lt_s consumes two.
    body.extend([0x45, 0x48, 0x0b]);
    body.extend(std::iter::repeat_n(0x6a, base));
    body.push(0x0b);
    assert_body_result(&body, base as i32);
    // The else arm exceeds the limit even though the condition now selects then.
    body[2 + 2 * base] = 1;
    body.splice(at_limit..at_limit, [0x41, 3, 0x6a]);
    bad_body(&body, at_limit, ErrorKind::LimitExceeded { limit });
}

#[test]
fn branches_join_innermost_blocks_across_mixed_nesting() {
    let body = [
        0, 0x41, 40, 0x02, 0x7f, // Outer block preserves 40 below its floor.
        0x41, 5, 0x20, 0, 0x04, 0x7f, // Both if arms preserve 5.
        0x02, 0x7f, 0x41, 20, 0x41, 2, 0x20, 1, 0x0d, 0, 0x6a, 0x0b,
        // Then: takes 2 or falls through to add 20 + 2; joins at the else jump.
        0x05, 0x02, 0x7f, 0x41, 3, 0x41, 1, 0x20, 1, 0x0d, 0x80, 0x80, 0x80, 0x80, 0, 0x45, 0x6a,
        0x0b, 0x0b, // Else: takes 1 (padded label) or falls through to make 3.
        0x20, 2, 0x0d, 0, 0x6a, 0x0b, // Outer branch drops 5 only when taken.
        0x6a, 0x0b, // Both exits preserve 40.
    ];
    let module = exported_function(&i32_type(3), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (args, expected) in [
        ([1, 1, 0], 47),
        ([1, 0, 0], 67),
        ([0, 1, 0], 46),
        ([0, 0, 0], 48),
        ([1, 1, -1], 42),
        ([1, 0, 1], 62),
        ([0, 1, i32::MIN], 41),
    ] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(expected));
    }
}

#[test]
fn exits_an_enclosing_block_across_loop_if_and_block_frames() {
    let body = [
        0, 0x41, 40, 0x02, 0x7f, // Target block preserves 40.
        0x41, 10, 0x03, 0x40, // Loop freezes 10.
        0x41, 20, 0x20, 0, 0x04, 0x7f, // If freezes 20.
        0x41, 8, 0x02, 0x7f, // Inner block freezes 8.
        0x41, 9, 0x41, 2, 0x20, 1, 0x0d, 0x83, 0x80, 0x80, 0x80, 0,
        // Padded depth 3: take only 2 (drop 10, 20, 8 and 9).
        0x20, 2, 0x0d, 0, 0x6a, 0x0b, // Inner fixup: take 2 or add 9 + 2.
        0x6a, 0x05, // Then adds 8 to the inner block result.
        0x41, 3, 0x20, 1, 0x0d, 2, // Second outer fixup, through the else arm.
        0x0b, 0x6a, 0x21, 0, 0x0b, // Add 20 and save the result before leaving the loop.
        0x20, 0, 0x6a, 0x0b, // Normal block exit adds 10 to the saved result.
        0x6a, 0x0b, // Both branch and normal exits add 40.
    ];
    let module = exported_function(&i32_type(3), &code_body(&body));
    let instance = module.instantiate().unwrap();
    for (args, expected) in [
        ([1, -1, 1], 42), // Taken nonzero-depth branch skips the inner fixup.
        ([0, 1, 0], 43),
        ([1, 0, 1], 80), // Outer fallthrough, inner taken.
        ([1, 0, 0], 89), // Both fall through.
        ([0, 0, 0], 73),
    ] {
        assert_eq!(instance.invoke_i32_with_args("answer", &args), Ok(expected));
    }
}

#[test]
fn nonlocal_branches_cannot_take_operands_from_below_the_active_floor() {
    for (target, operands, depth) in [
        ([0x02, 0x7f], &[][..], 0),
        ([0x02, 0x7f], &[0x41, 1][..], 1),
        ([0x03, 0x40], &[][..], 0),
    ] {
        // Both 7 and 8 are above the target's floor but frozen in the if.
        // Validate its else branch even though the literal selects then.
        let prefix = [
            &[0][..],
            &target,
            &[0x41, 7, 0x41, 8, 0x41, 1, 0x04, 0x7f, 0x41, 9, 0x05],
            operands,
        ]
        .concat();
        bad_body(
            &[&prefix[..], &[0x0d, 1, 0x0b, 0x0b, 0x0b]].concat(),
            prefix.len(),
            ErrorKind::StackUnderflow { depth },
        );
    }
}

#[test]
fn patches_multiple_branches_to_function_exit_and_only_fallthrough_traps() {
    let body = [
        0, 0x02, 0x7f, 0x41, 7, 0x20, 0, 0x0d, 0, // First exit returns 7.
        0x41, 8, 0x20, 1, 0x0d, 0, // Second exit drops 7 and returns 8.
        0x6a, 0x41, 0, 0x6d, 0x0b, 0x0b, // Divide by zero only on fallthrough.
    ];
    let module = exported_function(&i32_type(2), &code_body(&body));
    let instance = module.instantiate().unwrap();
    let invoke = |args| instance.invoke_i32_with_args("answer", args);
    assert_eq!(invoke(&[1, 1]), Ok(7));
    assert_eq!(invoke(&[0, 1]), Ok(8));
    assert_eq!(
        invoke(&[0, 0]),
        Err(InvokeError::Trap(Trap::IntegerDivideByZero))
    );
    assert_eq!(invoke(&[-1, 0]), Ok(7));
}

#[test]
fn block_requires_a_new_result_and_checks_all_fallthrough_code() {
    let prefix = &[0, 0x41, 9, 0x02, 0x7f][..];
    // A block cannot return a frozen value, and the normal path always needs
    // exactly one new value, even after a branch with literal condition 1.
    bad_body(
        &[prefix, &[0x0b, 0x0b]].concat(),
        5,
        ErrorKind::InvalidResultDepth { depth: 0 },
    );
    let taken = &[0x41, 7, 0x41, 1, 0x0d, 0];
    let branch = [prefix, taken].concat();
    bad_body(
        &[&branch[..], &[0x41, 8, 0x0b, 0x6a, 0x0b]].concat(),
        branch.len() + 2,
        ErrorKind::InvalidResultDepth { depth: 2 },
    );
    bad_body(
        &[&branch[..], &[0x12, 0x0b, 0x0b]].concat(),
        branch.len(),
        ErrorKind::UnsupportedOpcode { opcode: 0x12 },
    );
    bad_body(
        &[&branch[..], &[0x0d, 0x80]].concat(),
        branch.len() + 2,
        ErrorKind::TruncatedInteger {
            field: IntegerField::LabelIndex,
        },
    );
    bad_body(
        &[&branch[..], &[0x0b, 0x0b]].concat(),
        branch.len() + 1,
        ErrorKind::InvalidResultDepth { depth: 2 },
    );
}

#[test]
fn block_floors_protect_enclosing_arm_operands_even_on_untaken_paths() {
    // The untaken else arm has two values which the new block cannot access.
    let prefix = &[
        0, 0x41, 1, 0x04, 0x7f, 0x41, 7, 0x05, 0x41, 8, 0x41, 9, 0x02, 0x7f,
    ];
    for (bad, op, depth) in [
        (&[0x0d, 0][..], 0, 0),      // Neither condition nor result.
        (&[0x41, 1, 0x0d, 0], 2, 1), // Condition but no result.
        (&[0x41, 1, 0x6a], 2, 1),    // Arithmetic cannot use the frozen value either.
        (&[0x04, 0x7f], 0, 0),       // A nested if would also need its own condition.
    ] {
        let body = [&prefix[..], bad, &[0x0b, 0x6a, 0x6a, 0x0b, 0x0b]].concat();
        bad_body(
            &body,
            prefix.len() + op,
            ErrorKind::StackUnderflow { depth },
        );
    }
}

#[test]
fn validates_block_type_and_delimiters_at_the_body_boundary() {
    for found in [0x7b, 0, 0xff] {
        bad_body(
            &[0, 0x02, found],
            2,
            ErrorKind::UnexpectedByte {
                expected: 0x7f,
                found,
            },
        );
    }
    bad_body(&[0, 0x02], 2, ErrorKind::TruncatedByte { expected: 0x7f });
    let prefix = &[0, 0x02, 0x7f, 0x41, 7][..];
    for (tail, at, kind) in [
        (&[][..], 0, ErrorKind::TruncatedByte { expected: 0x0b }),
        (&[0x0b], 1, ErrorKind::TruncatedByte { expected: 0x0b }),
        (&[0x0b, 0x0b, 0x0b], 2, ErrorKind::TrailingBodyBytes),
        (&[0x05, 0x0b, 0x0b], 0, ErrorKind::UnexpectedElse),
    ] {
        bad_body(&[prefix, tail].concat(), prefix.len() + at, kind);
    }
    // The outer if still needs its own else after the inner block closes.
    bad_body(
        &[
            0, 0x41, 1, 0x04, 0x7f, 0x02, 0x7f, 0x41, 7, 0x0b, 0x0b, 0x0b,
        ],
        10,
        ErrorKind::MissingElse,
    );
}

#[test]
fn validates_branch_label_leb_and_rejects_out_of_range_depths() {
    for opcode in [0x0c, 0x0d] {
        check_branch_label_errors(opcode);
    }
}

fn check_branch_label_errors(opcode: u8) {
    let field = IntegerField::LabelIndex;
    // Decode the full immediate and check its destination before consuming
    // branch inputs: there are no available operands in any of these frames.
    for opener in [[0x02, 0x7f], [0x03, 0x40]] {
        let prefix = [&[0][..], &opener, &opener, &[opcode]].concat();
        for (label, index) in [
            (&[3][..], 3), // Beyond the function label.
            (&[0xff, 0xff, 0xff, 0xff, 0x0f], u32::MAX),
        ] {
            let body = [&prefix[..], label, &[0x0b, 0x0b, 0x0b]].concat();
            bad_body(
                &body,
                prefix.len(),
                ErrorKind::InvalidIndex { field, index },
            );
        }
        for fifth in [0x10, 0x80] {
            let body = [
                &prefix[..],
                &[0x80, 0x80, 0x80, 0x80, fifth, 0, 0x0b, 0x0b, 0x0b],
            ]
            .concat();
            bad_body(
                &body,
                prefix.len() + 4,
                ErrorKind::IntegerOverflow { field },
            );
        }
        let body = [&prefix[..], &[0x80]].concat();
        bad_body(&body, body.len(), ErrorKind::TruncatedInteger { field });
    }
    for (prefix, index) in [
        (&[0, 0x41, 1, 0x04, 0x7f][..], 2),
        (&[0, 0x03, 0x40, 0x41, 1, 0x04, 0x7f], 3),
        (&[0, 0x02, 0x7f, 0x41, 1, 0x04, 0x7f, 0x41, 7, 0x05], 3),
    ] {
        let branch = [prefix, &[opcode]].concat();
        bad_body(
            &[&branch[..], &[index, 0x0b]].concat(),
            branch.len(),
            ErrorKind::InvalidIndex {
                field,
                index: u32::from(index),
            },
        );
        bad_body(&branch, branch.len(), ErrorKind::TruncatedInteger { field });
    }
}

#[test]
fn bounds_blocks_and_ifs_on_the_same_control_stack() {
    let limit = Limit::ControlDepth;
    for mixed in [false, true] {
        let nested = |depth| {
            let mut body = vec![0];
            let is_if = |level| mixed && level % 2 == 1;
            let mut last_open = 0;
            for level in 0..depth {
                if is_if(level) {
                    body.extend([0x41, 1]);
                }
                last_open = body.len();
                body.extend([if is_if(level) { 0x04 } else { 0x02 }, 0x7f]);
            }
            // Take the largest supported depth at the control cap.
            body.extend([0x41, 9, 0x41, 1, 0x0d, (depth - 1) as u8]);
            for level in (0..depth).rev() {
                if is_if(level) {
                    body.extend([0x05, 0x41, 8]);
                }
                body.push(0x0b);
            }
            body.push(0x0b);
            (body, last_open)
        };
        assert_body_result(&nested(limit.maximum()).0, 9);
        let (body, last_open) = nested(limit.maximum() + 1);
        bad_body(&body, last_open, ErrorKind::LimitExceeded { limit });
    }
}

#[test]
fn counts_both_branches_but_not_block_or_end_as_body_instructions() {
    let limit = Limit::BodyInstructions;
    for opcode in [0x0c, 0x0d] {
        let conditional = opcode == 0x0d;
        let mut body = vec![0, 0x02, 0x7f, 0x02, 0x7f, 0x41, 1];
        body.extend(std::iter::repeat_n(
            0x45,
            limit.maximum() - 2 - usize::from(conditional),
        ));
        if conditional {
            body.extend([0x41, 1]);
        }
        let branch = body.len();
        body.extend([opcode, 1, 0x0b, 0x0b, 0x0b]); // Branch is instruction 127.
        assert_body_result(&body, i32::from(conditional));
        body.insert(branch, 0x45); // Now the branch itself exceeds the cap.
        body.truncate(branch + 2); // The cap is checked before the missing immediate.
        bad_body(&body, branch + 1, ErrorKind::LimitExceeded { limit });
    }
}

#[test]
fn counts_block_encoding_and_branch_immediate_in_body_bytes() {
    let limit = Limit::CodeBodyBytes;
    let ops = [
        0x02, 0x7f, 0x02, 0x7f, 0x41, 7, 0x41, 1, 0x0d, 0x81, 0x80, 0, 0x0b, 0x0b, 0x0b,
    ];
    let groups = (limit.maximum() - 1 - ops.len()) / 2;
    let mut body = vec![u8::try_from(groups).unwrap()];
    body.extend([0, 0x7f].repeat(groups));
    body.extend(ops);
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 7);
    body.insert(body.len() - 4, 0x80); // One more LEB padding byte for depth one.
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn counts_frozen_operands_toward_the_branch_stack_limit() {
    let limit = Limit::OperandStackDepth;
    let floor = limit.maximum() - 2;
    let mut body = vec![0];
    body.extend([0x41, 1].repeat(floor));
    body.extend([0x02, 0x7f, 0x02, 0x7f, 0x41, 7]);
    let condition = body.len();
    body.extend([0x41, 1, 0x0d, 1, 0x0b, 0x0b]);
    body.extend(std::iter::repeat_n(0x6a, floor));
    body.push(0x0b);
    assert_body_result(&body, floor as i32 + 7);
    body.splice(condition..condition, [0x41, 1]);
    bad_body(&body, condition + 2, ErrorKind::LimitExceeded { limit });
}

#[test]
fn charges_taken_br_if_but_not_its_skipped_tail_or_block_delimiters() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    // Leaf: 126 dispatches (result, 123 eqz, condition, br_if), skipping its tail.
    let mut leaf = vec![0, 0x02, 0x7f, 0x02, 0x7f, 0x41, 1];
    leaf.extend(std::iter::repeat_n(0x45, 123));
    leaf.extend([0x41, 1, 0x0d, 1, 0x45, 0x0b, 0x0b, 0x0b]);
    let entry = |extra| {
        let mut body = vec![1, 1, 0x7f];
        body.extend([0x10, 2]);
        body.extend(std::iter::repeat_n(0x45, extra));
        body.extend([0x21, 0]);
        body.extend([0x10, 2, 0x21, 0].repeat(6));
        body.extend([0x10, 2, 0x0b]);
        body
    };
    let module = exported_functions(
        0,
        &[(b"exact", 0), (b"over", 1)],
        &[&entry(1), &entry(2), &leaf],
    );
    let instance = module.instantiate().unwrap();
    // 7 * (call + 126 + local.set) + eqz + call + 126 = 1024.
    assert_eq!(instance.invoke_i32("exact"), Ok(0));
    // A second eqz in the caller runs the budget out before the final br_if.
    assert_eq!(
        instance.invoke_i32("over"),
        Err(InvokeError::LimitExceeded { limit })
    );
}

#[test]
fn continues_an_enclosing_loop_across_loop_if_and_block_frames() {
    let body = [
        1, 1, 0x7f, 0x41, 39, 0x02, 0x7f, // Keep 39 below the block floor.
        0x41, 1, 0x03, 0x40, // Keep a second value below the loop floor.
        0x20, 1, 0x41, 1, 0x6a, 0x21, 1, // Count trips in the declared local.
        0x41, 7, 0x03, 0x40, // Inner loop freezes 7.
        0x41, 3, 0x41, 1, 0x04, 0x7f, // If freezes 3.
        0x41, 4, 0x02, 0x7f, // Inner block freezes 4.
        0x41, 2, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x41, 0, 0x4a, // --param > 0
        0x0d, 0x83, 0, // Padded depth 3 resumes at the trip counter update.
        0x0b, 0x6a, 0x05, 0x41, 0, 0x0b, // On fallthrough the then arm yields 4 + 2.
        0x6a, 0x20, 1, 0x6a, 0x21, 1, 0x0b, // Add 3 and the if result into the counter.
        0x20, 1, 0x6a, 0x21, 1, 0x0b, // Add 7 into the counter.
        0x20, 1, 0x6a, 0x0b, 0x6a, 0x0b, // Add the two preserved values.
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    // 40 trips would overflow the operand storage if each kept its temporaries.
    // With arg 1, even the first condition is false.
    for (arg, expected) in [(40, 96), (1, 57), (5, 61), (1, 57)] {
        assert_eq!(
            instance.invoke_i32_with_args("answer", &[arg]),
            Ok(expected)
        );
    }
}

#[test]
fn loops_compose_with_forward_block_exits_and_if_joins() {
    let body = [
        1, 1, 0x7f, 0x41, 40, 0x03, 0x40, 0x03, 0x40,
        0x0b, // The elided empty inner loop shares the body start PC.
        0x20, 0, 0x04, 0x7f, // If the parameter is nonzero, forward-exit with 2.
        0x02, 0x7f, 0x41, 8, 0x41, 2, 0x41, 0x7f, 0x0d, 0, 0x6a, 0x0b, 0x05, 0x41, 1,
        0x0b, // Otherwise produce 1.
        0x20, 1, 0x6a, 0x21, 1, // Add the if result to the local.
        0x20, 0, 0x41, 0, 0x21, 0, 0x0d, 0, // Continue using the old parameter value.
        0x0b, 0x20, 1, 0x6a, 0x0b,
    ];
    let module = exported_function(&i32_type(1), &code_body(&body));
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32_with_args("answer", &[-1]), Ok(43));
    assert_eq!(instance.invoke_i32_with_args("answer", &[0]), Ok(41));
}

#[test]
fn rejects_loop_floor_violations_and_nonempty_results() {
    let prefix = &[0, 0x41, 42, 0x03, 0x40][..];
    for (ops, op, available) in [
        (&[0x0d, 0][..], 0, 0), // The saved prefix is not a condition.
        (&[0x45], 0, 0),        // A loop cannot read its prefix even if the depth would recover.
        (&[0x41, 1, 0x6a], 2, 1),
    ] {
        let body = [prefix, ops, &[0x0b, 0x0b]].concat();
        bad_body(
            &body,
            prefix.len() + op,
            ErrorKind::StackUnderflow { depth: available },
        );
    }
    // Even a literal nonzero condition does not waive checks on fallthrough.
    let branch = [prefix, &[0x41, 1, 0x0d, 0]].concat();
    for (tail, offset, kind) in [
        (
            &[0x41, 7, 0x0b, 0x0b][..],
            2,
            ErrorKind::InvalidResultDepth { depth: 1 },
        ),
        (
            &[0x12, 0x0b, 0x0b],
            0,
            ErrorKind::UnsupportedOpcode { opcode: 0x12 },
        ),
        (&[0x0b, 0x0b, 0x0b], 2, ErrorKind::TrailingBodyBytes),
    ] {
        bad_body(&[&branch[..], tail].concat(), branch.len() + offset, kind);
    }
    // A zero-result loop does not relax the function's one-result requirement.
    bad_body(
        &[0, 0x03, 0x40, 0x0b, 0x0b],
        4,
        ErrorKind::InvalidResultDepth { depth: 0 },
    );
}

#[test]
fn validates_loop_type_and_delimiters_within_the_body_frame() {
    for found in [0x7f, 0x7e, 0, 0xc0] {
        bad_body(
            &[0, 0x03, found],
            2,
            ErrorKind::UnexpectedByte {
                expected: 0x40,
                found,
            },
        );
    }
    let mut code = code_body(&[0, 0x03]);
    let boundary = code.len();
    code.extend([0x40, 0x0b]); // A block type outside the body can't complete it.
    bad_code(&code, boundary, ErrorKind::TruncatedByte { expected: 0x40 });
    let prefix = &[0, 0x41, 42, 0x03, 0x40][..];
    for (tail, offset, kind) in [
        (&[][..], 0, ErrorKind::TruncatedByte { expected: 0x0b }),
        (&[0x0b], 1, ErrorKind::TruncatedByte { expected: 0x0b }),
        (&[0x05, 0x0b, 0x0b], 0, ErrorKind::UnexpectedElse),
    ] {
        bad_body(&[prefix, tail].concat(), prefix.len() + offset, kind);
    }
}

#[test]
fn loops_share_the_control_depth_cap_with_both_forward_constructs() {
    let limit = Limit::ControlDepth;
    let nested = |depth: usize| {
        let mut body = vec![1, 1, 0x7f];
        let opener = |level| [0x03, 0x02, 0x04][level % 3];
        let mut last_open = 0;
        for level in 0..depth {
            let op = opener(level);
            if op == 0x04 {
                body.extend([0x41, 1]);
            }
            last_open = body.len();
            body.extend([op, if op == 0x03 { 0x40 } else { 0x7f }]);
        }
        for level in (0..depth).rev() {
            let op = opener(level);
            if op != 0x03 {
                body.extend([0x41, 7]);
                if op == 0x04 {
                    body.extend([0x05, 0x41, 8]);
                }
            }
            body.push(0x0b);
            if op != 0x03 {
                body.extend([0x21, 0]); // Consume the nested block or if result in its parent.
            }
        }
        body.extend([0x20, 0, 0x0b]);
        (body, last_open)
    };
    assert_body_result(&nested(limit.maximum()).0, 7);
    let (body, last_open) = nested(limit.maximum() + 1);
    bad_body(&body, last_open, ErrorKind::LimitExceeded { limit });
}

#[test]
fn counts_loop_delimiters_in_body_bytes_but_not_decoded_instructions() {
    let limit = Limit::BodyInstructions;
    let mut body = vec![0, 0x41, 9, 0x03, 0x40, 0x41, 0];
    body.extend(std::iter::repeat_n(0x45, limit.maximum() - 3));
    let branch = body.len();
    // Branch is decoded instruction 127. An empty loop can follow at the cap.
    body.extend([0x0d, 0, 0x0b, 0x03, 0x40, 0x0b, 0x0b]);
    assert_body_result(&body, 9);
    body.insert(branch, 0x45);
    bad_body(&body, branch + 1, ErrorKind::LimitExceeded { limit });

    let limit = Limit::CodeBodyBytes;
    let ops = [0x41, 9, 0x03, 0x40, 0x41, 0, 0x0d, 0x80, 0, 0x0b, 0x0b];
    let groups = (limit.maximum() - 1 - ops.len()) / 2;
    let mut body = vec![u8::try_from(groups).unwrap()];
    body.extend([0, 0x7f].repeat(groups));
    body.extend(ops);
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 9);
    body.insert(body.len() - 3, 0x80); // Add another label padding byte.
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn loop_operand_cap_includes_the_frozen_prefix() {
    let limit = Limit::OperandStackDepth;
    let floor = limit.maximum() - 1;
    let mut body = vec![1, 1, 0x7f];
    body.extend([0x41, 1].repeat(floor));
    body.extend([0x03, 0x40]);
    let condition = body.len();
    body.extend([0x41, 0, 0x0d, 0, 0x0b]);
    body.extend(std::iter::repeat_n(0x6a, floor - 1));
    body.push(0x0b);
    assert_body_result(&body, floor as i32);
    // Keep one temporary until after br_if, then consume it on fallthrough.
    body.splice(
        condition..condition + 4,
        [0x41, 7, 0x41, 0, 0x0d, 0, 0x21, 0],
    );
    bad_body(&body, condition + 2, ErrorKind::LimitExceeded { limit });
}

#[test]
fn loop_dispatches_share_the_budget_with_callers_and_reset_on_reuse() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    let module = exported_functions(
        1,
        &[(b"exact", 0), (b"over", 1), (b"spin", 3)],
        &[
            b"\0\x20\0\x22\0\x10\x02\x0b",       // local.get, local.tee, call
            b"\0\x20\0\x22\0\x22\0\x10\x02\x0b", // Same, with one extra local.tee.
            // Preserve 42; repeat (get, const, sub, tee, br_if 1) until --param == 0.
            b"\0\x41\x2a\x03\x40\x03\x40\x20\0\x41\x01\x6b\x22\0\x0d\x01\x0b\x0b\x0b",
            // Always continue to the outer loop.
            b"\0\x41\x2a\x03\x40\x03\x40\x41\x01\x0d\x01\x0b\x0b\x0b",
        ],
    );
    let instance = module.instantiate().unwrap();
    let invoke = |name, arg| instance.invoke_i32_with_args(name, &[arg]);
    // 3 caller dispatches + 1 const + 204 * 5 = 1024. The final false br_if
    // uses the last dispatch; both returns are free. With an extra caller op,
    // the budget runs out just before that br_if.
    assert_eq!(invoke("exact", 204), Ok(42));
    assert_eq!(
        invoke("over", 204),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(invoke("over", 1), Ok(42));
    assert_eq!(invoke("spin", 0), Err(InvokeError::LimitExceeded { limit }));
    assert_eq!(invoke("exact", 204), Ok(42));
}

#[test]
fn rejects_every_opcode_outside_the_slice() {
    for opcode in 0..=u8::MAX {
        if ![
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x1a,
            0x1b, 0x20, 0x21, 0x22, 0x23, 0x24, 0x3f, 0x40, 0x98, 0xa6, 0xa7, 0xac, 0xad,
        ]
        .contains(&opcode)
            && !(0x28..=0x3e).contains(&opcode)
            && !(0x41..=0x90).contains(&opcode)
            && !(0x99..=0x9e).contains(&opcode)
            && !(0xbc..=0xbf).contains(&opcode)
        {
            bad_body(
                &[0, 0x41, 3, opcode, 0x0b],
                3,
                ErrorKind::UnsupportedOpcode { opcode },
            );
        }
    }
}

#[test]
fn set_pops_and_tee_preserves_an_operand_but_both_require_one() {
    for opcode in [0x21, 0x22] {
        bad_body(
            &[1, 1, 0x7f, opcode, 0, 0x0b],
            3,
            ErrorKind::StackUnderflow { depth: 0 },
        );
    }
    let set = [1, 1, 0x7f, 0x41, 7, 0x21, 0, 0x0b];
    bad_body(&set, 7, ErrorKind::InvalidResultDepth { depth: 0 });
    let mut tee = set;
    tee[5] = 0x22;
    assert_body_result(&tee, 7);
}

#[test]
fn requires_two_operands_for_each_binary_op_and_one_result_at_end() {
    for opcode in 0x6a..=0x70 {
        bad_body(
            &[0, opcode, 0x0b],
            1,
            ErrorKind::StackUnderflow { depth: 0 },
        );
        bad_body(
            &[0, 0x41, 1, opcode, 0x0b],
            3,
            ErrorKind::StackUnderflow { depth: 1 },
        );
        // A reduction may leave only one operand, too.
        bad_body(
            &[0, 0x41, 1, 0x41, 2, 0x6a, opcode, 0x0b],
            6,
            ErrorKind::StackUnderflow { depth: 1 },
        );
    }
    bad_body(&[0, 0x0b], 1, ErrorKind::InvalidResultDepth { depth: 0 });
    bad_body(
        &[0, 0x41, 1, 0x41, 2, 0x0b],
        5,
        ErrorKind::InvalidResultDepth { depth: 2 },
    );
}

#[test]
fn does_not_skip_result_depth_validation_for_a_trapping_body() {
    for opcode in 0x6d..=0x70 {
        // Executing this operation would trap, but validation must still check `end`.
        bad_body(
            &[0, 0x41, 8, 0x41, 1, 0x41, 0, opcode, 0x0b],
            8,
            ErrorKind::InvalidResultDepth { depth: 2 },
        );
    }
}

#[test]
fn bounds_body_bytes_inclusively() {
    let limit = Limit::CodeBodyBytes;
    let mut body = vec![0, 0x41, 0];
    // Each addition costs seven bytes with a maximally padded but legal signed LEB32.
    for _ in 0..36 {
        body.extend([0x41, 0x81, 0x80, 0x80, 0x80, 0, 0x6a]);
    }
    body.push(0x0b);
    assert_eq!(body.len(), limit.maximum());
    assert_body_result(&body, 36);

    // Change only the encoding of the first immediate, not the instruction count or values.
    body.splice(2..3, [0x80, 0]);
    assert_eq!(body.len(), limit.maximum() + 1);
    bad_code(&code_body(&body), 1, ErrorKind::LimitExceeded { limit });
    // The bound also applies before trying to frame an unavailable body.
    let mut length_only = vec![1];
    push_u32(&mut length_only, body.len());
    bad_code(&length_only, 1, ErrorKind::LimitExceeded { limit });
}

#[test]
fn bounds_instruction_count_inclusively() {
    let limit = Limit::BodyInstructions;
    let pairs = (limit.maximum() - 1) / 2;
    assert_eq!(1 + 2 * pairs, limit.maximum());
    let ty = i32_type(1);
    let mut body = vec![0, 0x41, 0];
    for _ in 0..pairs {
        body.extend([0x20, 0, 0x6a]);
    }
    body.push(0x0b);
    let validated = exported_function(&ty, &code_body(&body));
    assert_eq!(
        validated
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("answer", &[1]),
        Ok(pairs as i32)
    );

    // A tee preserves the final depth but exceeds the instruction budget by one.
    let extra = body.len() - 1;
    body.splice(extra..extra, [0x22, 0]);
    bad_typed_body(&ty, &body, extra, ErrorKind::LimitExceeded { limit });
}

#[test]
fn bounds_operand_depth_inclusively() {
    let limit = Limit::OperandStackDepth;
    let ty = i32_type(1);
    let body_at_depth = |depth: usize, opcode| {
        let mut body = vec![0];
        for _ in 0..depth {
            body.extend([0x20, 0]);
        }
        body.extend([0x22, 0]); // A tee is valid at full depth; it doesn't push.
        body.push(opcode); // Reduce the top two operands at full depth.
        body.extend(std::iter::repeat_n(0x6a, depth - 2));
        body.push(0x0b);
        body
    };
    for (opcode, reduced) in [(0x6a, 2), (0x6d, 1), (0x6e, 1), (0x6f, 0), (0x70, 0)] {
        let validated = exported_function(&ty, &code_body(&body_at_depth(limit.maximum(), opcode)));
        assert_eq!(
            validated
                .instantiate()
                .unwrap()
                .invoke_i32_with_args("answer", &[1]),
            Ok(limit.maximum() as i32 - 2 + reduced),
        );
        bad_typed_body(
            &ty,
            &body_at_depth(limit.maximum() + 1, opcode),
            1 + 2 * limit.maximum(),
            ErrorKind::LimitExceeded { limit },
        );
    }
}

fn code_immediate(leb: &[u8]) -> Vec<u8> {
    let mut code = vec![1, u8::try_from(leb.len() + 3).unwrap(), 0, 0x41];
    code.extend_from_slice(leb);
    code.push(0x0b);
    code
}

#[test]
fn decodes_signed_boundaries_and_legal_padding() {
    let cases: &[(&[u8], i32)] = &[
        (&[0], 0),
        (&[0x3f], 63),
        (&[0x40], -64),
        (&[0x7f], -1),
        (&[0xc0, 0], 64),
        (&[0xbf, 0x7f], -65),
        (&[0x80, 0xc0, 0x00], 8192),
        (&[0xff, 0xff, 0xbf, 0x7f], -1_048_577),
        (&[0x81, 0x80, 0x80, 0x80, 0], 1),
        (&[0xff, 0xff, 0xff, 0xff, 0x7f], -1),
        (&[0x80, 0x80, 0x80, 0x80, 0x78], i32::MIN),
        (&[0xff, 0xff, 0xff, 0xff, 0x07], i32::MAX),
    ];
    for &(leb, expected) in cases {
        let bytes = module(&[
            (1, TYPE),
            (3, FUNCTION),
            (7, EXPORT),
            (10, &code_immediate(leb)),
        ]);
        assert_eq!(
            validate_module(&bytes)
                .unwrap()
                .instantiate()
                .unwrap()
                .invoke_i32("answer"),
            Ok(expected)
        );
    }
}

#[test]
fn accepts_legal_padding_in_counts_indices_and_nested_lengths() {
    let zero = [0x80, 0x80, 0x80, 0x80, 0];
    let one = [0x81, 0x80, 0x80, 0x80, 0];
    let ty = [&one[..], &[0x60], &zero, &one, &[0x7f]].concat();
    let function = [&one[..], &zero].concat();
    let export = [&one[..], &zero, &[0], &zero].concat();
    let code = [
        &one[..],
        &[0x88, 0x80, 0x80, 0x80, 0],
        &zero,
        &[0x41, 42, 0x0b],
    ]
    .concat();
    let bytes = module(&[(1, &ty), (3, &function), (7, &export), (10, &code)]);
    assert_eq!(
        validate_module(&bytes)
            .unwrap()
            .instantiate()
            .unwrap()
            .invoke_i32(""),
        Ok(42)
    );
}

#[test]
fn rejects_signed_leb_overflow_and_continuation_in_the_fifth_byte() {
    for fifth in [0x08, 0x0f, 0x10, 0x70, 0x77, 0x80, 0xf8, 0xff] {
        // Even with a terminating sixth byte, the error is at byte five.
        let code = code_immediate(&[0x80, 0x80, 0x80, 0x80, fifth, 0]);
        bad_payload(
            10,
            &code,
            8,
            ErrorKind::IntegerOverflow {
                field: IntegerField::I32Const,
            },
        );
    }
}

#[test]
fn rejects_unsigned_leb_overflow_and_max_u32_indices() {
    for fifth in [0x10, 0x7f, 0x80] {
        let leb = [0x80, 0x80, 0x80, 0x80, fifth, 0];
        let index = [&[1][..], &leb].concat();
        let cases = [
            (1, &leb[..], 4, IntegerField::TypeCount),
            (3, &index[..], 5, IntegerField::TypeIndex),
        ];
        for (id, payload, offset, field) in cases {
            bad_payload(id, payload, offset, ErrorKind::IntegerOverflow { field });
        }
        bad_payload(
            10,
            &index,
            5,
            ErrorKind::LengthOverflow {
                field: LengthField::CodeBodySize,
            },
        );
    }
    bad_payload(
        3,
        &[1, 0xff, 0xff, 0xff, 0xff, 0x0f],
        1,
        ErrorKind::InvalidIndex {
            field: IntegerField::TypeIndex,
            index: u32::MAX,
        },
    );
    let mut bytes = module(&[(1, TYPE), (3, FUNCTION)]);
    let exports = function_exports(&[(b"", u32::MAX as usize)]);
    let start = push_section(&mut bytes, 7, &exports);
    expect_error(
        &bytes,
        start + 3,
        ErrorKind::InvalidIndex {
            field: IntegerField::FunctionIndex,
            index: u32::MAX,
        },
    );
}

#[test]
fn bounds_type_function_code_and_export_counts() {
    use IntegerField::*;
    for (id, prefix, field, bad_counts) in [
        (1, &[][..], TypeCount, &[0, 9][..]),
        (1, &[1, 0x60], ParameterCount, &[5]),
        (1, &[1, 0x60, 0], ResultCount, &[2]),
        (3, &[], FunctionCount, &[0, 9]),
        (7, &[], ExportCount, &[9]),
        (10, &[], CodeCount, &[0, 9]),
    ] {
        for &count in bad_counts {
            let payload = [prefix, &[count]].concat();
            bad_payload(
                id,
                &payload,
                prefix.len(),
                ErrorKind::UnsupportedCount {
                    field,
                    count: u32::from(count),
                },
            );
        }
    }
    for (id, payload, index, expected) in [
        (1, &[1, 0x61, 0, 1, 0x7f][..], 1, 0x60),
        (1, &[1, 0x60, 0, 1, 0x70], 4, 0x7f),
        (7, &[1, 0, 1, 0], 2, 0),
    ] {
        bad_payload(
            id,
            payload,
            index,
            ErrorKind::UnexpectedByte {
                expected,
                found: payload[index],
            },
        );
    }
}

#[test]
fn rejects_malformed_parameter_type_vectors() {
    use IntegerField::ParameterCount;
    let mut ty = vec![1, 0x60];
    push_u32(&mut ty, u32::MAX as usize);
    bad_payload(
        1,
        &ty,
        2,
        ErrorKind::UnsupportedCount {
            field: ParameterCount,
            count: u32::MAX,
        },
    );
    bad_payload(
        1,
        &[1, 0x60, 0x80],
        3,
        ErrorKind::TruncatedInteger {
            field: ParameterCount,
        },
    );
    bad_payload(
        1,
        &[1, 0x60, 0x80, 0x80, 0x80, 0x80, 0x10],
        6,
        ErrorKind::IntegerOverflow {
            field: ParameterCount,
        },
    );
    bad_payload(
        1,
        &[1, 0x60, 2, 0x7f],
        4,
        ErrorKind::TruncatedByte { expected: 0x7f },
    );
    bad_payload(
        1,
        &[1, 0x60, 2, 0x7e, 0x7b, 1, 0x7f],
        4,
        ErrorKind::UnexpectedByte {
            expected: 0x7f,
            found: 0x7b,
        },
    );
}

#[test]
fn requires_each_standard_section_once_in_order_except_optional_export() {
    for missing in [1, 3, 10] {
        let sections: Vec<_> = SECTIONS
            .into_iter()
            .filter(|&(id, _)| id != missing)
            .collect();
        let bytes = module(&sections);
        expect_error(
            &bytes,
            bytes.len(),
            ErrorKind::MissingSection { id: missing },
        );
    }
    for (i, (id, payload)) in SECTIONS.into_iter().enumerate() {
        let mut bytes = module(&SECTIONS[..=i]);
        push_section(&mut bytes, 0, &[0]);
        let duplicate = bytes.len();
        push_section(&mut bytes, id, payload);
        expect_error(&bytes, duplicate, ErrorKind::SectionOutOfOrder { id });
    }
    for i in 0..SECTIONS.len() - 1 {
        let mut sections = SECTIONS;
        sections.swap(i, i + 1);
        let offset = module(&sections[..=i]).len();
        expect_error(
            &module(&sections),
            offset,
            ErrorKind::SectionOutOfOrder {
                id: sections[i + 1].0,
            },
        );
    }
}

#[test]
fn rejects_every_proper_prefix_of_the_demo_except_the_valid_empty_module() {
    let bytes = module(&SECTIONS);
    for end in 0..bytes.len() {
        if end != HEADER.len() {
            assert!(
                validate_module(&bytes[..end]).is_err(),
                "accepted {end}-byte prefix"
            );
        }
    }
}

#[test]
fn respects_section_and_body_boundaries_and_rejects_leftover_bytes() {
    use IntegerField::*;
    for (opcode, field) in [
        (0x41, I32Const),
        (0x20, LocalIndex),
        (0x21, LocalIndex),
        (0x22, LocalIndex),
    ] {
        for count in 0..=4 {
            let body = [&[0, opcode][..], &[0x80; 4][..count]].concat();
            let mut code = code_body(&body);
            // The apparent terminating LEB byte is in the section, but not in this body.
            let boundary = code.len();
            code.extend([0, 0x0b]);
            bad_typed_code(
                &i32_type(1),
                &code,
                boundary,
                ErrorKind::TruncatedInteger { field },
            );
        }
    }
    bad_payload(
        3,
        &[1, 0x80],
        2,
        ErrorKind::TruncatedInteger { field: TypeIndex },
    );
    bad_payload(
        10,
        &[1, 0x80],
        2,
        ErrorKind::TruncatedLength {
            field: LengthField::CodeBodySize,
        },
    );
    bad_payload(
        10,
        &[1, 5, 0, 0x41, 42, 0x0b],
        6,
        ErrorKind::TruncatedBody {
            declared: 5,
            available: 4,
        },
    );
    bad_payload(
        10,
        &[1, 3, 0, 0x41, 42, 0x0b],
        5,
        ErrorKind::TruncatedByte { expected: 0x0b },
    );
    bad_payload(
        10,
        &[1, 5, 0, 0x41, 42, 0x0b, 0x0b],
        6,
        ErrorKind::TrailingBodyBytes,
    );
    // A second balance-preserving operation cannot continue past the first end.
    bad_body(
        &[0, 0x41, 40, 0x41, 2, 0x6a, 0x0b, 0x41, 1, 0x6b, 0x0b],
        7,
        ErrorKind::TrailingBodyBytes,
    );
    for (id, payload) in SECTIONS {
        let trailing = [payload, &[0]].concat();
        bad_payload(
            id,
            &trailing,
            payload.len(),
            ErrorKind::TrailingSectionBytes { id },
        );
    }
}

#[test]
fn rejects_invalid_suffixes_even_after_a_complete_valid_function() {
    let complete = module(&SECTIONS);
    for (id, kind) in [
        (2, ErrorKind::UnsupportedSection { id: 2 }),
        (8, ErrorKind::SectionOutOfOrder { id: 8 }),
    ] {
        let mut bytes = complete.clone();
        push_section(&mut bytes, id, &[0]);
        expect_error(&bytes, complete.len(), kind);
    }
    let mut bytes = complete.clone();
    let custom = push_section(&mut bytes, 0, &[1, 0xff]);
    expect_error(&bytes, custom + 1, ErrorKind::InvalidUtf8Name);
    expect_error(
        &[complete.as_slice(), &[0xff]].concat(),
        complete.len(),
        ErrorKind::UnknownSectionId { id: 0xff },
    );
}
