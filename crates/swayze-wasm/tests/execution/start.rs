use super::{code_bodies, expect_error, function_exports, module, push_section};
use swayze_wasm::{
    ErrorKind, ExecutionLimit, InstantiationError, IntegerField, InvokeError, ResultKind, Trap,
    validate_module,
};

const VOID: &[u8] = &[1, 0x60, 0, 0];
const I32: &[u8] = &[1, 0x60, 0, 1, 0x7f];
const ONE_FUNCTION: &[u8] = &[1, 0];
const EMPTY_BODY: &[u8] = &[0, 0x0b];
const TRAP_BODY: &[u8] = &[0, 0x00, 0x0b];
const FUNCTION_INDEX: IntegerField = IntegerField::FunctionIndex;

#[test]
fn accepts_padded_start_index_in_an_exact_frame() {
    let mut bytes = module(&[(1, VOID), (3, &[2, 0, 0])]);
    // Both the section size (5) and the function index (1) have legal padding.
    bytes.extend([8, 0x85, 0, 0x81, 0x80, 0x80, 0x80, 0]);
    push_section(&mut bytes, 0, &[0]);
    push_section(&mut bytes, 10, &code_bodies(&[TRAP_BODY, EMPTY_BODY]));
    let instance_module = validate_module(&bytes).unwrap();
    instance_module.instantiate().unwrap();
}

#[test]
fn rejects_malformed_out_of_bounds_and_trailing_start_indices_before_signature() {
    // Function 0 has the wrong start signature, but index and frame errors win.
    let prefix = module(&[(1, I32), (3, ONE_FUNCTION)]);
    let check = |payload: &[u8], relative, kind| {
        let mut bytes = prefix.clone();
        let start = push_section(&mut bytes, 8, payload);
        push_section(&mut bytes, 0, &[0]); // Cannot supply a byte to the start frame.
        push_section(&mut bytes, 10, &code_bodies(&[TRAP_BODY]));
        expect_error(&bytes, start + relative, kind);
    };
    for len in 0..=4 {
        check(
            &[0x80; 4][..len],
            len,
            ErrorKind::TruncatedInteger {
                field: FUNCTION_INDEX,
            },
        );
    }
    for fifth in [0x10, 0x80] {
        check(
            &[0x80, 0x80, 0x80, 0x80, fifth, 0],
            4,
            ErrorKind::IntegerOverflow {
                field: FUNCTION_INDEX,
            },
        );
    }
    for (payload, index) in [
        (&[0x81, 0, 0][..], 1),
        (&[0xff, 0xff, 0xff, 0xff, 0x0f], u32::MAX),
    ] {
        check(
            payload,
            0,
            ErrorKind::InvalidIndex {
                field: FUNCTION_INDEX,
                index,
            },
        );
    }
    check(&[0x80, 0, 0], 2, ErrorKind::TrailingSectionBytes { id: 8 });
}

#[test]
fn rejects_duplicate_or_out_of_order_start_and_missing_declarations() {
    let index = &[0];
    let code = code_bodies(&[EMPTY_BODY]);
    let prefix = module(&[(1, VOID), (3, ONE_FUNCTION)]);
    for (earlier, earlier_payload, id, payload) in [
        (8, index.as_slice(), 8, &[][..]), // Order is checked before the payload.
        (8, index, 7, &[0]),
        (9, &[0], 8, index),
        (10, &code, 8, index),
    ] {
        let mut bytes = prefix.clone();
        push_section(&mut bytes, earlier, earlier_payload);
        push_section(&mut bytes, 0, &[0]);
        let at = bytes.len();
        push_section(&mut bytes, id, payload);
        expect_error(&bytes, at, ErrorKind::SectionOutOfOrder { id });
    }
    let mut bytes = module(&[(1, VOID)]);
    let at = push_section(&mut bytes, 8, index);
    expect_error(
        &bytes,
        at,
        ErrorKind::InvalidIndex {
            field: FUNCTION_INDEX,
            index: 0,
        },
    );
    let bytes = module(&[(1, VOID), (3, ONE_FUNCTION), (8, index)]);
    expect_error(&bytes, bytes.len(), ErrorKind::MissingSection { id: 10 });
    let bytes = module(&[
        (3, ONE_FUNCTION),
        (8, index),
        (10, &code_bodies(&[TRAP_BODY])),
    ]);
    expect_error(&bytes, bytes.len(), ErrorKind::MissingSection { id: 1 });
}

