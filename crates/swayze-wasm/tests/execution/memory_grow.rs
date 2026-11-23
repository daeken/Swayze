use super::*;
use swayze_wasm::InstantiationError;

const GROW: u8 = 0x40;
const ZERO_PAGES: &[u8] = &[1, 0, 0];
const MAX_ZERO: &[u8] = &[1, 1, 0, 0];
const MAX_ONE: &[u8] = &[1, 1, 0, 1];
const GROW_BODY: &[u8] = &[0, 0x20, 0, GROW, 0, 0x0b];
const SIZE_BODY: &[u8] = &[0, SIZE, 0, 0x0b];

#[test]
fn requires_memory_and_literal_zero_even_in_dead_code() {
    for prefix in [
        &[0, 0x41, 1][..],                  // Reachable.
        &[0, 0x00],                         // Polymorphic dead tail.
        &[0, 0x41, 0, 0x04, 0x7f, 0x41, 1], // Untaken then arm.
    ] {
        let operand = prefix.len() + 1;
        let body = [prefix, &[GROW, 0, 0x0b]].concat();
        bad_body(&body, prefix.len(), ErrorKind::MissingMemory);
        bad_memory_body(&[0], &body, prefix.len(), ErrorKind::MissingMemory);
        bad_memory_body(
            ZERO_PAGES,
            &body[..operand],
            operand,
            ErrorKind::TruncatedByte { expected: 0 },
        );
        for found in [1, 0x80, 0xff] {
            bad_memory_body(
                ZERO_PAGES,
                &[prefix, &[GROW, found, 0, 0x0b]].concat(),
                operand,
                ErrorKind::UnexpectedByte { expected: 0, found },
            );
        }
    }

    // A dead frame supplies the delta implicitly, but the result is explicit.
    let body = [0, 0x00, GROW, 0, 0x0b];
    let module = memory_functions(ZERO_PAGES, &[0], &[0], &[(b"dead", 0)], &[&body]);
    let instance = module.instantiate().unwrap();
    assert_eq!(
        instance.invoke_i32("dead"),
        Err(InvokeError::Trap(Trap::Unreachable))
    );
}

#[test]
fn consumes_one_operand_above_the_floor_and_replaces_it_even_at_the_stack_cap() {
    for prefix in [
        &[0][..],
        &[0, 0x41, 1, 0x02, 0x7f],
        &[0, 0x41, 1, 0x03, 0x40],
    ] {
        let body = [prefix, &[GROW, 0, 0x0b]].concat();
        bad_memory_body(
            ZERO_PAGES,
            &body,
            prefix.len(),
            ErrorKind::StackUnderflow { depth: 0 },
        );
    }
    let cap = Limit::OperandStackDepth.maximum();
    let mut body = vec![0];
    body.extend([0x41, 1].repeat(cap));
    body.extend([GROW, 0]);
    body.extend(std::iter::repeat_n(0x6a, cap - 1)); // Sum the returned zero and cap - 1 ones.
    body.push(0x0b);
    let module = memory_functions(
        ZERO_PAGES,
        &[0],
        &[0, 0],
        &[(b"run", 0), (b"size", 1)],
        &[&body, SIZE_BODY],
    );
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("run"), Ok(cap as i32 - 1));
    assert_eq!(instance.invoke_i32("size"), Ok(1));
}

#[test]
fn counts_as_one_decoded_instruction_with_the_reserved_byte_inside_the_body() {
    let limit = Limit::BodyInstructions;
    let body = |nops| {
        let mut body = vec![0, 0x41, 1];
        body.extend(std::iter::repeat_n(0x01, nops));
        body.extend([GROW, 0, 0x0b]);
        body
    };
    let nops = limit.maximum() - 2; // One const and one grow.
    let accepted = memory_functions(ZERO_PAGES, &[0], &[0], &[(b"grow", 0)], &[&body(nops)]);
    assert_eq!(accepted.instantiate().unwrap().invoke_i32("grow"), Ok(0));

    let mut bytes = module(&[(1, TYPE), (3, FUNCTION), (5, ZERO_PAGES)]);
    let code = code_body(&body(nops + 1));
    let start = push_section(&mut bytes, 10, &code);
    expect_error(
        &bytes,
        start + code.len() - 3,
        ErrorKind::LimitExceeded { limit },
    );
}

