use super::*;
use crate::i64_operands::push_i64;
use swayze_wasm::ValType::{I32, I64};

const LOAD64: u8 = 0x29;
const STORE64: u8 = 0x37;
const NEED_I32: ErrorKind = ErrorKind::TypeMismatch {
    expected: I32,
    actual: I64,
};
const NEED_I64: ErrorKind = ErrorKind::TypeMismatch {
    expected: I64,
    actual: I32,
};

// Natural alignment exponent, signed/full load, unsigned load, store.
const WIDTHS: [(u8, u8, Option<u8>, u8); 4] = [
    (0, 0x30, Some(0x31), 0x3c),
    (1, 0x32, Some(0x33), 0x3d),
    (2, 0x34, Some(0x35), 0x3e),
    (3, LOAD64, None, STORE64),
];

pub(super) fn ops() -> impl Iterator<Item = (u8, u8, bool)> {
    WIDTHS
        .into_iter()
        .flat_map(|(align, load, unsigned, store)| {
            [(load, align, false), (store, align, true)]
                .into_iter()
                .chain(unsigned.map(|op| (op, align, false)))
        })
}

// The host interface remains i32-only. Reassemble the entire expected/stored
// bit pattern from parameter 1 (low word) and parameter 2 (high word).
const I64_ARG: &[u8] = &[
    0x20, 2, 0xad, 0x42, 32, 0x86, // extend high as unsigned and shift by 32
    0x20, 1, 0xad, 0x84, // extend low as unsigned and OR
];

fn args(address: i32, pattern: i64) -> [i32; 3] {
    [address, pattern as i32, (pattern >> 32) as i32]
}

fn checked_load(opcode: u8, memarg: &[u8]) -> Vec<u8> {
    [&[0, 0x20, 0, opcode], memarg, I64_ARG, &[0x51, 0x0b]].concat()
}

fn write(opcode: u8, memarg: &[u8]) -> Vec<u8> {
    [
        &[0, 0x20, 0][..],
        I64_ARG,
        &[opcode],
        memarg,
        &[0x41, 42, 0x0b],
    ]
    .concat()
}

fn width_module(memory: &[u8], align: u8, load: u8, unsigned: Option<u8>, store: u8) -> Module {
    // All width-specific functions use legal five-byte padding and offset 1.
    let padded = [align | 0x80, 0x80, 0x80, 0x80, 0, 0x81, 0x80, 0x80, 0x80, 0];
    memory_functions(
        memory,
        &[3, 1, 2, 0],
        &[0, 0, 0, 0, 1, 2, 1, 3],
        &[
            (b"signed", 0),
            (b"unsigned", 1),
            (b"write", 2),
            (b"word", 3),
            (b"byte", 4),
            (b"write_i32", 5),
            (b"grow", 6),
            (b"size", 7),
        ],
        &[
            &checked_load(load, &padded),
            &checked_load(unsigned.unwrap_or(load), &padded),
            &write(store, &padded),
            &checked_load(LOAD64, &[3, 0]),
            &[0, 0x20, 0, LOAD8_U, 0, 0, 0x0b],
            WRITE,
            &[0, 0x20, 0, 0x40, 0, 0x0b],
            &[0, SIZE, 0, 0x0b],
        ],
    )
}

