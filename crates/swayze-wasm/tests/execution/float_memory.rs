use super::*;
use crate::i64_signatures::types;
use swayze_wasm::{Instance, ResultKind, ValType, Value};

#[derive(Clone, Copy)]
struct FloatAccess {
    ty: ValType,
    align: u8,
    load: u8,
    store: u8,
    to_integer: u8,
    from_integer: u8,
    snan: u64,
}

const FORMATS: [FloatAccess; 2] = [
    FloatAccess {
        ty: ValType::F32,
        align: 2,
        load: 0x2a,
        store: 0x38,
        to_integer: 0xbc,
        from_integer: 0xbe,
        snan: 0x7f80_0b12,
    },
    FloatAccess {
        ty: ValType::F64,
        align: 3,
        load: 0x2b,
        store: 0x39,
        to_integer: 0xbd,
        from_integer: 0xbf,
        snan: 0xfff0_000b_89ab_cdef,
    },
];

pub(super) fn ops() -> impl Iterator<Item = (u8, u8)> {
    FORMATS
        .into_iter()
        .flat_map(|f| [(f.load, f.align), (f.store, f.align)])
}

impl FloatAccess {
    fn width(self) -> usize {
        1 << self.align
    }

    fn integer(self, bits: u64) -> Value {
        if self.align == 2 {
            Value::I32(bits as i32)
        } else {
            Value::I64(bits as i64)
        }
    }

    fn read_result(self) -> ResultKind {
        if self.align == 2 {
            ResultKind::I32
        } else {
            ResultKind::I64
        }
    }

    fn const_bits(self, bits: u64) -> Vec<u8> {
        let opcode = if self.align == 2 { 0x43 } else { 0x44 };
        [&[opcode][..], &bits.to_le_bytes()[..self.width()]].concat()
    }

    fn read(self, instance: &Instance<'_>, addr: i32) -> Result<Option<Value>, InvokeError> {
        instance.invoke_with_values("load", &[Value::I32(addr)], self.read_result())
    }

    fn write(
        self,
        instance: &Instance<'_>,
        addr: i32,
        bits: u64,
    ) -> Result<Option<Value>, InvokeError> {
        instance.invoke_with_values(
            "store",
            &[Value::I32(addr), self.integer(bits)],
            ResultKind::I32,
        )
    }

    fn module(self, memory: &[u8], memarg: &[u8]) -> Module {
        // Floats never cross the integer host ABI: reinterpret in the guest so
        // the host can compare all result bits, even for signaling NaNs.
        let int = if self.align == 2 { 0x7f } else { 0x7e };
        let ty = types(&[
            (&[0x7f], Some(int)),
            (&[0x7f, int], Some(0x7f)),
            (&[0x7f], Some(0x7f)),
            (&[0x7f, 0x7f], Some(0x7f)),
        ]);
        let read = [
            &[0, 0x20, 0, self.load][..],
            memarg,
            &[self.to_integer, 0x0b],
        ]
        .concat();
        let write = [
            &[0, 0x20, 0, 0x20, 1, self.from_integer, self.store][..],
            memarg,
            &[0x41, 42, 0x0b],
        ]
        .concat();
        validate_module(&module(&[
            (1, &ty),
            (3, &[4, 0, 1, 2, 3]),
            (5, memory),
            (
                7,
                &function_exports(&[(b"load", 0), (b"store", 1), (b"byte", 2), (b"word", 3)]),
            ),
            (
                10,
                &code_bodies(&[&read, &write, &[0, 0x20, 0, LOAD8_U, 0, 0, 0x0b], WRITE]),
            ),
        ]))
        .unwrap()
    }
}