#[test]
fn growth_observes_the_declared_maximum_and_never_wraps_the_unsigned_delta() {
    for (limits, initial, grows) in [
        (MAX_ZERO, 0, false),
        (ZERO_PAGES, 0, true),
        (MAX_ONE, 0, true),
        (ONE_PAGE, 1, false),
        (TWO_BOUNDS, 1, false),
    ] {
        let module = memory_functions(
            limits,
            &[0, 1, 2],
            &[0, 1, 1, 2],
            &[(b"size", 0), (b"grow", 1), (b"read", 2), (b"write", 3)],
            &[SIZE_BODY, GROW_BODY, READ, WRITE],
        );
        let instance = module.instantiate().unwrap();
        let size = || instance.invoke_i32("size");
        let grow = |delta| instance.invoke_i32_with_args("grow", &[delta]);
        let read = |address| instance.invoke_i32_with_args("read", &[address]);
        let write = |address, value| instance.invoke_i32_with_args("write", &[address, value]);
        assert_eq!(size(), Ok(initial));
        assert_eq!(grow(0), Ok(initial));
        if initial == 0 {
            assert_eq!(read(0), Err(OOB));
        }
        for delta in [2, i32::MAX, i32::MIN, -1] {
            assert_eq!(grow(delta), Ok(-1));
            assert_eq!(size(), Ok(initial));
        }
        assert_eq!(grow(1), Ok(if grows { 0 } else { -1 }));
        let current = initial + i32::from(grows);
        assert_eq!(size(), Ok(current));
        if current == 1 {
            assert_eq!(read(0), Ok(0));
            assert_eq!(read(PAGE - 4), Ok(0));
            assert_eq!(write(PAGE - 4, 0x1234_5678), Ok(42));
        }
        for delta in [1, 2, i32::MAX, i32::MIN, -1] {
            assert_eq!(grow(delta), Ok(-1));
        }
        assert_eq!(grow(0), Ok(current));
        assert_eq!(size(), Ok(current));
        if current == 1 {
            assert_eq!(read(PAGE - 4), Ok(0x1234_5678));
            assert_eq!(read(PAGE - 3), Err(OOB));
        } else {
            assert_eq!(read(0), Err(OOB));
        }
        let fresh = module.instantiate().unwrap();
        assert_eq!(fresh.invoke_i32("size"), Ok(initial));
        assert_eq!(
            fresh.invoke_i32_with_args("read", &[PAGE - 4]),
            if initial == 1 { Ok(0) } else { Err(OOB) }
        );
    }
}

#[test]
fn direct_and_indirect_calls_share_growth_even_if_the_invocation_traps() {
    let code = code_bodies(&[
        GROW_BODY,
        &[0, 0x41, 1, 0x10, 0, SIZE, 0, 0x6a, 0x0b], // old + current after direct call
        &[0, 0x41, 1, 0x41, 0, 0x11, 0, 0, SIZE, 0, 0x6a, 0x0b], // same via table
        &[
            0, 0x10, 2, 0x1a, // call the indirect wrapper and discard its result
            0x41, 0, 0x41, 12, STORE, 2, 0, 0x00, 0x0b,
        ],
        SIZE_BODY,
        READ,
    ]);
    let bytes = module(&[
        (1, &i32_types(&[1, 0])),
        (3, &[6, 0, 1, 1, 1, 1, 0]),
        (4, &[1, 0x70, 0, 1]),
        (5, ZERO_PAGES),
        (
            7,
            &function_exports(&[
                (b"direct", 1),
                (b"indirect", 2),
                (b"trap", 3),
                (b"size", 4),
                (b"read", 5),
            ]),
        ),
        (9, &[1, 0, 0x41, 0, 0x0b, 1, 0]), // table[0] = grow function
        (10, &code),
    ]);
    let module = validate_module(&bytes).unwrap();
    for name in ["direct", "indirect", "trap"] {
        let instance = module.instantiate().unwrap();
        assert_eq!(instance.invoke_i32("size"), Ok(0));
        let (result, stored) = if name == "trap" {
            (Err(InvokeError::Trap(Trap::Unreachable)), 12)
        } else {
            (Ok(1), 0)
        };
        assert_eq!(instance.invoke_i32(name), result);
        assert_eq!(instance.invoke_i32("size"), Ok(1));
        assert_eq!(instance.invoke_i32_with_args("read", &[0]), Ok(stored));
    }
}

