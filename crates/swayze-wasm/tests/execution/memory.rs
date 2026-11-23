use super::*;

#[path = "data.rs"]
mod data;

const PAGE: i32 = 64 * 1024;
const ONE_PAGE: &[u8] = &[1, 0, 1];
const TWO_BOUNDS: &[u8] = &[1, 1, 1, 1];
const LOAD: u8 = 0x28;
const STORE: u8 = 0x36;
const READ: &[u8] = &[0, 0x20, 0, LOAD, 2, 0, 0x0b];
const WRITE: &[u8] = &[0, 0x20, 0, 0x20, 1, STORE, 2, 0, 0x41, 42, 0x0b];
const OOB: InvokeError = InvokeError::Trap(Trap::MemoryOutOfBounds);

fn memory_functions(
    memory: &[u8],
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
        (5, memory),
        (7, &function_exports(exports)),
        (10, &code_bodies(bodies)),
    ]))
    .unwrap()
}

fn bad_memory_body(memory: &[u8], body: &[u8], offset: usize, kind: ErrorKind) {
    // Place a valid body immediately after the invalid one so a truncated
    // immediate cannot be completed by reading into the rest of the section.
    let mut bytes = module(&[(1, TYPE), (3, TWO_FUNCTIONS), (5, memory)]);
    let start = push_section(&mut bytes, 10, &code_bodies(&[body, &[0, 0x41, 0, 0x0b]]));
    assert!(body.len() < 128);
    expect_error(&bytes, start + 2 + offset, kind);
}

#[test]
fn accepts_memory_only_and_empty_memory_sections_and_enforces_order() {
    for payload in [
        &[0][..],
        &[1, 0, 0],
        ONE_PAGE,
        &[1, 1, 0, 0],
        &[1, 1, 0, 1],
        TWO_BOUNDS,
        // Count 1, minimum 0, maximum 1 with legal five-byte uLEB32 padding.
        &[
            0x81, 0x80, 0x80, 0x80, 0, 1, 0x80, 0x80, 0x80, 0x80, 0, 0x81, 0x80, 0x80, 0x80, 0,
        ],
    ] {
        let bytes = module(&[(0, &[0]), (5, payload), (0, &[0])]);
        let validated = validate_module(&bytes).unwrap();
        assert_eq!(
            validated.instantiate().unwrap().invoke_i32(""),
            Err(InvokeError::ExportNotFound)
        );
    }

    for preceding in [
        &[(5, &[0][..])][..],
        &[(1, TYPE), (3, FUNCTION), (7, EXPORT)][..],
        &[(1, TYPE), (3, FUNCTION), (10, CODE)][..],
    ] {
        let mut bytes = module(preceding);
        let id_offset = bytes.len();
        push_section(&mut bytes, 5, ONE_PAGE);
        expect_error(&bytes, id_offset, ErrorKind::SectionOutOfOrder { id: 5 });
    }
    let mut bytes = module(&[(1, TYPE), (5, ONE_PAGE)]);
    let id_offset = bytes.len();
    push_section(&mut bytes, 3, FUNCTION);
    expect_error(&bytes, id_offset, ErrorKind::SectionOutOfOrder { id: 3 });

    let incomplete = module(&[(1, TYPE), (5, ONE_PAGE)]);
    expect_error(
        &incomplete,
        incomplete.len(),
        ErrorKind::MissingSection { id: 3 },
    );
}