#[test]
fn reports_start_signature_at_the_start_of_the_padded_index() {
    let code = code_bodies(&[TRAP_BODY]);
    for (ty, parameters, result) in [
        (&[1, 0x60, 1, 0x7f, 0][..], 1, ResultKind::Void),
        (I32, 0, ResultKind::I32),
    ] {
        let mut bytes = module(&[(1, ty), (3, ONE_FUNCTION)]);
        let at = push_section(&mut bytes, 8, &[0x80, 0]);
        let code_at = bytes.len();
        push_section(&mut bytes, 10, &code);
        expect_error(
            &bytes,
            at,
            ErrorKind::InvalidStartSignature { parameters, result },
        );
        bytes.truncate(code_at);
        let bad_code = code_bodies(&[&[0, 0xff, 0x0b]]);
        let at = push_section(&mut bytes, 10, &bad_code);
        expect_error(
            &bytes,
            at + 3,
            ErrorKind::UnsupportedOpcode { opcode: 0xff },
        );
    }
}

#[test]
fn private_or_exported_start_runs_once_after_initialization_on_each_instance() {
    let types = [2, 0x60, 0, 1, 0x7f, 0x60, 0, 0];
    let functions = [4, 0, 0, 1, 1];
    let code = code_bodies(&[
        b"\0\x23\0\x0b",         // Read global 0.
        b"\0\x41\0\x2d\0\0\x0b", // Read memory[0].
        // Add global 0 to memory[0], then store the sum back into both.
        b"\0\x23\0\x41\0\x2d\0\0\x6a\x24\0\x41\0\x23\0\x3a\0\0\x0b",
        b"\0\x41\0\x11\x01\0\x0b", // Start: call the function at table[0] as void.
    ]);
    for step_index in [2, 3] {
        let exports = function_exports(&[(b"global", 0), (b"memory", 1), (b"step", step_index)]);
        let bytes = module(&[
            (1, &types),
            (3, &functions),
            (4, &[1, 0x70, 0, 1]),             // One-entry funcref table.
            (5, &[1, 0, 1]),                   // One page.
            (6, &[1, 0x7f, 1, 0x41, 4, 0x0b]), // Mutable global initialized to 4.
            (7, &exports),
            (8, &[3]),
            (9, &[1, 0, 0x41, 0, 0x0b, 1, 2]), // table[0] = function 2.
            (10, &code),
            (11, &[1, 0, 0x41, 0, 0x0b, 1, 5]), // memory[0] = 5.
        ]);
        let validated = validate_module(&bytes).unwrap();
        let first = validated.instantiate().unwrap();
        assert_eq!(first.invoke_i32("global"), Ok(9));
        assert_eq!(first.invoke_i32("memory"), Ok(9));
        assert_eq!(first.invoke_i32("global"), Ok(9));
        assert_eq!(first.invoke_void("start"), Err(InvokeError::ExportNotFound));
        assert_eq!(first.invoke_void("step"), Ok(()));
        let second = validated.instantiate().unwrap();
        assert_eq!(first.invoke_i32("global"), Ok(18));
        assert_eq!(first.invoke_i32("memory"), Ok(18));
        assert_eq!(second.invoke_i32("global"), Ok(9));
        assert_eq!(second.invoke_i32("memory"), Ok(9));
    }
}

#[test]
fn preflight_errors_take_precedence_over_a_start_trap() {
    let code = code_bodies(&[TRAP_BODY]);
    for (elements, data, error) in [
        (
            true,
            true,
            InstantiationError::ElementSegmentOutOfBounds { segment_index: 0 },
        ),
        (
            false,
            true,
            InstantiationError::DataSegmentOutOfBounds { segment_index: 0 },
        ),
        (
            false,
            false,
            InstantiationError::StartTrap(Trap::Unreachable),
        ),
    ] {
        let bytes = module(&[
            (1, VOID),
            (3, ONE_FUNCTION),
            (4, &[1, 0x70, 0, 0]), // Zero-entry table.
            (5, &[1, 0, 0]),       // Zero-page memory.
            (8, &[0]),
            (
                9,
                if elements {
                    &[1, 0, 0x41, 0, 0x0b, 1, 0]
                } else {
                    &[0]
                },
            ),
            (10, &code),
            (
                11,
                if data {
                    &[1, 0, 0x41, 0, 0x0b, 1, 1]
                } else {
                    &[0]
                },
            ),
        ]);
        let validated = validate_module(&bytes).unwrap();
        assert_eq!(validated.instantiate().unwrap_err(), error);
    }
}