#[test]
fn float_accesses_preserve_raw_bits_and_alias_integer_bytes() {
    for f in FORMATS {
        let sign = 1 << (f.width() * 8 - 1);
        let patterns = [0, sign, f.snan];
        for hint in [0, f.align] {
            // A base of zero with padded offset 1 makes the effective address
            // unaligned even when the hint is the natural alignment.
            let module = f.module(ONE_PAGE, &[hint | 0x80, 0, 0x81, 0]);
            let instance = module.instantiate().unwrap();
            let byte = |addr| instance.invoke_i32_with_args("byte", &[addr]);
            let word = |addr, bits| instance.invoke_i32_with_args("word", &[addr, bits]);
            assert_eq!(word(0, 0x4444_4455), Ok(42));
            assert_eq!(word(1 + f.width() as i32, 0x66), Ok(42));
            for (index, bits) in patterns.into_iter().enumerate() {
                // Integer writes produce the bytes, independently of float stores.
                let bytes = bits.to_le_bytes();
                let (words, []) = bytes[..f.width()].as_chunks::<4>() else {
                    unreachable!("full words for both formats");
                };
                for (i, &chunk) in words.iter().enumerate() {
                    assert_eq!(word(1 + 4 * i as i32, i32::from_le_bytes(chunk)), Ok(42));
                }
                assert_eq!(f.read(&instance, 0), Ok(Some(f.integer(bits))));

                // The stored pattern differs from both this and the next
                // iteration's integer writes. Inspect all bytes and both neighbors.
                let next = patterns[(index + 2) % patterns.len()];
                assert_eq!(f.write(&instance, 0, next), Ok(Some(Value::I32(42))));
                for (i, &expected) in next.to_le_bytes()[..f.width()].iter().enumerate() {
                    assert_eq!(byte(1 + i as i32), Ok(i32::from(expected)));
                }
                assert_eq!(byte(0), Ok(0x55));
                assert_eq!(byte(1 + f.width() as i32), Ok(0x66));
            }
        }
    }
}

#[test]
fn float_accesses_check_the_whole_range_without_wrapping_or_partial_stores() {
    for f in FORMATS {
        let zero_pages = f.module(&[1, 0, 0], &[f.align, 0]);
        let instance = zero_pages.instantiate().unwrap();
        assert_eq!(f.read(&instance, 0), Err(OOB));
        assert_eq!(f.write(&instance, 0, f.snan), Err(OOB));

        let one_page = f.module(ONE_PAGE, &[f.align, 1]);
        let instance = one_page.instantiate().unwrap();
        let load = |addr| f.read(&instance, addr);
        let store = |addr, bits| f.write(&instance, addr, bits);
        let last = PAGE - f.width() as i32 - 1; // Offset 1 lands exactly at the exclusive end.
        assert_eq!(store(last, f.snan), Ok(Some(Value::I32(42))));
        for invalid in [last + 1, -1] {
            assert_eq!(load(invalid), Err(OOB));
            assert_eq!(store(invalid, u64::MAX), Err(OOB));
            assert_eq!(load(last), Ok(Some(f.integer(f.snan))));
            assert_eq!(instance.invoke_i32_with_args("byte", &[0]), Ok(0));
        }

        // Positive base + UINT32_MAX must neither wrap nor treat the offset as signed.
        let max_offset = f.module(ONE_PAGE, &[f.align, 0xff, 0xff, 0xff, 0xff, 0x0f]);
        let instance = max_offset.instantiate().unwrap();
        assert_eq!(f.read(&instance, 1), Err(OOB));
        assert_eq!(f.write(&instance, 1, f.snan), Err(OOB));
        assert_eq!(instance.invoke_i32_with_args("byte", &[0]), Ok(0));
    }
}