#[test]
fn rejects_bad_limits_and_checks_the_entire_memory_section() {
    use IntegerField::{MemoryCount, MemoryMaximum, MemoryMinimum};

    bad_payload(
        5,
        &[2],
        0,
        ErrorKind::UnsupportedCount {
            field: MemoryCount,
            count: 2,
        },
    );
    bad_payload(5, &[1], 1, ErrorKind::TruncatedMemoryLimitsFlags);
    for flags in [2, 3, 4, 5, 0x80, 0x81, 0xff] {
        bad_payload(
            5,
            &[1, flags, 0, 0],
            1,
            ErrorKind::UnsupportedMemoryLimitsFlags { flags },
        );
    }
    for (prefix, field) in [
        (&[][..], MemoryCount),
        (&[1, 0][..], MemoryMinimum),
        (&[1, 1, 0][..], MemoryMaximum),
    ] {
        let mut truncated = prefix.to_vec();
        for n in 0..=4 {
            bad_payload(
                5,
                &truncated,
                prefix.len() + n,
                ErrorKind::TruncatedInteger { field },
            );
            truncated.push(0x80);
        }
        for fifth in [0x10, 0x80] {
            let too_long = [prefix, &[0x80; 4], &[fifth, 0]].concat();
            bad_payload(
                5,
                &too_long,
                prefix.len() + 4,
                ErrorKind::IntegerOverflow { field },
            );
        }
        let oversized = [prefix, &[0xff, 0xff, 0xff, 0xff, 0x0f]].concat();
        bad_payload(
            5,
            &oversized,
            prefix.len(),
            ErrorKind::UnsupportedCount {
                field,
                count: u32::MAX,
            },
        );
    }
    bad_payload(
        5,
        &[1, 1, 0, 2],
        3,
        ErrorKind::UnsupportedCount {
            field: MemoryMaximum,
            count: 2,
        },
    );
    bad_payload(
        5,
        &[1, 1, 1, 0],
        3,
        ErrorKind::InvalidMemoryLimits {
            minimum: 1,
            maximum: 0,
        },
    );
    bad_payload(5, &[0, 0], 1, ErrorKind::TrailingSectionBytes { id: 5 });
    bad_payload(
        5,
        &[1, 0, 1, 0],
        3,
        ErrorKind::TrailingSectionBytes { id: 5 },
    );
}

#[test]
fn rejects_memory_exports_and_other_memory_features() {
    let mut bytes = module(&[(1, TYPE), (3, FUNCTION), (5, ONE_PAGE)]);
    let start = push_section(&mut bytes, 7, b"\x01\x01m\x02\x00");
    expect_error(
        &bytes,
        start + 3,
        ErrorKind::UnexpectedByte {
            expected: 0,
            found: 2,
        },
    );
    // Import, start and data count are rejected even alongside a defined memory.
    for id in [2, 8, 12] {
        let mut bytes = HEADER.to_vec();
        if id > 5 {
            push_section(&mut bytes, 5, ONE_PAGE);
        }
        let id_offset = bytes.len();
        push_section(&mut bytes, id, &[0]);
        if id < 5 {
            push_section(&mut bytes, 5, ONE_PAGE);
        }
        expect_error(&bytes, id_offset, ErrorKind::UnsupportedSection { id });
    }
    for opcode in [0x3f, 0x40, 0xfc] {
        bad_memory_body(
            ONE_PAGE,
            &[0, 0x41, 0, opcode, 0, 0x0b],
            3,
            ErrorKind::UnsupportedOpcode { opcode },
        );
    }
}

#[test]
fn checks_memory_presence_and_framed_memarg_immediates() {
    use IntegerField::{MemoryAlignment, MemoryOffset};

    for opcode in [LOAD, STORE] {
        let body = [0, 0x41, 0, 0x41, 0, opcode, 0, 0, 0x0b];
        bad_body(&body, 5, ErrorKind::MissingMemory);
        bad_memory_body(&[0], &body, 5, ErrorKind::MissingMemory);

        for (prefix, field) in [(&body[..6], MemoryAlignment), (&body[..7], MemoryOffset)] {
            let mut truncated = prefix.to_vec();
            for n in 0..=4 {
                bad_memory_body(
                    ONE_PAGE,
                    &truncated,
                    prefix.len() + n,
                    ErrorKind::TruncatedInteger { field },
                );
                truncated.push(0x80);
            }
            for fifth in [0x10, 0x80] {
                let too_long = [prefix, &[0x80; 4], &[fifth, 0, 0x0b]].concat();
                bad_memory_body(
                    ONE_PAGE,
                    &too_long,
                    prefix.len() + 4,
                    ErrorKind::IntegerOverflow { field },
                );
            }
        }
        let mut bad_alignment = body;
        bad_alignment[6] = 3;
        bad_memory_body(
            ONE_PAGE,
            &bad_alignment,
            6,
            ErrorKind::InvalidMemoryAlignment { exponent: 3 },
        );
        let max_exponent = [&body[..6], &[0xff, 0xff, 0xff, 0xff, 0x0f], &body[7..]].concat();
        bad_memory_body(
            ONE_PAGE,
            &max_exponent,
            6,
            ErrorKind::InvalidMemoryAlignment { exponent: u32::MAX },
        );
    }
}