#[test]
fn i64_accesses_extend_and_truncate_at_every_width_and_share_bytes_with_i32() {
    for (align, load, unsigned, store) in WIDTHS {
        let width = 1usize << align;
        let module = width_module(ONE_PAGE, align, load, unsigned, store);
        let instance = module.instantiate().unwrap();
        let invoke =
            |name, address, pattern| instance.invoke_i32_with_args(name, &args(address, pattern));
        let pattern = 0x9876_54f2_e1d0_bca9u64 as i64;

        // Effective address 1 is unaligned for widths > 1 despite natural
        // alignment hints. An independent i32 byte reader checks endianness,
        // truncation, and both sides of the store, not just a round trip.
        for offset in [0, 4, 8] {
            assert_eq!(
                instance.invoke_i32_with_args("write_i32", &[offset, 0x1234_5678]),
                Ok(42)
            );
        }
        assert_eq!(invoke("write", 0, pattern), Ok(42));
        let mut bytes = [0x78, 0x56, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0x78, 0x56];
        bytes[1..1 + width].copy_from_slice(&pattern.to_le_bytes()[..width]);
        for (address, expected) in bytes.into_iter().enumerate() {
            assert_eq!(
                instance.invoke_i32_with_args("byte", &[address as i32]),
                Ok(i32::from(expected))
            );
        }
        let shift = 64 - width * 8;
        let signed = (pattern << shift) >> shift;
        let zero_extended = ((pattern as u64) << shift >> shift) as i64;
        assert_eq!(invoke("signed", 0, signed), Ok(1));
        assert_eq!(invoke("unsigned", 0, zero_extended), Ok(1));

        // Overlap with an existing i32 store, independently of every i64
        // store. Check both sides of the sign boundary and all ones using
        // full-width guest i64.eq (not wrapping the load to i32).
        if width <= 4 {
            let sign_bit = 1i64 << (width * 8 - 1);
            for (bits, expected_signed) in [
                (sign_bit - 1, sign_bit - 1),
                (sign_bit, -sign_bit),
                (sign_bit * 2 - 1, -1),
            ] {
                let mask = (sign_bit * 2 - 1) as i32;
                let word = (0xfedc_ba98u32 as i32 & !mask) | bits as i32;
                assert_eq!(
                    instance.invoke_i32_with_args("write_i32", &[1, word]),
                    Ok(42)
                );
                assert_eq!(invoke("signed", 0, expected_signed), Ok(1));
                assert_eq!(invoke("unsigned", 0, bits), Ok(1));
            }
        } else {
            assert_eq!(
                instance.invoke_i32_with_args("write_i32", &[4, 0x1122_3344]),
                Ok(42)
            );
            assert_eq!(invoke("signed", 0, 0x9811_2233_44d0_bca9u64 as i64), Ok(1));
        }
    }
}

#[test]
fn each_i64_access_checks_zero_pages_and_both_sides_of_the_last_valid_address() {
    for (align, load, unsigned, store) in WIDTHS {
        let width = 1 << align;
        for (memory, can_grow) in [(&[1, 1, 0, 0][..], false), (&[1, 1, 0, 1][..], true)] {
            let module = width_module(memory, align, load, unsigned, store);
            let instance = module.instantiate().unwrap();
            let invoke = |name, address, pattern| {
                instance.invoke_i32_with_args(name, &args(address, pattern))
            };
            assert_eq!(instance.invoke_i32("size"), Ok(0));
            for name in ["signed", "unsigned", "write"] {
                assert_eq!(invoke(name, 0, 0), Err(OOB));
            }
            assert_eq!(
                instance.invoke_i32_with_args("grow", &[1]),
                Ok(if can_grow { 0 } else { -1 })
            );
            assert_eq!(instance.invoke_i32("size"), Ok(i32::from(can_grow)));
            if !can_grow {
                for name in ["signed", "unsigned", "write"] {
                    assert_eq!(invoke(name, 0, 0), Err(OOB));
                }
                continue;
            }

            let last = PAGE - width - 1; // memarg adds one
            for name in ["signed", "unsigned"] {
                assert_eq!(invoke(name, last, 0), Ok(1));
            }
            // The first invalid store overlaps the tail by width - 1 bytes.
            // Verify the whole tail after the trap, and guard against wrapping
            // UINT32_MAX + 1 back to address zero.
            assert_eq!(invoke("write", last, 42), Ok(42));
            for name in ["signed", "unsigned"] {
                assert_eq!(invoke(name, last, 42), Ok(1));
            }
            for name in ["signed", "unsigned", "write"] {
                assert_eq!(invoke(name, last + 1, -1), Err(OOB));
                assert_eq!(invoke(name, -1, -1), Err(OOB));
            }
            assert_eq!(invoke("word", PAGE - 8, 42 << (8 * (8 - width))), Ok(1));
            assert_eq!(invoke("word", 0, 0), Ok(1));
            assert_eq!(instance.invoke_i32_with_args("grow", &[1]), Ok(-1));
        }
    }
}

