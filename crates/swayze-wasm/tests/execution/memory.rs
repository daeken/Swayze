use super::*;

#[path = "data.rs"]
mod data;
#[path = "memory_grow.rs"]
mod grow;
#[path = "i64_memory.rs"]
mod i64_memory;

const PAGE: i32 = 64 * 1024;
const ONE_PAGE: &[u8] = &[1, 0, 1];
const TWO_BOUNDS: &[u8] = &[1, 1, 1, 1];
const LOAD: u8 = 0x28;
const STORE: u8 = 0x36;
const LOAD8_S: u8 = 0x2c;
const LOAD8_U: u8 = 0x2d;
const LOAD16_S: u8 = 0x2e;
const LOAD16_U: u8 = 0x2f;
const STORE8: u8 = 0x3a;
const STORE16: u8 = 0x3b;
const SIZE: u8 = 0x3f;
const LOADS: [u8; 5] = [LOAD, LOAD8_S, LOAD8_U, LOAD16_S, LOAD16_U];
const STORES: [u8; 3] = [STORE, STORE8, STORE16];
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

fn all_widths(memory: &[u8]) -> Module {
    memory_functions(
        memory,
        &[1, 2],
        &[0, 1, 0, 0, 0, 0, 1, 1],
        &[
            (b"read", 0),
            (b"write", 1),
            (b"read8_s", 2),
            (b"read8_u", 3),
            (b"read16_s", 4),
            (b"read16_u", 5),
            (b"write8", 6),
            (b"write16", 7),
        ],
        &[
            READ,
            WRITE,
            &[0, 0x20, 0, LOAD8_S, 0, 0, 0x0b],
            &[0, 0x20, 0, LOAD8_U, 0, 0, 0x0b],
            &[0, 0x20, 0, LOAD16_S, 1, 0, 0x0b],
            &[0, 0x20, 0, LOAD16_U, 0, 0, 0x0b],
            &[0, 0x20, 0, 0x20, 1, STORE8, 0, 0, 0x41, 42, 0x0b],
            &[0, 0x20, 0, 0x20, 1, STORE16, 1, 0, 0x41, 42, 0x0b],
        ],
    )
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
    // Import and data count are rejected even alongside a defined memory.
    for id in [2, 12] {
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
    for opcode in [0x2a, 0x2b, 0x38, 0x39, 0xfc] {
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

    for (opcode, natural) in [
        (LOAD, 2),
        (STORE, 2),
        (LOAD8_S, 0),
        (LOAD8_U, 0),
        (STORE8, 0),
        (LOAD16_S, 1),
        (LOAD16_U, 1),
        (STORE16, 1),
    ]
    .into_iter()
    .chain(i64_memory::ops().map(|(opcode, natural, _)| (opcode, natural)))
    {
        let body = [0, 0x41, 0, 0x41, 0, opcode, 0, 0, 0x0b];
        bad_body(&body, 5, ErrorKind::MissingMemory);
        bad_memory_body(&[0], &body, 5, ErrorKind::MissingMemory);
        let dead = [0, 0x00, opcode, natural, 0x80];
        bad_body(
            &dead[..4],
            4,
            ErrorKind::TruncatedInteger {
                field: MemoryOffset,
            },
        );
        bad_memory_body(
            ONE_PAGE,
            &dead,
            5,
            ErrorKind::TruncatedInteger {
                field: MemoryOffset,
            },
        );
        bad_body(
            &[0, 0x00, opcode, natural, 0, 0x0b],
            2,
            ErrorKind::MissingMemory,
        );

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
        bad_alignment[6] = natural + 1;
        bad_memory_body(
            ONE_PAGE,
            &bad_alignment,
            6,
            ErrorKind::InvalidMemoryAlignment {
                exponent: u32::from(natural + 1),
            },
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
        let underflow = |code: &[u8], depth| {
            let body = [prefix, code].concat();
            bad_memory_body(
                ONE_PAGE,
                &body,
                body.len() - 3,
                ErrorKind::StackUnderflow { depth },
            );
        };
        for opcode in LOADS.into_iter().chain(STORES) {
            underflow(&[opcode, 0, 0], 0);
        }
        for opcode in STORES {
            underflow(&[0x41, 1, opcode, 0, 0], 1);
        }
    }
    // A successful store has no result.
    for opcode in STORES {
        bad_memory_body(
            ONE_PAGE,
            &[0, 0x41, 0, 0x41, 1, opcode, 0, 0, 0x0b],
            8,
            ErrorKind::InvalidResultDepth { depth: 0 },
        );
    }
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
        let module = all_widths(memory);
        let instance = module.instantiate().unwrap();
        for name in ["read", "read8_s", "read8_u", "read16_s", "read16_u"] {
            assert_eq!(instance.invoke_i32_with_args(name, &[0]), Err(OOB));
        }
        for name in ["write", "write8", "write16"] {
            assert_eq!(instance.invoke_i32_with_args(name, &[0, 42]), Err(OOB));
        }
    }
}

#[test]
fn memory_size_pushes_the_actual_page_count_not_the_maximum() {
    for (limits, expected) in [
        (&[1, 0, 0][..], 0),
        (&[1, 1, 0, 0][..], 0),
        (&[1, 1, 0, 1][..], 0),
        (ONE_PAGE, 1),
        (TWO_BOUNDS, 1),
    ] {
        let module = memory_functions(limits, &[0], &[0], &[(b"size", 0)], &[&[0, SIZE, 0, 0x0b]]);
        assert_eq!(
            module.instantiate().unwrap().invoke_i32("size"),
            Ok(expected)
        );
    }
}

#[test]
fn memory_size_push_counts_toward_the_operand_limit() {
    let limit = Limit::OperandStackDepth;
    let body = |prefix_depth| {
        let mut body = vec![0];
        body.extend([0x41, 1].repeat(prefix_depth));
        body.extend([SIZE, 0]);
        body.extend(std::iter::repeat_n(0x6a, prefix_depth)); // reduce to one result
        body.push(0x0b);
        body
    };
    let cap = limit.maximum();
    let module = memory_functions(ONE_PAGE, &[0], &[0], &[(b"sum", 0)], &[&body(cap - 1)]);
    assert_eq!(
        module.instantiate().unwrap().invoke_i32("sum"),
        Ok(cap as i32)
    );
    bad_memory_body(
        ONE_PAGE,
        &body(cap),
        1 + 2 * cap,
        ErrorKind::LimitExceeded { limit },
    );
}

#[test]
fn memory_size_requires_defined_memory_and_a_literal_zero_byte_even_in_untaken_code() {
    // The second prefix is the start of an untaken then arm.
    for prefix in [&[0][..], &[0, 0x41, 0, 0x04, 0x7f]] {
        let body = [prefix, &[SIZE, 0]].concat();
        bad_body(&body, prefix.len(), ErrorKind::MissingMemory);
        bad_memory_body(&[0], &body, prefix.len(), ErrorKind::MissingMemory);
        bad_memory_body(
            ONE_PAGE,
            &body[..body.len() - 1],
            prefix.len() + 1,
            ErrorKind::TruncatedByte { expected: 0 },
        );
        // Nonzero indices and the padded LEB32 encoding of zero are invalid.
        for found in [1, 0x80, 0xff] {
            bad_memory_body(
                ONE_PAGE,
                &[prefix, &[SIZE, found, 0, 0x0b]].concat(),
                prefix.len() + 1,
                ErrorKind::UnexpectedByte { expected: 0, found },
            );
        }
    }
}

#[test]
fn narrow_loads_extend_at_sign_boundaries_and_stores_truncate_in_little_endian_order() {
    let module = all_widths(ONE_PAGE);
    let instance = module.instantiate().unwrap();
    let read = |name, address| instance.invoke_i32_with_args(name, &[address]);
    let write = |name, address, value| instance.invoke_i32_with_args(name, &[address, value]);
    assert_eq!(write("write", 0, 0x4433_2211), Ok(42));
    // Address 1 is unaligned for 16 bits even with the natural alignment hint.
    for (value, signed) in [(0x7fff, 32767), (0x8000, -32768), (0xffff, -1)] {
        assert_eq!(write("write16", 1, 0x1234_0000 | value), Ok(42));
        assert_eq!(read("read16_s", 1), Ok(signed));
        assert_eq!(read("read16_u", 1), Ok(value));
        assert_eq!(read("read", 0), Ok(0x4400_0011 | (value << 8)));
        assert_eq!(read("read8_u", 1), Ok(value & 0xff));
        assert_eq!(read("read8_u", 2), Ok(value >> 8));
    }
    for (value, signed) in [(0x7f, 127), (0x80, -128), (0xff, -1)] {
        assert_eq!(write("write8", 2, 0x1234_5600 | value), Ok(42));
        assert_eq!(read("read8_s", 2), Ok(signed));
        assert_eq!(read("read8_u", 2), Ok(value));
        assert_eq!(read("read16_u", 2), Ok(0x4400 | value));
        assert_eq!(read("read", 0), Ok(0x4400_ff11 | (value << 16)));
    }
}

#[test]
fn narrow_accesses_check_their_full_width_at_the_end_of_memory() {
    let module = all_widths(ONE_PAGE);
    let instance = module.instantiate().unwrap();
    let read = |name, address| instance.invoke_i32_with_args(name, &[address]);
    let write = |name, address, value| instance.invoke_i32_with_args(name, &[address, value]);
    assert_eq!(write("write", PAGE - 4, 0x4433_2211), Ok(42));
    assert_eq!(write("write16", PAGE - 2, 0x0123_89ab), Ok(42));
    assert_eq!(read("read16_s", PAGE - 2), Ok(-30293));
    assert_eq!(read("read16_u", PAGE - 2), Ok(0x89ab));
    assert_eq!(write("write8", PAGE - 1, 0x0123_45fe), Ok(42));
    assert_eq!(read("read8_s", PAGE - 1), Ok(-2));
    assert_eq!(read("read8_u", PAGE - 1), Ok(254));
    for name in ["read16_s", "read16_u"] {
        assert_eq!(read(name, PAGE - 1), Err(OOB));
    }
    for name in ["read8_s", "read8_u"] {
        assert_eq!(read(name, PAGE), Err(OOB));
    }
    assert_eq!(write("write16", PAGE - 1, -1), Err(OOB));
    assert_eq!(write("write8", PAGE, -1), Err(OOB));
    assert_eq!(read("read", PAGE - 4), Ok(0xfeab_2211u32 as i32));
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
    for (load, store, width) in [
        (LOAD, STORE, 4),
        (LOAD8_U, STORE8, 1),
        (LOAD16_S, STORE16, 2),
    ] {
        let read_plus_1 = &[0, 0x20, 0, load, 0, 1, 0x0b];
        let write_plus_1 = &[0, 0x20, 0, 0x20, 1, store, 0, 1, 0x41, 42, 0x0b];
        let read_max = &[0, 0x20, 0, load, 0, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x0b];
        let write_max = &[
            0, 0x20, 0, 0x20, 1, store, 0, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x41, 42, 0x0b,
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
        // Each sum would wrap to an in-bounds address in u32. The base -1
        // must also be treated as unsigned before addition.
        for (read, write, base) in [
            ("read1", "write1", -1),
            ("read_max", "write_max", 1),
            ("read_max", "write_max", PAGE - width + 1),
        ] {
            assert_eq!(instance.invoke_i32_with_args(read, &[base]), Err(OOB));
            assert_eq!(instance.invoke_i32_with_args(write, &[base, 123]), Err(OOB));
        }
        assert_eq!(instance.invoke_i32_with_args("read", &[0]), Ok(0));
        assert_eq!(instance.invoke_i32_with_args("read", &[PAGE - 4]), Ok(0));
        let last = PAGE - width - 1;
        assert_eq!(
            instance.invoke_i32_with_args("write1", &[last, 123]),
            Ok(42)
        );
        assert_eq!(instance.invoke_i32_with_args("read1", &[last]), Ok(123));
        assert_eq!(
            instance.invoke_i32_with_args("read1", &[last + 1]),
            Err(OOB)
        );
    }
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