#[test]
fn checks_load_and_store_stack_effects_above_each_control_floor() {
    const BLOCK: &[u8] = &[0, 0x41, 0, 0x02, 0x7f];
    const LOOP: &[u8] = &[0, 0x41, 0, 0x03, 0x40];
    // The then arm is untaken, the else arm is untaken in the next case.
    const THEN: &[u8] = &[0, 0x41, 0, 0x41, 0, 0x04, 0x7f];
    const ELSE: &[u8] = &[0, 0x41, 0, 0x41, 1, 0x04, 0x7f, 0x41, 0, 0x05];
    for prefix in [&[0][..], BLOCK, LOOP, THEN, ELSE] {
        for (code, depth) in [
            (&[LOAD, 0, 0][..], 0),
            (&[STORE, 0, 0][..], 0),
            (&[0x41, 1, STORE, 0, 0][..], 1),
        ] {
            let body = [prefix, code].concat();
            bad_memory_body(
                ONE_PAGE,
                &body,
                body.len() - 3,
                ErrorKind::StackUnderflow { depth },
            );
        }
    }
    // A successful store has no result.
    bad_memory_body(
        ONE_PAGE,
        &[0, 0x41, 0, 0x41, 1, STORE, 0, 0, 0x0b],
        8,
        ErrorKind::InvalidResultDepth { depth: 0 },
    );
}

#[test]
fn validates_an_unexported_body_and_its_tail_even_after_a_trapping_load() {
    let mut bytes = module(&[(1, TYPE), (3, TWO_FUNCTIONS), (5, &[1, 0, 0]), (7, EXPORT)]);
    let malformed = &[0, 0x41, 0, LOAD, 0, 0, 0x0b, 0];
    let start = push_section(
        &mut bytes,
        10,
        &code_bodies(&[&[0, 0x41, 0, 0x0b], malformed]),
    );
    expect_error(&bytes, start + 7 + 7, ErrorKind::TrailingBodyBytes);
}

#[test]
fn a_defined_zero_page_memory_validates_but_every_access_traps() {
    for memory in [&[1, 0, 0][..], &[1, 1, 0, 0], &[1, 1, 0, 1]] {
        let module = memory_functions(
            memory,
            &[1, 2],
            &[0, 1],
            &[(b"read", 0), (b"write", 1)],
            &[READ, WRITE],
        );
        let instance = module.instantiate().unwrap();
        assert_eq!(instance.invoke_i32_with_args("read", &[0]), Err(OOB));
        assert_eq!(instance.invoke_i32_with_args("write", &[0, 42]), Err(OOB));
    }
}