#[test]
fn float_accesses_require_exact_types_above_live_and_dead_floors() {
    use ValType::{I32, I64};
    let mismatch = |expected, actual| ErrorKind::TypeMismatch { expected, actual };
    for (i, f) in FORMATS.into_iter().enumerate() {
        let i32_zero = [0x41, 0];
        let i64_zero = [0x42, 0];
        let zero = f.const_bits(0);
        let other = FORMATS[1 - i];
        let other_zero = other.const_bits(0);
        let load = [f.load, f.align, 0];
        let store = [f.store, f.align, 0];
        // Keep an i32 below the active floor in both a live and a dead block.
        let live = &[0, 0x41, 42, 0x02, 0x40][..];
        let dead = &[0, 0x41, 42, 0x02, 0x40, 0x00][..];
        let bad = |prefix: &[u8], args: &[&[u8]], op: &[u8], kind| {
            let args = args.concat();
            let body = [prefix, &args, op, &[0x0b, 0x0b]].concat();
            bad_memory_body(ONE_PAGE, &body, prefix.len() + args.len(), kind);
        };
        for prefix in [live, dead] {
            for (addr, ty) in [(&i64_zero[..], I64), (&zero, f.ty), (&other_zero, other.ty)] {
                bad(prefix, &[addr], &load, mismatch(I32, ty));
                bad(prefix, &[addr, &zero], &store, mismatch(I32, ty));
            }
            for (value, ty) in [
                (&i32_zero[..], I32),
                (&i64_zero, I64),
                (&other_zero, other.ty),
            ] {
                // When both operands are wrong, the top value wins.
                bad(prefix, &[&i64_zero, value], &store, mismatch(f.ty, ty));
            }
        }
        for op in [load, store] {
            bad(live, &[], &op, ErrorKind::StackUnderflow { depth: 0 });
        }
        bad(
            live,
            &[&other_zero],
            &store,
            ErrorKind::StackUnderflow { depth: 1 },
        );
        bad(dead, &[&other_zero], &store, mismatch(f.ty, other.ty));

        // A load still pushes its concrete float type when the address is
        // implicit. It cannot serve as the input to the other reinterpretation.
        let body = [dead, &load, &[other.to_integer, 0x1a, 0x0b, 0x0b]].concat();
        bad_memory_body(
            ONE_PAGE,
            &body,
            dead.len() + load.len(),
            mismatch(other.ty, f.ty),
        );
        for tail in [
            [&load[..], &[f.to_integer, 0x1a]].concat(),
            store.to_vec(),
            [&zero, &store[..]].concat(),
        ] {
            let body = [dead, &tail, &[0x0b, 0x0b]].concat();
            let module = memory_functions(ONE_PAGE, &[0], &[0], &[(b"test", 0)], &[&body]);
            assert_eq!(
                module.instantiate().unwrap().invoke_i32("test"),
                Err(InvokeError::Trap(Trap::Unreachable))
            );
        }
    }
}

#[test]
fn float_accesses_cost_one_dispatch_and_do_not_run_without_fuel() {
    let limit = ExecutionLimit::InstructionDispatches;
    // Five dispatches per loop iteration; four in the exact tail.
    let iterations = (limit.maximum() - 4) / 5;
    let nops = (limit.maximum() - 4) % 5;
    for f in FORMATS {
        for store in [false, true] {
            let body = |extra| {
                let mut body = b"\0\x03\x40\x20\0\x41\x01\x6b\x22\0\x0d\0\x0b".to_vec();
                body.extend(std::iter::repeat_n(0x01, nops + extra));
                body.extend([0x41, 42, 0x20, 1]); // Result, then address from parameter 1.
                if store {
                    body.extend(f.const_bits(f.snan));
                    body.extend([f.store, f.align, 0]);
                } else {
                    body.extend([f.load, f.align, 0, 0x1a]);
                }
                body.push(0x0b);
                body
            };
            let module = memory_functions(
                ONE_PAGE,
                &[2, 1],
                &[0, 0, 0, 1],
                &[
                    (b"exact", 0),
                    (b"over", 1),
                    (b"skip_access", 2),
                    (b"byte", 3),
                ],
                &[
                    &body(0),
                    &body(1),
                    &body(if store { 1 } else { 2 }),
                    &[0, 0x20, 0, LOAD8_U, 0, 0, 0x0b],
                ],
            );
            let instance = module.instantiate().unwrap();
            let invoke =
                |name, address| instance.invoke_i32_with_args(name, &[iterations as i32, address]);
            let exhausted = Err(InvokeError::LimitExceeded { limit });
            assert_eq!(invoke("over", 0), exhausted);
            // On an invalid address, distinguish access trapping from fuel
            // exhaustion before the access even starts.
            assert_eq!(invoke("skip_access", PAGE), exhausted);
            assert_eq!(invoke("exact", PAGE), Err(OOB));
            assert_eq!(instance.invoke_i32_with_args("byte", &[0]), Ok(0));
            assert_eq!(invoke("exact", 0), Ok(42));
            if store {
                assert_eq!(
                    instance.invoke_i32_with_args("byte", &[0]),
                    Ok(i32::from(f.snan as u8))
                );
            }
        }
    }
}