#[test]
fn each_width_checks_then_crosses_the_page_seam_after_growth() {
    for (align, load, unsigned, store) in WIDTHS {
        let width = 1usize << align;
        let module = width_module(&[1, 0, 1], align, load, unsigned, store);
        let instance = module.instantiate().unwrap();
        let invoke =
            |name, address, pattern| instance.invoke_i32_with_args(name, &args(address, pattern));
        let address = PAGE - 2; // Offset 1: effective address PAGE - 1.
        let pattern = 0x9876_54f2_e1d0_bca9u64 as i64;
        // A byte fits in page one, but no wider store may change even that byte.
        if width > 1 {
            assert_eq!(invoke("write", address, -1), Err(OOB));
            for name in ["signed", "unsigned"] {
                assert_eq!(invoke(name, address, -1), Err(OOB));
            }
            assert_eq!(instance.invoke_i32_with_args("byte", &[PAGE - 1]), Ok(0));
        }
        assert_eq!(instance.invoke_i32_with_args("byte", &[PAGE]), Err(OOB));
        assert_eq!(instance.invoke_i32_with_args("grow", &[1]), Ok(1));
        let shift = 64 - width * 8;
        for base in [address, address + 1] {
            assert_eq!(invoke("write", base, pattern), Ok(42));
            assert_eq!(invoke("signed", base, (pattern << shift) >> shift), Ok(1));
            let zero_extended = ((pattern as u64) << shift >> shift) as i64;
            assert_eq!(invoke("unsigned", base, zero_extended), Ok(1));
            for (i, &byte) in pattern.to_le_bytes()[..width].iter().enumerate() {
                assert_eq!(
                    instance.invoke_i32_with_args("byte", &[base + 1 + i as i32]),
                    Ok(i32::from(byte))
                );
            }
        }
        let last = 2 * PAGE - width as i32 - 1;
        assert_eq!(invoke("write", last, pattern), Ok(42));
        assert_eq!(invoke("write", last + 1, -1), Err(OOB));
        assert_eq!(invoke("signed", last, (pattern << shift) >> shift), Ok(1));
    }
}

#[test]
fn max_u32_memarg_offsets_never_wrap_back_into_memory() {
    let max_offset = [0xff, 0xff, 0xff, 0xff, 0x0f];
    for (opcode, align, store) in ops() {
        let memarg = [&[align][..], &max_offset].concat();
        let body = if store {
            write(opcode, &memarg)
        } else {
            checked_load(opcode, &memarg)
        };
        let module = memory_functions(
            ONE_PAGE,
            &[3],
            &[0, 0],
            &[(b"test", 0), (b"word", 1)],
            &[&body, &checked_load(LOAD64, &[3, 0])],
        );
        let instance = module.instantiate().unwrap();
        assert_eq!(
            instance.invoke_i32_with_args("test", &args(1, -1)),
            Err(OOB)
        );
        assert_eq!(instance.invoke_i32_with_args("word", &args(0, 0)), Ok(1));
    }
}

#[test]
fn types_and_underflow_are_checked_above_the_active_floor() {
    for (opcode, align, store) in ops() {
        let memop = [opcode, align, 0];
        let bad = |prefix: &[u8], inputs: &[u8], kind| {
            let body = [prefix, inputs, &memop, &[0x0b]].concat();
            bad_memory_body(ONE_PAGE, &body, prefix.len() + inputs.len(), kind);
        };
        // The block freezes the i32 prefix. `unreachable` makes missing
        // operands implicit, but concrete operands must still be typed.
        for prefix in [
            &[0][..],
            &[0, 0x41, 42, 0x02, 0x40],
            &[0, 0x00],
            &[0, 0x41, 42, 0x02, 0x40, 0x00],
        ] {
            if store {
                bad(prefix, &[0x41, 0, 0x41, 0], NEED_I64);
                bad(prefix, &[0x42, 0, 0x42, 0], NEED_I32);
                bad(prefix, &[0x42, 0, 0x41, 0], NEED_I64); // value checked first
            } else {
                bad(prefix, &[0x42, 0], NEED_I32);
            }
        }
        let live = &[0, 0x41, 42, 0x02, 0x40];
        bad(live, &[], ErrorKind::StackUnderflow { depth: 0 });
        if store {
            bad(live, &[0x41, 0], ErrorKind::StackUnderflow { depth: 1 });
            bad(&[0, 0x00], &[0x41, 0], NEED_I64); // only the address could be implicit
            let body = [&[0, 0x41, 0, 0x42, 0][..], &memop, &[0x0b]].concat();
            bad_memory_body(
                ONE_PAGE,
                &body,
                body.len() - 1,
                ErrorKind::InvalidResultDepth { depth: 0 },
            );
        }

        // None or only the top operand may be concrete in a dead tail.
        for inputs in [&[][..], if store { &[0x42, 0] } else { &[0x41, 0] }] {
            let mut body = [&[0, 0x00][..], inputs, &memop].concat();
            if !store {
                // The load still produces a concrete i64 in dead code.
                bad_memory_body(
                    ONE_PAGE,
                    &[&body[..], &[0x45, 0x0b]].concat(),
                    body.len(),
                    NEED_I32,
                );
                body.push(0x50); // i64.eqz -> i32
            }
            body.push(0x0b);
            let module = memory_functions(ONE_PAGE, &[0], &[0], &[(b"test", 0)], &[&body]);
            assert_eq!(
                module.instantiate().unwrap().invoke_i32("test"),
                Err(InvokeError::Trap(Trap::Unreachable))
            );
        }
    }
}