#[test]
fn memory_is_zeroed_and_access_is_unaligned_little_endian_with_offsets() {
    // `read_offset` uses five-byte padded alignment=1 and offset=1.
    let read_offset = &[
        0, 0x20, 0, LOAD, 0x81, 0x80, 0x80, 0x80, 0, 0x81, 0x80, 0x80, 0x80, 0, 0x0b,
    ];
    let write_offset = &[0, 0x20, 0, 0x20, 1, STORE, 0, 1, 0x41, 42, 0x0b];
    let module = memory_functions(
        TWO_BOUNDS,
        &[1, 2],
        &[0, 1, 0, 1],
        &[
            (b"read", 0),
            (b"write", 1),
            (b"read_offset", 2),
            (b"write_offset", 3),
        ],
        &[READ, WRITE, read_offset, write_offset],
    );
    let instance = module.instantiate().unwrap();
    let read = |address| instance.invoke_i32_with_args("read", &[address]);
    assert_eq!(read(0), Ok(0));
    assert_eq!(read(PAGE - 4), Ok(0));
    let value = 0xfedc_ba98u32 as i32;
    assert_eq!(instance.invoke_i32_with_args("write", &[1, value]), Ok(42));
    // Values at overlapping addresses distinguish byte order, not just round-trip.
    assert_eq!(read(0), Ok(0xdcba_9800u32 as i32));
    assert_eq!(read(2), Ok(0x00fe_dcba));
    assert_eq!(
        instance.invoke_i32_with_args("read_offset", &[0]),
        Ok(value)
    );
    assert_eq!(
        instance.invoke_i32_with_args("write_offset", &[2, 0x4433_2211]),
        Ok(42)
    );
    assert_eq!(read(1), Ok(0x2211_ba98));
    assert_eq!(read(4), Ok(0x0044_3322));
}

#[test]
fn checks_the_last_word_and_never_writes_part_of_an_out_of_bounds_store() {
    let module = memory_functions(
        ONE_PAGE,
        &[1, 2],
        &[0, 1],
        &[(b"read", 0), (b"write", 1)],
        &[READ, WRITE],
    );
    let instance = module.instantiate().unwrap();
    let read = |address| instance.invoke_i32_with_args("read", &[address]);
    let write = |address, value| instance.invoke_i32_with_args("write", &[address, value]);
    let last = PAGE - 4;
    assert_eq!(write(last, 0x1122_3344), Ok(42));
    for address in last + 1..=PAGE {
        assert_eq!(read(address), Err(OOB));
        assert_eq!(write(address, -1), Err(OOB));
        assert_eq!(read(last), Ok(0x1122_3344));
    }
}

#[test]
fn effective_addresses_do_not_wrap_at_32_bits() {
    let read_plus_1 = &[0, 0x20, 0, LOAD, 2, 1, 0x0b];
    let write_plus_1 = &[0, 0x20, 0, 0x20, 1, STORE, 2, 1, 0x41, 42, 0x0b];
    let read_max = &[0, 0x20, 0, LOAD, 2, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x0b];
    let write_max = &[
        0, 0x20, 0, 0x20, 1, STORE, 2, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x41, 42, 0x0b,
    ];
    let module = memory_functions(
        ONE_PAGE,
        &[1, 2],
        &[0, 1, 0, 1, 0],
        &[
            (b"read1", 0),
            (b"write1", 1),
            (b"read_max", 2),
            (b"write_max", 3),
            (b"read", 4),
        ],
        &[read_plus_1, write_plus_1, read_max, write_max, READ],
    );
    let instance = module.instantiate().unwrap();
    // Each sum would hit an in-bounds address if base+offset were wrapping u32.
    // For (-1, 1), adding the base as a signed i32 would also get this wrong.
    for (read, write, base) in [
        ("read1", "write1", -1),
        ("read_max", "write_max", 1),
        ("read_max", "write_max", PAGE),
    ] {
        assert_eq!(instance.invoke_i32_with_args(read, &[base]), Err(OOB));
        assert_eq!(instance.invoke_i32_with_args(write, &[base, 123]), Err(OOB));
    }
    assert_eq!(instance.invoke_i32_with_args("read", &[0]), Ok(0));
    assert_eq!(instance.invoke_i32_with_args("read", &[PAGE - 4]), Ok(0));
    assert_eq!(instance.invoke_i32_with_args("read1", &[PAGE - 5]), Ok(0));
    assert_eq!(
        instance.invoke_i32_with_args("read1", &[PAGE - 4]),
        Err(OOB)
    );
}

