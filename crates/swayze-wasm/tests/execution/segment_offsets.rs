use super::{CODE, FUNCTION, TYPE, code_bodies, expect_error, module, push_section};
use swayze_wasm::{
    ErrorKind::*, InstantiationError, IntegerField, ValType, Value, validate_module,
};

// Segment section, target section, and target declaration with initial size one
// (one slot for a table, one page for a memory).
const TARGETS: [(u8, u8, &[u8]); 2] = [(9, 4, &[1, 0x70, 0, 1]), (11, 5, &[1, 0, 1])];
const GLOBAL_INDEX: IntegerField = IntegerField::GlobalIndex;

#[test]
fn global_indices_are_framed_and_checked_before_mutability_type_or_end() {
    let globals = b"\x06\
        \x7f\0\x41\0\x0b\
        \x7f\x01\x41\0\x0b\
        \x7e\0\x42\0\x0b\
        \x7d\0\x43\0\0\0\0\x0b\
        \x7c\0\x44\0\0\0\0\0\0\0\0\x0b\
        \x7e\x01\x42\0\x0b";
    for (id, target, limits) in TARGETS {
        let check = |expr_and_tail: &[u8], relative, kind| {
            let mut bytes = module(&[(target, limits), (6, globals)]);
            let payload = [&[1, 0][..], expr_and_tail].concat();
            let start = push_section(&mut bytes, id, &payload);
            push_section(&mut bytes, 0, &[0]); // Cannot complete this expression.
            expect_error(&bytes, start + 2 + relative, kind);
        };
        let op_and_padding = [0x23, 0x80, 0x80, 0x80, 0x80];
        for n in 1..=op_and_padding.len() {
            check(
                &op_and_padding[..n],
                n,
                TruncatedInteger {
                    field: GLOBAL_INDEX,
                },
            );
        }
        for fifth in [0x10, 0x80] {
            check(
                &[0x23, 0x80, 0x80, 0x80, 0x80, fifth, 0, 0x01],
                5,
                IntegerOverflow {
                    field: GLOBAL_INDEX,
                },
            );
        }
        check(
            &[0x23, 0x86, 0, 0x01],
            1,
            InvalidIndex {
                field: GLOBAL_INDEX,
                index: 6,
            },
        );
        check(
            &[0x23, 0x81, 0, 0x01],
            1,
            MutableGlobalInSegmentOffset { index: 1 },
        );
        // When both restrictions fail, report mutability before the operand type.
        check(&[0x23, 5], 1, MutableGlobalInSegmentOffset { index: 5 });
        for (index, actual) in [(2, ValType::I64), (3, ValType::F32), (4, ValType::F64)] {
            check(
                &[0x23, index, 0x01],
                0,
                TypeMismatch {
                    expected: ValType::I32,
                    actual,
                },
            );
        }
        check(&[0x23, 0], 2, TruncatedByte { expected: 0x0b });
        check(
            &[0x23, 0, 0x41, 0, 0x0b, 0],
            2,
            UnexpectedByte {
                expected: 0x0b,
                found: 0x41,
            },
        );
        check(&[0x23, 0, 0x0b, 0, 0xff], 4, TrailingSectionBytes { id });

        for empty_globals in [None, Some(&[0][..])] {
            let mut bytes = module(&[(target, limits)]);
            if let Some(empty_globals) = empty_globals {
                push_section(&mut bytes, 6, empty_globals);
            }
            let start = push_section(&mut bytes, id, &[1, 0, 0x23, 0, 0x0b, 0]);
            expect_error(
                &bytes,
                start + 3,
                InvalidIndex {
                    field: GLOBAL_INDEX,
                    index: 0,
                },
            );
        }
    }
}