#[test]
fn i64_memory_opcodes_obey_operand_and_decoded_instruction_caps() {
    for (opcode, align, store) in ops() {
        let depth_limit = Limit::OperandStackDepth;
        let depth = depth_limit.maximum();
        let body = |fillers| {
            let mut body = vec![0];
            body.extend([0x41, 42].repeat(fillers));
            body.extend([0x41, 0]);
            if store {
                body.extend([0x42, 0]);
            }
            body.extend([opcode, align, 0]);
            if !store {
                body.push(0x1a);
            }
            body.extend(std::iter::repeat_n(0x1a, fillers - 1));
            body.push(0x0b);
            body
        };
        let fillers = depth - 1 - usize::from(store);
        let accepted = memory_functions(ONE_PAGE, &[0], &[0], &[(b"test", 0)], &[&body(fillers)]);
        assert_eq!(accepted.instantiate().unwrap().invoke_i32("test"), Ok(42));
        bad_memory_body(
            ONE_PAGE,
            &body(fillers + 1),
            1 + 2 * depth,
            ErrorKind::LimitExceeded { limit: depth_limit },
        );

        let limit = Limit::BodyInstructions;
        // Fill the body up to the limit, then add enough padding so the
        // memory opcode itself is the first over the limit.
        let setup = if store {
            &[0x41, 42, 0x41, 0, 0x42, 0][..]
        } else {
            &[0x41, 0][..]
        };
        let suffix = if store {
            &[0x0b][..]
        } else {
            &[0x50, 0x0b][..]
        };
        let nops = limit.maximum() - if store { 4 } else { 3 };
        let body = |extra| {
            [
                &[0][..],
                &vec![0x01; nops + extra],
                setup,
                &[opcode, align, 0],
                suffix,
            ]
            .concat()
        };
        let accepted = memory_functions(ONE_PAGE, &[0], &[0], &[(b"test", 0)], &[&body(0)]);
        assert_eq!(
            accepted.instantiate().unwrap().invoke_i32("test"),
            Ok(if store { 42 } else { 1 })
        );
        let extra = if store { 1 } else { 2 };
        let body = body(extra);
        let mut bytes = module(&[(1, TYPE), (3, FUNCTION), (5, ONE_PAGE)]);
        let code = code_body(&body);
        let start = push_section(&mut bytes, 10, &code) + code.len() - body.len();
        expect_error(
            &bytes,
            start + 1 + nops + extra + setup.len(),
            ErrorKind::LimitExceeded { limit },
        );
    }
}

#[test]
fn each_i64_memory_opcode_costs_one_dispatch_and_exhausted_stores_do_not_write() {
    let limit = ExecutionLimit::InstructionDispatches;
    // Each iteration consumes five dispatches, and each tail consumes four.
    let iterations = (limit.maximum() - 4) / 5;
    let nops = (limit.maximum() - 4) % 5;
    let pattern = 0x7766_5544_3322_1109i64;
    for (opcode, align, store) in ops() {
        let body = |extra| {
            let mut body = b"\0\x03\x40\x20\0\x41\x01\x6b\x22\0\x0d\0\x0b".to_vec();
            body.extend(std::iter::repeat_n(0x01, nops + extra));
            if store {
                body.extend([0x41, 42]);
            }
            body.extend([0x41, 0]);
            if store {
                push_i64(&mut body, pattern);
            }
            body.extend([opcode, align, 0]);
            if !store {
                body.extend([0x42, 0, 0x51]);
            }
            body.push(0x0b);
            body
        };
        let module = memory_functions(
            ONE_PAGE,
            &[1, 3],
            &[0, 0, 1],
            &[(b"exact", 0), (b"over", 1), (b"word", 2)],
            &[&body(0), &body(1), &checked_load(LOAD64, &[3, 0])],
        );
        let instance = module.instantiate().unwrap();
        assert_eq!(
            instance.invoke_i32_with_args("over", &[iterations as i32]),
            Err(InvokeError::LimitExceeded { limit })
        );
        assert_eq!(instance.invoke_i32_with_args("word", &args(0, 0)), Ok(1));
        assert_eq!(
            instance.invoke_i32_with_args("exact", &[iterations as i32]),
            Ok(if store { 42 } else { 1 })
        );
        if store {
            let shift = 64 - (8 << align);
            let low = pattern << shift >> shift;
            assert_eq!(instance.invoke_i32_with_args("word", &args(0, low)), Ok(1));
        }
    }
}