#[test]
fn instances_are_independent_but_exports_and_callees_share_one_instances_memory() {
    let call_then_read = &[
        0, 0x20, 0, 0x20, 1, 0x10, 1, // unexported writer returns 42
        0x20, 0, LOAD, 2, 0, 0x6a, 0x0b, // caller reads the write and adds the two results
    ];
    let module = memory_functions(
        ONE_PAGE,
        &[1, 2],
        &[0, 1, 1],
        &[(b"read", 0), (b"call_then_read", 2)],
        &[READ, WRITE, call_then_read],
    );
    let a = module.instantiate().unwrap();
    let b = module.instantiate().unwrap();
    assert_eq!(
        a.invoke_i32_with_args("call_then_read", &[3, 1000]),
        Ok(1042)
    );
    assert_eq!(b.invoke_i32_with_args("read", &[3]), Ok(0));
    assert_eq!(b.invoke_i32_with_args("call_then_read", &[3, 500]), Ok(542));
    assert_eq!(a.invoke_i32_with_args("read", &[3]), Ok(1000));
    assert_eq!(b.invoke_i32_with_args("read", &[3]), Ok(500));
    assert_eq!(
        module
            .instantiate()
            .unwrap()
            .invoke_i32_with_args("read", &[3]),
        Ok(0)
    );
}

#[test]
fn stores_prior_to_a_callee_trap_persist_while_later_stores_are_skipped() {
    let inner = &[
        0, 0x41, 0, 0x41, 12, STORE, 2, 0, // mem[0] = 12
        0x41, 0x7f, LOAD, 2, 0, // load(-1) traps
        0x41, 4, 0x41, 13, STORE, 2, 0, 0x0b, // never stores to mem[4]
    ];
    let outer = &[
        0, 0x41, 8, 0x41, 14, STORE, 2, 0, // mem[8] = 14
        0x10, 1, // call the trapping callee
        0x41, 12, 0x41, 15, STORE, 2, 0, 0x0b, // never stores to mem[12]
    ];
    let module = memory_functions(
        ONE_PAGE,
        &[1, 0],
        &[0, 1, 1],
        &[(b"read", 0), (b"trap", 2)],
        &[READ, inner, outer],
    );
    let instance = module.instantiate().unwrap();
    assert_eq!(instance.invoke_i32("trap"), Err(OOB));
    for (address, value) in [(0, 12), (4, 0), (8, 14), (12, 0)] {
        assert_eq!(instance.invoke_i32_with_args("read", &[address]), Ok(value));
    }
}

#[test]
fn stores_use_the_fixed_dispatch_budget_and_the_rejected_dispatch_does_not_write() {
    let loop_store = &[
        0, 0x20, 0, 0x45, 0x21, 0, // three dispatches; local 0 becomes zero
        0x03, 0x40, // loop
        0x41, 0, 0x20, 0, 0x41, 1, 0x6a, 0x22, 0, STORE, 2,
        0, // store incremented local at mem[0]
        0x41, 1, 0x0d, 0, // continue; eight dispatches per iteration total
        0x0b, 0x41, 0, 0x0b,
    ];
    let module = memory_functions(
        ONE_PAGE,
        &[1],
        &[0, 0],
        &[(b"read", 0), (b"loop_store", 1)],
        &[READ, loop_store],
    );
    let instance = module.instantiate().unwrap();
    assert_eq!(
        instance.invoke_i32_with_args("loop_store", &[1]),
        Err(InvokeError::LimitExceeded {
            limit: ExecutionLimit::InstructionDispatches
        })
    );
    // The next store would be dispatch 3 + 127 * 8 + 6 = 1025.
    assert_eq!(instance.invoke_i32_with_args("read", &[0]), Ok(127));
}