#[test]
fn target_index_and_presence_precede_the_offset_expression() {
    for (id, target, _) in TARGETS {
        let (field, missing) = if id == 9 {
            (IntegerField::TableIndex, MissingTable)
        } else {
            (IntegerField::MemoryIndex, MissingMemory)
        };
        let mut bytes = module(&[(target, &[0])]);
        let start = push_section(&mut bytes, id, &[1, 0x81, 0, 0x23, 0x80]);
        expect_error(&bytes, start + 1, InvalidIndex { field, index: 1 });
        bytes[start + 1] = 0x80; // Valid padded zero; missing target wins over truncated GlobalIndex.
        expect_error(&bytes, start + 1, missing);
    }
}

#[test]
fn global_offsets_use_unsigned_i32_bits_and_initial_target_bounds() {
    for (id, target, limits) in TARGETS {
        let (last, end, past_end, error) = if id == 9 {
            (
                &[0][..],
                &[1][..],
                &[2][..],
                InstantiationError::ElementSegmentOutOfBounds { segment_index: 0 },
            )
        } else {
            (
                &[0xff, 0xff, 3][..],
                &[0x80, 0x80, 4][..],
                &[0x81, 0x80, 4][..],
                InstantiationError::DataSegmentOutOfBounds { segment_index: 0 },
            )
        };
        for (literal, tail, fits) in [
            (last, &[1, 0][..], true),
            (end, &[0], true),
            (end, &[1, 0], false),
            (past_end, &[0], false),
            (&[0x7f], &[0], false), // -1: empty still requires an in-bounds offset.
            (&[0x7f], &[1, 0], false), // -1 + one must not wrap back to zero.
        ] {
            let globals = [&[1, 0x7f, 0, 0x41][..], literal, &[0x0b]].concat();
            let segments = [&[1, 0, 0x23, 0, 0x0b][..], tail].concat();
            let mut bytes = module(&[(1, TYPE), (3, FUNCTION), (target, limits), (6, &globals)]);
            if id == 9 {
                push_section(&mut bytes, 9, &segments);
            }
            push_section(&mut bytes, 10, CODE);
            if id == 11 {
                push_section(&mut bytes, 11, &segments);
            }
            let validated = validate_module(&bytes).unwrap();
            let result = validated.instantiate();
            if fits {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err(), error);
            }
        }
    }
}

#[test]
fn literal_and_global_offsets_install_in_source_order_before_start() {
    let code = code_bodies(&[
        b"\0\x41\x02\x11\0\0\x0b",       // Start: call table[2] as void.
        b"\0\0\x0b",                     // Trap if the first element segment incorrectly wins.
        b"\0\x41\x02\x2d\0\0\x24\0\x0b", // Copy memory[2] to global 0.
    ]);
    let globals = [2, 0x7f, 1, 0x41, 0, 0x0b, 0x7f, 0, 0x41, 1, 0x0b];
    // Global index 1 in each offset is padded to five bytes. Its value is 1.
    let elements = [
        2, 0, 0x41, 2, 0x0b, 1, 1, 0, 0x23, 0x81, 0x80, 0x80, 0x80, 0, 0x0b, 2, 2, 2,
    ];
    let data = [
        2, 0, 0x41, 2, 0x0b, 1, 99, 0, 0x23, 0x81, 0x80, 0x80, 0x80, 0, 0x0b, 2, 8, 42,
    ];
    let validated = validate_module(&module(&[
        (1, &[1, 0x60, 0, 0]),
        (2, &[0]), // Empty imports do not affect the index space.
        (3, &[3, 0, 0, 0]),
        (4, &[1, 0x70, 0, 3]),
        (5, &[1, 0, 1]),
        (6, &globals),
        (7, b"\x01\x04seen\x03\0"),
        (8, &[0]),
        (9, &elements),
        (10, &code),
        (11, &data),
    ]))
    .unwrap();
    for _ in 0..2 {
        let instance = validated.instantiate().unwrap();
        assert_eq!(instance.read_global("seen"), Ok(Value::I32(42)));
    }
}