#[test]
fn a_trapping_start_discards_stores_to_the_unpublished_instance() {
    let code = code_bodies(&[
        b"\0\x10\x01\x0b", // Start calls the private callee.
        // Return early if memory[0] is nonzero; otherwise set it and divide by zero.
        b"\0\x41\0\x2d\0\0\x04\x40\x0f\x0b\x41\0\x41\x01\x3a\0\0\x41\x01\x41\0\x6d\x1a\x0b",
    ]);
    let bytes = module(&[
        (1, VOID),
        (3, &[2, 0, 0]),
        (5, &[1, 0, 1]),
        (8, &[0]),
        (10, &code),
    ]);
    let validated = validate_module(&bytes).unwrap();
    for _ in 0..2 {
        let error = validated.instantiate().unwrap_err();
        assert_eq!(
            error,
            InstantiationError::StartTrap(Trap::IntegerDivideByZero)
        );
        assert_eq!(
            std::error::Error::source(&error).and_then(|source| source.downcast_ref::<Trap>()),
            Some(&Trap::IntegerDivideByZero),
        );
    }
}

fn start_calling_helper(entry: &[u8], helper: &[u8]) -> swayze_wasm::Module {
    let bytes = module(&[
        (1, &[2, 0x60, 0, 0, 0x60, 1, 0x7f, 0]),
        (3, &[2, 0, 1]),
        (7, &function_exports(&[(b"again", 0)])),
        (8, &[0]),
        (10, &code_bodies(&[entry, helper])),
    ]);
    validate_module(&bytes).unwrap()
}

#[test]
fn start_shares_dispatches_with_callees_and_leaves_later_invocations_fresh() {
    let limit = ExecutionLimit::InstructionDispatches;
    assert_eq!(limit.maximum(), 1024);
    // Five dispatches per decrement, including the final untaken br_if.
    let countdown = b"\0\x03\x40\x20\0\x41\x01\x6b\x22\0\x0d\0\x0b\x0b";
    // Two nops, const 204, call, then 204 * 5 dispatches: exactly 1024.
    let exact = b"\0\x01\x01\x41\xcc\x01\x10\x01\x0b";
    let validated = start_calling_helper(exact, countdown);
    let instance = validated.instantiate().unwrap();
    assert_eq!(instance.invoke_void("again"), Ok(()));

    // Appending unreachable would trap if another dispatch remained.
    let over = b"\0\x01\x01\x41\xcc\x01\x10\x01\x00\x0b";
    let trap = b"\0\x01\x01\x41\xcb\x01\x10\x01\x00\x0b"; // 203 iterations.
    for (body, error) in [
        (
            over.as_slice(),
            InstantiationError::StartLimitExceeded { limit },
        ),
        (trap, InstantiationError::StartTrap(Trap::Unreachable)),
    ] {
        let validated = start_calling_helper(body, countdown);
        assert_eq!(validated.instantiate().unwrap_err(), error);
    }
}

#[test]
fn start_counts_toward_the_live_frame_limit() {
    let limit = ExecutionLimit::CallDepth;
    let recurse = b"\0\x20\0\x41\x01\x6b\x22\0\x04\x40\x20\0\x10\x01\x0b\x0b";
    let entry = |n| [0, 0x41, n, 0x10, 1, 0x0b];
    let bound = u8::try_from(limit.maximum()).unwrap();
    start_calling_helper(&entry(bound - 1), recurse)
        .instantiate()
        .unwrap();
    assert_eq!(
        start_calling_helper(&entry(bound), recurse)
            .instantiate()
            .unwrap_err(),
        InstantiationError::StartLimitExceeded { limit },
    );
}