#[test]
fn grow_consumes_one_dispatch_and_does_not_run_if_the_budget_is_exhausted() {
    let limit = ExecutionLimit::InstructionDispatches;
    let counter_loop = [
        0, 0x03, 0x40, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0, 0x0d, 0, 0x0b,
    ];
    // The loop dispatches five instructions per iteration, then we prepare
    // the operand and use enough nops to put grow at the exact dispatch cap.
    let iterations = (limit.maximum() - 2) / 5;
    let nops = (limit.maximum() - 2) % 5;
    let body = |extra| {
        let mut body = counter_loop.to_vec();
        body.extend([0x41, 1]);
        body.extend(std::iter::repeat_n(0x01, nops + extra));
        body.extend([GROW, 0, 0x0b]);
        body
    };
    let module = memory_functions(
        ZERO_PAGES,
        &[1, 0],
        &[0, 0, 1],
        &[(b"at_cap", 0), (b"over_cap", 1), (b"size", 2)],
        &[&body(0), &body(1), SIZE_BODY],
    );
    let instance = module.instantiate().unwrap();
    let args = &[iterations as i32];
    assert_eq!(
        instance.invoke_i32_with_args("over_cap", args),
        Err(InvokeError::LimitExceeded { limit })
    );
    assert_eq!(instance.invoke_i32("size"), Ok(0));
    assert_eq!(instance.invoke_i32_with_args("at_cap", args), Ok(0));
    assert_eq!(instance.invoke_i32("size"), Ok(1));
    assert_eq!(instance.invoke_i32_with_args("at_cap", args), Ok(-1));
}

#[test]
fn start_growth_is_visible_but_active_data_must_fit_before_start_dispatch() {
    let start = [
        0, 0x41, 1, GROW, 0, 0x1a, 0x41, 0, 0x41, 42, STORE, 2, 0, 0x0b,
    ];
    let trapping_start = [&start[..start.len() - 1], &[0x00, 0x0b]].concat();
    let with_start = |start: &[u8], data: &[u8]| {
        validate_module(&module(&[
            (
                1,
                &[3, 0x60, 0, 0, 0x60, 0, 1, 0x7f, 0x60, 1, 0x7f, 1, 0x7f],
            ),
            (3, &[3, 0, 1, 2]),
            (5, MAX_ONE),
            (7, &function_exports(&[(b"size", 1), (b"read", 2)])),
            (8, &[0]),
            (10, &code_bodies(&[start, SIZE_BODY, READ])),
            (11, data),
        ]))
        .unwrap()
    };
    let empty_at_zero = &[1, 0, 0x41, 0, 0x0b, 0];
    let module = with_start(&start, empty_at_zero);
    for _ in 0..2 {
        let instance = module.instantiate().unwrap();
        assert_eq!(instance.invoke_i32("size"), Ok(1));
        assert_eq!(instance.invoke_i32_with_args("read", &[0]), Ok(42));
        assert_eq!(instance.invoke_i32_with_args("read", &[PAGE - 4]), Ok(0));
    }
    let module = with_start(&trapping_start, empty_at_zero);
    assert_eq!(
        module.instantiate().unwrap_err(),
        InstantiationError::StartTrap(Trap::Unreachable)
    );
    for data in [&[1, 0, 0x41, 0, 0x0b, 1, 11][..], &[1, 0, 0x41, 1, 0x0b, 0]] {
        // A byte at 0 and an empty segment at offset 1 would both fit after
        // grow, but neither fits in the initial zero pages.
        let module = with_start(&trapping_start, data);
        assert_eq!(
            module.instantiate().unwrap_err(),
            InstantiationError::DataSegmentOutOfBounds { segment_index: 0 }
        );
    }
}
