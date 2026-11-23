//! Opt-in differential Wasm smoke check: run tools/check-wasm-reference.sh.

use std::{error::Error, process::Command};
use swayze_engine::{
    WasmInstantiationError, WasmInvokeError, WasmResultKind, WasmTrap, WasmValue, validate_module,
};

const EXPORT: &str = "answer";
const REFERENCE: &str = r#"
if (process.versions.node.split('.')[0] !== '22') {
    throw new Error(`expected Node.js v22, got ${process.version}`);
}
const fixtures = process.argv.slice(1);
console.log(process.version);
// The JS Number boundary is not an exact NaN transport. Only compare non-NaN
// float arguments and results here; the Rust tests cover exact NaN payloads.
const bits = new DataView(new ArrayBuffer(8));
function decodeArg(arg) {
    if (arg.endsWith('n')) return BigInt(arg.slice(0, -1));
    if (!arg.startsWith('f')) return Number(arg);
    const payload = `0x${arg.slice(4)}`;
    let value;
    if (arg.startsWith('f32=')) {
        bits.setUint32(0, Number(payload));
        value = bits.getFloat32(0);
    } else if (arg.startsWith('f64=')) {
        bits.setBigUint64(0, BigInt(payload));
        value = bits.getFloat64(0);
    } else {
        throw new Error(`unknown argument encoding: ${arg}`);
    }
    if (Number.isNaN(value)) throw new Error('NaN at the reference host boundary');
    return value;
}
for (const fixture of fixtures) {
    const [hex, nameBytes, result, ...args] = fixture.split(':');
    const name = Buffer.from(nameBytes, 'hex').toString('utf8');
    const module = new WebAssembly.Module(Buffer.from(hex, 'hex'));
    let instance;
    try {
        instance = new WebAssembly.Instance(module);
    } catch (error) {
        if (!(error instanceof WebAssembly.RuntimeError)) throw error;
        console.log('instantiation-error');
        continue;
    }
    const invoke = instance.exports[name];
    try {
        const value = invoke(...args.map(decodeArg));
        if (result === 'void' && value === undefined) {
            console.log('void');
        } else if (result === 'i64' && typeof value === 'bigint') {
            console.log(`${value}n`);
        } else if (result === 'i32' && typeof value === 'number' && (value | 0) === value) {
            console.log(value);
        } else if (result === 'f32' && typeof value === 'number' && !Number.isNaN(value)) {
            bits.setFloat32(0, value);
            console.log(`f32=${bits.getUint32(0).toString(16)}`);
        } else if (result === 'f64' && typeof value === 'number' && !Number.isNaN(value)) {
            bits.setFloat64(0, value);
            console.log(`f64=${bits.getBigUint64(0).toString(16)}`);
        } else {
            throw new Error(`unexpected ${result} result for ${hex}: ${value}`);
        }
    } catch (error) {
        if (!(error instanceof WebAssembly.RuntimeError)) throw error;
        console.log('trap');
    }
}
"#;

// Arithmetic right shift leaves the sign extension needed for valid padding.
fn sleb32(mut value: i32, min_width: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let low = (value & 0x7f) as u8;
        value >>= 7;
        let sign = if low & 0x40 == 0 { 0 } else { -1 };
        if value == sign && bytes.len() + 1 >= min_width {
            bytes.push(low);
            return bytes;
        }
        bytes.push(low | 0x80);
    }
}

fn singleton_module(
    parameter_count: u8,
    locals: &[u8],
    expression: &[u8],
    memory_pages: Option<u8>,
) -> Vec<u8> {
    let mut module = b"\0asm\x01\0\0\0".to_vec();
    module.extend([1, 5 + parameter_count, 1, 0x60, parameter_count]);
    module.extend(std::iter::repeat_n(0x7f, usize::from(parameter_count)));
    module.extend([1, 0x7f, 3, 2, 1, 0]); // One i32 result; one function of type 0.
    if let Some(pages) = memory_pages {
        module.extend([5, 3, 1, 0, pages]); // One defined memory, minimum only.
    }
    let name_len = EXPORT.len() as u8;
    module.extend([7, name_len + 4, 1, name_len]); // Export section, one name.
    module.extend(EXPORT.bytes());
    module.extend([0, 0]); // Function export, index 0.

    // These tiny bodies fit single-byte section and body lengths.
    assert!(expression.len() + locals.len() + 3 < 128);
    let body_len = (expression.len() + locals.len() + 1) as u8;
    module.extend([10, body_len + 2, 1, body_len]);
    module.extend_from_slice(locals);
    module.extend_from_slice(expression);
    module.push(0x0b); // end
    module
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Invocation(Result<i32, WasmTrap>),
    I64Invocation(Result<i64, WasmTrap>),
    F32Invocation(Result<u32, WasmTrap>),
    F64Invocation(Result<u64, WasmTrap>),
    VoidInvocation(Result<(), WasmTrap>),
    DataSegmentOutOfBounds(usize),
    ElementSegmentOutOfBounds(usize),
    StartTrap(WasmTrap),
}

impl Outcome {
    fn result_kind(self) -> WasmResultKind {
        match self {
            Self::VoidInvocation(_) => WasmResultKind::Void,
            Self::I64Invocation(_) => WasmResultKind::I64,
            Self::F32Invocation(_) => WasmResultKind::F32,
            Self::F64Invocation(_) => WasmResultKind::F64,
            _ => WasmResultKind::I32,
        }
    }
}

struct Fixture<A = i32> {
    expected: Outcome,
    bytes: Vec<u8>,
    export: &'static str,
    args: Vec<A>,
}

impl Fixture {
    fn new(expected: i32, args: &[i32], expression: &[u8]) -> Self {
        Self::with_locals(expected, args, &[0], expression)
    }

    fn with_locals(expected: i32, args: &[i32], locals: &[u8], expression: &[u8]) -> Self {
        Self::with_outcome(Ok(expected), args, locals, expression)
    }

    fn with_outcome(
        expected: Result<i32, WasmTrap>,
        args: &[i32],
        locals: &[u8],
        expression: &[u8],
    ) -> Self {
        Self {
            expected: Outcome::Invocation(expected),
            bytes: singleton_module(u8::try_from(args.len()).unwrap(), locals, expression, None),
            export: EXPORT,
            args: args.to_vec(),
        }
    }

    fn into_typed(self) -> Fixture<WasmValue> {
        Fixture {
            expected: self.expected,
            bytes: self.bytes,
            export: self.export,
            args: self.args.into_iter().map(WasmValue::I32).collect(),
        }
    }
}

impl Fixture<WasmValue> {
    fn encode(&self) -> String {
        use std::fmt::Write;
        let result = match self.expected.result_kind() {
            WasmResultKind::Void => "void",
            WasmResultKind::I32 => "i32",
            WasmResultKind::I64 => "i64",
            WasmResultKind::F32 => "f32",
            WasmResultKind::F64 => "f64",
        };
        let mut encoded = format!(
            "{}:{}:{result}",
            hex(&self.bytes),
            hex(self.export.as_bytes())
        );
        for arg in &self.args {
            match arg {
                WasmValue::I32(arg) => write!(&mut encoded, ":{arg}"),
                WasmValue::I64(arg) => write!(&mut encoded, ":{arg}n"),
                WasmValue::F32(bits) => write!(&mut encoded, ":f32={bits:x}"),
                WasmValue::F64(bits) => write!(&mut encoded, ":f64={bits:x}"),
            }
            .unwrap();
        }
        encoded
    }
}

fn fixtures() -> Vec<Fixture> {
    let mut values = vec![i32::MIN, i32::MAX, -1, 0, 1];
    // Straddle the negative and positive limits of each short signed encoding.
    for bits in [6, 13, 20, 27] {
        let limit = 1_i32 << bits;
        values.extend([-limit - 1, -limit, limit - 1, limit]);
    }
    let mut cases = Vec::new();
    for value in values {
        let canonical_width = sleb32(value, 1).len();
        for width in canonical_width..=5 {
            let mut expression = vec![0x41]; // i32.const
            expression.extend(sleb32(value, width));
            cases.push(Fixture::new(value, &[], &expression));
        }
    }
    // Exercise operand ordering and overflow, with a third live operand.
    for (a, b, c) in [
        (17_i32, 4, 3),
        (i32::MAX, i32::MIN, -1),
        (i32::MAX, i32::MAX, 2),
        (-65536, 65536, 0),
    ] {
        for (opcode, op) in [
            (0x6a, i32::wrapping_add as fn(i32, i32) -> i32),
            (0x6b, i32::wrapping_sub),
            (0x6c, i32::wrapping_mul),
        ] {
            let mut expression = Vec::new();
            for value in [a, b, c] {
                expression.push(0x41);
                expression.extend(sleb32(value, 1));
            }
            expression.extend([opcode, 0x6b]);
            cases.push(Fixture::new(a.wrapping_sub(op(b, c)), &[], &expression));
            cases.push(Fixture::new(
                a.wrapping_sub(op(b, c)),
                &[a, b, c],
                &[0x20, 0, 0x20, 1, 0x20, 2, opcode, 0x6b],
            ));
        }
    }
    cases.push(Fixture::new(i32::MIN, &[i32::MIN], &[0x20, 0]));
    cases.push(Fixture::new(
        42,
        &[4, 8, 15, 42],
        &[0x20, 0x83, 0x80, 0x80, 0x80, 0], // Padded local index 3.
    ));
    // A local is initially zero; parameter writes affect only this call.
    cases.push(Fixture::with_locals(0, &[99], &[1, 1, 0x7f], &[0x20, 1]));
    cases.push(Fixture::new(42, &[99], &[0x41, 42, 0x21, 0, 0x20, 0]));
    cases.push(Fixture::with_locals(
        42,
        &[],
        &[2, 3, 0x7f, 13, 0x7f],
        &[0x41, 21, 0x22, 15, 0x20, 15, 0x6a],
    ));
    use WasmTrap::{IntegerDivideByZero, IntegerOverflow};
    for (opcode, args, outcome) in [
        (0x6d, [-11, 3], Ok(-3)),
        (0x6d, [i32::MIN, -1], Err(IntegerOverflow)),
        (0x6e, [-1, 2], Ok(i32::MAX)),
        (0x6e, [-1, 1], Ok(-1)),
        (0x6f, [-11, 3], Ok(-2)),
        (0x6f, [i32::MIN, -1], Ok(0)),
        (0x70, [-1, 2], Ok(1)),
        (0x70, [1, -1], Ok(1)),
    ] {
        cases.push(Fixture::with_outcome(
            outcome,
            &args,
            &[0],
            &[0x20, 0, 0x20, 1, opcode],
        ));
    }
    for opcode in 0x6d..=0x70 {
        cases.push(Fixture::with_outcome(
            Err(IntegerDivideByZero),
            &[i32::MIN, 0],
            &[0],
            &[0x20, 0, 0x20, 1, opcode],
        ));
    }
    // If the first parameter is zero, the nested condition selects 2 or 3.
    // Otherwise only the then arm runs and attempts signed division by zero.
    let nested = [
        0x20, 0, 0x04, 0x7f, 0x41, 1, 0x41, 0, 0x6d, 0x05, 0x20, 1, 0x04, 0x7f, 0x41, 2, 0x05,
        0x41, 3, 0x0b, 0x0b,
    ];
    for (args, outcome) in [
        ([0, 0], Ok(3)),
        ([0, -1], Ok(2)),
        ([-1, 0], Err(IntegerDivideByZero)),
    ] {
        cases.push(Fixture::with_outcome(outcome, &args, &[0], &nested));
    }
    for (arg, expected) in [(0, 1), (i32::MIN, 0)] {
        cases.push(Fixture::new(expected, &[arg], &[0x20, 0, 0x45]));
    }
    for (args, expected) in [
        ([-1, 1], [0, 1, 1, 0, 0, 1, 1, 0, 0, 1]),
        ([1, -1], [0, 1, 0, 1, 1, 0, 0, 1, 1, 0]),
        ([i32::MIN; 2], [1, 0, 0, 0, 0, 0, 1, 1, 1, 1]),
    ] {
        for (opcode, result) in (0x46..=0x4f).zip(expected) {
            cases.push(Fixture::new(result, &args, &[0x20, 0, 0x20, 1, opcode]));
        }
    }
    for (opcode, expected) in [(0x48, 42), (0x49, 7)] {
        cases.push(Fixture::new(
            expected,
            &[-1, 1],
            &[
                0x20, 0, 0x20, 1, opcode, 0x04, 0x7f, 0x41, 42, 0x05, 0x41, 7, 0x0b,
            ],
        ));
    }
    // Body 0 returns its argument; body 1 increments it. Exports reference
    // function indices out of order and include a non-ASCII alias for body 1.
    let multi = b"\0asm\x01\0\0\0\x01\x06\x01\x60\x01\x7f\x01\x7f\
                  \x03\x03\x02\0\0\x07\x0e\x03\x01x\0\x01\x01y\0\0\x02\xcf\x80\0\x01\
                  \x0a\x0e\x02\x04\0\x20\0\x0b\x07\0\x20\0\x41\x01\x6a\x0b";
    for (export, arg, expected) in [("x", 41, 42), ("y", 41, 41), ("π", i32::MAX, i32::MIN)] {
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(expected)),
            bytes: multi.to_vec(),
            export,
            args: vec![arg],
        });
    }
    // A forward call to an unexported function doubles the argument.
    let forward = b"\0asm\x01\0\0\0\x01\x06\x01\x60\x01\x7f\x01\x7f\
                    \x03\x03\x02\0\0\x07\x0a\x01\x06answer\0\0\
                    \x0a\x10\x02\x06\0\x20\0\x10\x01\x0b\x07\0\x20\0\x41\x02\x6c\x0b";
    cases.push(Fixture {
        expected: Outcome::Invocation(Ok(42)),
        bytes: forward.to_vec(),
        export: EXPORT,
        args: vec![21],
    });
    // Count down on the way in and add one per return. All calls terminate
    // well within Swayze's budgets; the host limits aren't compared to Node.
    let recursive = [
        0x20, 0, 0x04, 0x7f, 0x20, 0, 0x41, 1, 0x6b, 0x10, 0, 0x41, 1, 0x6a, 0x05, 0x41, 0, 0x0b,
    ];
    for input in [0, 1, 7] {
        cases.push(Fixture::new(input, &[input], &recursive));
    }
    // A no-argument export calls a two-argument function, also exported.
    let mixed = b"\0asm\x01\0\0\0\x01\x0b\x02\x60\0\x01\x7f\x60\x02\x7f\x7f\x01\x7f\
                  \x03\x03\x02\0\x01\x07\x09\x02\x01x\0\0\x01y\0\x01\
                  \x0a\x12\x02\x08\0\x41\x2d\x41\x03\x10\x01\x0b\
                  \x07\0\x20\0\x20\x01\x6b\x0b";
    for (export, args, expected) in [("x", vec![], 42), ("y", vec![45, 3], 42)] {
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(expected)),
            bytes: mixed.to_vec(),
            export,
            args,
        });
    }
    // Export x keeps an operand live across a call to a private void helper.
    // The helper stores its argument in a global and explicitly returns.
    let void_helper = b"\0asm\x01\0\0\0\x01\x0a\x02\x60\x01\x7f\x01\x7f\x60\x01\x7f\0\
                        \x03\x03\x02\0\x01\x06\x06\x01\x7f\x01\x41\0\x0b\x07\x05\x01\x01x\0\0\
                        \x0a\x15\x02\x0b\0\x41\x03\x20\0\x10\x01\x23\0\x6a\x0b\
                        \x07\0\x20\0\x24\0\x0f\x0b";
    for (input, expected) in [(39, 42), (-8, -5), (i32::MAX, i32::MIN + 2)] {
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(expected)),
            bytes: void_helper.to_vec(),
            export: "x",
            args: vec![input],
        });
    }
    // A taken branch keeps 39, discards 7, and preserves 3 below the block.
    // Fallthrough adds 7 and 39 before returning the block result.
    let branch = [
        0x41, 3, 0x02, 0x7f, 0x41, 7, 0x41, 39, 0x20, 0, 0x0d, 0, 0x6a, 0x0b, 0x6a,
    ];
    for (condition, expected) in [(0, 49), (1, 42), (-1, 42)] {
        cases.push(Fixture::new(expected, &[condition], &branch));
    }
    // Sum a positive countdown in local 1, keeping 3 below the loop floor.
    let countdown = [
        0x41, 3, 0x03, 0x40, 0x20, 1, 0x20, 0, 0x6a, 0x21, 1, 0x20, 0, 0x41, 1, 0x6b, 0x22, 0,
        0x0d, 0, 0x0b, 0x20, 1, 0x6a,
    ];
    for (arg, expected) in [(1, 4), (4, 13), (8, 39)] {
        cases.push(Fixture::with_locals(
            expected,
            &[arg],
            &[1, 1, 0x7f],
            &countdown,
        ));
    }
    use WasmTrap::MemoryOutOfBounds;
    let load = [0x20, 0, 0x28, 2, 0];
    let store_then_load = [0x20, 0, 0x20, 1, 0x36, 2, 0, 0x20, 0, 0x28, 2, 0];
    for (pages, args, expression, expected) in [
        (1, &[1][..], &load[..], Ok(0)),
        (0, &[0][..], &load[..], Err(MemoryOutOfBounds)),
        (1, &[65533][..], &load[..], Err(MemoryOutOfBounds)),
        (1, &[-1][..], &load[..], Err(MemoryOutOfBounds)),
        (1, &[65532, -456][..], &store_then_load[..], Ok(-456)),
        (
            1,
            &[1, 0x1234_5678][..],
            &store_then_load[..],
            Ok(0x1234_5678),
        ),
    ] {
        cases.push(Fixture {
            expected: Outcome::Invocation(expected),
            bytes: singleton_module(args.len() as u8, &[0], expression, Some(pages)),
            export: EXPORT,
            args: args.to_vec(),
        });
    }
    for (store, load, bits, address, value, result) in [
        (0x3a, 0x2c, 0, 65535, 0x180, -128),
        (0x3a, 0x2d, 0, 65535, 0x180, 128),
        (0x3b, 0x2e, 1, 65533, 0x18000, -32768),
        (0x3b, 0x2f, 1, 65533, 0x18000, 32768),
    ] {
        let body = [0x20, 0, 0x20, 1, store, bits, 0, 0x20, 0, load, bits, 0];
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(result)),
            bytes: singleton_module(2, &[0], &body, Some(1)),
            export: EXPORT,
            args: vec![address, value],
        });
    }
    for pages in [0, 1] {
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(i32::from(pages))),
            bytes: singleton_module(0, &[0], &[0x3f, 0], Some(pages)),
            export: EXPORT,
            args: vec![],
        });
    }
    cases.extend([
        with_data(
            &[(0, &[1, 2, 3, 4]), (1, &[9])],
            0,
            Outcome::Invocation(Ok(0x0403_0901)),
        ),
        with_data(
            &[(65535, &[42]), (65536, &[])],
            65532,
            Outcome::Invocation(Ok(0x2a00_0000)),
        ),
        with_data(
            &[(0, &[1]), (65537, &[])],
            0,
            Outcome::DataSegmentOutOfBounds(1),
        ),
        with_data(&[(-1, &[1])], 0, Outcome::DataSegmentOutOfBounds(0)),
    ]);
    // The callee adds the argument to mutable global 1 and returns it.
    // The caller adds immutable global 0. Each fixture starts a fresh instance.
    let globals = b"\0asm\x01\0\0\0\x01\x06\x01\x60\x01\x7f\x01\x7f\
                    \x03\x03\x02\0\0\x06\x0b\x02\x7f\0\x41\x28\x0b\x7f\x01\x41\x02\x0b\
                    \x07\x0a\x01\x06answer\0\0\x0a\x17\x02\
                    \x09\0\x20\0\x10\x01\x23\0\x6a\x0b\
                    \x0b\0\x20\0\x23\x01\x6a\x24\x01\x23\x01\x0b";
    for (input, expected) in [(0, 42), (-41, 1), (i32::MAX, i32::MIN + 41)] {
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(expected)),
            bytes: globals.to_vec(),
            export: EXPORT,
            args: vec![input],
        });
    }
    for (opcode, input, expected) in [
        (0x67, 0, 32),
        (0x67, i32::MIN, 0),
        (0x68, 0, 32),
        (0x68, i32::MIN, 31),
        (0x69, -1, 32),
    ] {
        cases.push(Fixture::new(expected, &[input], &[0x20, 0, opcode]));
    }
    for (opcode, lhs, rhs, expected) in [
        (0x71, 0b1100, 0b1010, 0b1000),
        (0x72, 0b1100, 0b1010, 0b1110),
        (0x73, 0b1100, 0b1010, 0b0110),
        (0x74, 1, 33, 2),
        (0x74, 1, i32::MIN, 1),
        (0x75, i32::MIN, -1, -1),
        (0x76, i32::MIN, -1, 1),
        (0x77, i32::MIN, 33, 1),
        (0x78, 1, 33, i32::MIN),
    ] {
        cases.push(Fixture::new(
            expected,
            &[lhs, rhs],
            &[0x20, 0, 0x20, 1, opcode],
        ));
    }
    // Exit the outer block from an if, crossing the live value 7.
    let outer_exit = [
        0x41, 3, 0x02, 0x7f, 0x41, 7, 0x20, 0, 0x04, 0x7f, 0x41, 39, 0x41, 1, 0x0d, 1, 0x05, 0x41,
        39, 0x0b, 0x6a, 0x0b, 0x6a,
    ];
    for (input, expected) in [(0, 49), (1, 42)] {
        cases.push(Fixture::new(expected, &[input], &outer_exit));
    }
    // Continue the outer loop from its nested block until the count is zero.
    let outer_continue = [
        0x41, 3, 0x03, 0x40, 0x20, 1, 0x20, 0, 0x6a, 0x21, 1, 0x02, 0x7f, 0x20, 0, 0x41, 1, 0x6b,
        0x22, 0, 0x0d, 1, 0x20, 1, 0x0b, 0x21, 1, 0x0b, 0x20, 1, 0x6a,
    ];
    for (input, expected) in [(1, 4), (4, 13)] {
        cases.push(Fixture::with_locals(
            expected,
            &[input],
            &[1, 1, 0x7f],
            &outer_continue,
        ));
    }
    // Drop a scratch operand, then select between two candidates.
    let select = [0x41, 9, 0x1a, 0x01, 0x41, 42, 0x41, 7, 0x20, 0, 0x1b];
    for (predicate, expected) in [(0, 7), (1, 42), (i32::MIN, 42)] {
        cases.push(Fixture::new(expected, &[predicate], &select));
    }
    // Even an unselected candidate is evaluated and can trap.
    cases.push(Fixture::with_outcome(
        Err(WasmTrap::IntegerDivideByZero),
        &[],
        &[0],
        &[0x41, 42, 0x41, 1, 0x41, 0, 0x6d, 0x41, 1, 0x1b],
    ));
    // An omitted else skips the local write and leaves the prefix intact.
    let void_if = [
        0x41, 3, 0x20, 0, 0x04, 0x40, 0x41, 39, 0x21, 0, 0x0b, 0x20, 0, 0x6a,
    ];
    for (condition, expected) in [(0, 3), (1, 42), (-1, 42)] {
        cases.push(Fixture::new(expected, &[condition], &void_if));
    }
    // The void block exit discards 7 and has no label result.
    let void_exit = [
        0x41, 3, 0x02, 0x40, 0x41, 7, 0x20, 0, 0x0d, 0, 0x21, 0, 0x0b, 0x20, 0, 0x6a,
    ];
    for (condition, expected) in [(0, 10), (1, 4)] {
        cases.push(Fixture::new(expected, &[condition], &void_exit));
    }
    for body in [&[0x00][..], &[0x41, 9, 0x00, 0x1a, 0x6a][..]] {
        cases.push(Fixture::with_outcome(
            Err(WasmTrap::Unreachable),
            &[],
            &[0],
            body,
        ));
    }
    // The unreachable then arm supplies its result polymorphically.
    let trap_or_answer = [0x20, 0, 0x04, 0x7f, 0x00, 0x05, 0x41, 42, 0x0b];
    cases.push(Fixture::new(42, &[0], &trap_or_answer));
    cases.push(Fixture::with_outcome(
        Err(WasmTrap::Unreachable),
        &[-1],
        &[0],
        &trap_or_answer,
    ));
    // Pass a result through the outer block's label, skipping dead code.
    cases.push(Fixture::new(
        42,
        &[],
        &[
            0x41, 3, 0x02, 0x7f, 0x02, 0x40, 0x41, 39, 0x0c, 1, 0x6a, 0x1a, 0x0b, 0x00, 0x0b, 0x6a,
        ],
    ));
    // Exit the outer void block with br_if, otherwise continue the loop with br.
    let countdown_with_exit = [
        0x02, 0x40, 0x03, 0x40, 0x20, 0, 0x45, 0x0d, 1, 0x20, 0, 0x41, 1, 0x6b, 0x21, 0, 0x0c, 0,
        0x0b, 0x0b, 0x20, 0,
    ];
    for input in [0, 4] {
        cases.push(Fixture::new(0, &[input], &countdown_with_exit));
    }
    // A taken branch crosses the if to return from the function. On
    // fallthrough the candidate is dropped and the ordinary result is 7.
    let function_label = [
        0x20, 0, 0x04, 0x40, 0x41, 42, 0x20, 1, 0x0d, 1, 0x1a, 0x0b, 0x41, 7,
    ];
    for (args, expected) in [([0, 1], 7), ([1, 0], 7), ([1, -1], 42)] {
        cases.push(Fixture::new(expected, &args, &function_label));
    }
    cases.push(Fixture::new(42, &[], &[0x41, 7, 0x41, 42, 0x0f, 0x00]));
    // Both arms can branch to their shared if label. The branch leaves the
    // chosen result on top of the prefix below the if.
    let if_end = [
        0x41, 3, 0x20, 0, 0x04, 0x7f, 0x41, 39, 0x0c, 0, 0x00, 0x05, 0x41, 4, 0x20, 1, 0x0d, 0,
        0x1a, 0x41, 8, 0x0b, 0x6a,
    ];
    for (args, expected) in [([1, 0], 42), ([0, -1], 7), ([0, 0], 11)] {
        cases.push(Fixture::new(expected, &args, &if_end));
    }
    // Table slots 0 and 2 exit the inner block and preserve its stack prefix.
    // Slot 1 and the default exit the outer block and discard that prefix.
    let table = [
        0x02, 0x7f, 0x41, 3, 0x02, 0x7f, 0x41, 39, 0x20, 0, 0x0e, 3, 0, 1, 0, 1, 0x0b, 0x6a, 0x0b,
    ];
    for (input, expected) in [(0, 42), (1, 39), (2, 42), (3, 39), (-1, 39)] {
        cases.push(Fixture::new(expected, &[input], &table));
    }
    // An empty vector still dispatches to its mandatory function-label default.
    cases.push(Fixture::new(
        42,
        &[],
        &[0x41, 42, 0x41, 0, 0x0e, 0, 0, 0x00],
    ));
    // Export x returns no value, unless its argument selects the trap arm.
    let void_export = b"\0asm\x01\0\0\0\x01\x05\x01\x60\x01\x7f\0\
                        \x03\x02\x01\0\x07\x05\x01\x01x\0\0\
                        \x0a\x0a\x01\x08\0\x20\0\x04\x40\x00\x0b\x0b";
    for (input, expected) in [(0, Ok(())), (-1, Err(WasmTrap::Unreachable))] {
        cases.push(Fixture {
            expected: Outcome::VoidInvocation(expected),
            bytes: void_export.to_vec(),
            export: "x",
            args: vec![input],
        });
    }
    // Two slots, with an empty segment allowed exactly at the end.
    cases.extend([
        with_elements(&[(0, &[0, 0]), (2, &[])], Outcome::Invocation(Ok(42))),
        with_elements(
            &[(0, &[0]), (3, &[])],
            Outcome::ElementSegmentOutOfBounds(1),
        ),
        with_elements(&[(2, &[0])], Outcome::ElementSegmentOutOfBounds(0)),
        with_elements(&[(-1, &[])], Outcome::ElementSegmentOutOfBounds(0)),
    ]);
    // Equal signatures at different type indices match. Other entries cover
    // a signature mismatch, a null slot, and unsigned index bounds.
    let indirect =
        b"\0asm\x01\0\0\0\x01\x0f\x03\x60\x01\x7f\x01\x7f\x60\x01\x7f\x01\x7f\x60\0\x01\x7f\
                     \x03\x04\x03\0\x01\x02\x04\x04\x01\x70\0\x03\x07\x05\x01\x01x\0\0\
                     \x09\x08\x01\0\x41\0\x0b\x02\x01\x02\
                     \x0a\x18\x03\x09\0\x20\0\x20\0\x11\0\0\x0b\
                     \x07\0\x20\0\x41\x01\x6a\x0b\x04\0\x41\x2a\x0b";
    for (selector, result) in [
        (0, Ok(1)),
        (1, Err(WasmTrap::IndirectCallTypeMismatch)),
        (2, Err(WasmTrap::UninitializedElement)),
        (3, Err(WasmTrap::TableOutOfBounds)),
        (-1, Err(WasmTrap::TableOutOfBounds)),
    ] {
        cases.push(Fixture {
            expected: Outcome::Invocation(result),
            bytes: indirect.to_vec(),
            export: "x",
            args: vec![selector],
        });
    }
    // Start increments the initialized global once, or prevents publication
    // of the instance by trapping. The regular export just reads the global.
    let start_prefix = b"\0asm\x01\0\0\0\x01\x08\x02\x60\0\x01\x7f\x60\0\0\
                         \x03\x03\x02\0\x01\x06\x06\x01\x7f\x01\x41\x29\x0b\
                         \x07\x05\x01\x01x\0\0\x08\x01\x01";
    for (code, expected) in [
        (
            &b"\x0a\x10\x02\x04\0\x23\0\x0b\x09\0\x23\0\x41\x01\x6a\x24\0\x0b"[..],
            Outcome::Invocation(Ok(42)),
        ),
        (
            &b"\x0a\x0a\x02\x04\0\x23\0\x0b\x03\0\0\x0b"[..],
            Outcome::StartTrap(WasmTrap::Unreachable),
        ),
    ] {
        let mut bytes = start_prefix.to_vec();
        bytes.extend_from_slice(code);
        cases.push(Fixture {
            expected,
            bytes,
            export: "x",
            args: vec![],
        });
    }
    // Declare the maximum explicitly so Node and Swayze share the ceiling.
    for (min, max, delta, old_pages) in [
        (0, 1, 1, 0),
        (0, 1, 0, 0),
        (0, 1, -1, -1),
        (0, 1, 2, -1),
        (0, 0, 1, -1),
        (1, 1, 1, -1),
        (1, 1, 0, 1),
    ] {
        let mut bytes =
            b"\0asm\x01\0\0\0\x01\x06\x01\x60\x01\x7f\x01\x7f\x03\x02\x01\0\x05\x04\x01\x01"
                .to_vec();
        bytes.extend([min, max]);
        bytes.extend_from_slice(b"\x07\x05\x01\x01x\0\0\x0a\x08\x01\x06\0\x20\0\x40\0\x0b");
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(old_pages)),
            bytes,
            export: "x",
            args: vec![delta],
        });
    }
    // The internal i64 operations still cross the host boundary as i32.
    cases.extend([
        Fixture::new(42, &[], &[0x42, 0xaa, 0x80, 0x80, 0x80, 0x10, 0xa7]),
        Fixture::new(-1, &[-1], &[0x20, 0, 0xac, 0xa7]),
        Fixture::new(i32::MIN, &[i32::MIN], &[0x20, 0, 0xad, 0xa7]),
        Fixture::new(42, &[], &[0x42, 0x7f, 0x1a, 0x41, 42]),
    ]);
    cases.extend([
        Fixture::new(1, &[], &[0x42, 0, 0x50]),
        Fixture::new(0, &[], &[0x42, 0x80, 0x80, 0x80, 0x80, 0x10, 0x50]),
        Fixture::new(1, &[], &[0x42, 0x7f, 0x42, 1, 0x53]),
        Fixture::new(0, &[], &[0x42, 0x7f, 0x42, 1, 0x54]),
        Fixture::new(0, &[], &[0x42, 0x7f, 0x42, 1, 0x7c, 0xa7]),
        Fixture::new(-1, &[], &[0x42, 0, 0x42, 1, 0x7d, 0xa7]),
        Fixture::new(42, &[], &[0x42, 6, 0x42, 7, 0x7e, 0xa7]),
    ]);
    // Use an explicit i64 comparison where wrapping would hide high bits.
    cases.extend([
        Fixture::new(-3, &[], &[0x42, 0x79, 0x42, 2, 0x7f, 0xa7]),
        Fixture::new(
            1,
            &[],
            &[
                0x42, 0x7f, 0x42, 2, 0x80, 0x42, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0, 0x51,
            ],
        ),
        Fixture::new(-1, &[], &[0x42, 0x79, 0x42, 2, 0x81, 0xa7]),
        Fixture::new(1, &[], &[0x42, 0x7f, 0x42, 2, 0x82, 0xa7]),
        Fixture::with_outcome(
            Err(WasmTrap::IntegerDivideByZero),
            &[],
            &[0],
            &[0x42, 1, 0x42, 0, 0x80, 0xa7],
        ),
    ]);
    for (op, expected) in [(0x7f, Err(WasmTrap::IntegerOverflow)), (0x81, Ok(0))] {
        let mut expression = vec![0x42];
        expression.extend([0x80; 9]);
        expression.extend([0x7f, 0x42, 0x7f, op, 0xa7]); // i64::MIN and -1.
        cases.push(Fixture::with_outcome(expected, &[], &[0], &expression));
    }
    // Check the count result and full-width shifted/rotated bits through i64.eq.
    cases.extend([
        Fixture::new(64, &[], &[0x42, 0, 0x79, 0xa7]),
        Fixture::new(64, &[], &[0x42, 0, 0x7a, 0xa7]),
        Fixture::new(64, &[], &[0x42, 0x7f, 0x7b, 0xa7]),
        Fixture::new(1, &[], &[0x42, 1, 0x42, 0xc1, 0, 0x86, 0x42, 2, 0x51]),
        Fixture::new(1, &[], &[0x42, 0x7f, 0x42, 0x7f, 0x88, 0x42, 1, 0x51]),
        Fixture::new(1, &[], &[0x42, 0x7f, 0x42, 0x7f, 0x87, 0x42, 0x7f, 0x51]),
        Fixture::new(
            1,
            &[],
            &[0x42, 1, 0x42, 0x7f, 0x89, 0x42, 1, 0x42, 1, 0x8a, 0x51],
        ),
    ]);
    // Both candidates have low word 42. Check which high word survives select.
    for (condition, expected) in [(0, 0), (1, 1), (-1, 1)] {
        cases.push(Fixture::new(
            expected,
            &[condition],
            &[
                0x42, 0xaa, 0x80, 0x80, 0x80, 0x10, // (1 << 32) + 42
                0x42, 42, 0x20, 0, 0x1b, 0x42, 42, 0x52, // select != 42
            ],
        ));
    }
    cases.push(Fixture::with_outcome(
        Err(WasmTrap::Unreachable),
        &[],
        &[0],
        &[0x00, 0x1b, 0xa7],
    ));
    // Declared slots follow the i32 parameter. Check typed zero and tee/get
    // without truncating the stored i64 value to its low word.
    cases.extend([
        Fixture::with_locals(1, &[42], &[2, 1, 0x7f, 1, 0x7e], &[0x20, 2, 0x50]),
        Fixture::with_locals(
            42,
            &[42],
            &[2, 1, 0x7f, 1, 0x7e],
            &[
                0x42, 0xaa, 0x80, 0x80, 0x80, 0x10, 0x22, 2, 0x1a, // i64.const; tee; drop
                0x20, 2, 0x42, 42, 0x52, 0x04, 0x7f, 0x20, 0, 0x05, 0x41, 0, 0x0b,
            ],
        ),
    ]);
    // Compare all 64 bits in the initialized global and after a write.
    let i64_global = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7f\x03\x02\x01\0\
                       \x06\x06\x01\x7e\x01\x42\x2a\x0b\x07\x05\x01\x01x\0\0";
    for body in [
        &b"\0\x23\0\x42\x2a\x51\x0b"[..],
        &b"\0\x42\xaa\x80\x80\x80\x10\x24\0\x23\0\x42\x2a\x52\x0b"[..],
    ] {
        let mut bytes = i64_global.to_vec();
        let body_len = u8::try_from(body.len()).unwrap();
        bytes.extend([10, body_len + 2, 1, body_len]);
        bytes.extend_from_slice(body);
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(1)),
            bytes,
            export: "x",
            args: vec![],
        });
    }
    // Both a value-producing if and a block transfer preserve full i64 bits.
    let wide = [0x42, 0xaa, 0x80, 0x80, 0x80, 0x10]; // (1 << 32) + 42
    let mut if_expression = vec![0x20, 0, 0x04, 0x7e];
    if_expression.extend(wide);
    if_expression.extend([0x05, 0x42, 42, 0x0b, 0x42, 42, 0x52]);
    let mut block_expression = vec![0x02, 0x7e];
    block_expression.extend(wide);
    block_expression.extend([0x20, 0, 0x0d, 0, 0x42, 1, 0x7c, 0x0b]);
    block_expression.extend(wide);
    block_expression.push(0x51);
    for (condition, expected) in [(0, 0), (-1, 1)] {
        cases.push(Fixture::new(expected, &[condition], &if_expression));
        cases.push(Fixture::new(expected, &[condition], &block_expression));
    }
    // All addresses are i32. Compare the full stored value, not just its low word.
    let mut round_trip = vec![0x20, 0];
    round_trip.extend(wide);
    round_trip.extend([0x37, 3, 0, 0x20, 0, 0x29, 3, 0]);
    round_trip.extend(wide);
    round_trip.push(0x51);
    for (address, expected) in [
        (1, Ok(1)),
        (65528, Ok(1)),
        (65529, Err(WasmTrap::MemoryOutOfBounds)),
    ] {
        cases.push(Fixture {
            expected: Outcome::Invocation(expected),
            bytes: singleton_module(1, &[0], &round_trip, Some(1)),
            export: EXPORT,
            args: vec![address],
        });
    }
    // store32 of -1: signed and unsigned loads return different i64 values.
    for (op, expected) in [(0x34, 1), (0x35, 0)] {
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(expected)),
            bytes: singleton_module(
                0,
                &[0],
                &[
                    0x41, 1, 0x42, 0x7f, 0x3e, 2, 0, 0x41, 1, op, 2, 0, 0x42, 0x7f, 0x51,
                ],
                Some(1),
            ),
            export: EXPORT,
            args: vec![],
        });
    }
    // Call a private (i64, i32) -> i64 function behind an i32 export.
    // The indirect call uses a second type index with an equal or reordered signature.
    for (indirect, reordered) in [(false, false), (true, false), (true, true)] {
        let mut bytes = b"\0asm\x01\0\0\0\x01\x12\x03\x60\x01\x7f\x01\x7f\
                          \x60\x02\x7e\x7f\x01\x7e\x60\x02"
            .to_vec();
        bytes.extend(if reordered {
            [0x7f, 0x7e]
        } else {
            [0x7e, 0x7f]
        });
        bytes.extend(b"\x01\x7e\x03\x03\x02\x01\0");
        if indirect {
            bytes.extend(b"\x04\x04\x01\x70\0\x01");
        }
        bytes.extend(b"\x07\x05\x01\x01x\0\x01");
        if indirect {
            bytes.extend(b"\x09\x07\x01\0\x41\0\x0b\x01\0");
        }
        let callee = b"\0\x20\0\x20\x01\xac\x7c\x0b"; // i64 + extend_i32_s(i32)
        let mut caller = vec![0];
        let mut call_args = wide.to_vec();
        call_args.extend([0x20, 0]);
        if reordered {
            caller.extend([0x20, 0]);
            caller.extend(wide);
        } else {
            caller.extend(call_args);
        }
        if indirect {
            caller.extend([0x41, 0, 0x11, 2, 0]);
        } else {
            caller.extend([0x10, 0]);
        }
        caller.extend([0x42, 0xab, 0x80, 0x80, 0x80, 0x10, 0x51, 0x0b]);
        let caller_len = u8::try_from(caller.len()).unwrap();
        let callee_len = u8::try_from(callee.len()).unwrap();
        bytes.extend([10, 3 + callee_len + caller_len, 2, callee_len]);
        bytes.extend(callee);
        bytes.push(caller_len);
        bytes.extend(caller);
        cases.push(Fixture {
            expected: Outcome::Invocation(if reordered {
                Err(WasmTrap::IndirectCallTypeMismatch)
            } else {
                Ok(1)
            }),
            bytes,
            export: "x",
            args: vec![1],
        });
    }
    // Keep the NaNs inside Wasm; return their bits as integers across the host boundary.
    let signaling_f32 = 0x7f80_0042_u32.to_le_bytes();
    let mut select_f32 = vec![0x43];
    select_f32.extend(signaling_f32);
    select_f32.extend([0x43, 0, 0, 0, 0x80, 0x20, 0, 0x1b, 0xbc]);
    cases.push(Fixture::new(0x7f80_0042, &[1], &select_f32));
    cases.push(Fixture::new(i32::MIN, &[0], &select_f32));
    cases.push(Fixture::new(-42, &[-42], &[0x20, 0, 0xbe, 0xbc]));
    // Reinterpret results back to integers so even a signaling NaN is exact.
    cases.extend([
        Fixture::new(
            0x7f80_0042,
            &[0xff80_0042_u32 as i32],
            &[0x20, 0, 0xbe, 0x8b, 0xbc],
        ),
        Fixture::new(i32::MIN, &[0], &[0x20, 0, 0xbe, 0x8c, 0xbc]),
        Fixture::new(
            0xff80_0042_u32 as i32,
            &[0x7f80_0042, i32::MIN],
            &[0x20, 0, 0xbe, 0x20, 1, 0xbe, 0x98, 0xbc],
        ),
    ]);
    // NaN is unordered, signed zeros compare equal, and negative subnormals
    // remain ordered rather than flushing to zero.
    for (op, lhs, rhs, expected) in [
        (0x5b, 0, i32::MIN, 1),
        (0x5c, 0x7f80_0042, 0x7f80_0042, 1),
        (0x5d, i32::MIN | 1, i32::MIN, 1),
        (0x5e, 0x7f80_0000, 0x7fc0_0042, 0),
    ] {
        cases.push(Fixture::new(
            expected,
            &[lhs, rhs],
            &[0x20, 0, 0xbe, 0x20, 1, 0xbe, op],
        ));
    }
    for (op, lhs, rhs, expected) in [
        (0x61, 0_u64, 1_u64 << 63, 1),
        (0x62, 0x7ff0_0000_0000_0042, 0, 1),
        (0x65, (1 << 63) | 2, (1 << 63) | 1, 1),
        (0x66, 0x7ff8_0000_0000_0042, 0x7ff0_0000_0000_0000, 0),
    ] {
        let mut expression = vec![0x44];
        expression.extend(lhs.to_le_bytes());
        expression.push(0x44);
        expression.extend(rhs.to_le_bytes());
        expression.push(op);
        cases.push(Fixture::new(expected, &[], &expression));
    }
    // The i32 parameter precedes f64 and f32 locals, which start with +0 bits.
    let float_locals = &[2, 1, 0x7c, 1, 0x7d];
    cases.push(Fixture::with_locals(
        0,
        &[42],
        float_locals,
        &[0x20, 2, 0xbc],
    ));
    cases.push(Fixture::with_locals(
        0x7f80_0042,
        &[0x7f80_0042],
        float_locals,
        &[0x20, 0, 0xbe, 0x22, 2, 0x1a, 0x20, 2, 0xbc],
    ));
    // Store an f64 negative zero, then test that local.get preserved its sign bit.
    cases.push(Fixture::with_locals(
        1,
        &[0],
        float_locals,
        &[
            0x44, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x21, 1, 0x20, 1, 0xbd, 0x42, 0, 0x53,
        ],
    ));
    // A mutable f32 sNaN and an immutable f64 negative zero, both private.
    let globals = b"\x06\x15\x02\x7d\x01\x43\x42\0\x80\x7f\x0b\
                    \x7c\0\x44\0\0\0\0\0\0\0\x80\x0b";
    for (expected, arg, expression) in [
        (0x7f80_0042, 0, &[0x23, 0, 0xbc][..]),
        (
            i32::MIN,
            i32::MIN,
            &[0x20, 0, 0xbe, 0x24, 0, 0x23, 0, 0xbc][..],
        ),
        (1, 0, &[0x23, 1, 0xbd, 0x42, 0, 0x53][..]),
    ] {
        let mut bytes = b"\0asm\x01\0\0\0\x01\x06\x01\x60\x01\x7f\x01\x7f\x03\x02\x01\0".to_vec();
        bytes.extend(globals);
        bytes.extend(b"\x07\x05\x01\x01x\0\0");
        let len = 2 + u8::try_from(expression.len()).unwrap();
        bytes.extend([10, len + 2, 1, len, 0]);
        bytes.extend(expression);
        bytes.push(0x0b);
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(expected)),
            bytes,
            export: "x",
            args: vec![arg],
        });
    }
    // Check bit-preserving float joins without crossing a floating-point host boundary.
    for (condition, expected) in [(1, 0x7f80_0042), (0, i32::MIN)] {
        cases.push(Fixture::new(
            expected,
            &[condition],
            &[
                0x20, 0, 0x04, 0x7d, 0x43, 0x42, 0, 0x80, 0x7f, 0x05, 0x43, 0, 0, 0, 0x80, 0x0b,
                0xbc,
            ],
        ));
    }
    for condition in [0, 1] {
        // br_if leaves the same f64 signaling NaN on the stack on both paths.
        cases.push(Fixture::new(
            1,
            &[condition],
            &[
                0x02, 0x7c, 0x44, 0x42, 0, 0, 0, 0, 0, 0xf0, 0x7f, 0x20, 0, 0x0d, 0, 0x0b, 0xbd,
                0x44, 0x42, 0, 0, 0, 0, 0, 0xf0, 0x7f, 0xbd, 0x51,
            ],
        ));
    }
    // Mix integer and float accesses to the same bytes; never pass a float to the host.
    let f32_alias = [
        0x20, 0, 0x20, 1, 0x36, 2, 0, // i32.store bits at address
        0x20, 0, 0x2a, 2, 0, 0xbc, // i32.reinterpret_f32(f32.load)
    ];
    for (address, expected) in [
        (1, Ok(0x7f80_0042)),
        (65533, Err(WasmTrap::MemoryOutOfBounds)),
    ] {
        cases.push(Fixture {
            expected: Outcome::Invocation(expected),
            bytes: singleton_module(2, &[0], &f32_alias, Some(1)),
            export: EXPORT,
            args: vec![address, 0x7f80_0042],
        });
    }
    let f64_alias = [
        0x20, 0, 0x44, 0x42, 0, 0, 0, 0, 0, 0xf0, 0x7f, 0x39, 3, 0, // f64.store signaling NaN
        0x20, 0, 0x29, 3, 0, // i64.load
        0x44, 0x42, 0, 0, 0, 0, 0, 0xf0, 0x7f, 0xbd, 0x51,
    ];
    for (address, expected) in [(65528, Ok(1)), (65529, Err(WasmTrap::MemoryOutOfBounds))] {
        cases.push(Fixture {
            expected: Outcome::Invocation(expected),
            bytes: singleton_module(1, &[0], &f64_alias, Some(1)),
            export: EXPORT,
            args: vec![address],
        });
    }
    // A private (f32, f64) -> f32 callee returns its first parameter verbatim.
    // Call it from an integer export, including through a distinct type index.
    for (indirect, reordered) in [(false, false), (true, false), (true, true)] {
        let mut bytes = b"\0asm\x01\0\0\0\x01\x11\x03\x60\x02\x7d\x7c\x01\x7d\
                          \x60\0\x01\x7f\x60\x02"
            .to_vec();
        bytes.extend(if reordered {
            [0x7c, 0x7d]
        } else {
            [0x7d, 0x7c]
        });
        bytes.extend(b"\x01\x7d\x03\x03\x02\0\x01");
        if indirect {
            bytes.extend(b"\x04\x04\x01\x70\0\x01");
        }
        bytes.extend(b"\x07\x05\x01\x01x\0\x01");
        if indirect {
            bytes.extend(b"\x09\x07\x01\0\x41\0\x0b\x01\0");
        }
        let mut caller = vec![0];
        let snan = [0x43, 0x42, 0, 0x80, 0x7f];
        let negative_zero = [0x44, 0, 0, 0, 0, 0, 0, 0, 0x80];
        if reordered {
            caller.extend(negative_zero);
            caller.extend(snan);
        } else {
            caller.extend(snan);
            caller.extend(negative_zero);
        }
        if indirect {
            caller.extend([0x41, 0, 0x11, 2, 0]);
        } else {
            caller.extend([0x10, 0]);
        }
        caller.extend([0xbc, 0x0b]);
        let len = u8::try_from(caller.len()).unwrap();
        bytes.extend([10, 7 + len, 2, 4, 0, 0x20, 0, 0x0b, len]);
        bytes.extend(caller);
        cases.push(Fixture {
            expected: Outcome::Invocation(if reordered {
                Err(WasmTrap::IndirectCallTypeMismatch)
            } else {
                Ok(0x7f80_0042)
            }),
            bytes,
            export: "x",
            args: vec![],
        });
    }
    cases
}

fn typed_fixtures() -> impl Iterator<Item = Fixture<WasmValue>> {
    // One export (i64, i32) -> i64: add the sign-extended i32 to the i64.
    let add = b"\0asm\x01\0\0\0\x01\x07\x01\x60\x02\x7e\x7f\x01\x7e\
                \x03\x02\x01\0\x07\x05\x01\x01x\0\0\x0a\x0a\x01\x08\0\x20\0\x20\x01\xac\x7c\x0b";
    let add_cases = [(i64::MAX, 1), (i64::MIN, -1), (1_i64 << 53, 1), (-1, 0)]
        .into_iter()
        .map(|(wide, narrow)| Fixture {
            expected: Outcome::I64Invocation(Ok(wide.wrapping_add(i64::from(narrow)))),
            bytes: add.to_vec(),
            export: "x",
            args: vec![WasmValue::I64(wide), WasmValue::I32(narrow)],
        });
    let void_with_i64 = b"\0asm\x01\0\0\0\x01\x05\x01\x60\x01\x7e\0\
                          \x03\x02\x01\0\x07\x05\x01\x01x\0\0\x0a\x0b\x01\x09\0\x20\0\x50\x04\x40\0\x0b\x0b";
    let void_cases = [(1_i64 << 32, Ok(())), (0, Err(WasmTrap::Unreachable))]
        .into_iter()
        .map(|(arg, expected)| Fixture {
            expected: Outcome::VoidInvocation(expected),
            bytes: void_with_i64.to_vec(),
            export: "x",
            args: vec![WasmValue::I64(arg)],
        });
    let f64_const = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7e\
                      \x03\x02\x01\0\x07\x05\x01\x01x\0\0\x0a\x0e\x01\x0c\0\x44\x42\0\0\0\0\0\xf0\x7f\xbd\x0b";
    let f64_roundtrip = b"\0asm\x01\0\0\0\x01\x06\x01\x60\x01\x7e\x01\x7e\
                          \x03\x02\x01\0\x07\x05\x01\x01x\0\0\x0a\x08\x01\x06\0\x20\0\xbf\xbd\x0b";
    let f64_bits = 0x7ff0_0000_0000_0042_i64;
    let float_cases = [
        Fixture {
            expected: Outcome::I64Invocation(Ok(f64_bits)),
            bytes: f64_const.to_vec(),
            export: "x",
            args: vec![],
        },
        Fixture {
            expected: Outcome::I64Invocation(Ok(f64_bits)),
            bytes: f64_roundtrip.to_vec(),
            export: "x",
            args: vec![WasmValue::I64(f64_bits)],
        },
    ];
    let sign_cases = [
        (&[0x99][..], f64_bits | i64::MIN, f64_bits), // abs
        (&[0x9a][..], 0, i64::MIN),                   // neg
        (
            &[0x44, 0, 0, 0, 0, 0, 0, 0, 0x80, 0xa6][..],
            f64_bits,
            f64_bits | i64::MIN,
        ), // copysign
    ]
    .into_iter()
    .map(|(operations, arg, expected)| {
        let mut bytes = b"\0asm\x01\0\0\0\x01\x06\x01\x60\x01\x7e\x01\x7e\
                          \x03\x02\x01\0\x07\x05\x01\x01x\0\0"
            .to_vec();
        let len = 6 + u8::try_from(operations.len()).unwrap();
        bytes.extend([10, len + 2, 1, len, 0, 0x20, 0, 0xbf]);
        bytes.extend(operations);
        bytes.extend([0xbd, 0x0b]);
        Fixture {
            expected: Outcome::I64Invocation(Ok(expected)),
            bytes,
            export: "x",
            args: vec![WasmValue::I64(arg)],
        }
    });
    add_cases
        .chain(void_cases)
        .chain(float_cases)
        .chain(sign_cases)
        .chain(float_host_fixtures())
}

fn float_host_fixtures() -> impl Iterator<Item = Fixture<WasmValue>> {
    let modules = |param, result, op| {
        let mut bytes = b"\0asm\x01\0\0\0\x01\x06\x01\x60\x01".to_vec();
        bytes.extend([param, 1, result]);
        bytes.extend(b"\x03\x02\x01\0\x07\x05\x01\x01x\0\0\x0a\x07\x01\x05\0\x20\0");
        bytes.extend([op, 0x0b]);
        bytes
    };
    // Reinterpret across the guest boundary in both directions independently.
    let mut cases = Vec::new();
    for bits in [0x8000_0000_u32, 1, 0x7f80_0000] {
        cases.push(Fixture {
            expected: Outcome::Invocation(Ok(bits as i32)),
            bytes: modules(0x7d, 0x7f, 0xbc),
            export: "x",
            args: vec![WasmValue::F32(bits)],
        });
        cases.push(Fixture {
            expected: Outcome::F32Invocation(Ok(bits)),
            bytes: modules(0x7f, 0x7d, 0xbe),
            export: "x",
            args: vec![WasmValue::I32(bits as i32)],
        });
    }
    for bits in [0x8000_0000_0000_0000_u64, 1, 0x7ff0_0000_0000_0000] {
        cases.push(Fixture {
            expected: Outcome::I64Invocation(Ok(bits as i64)),
            bytes: modules(0x7c, 0x7e, 0xbd),
            export: "x",
            args: vec![WasmValue::F64(bits)],
        });
        cases.push(Fixture {
            expected: Outcome::F64Invocation(Ok(bits)),
            bytes: modules(0x7e, 0x7c, 0xbf),
            export: "x",
            args: vec![WasmValue::I64(bits as i64)],
        });
    }
    // Exercise all four operations at both widths through the non-NaN host ABI.
    for (opcode, input, expected) in [
        (0x8d, -0.25_f32, -0.0_f32),
        (0x8e, -0.25, -1.0),
        (0x8f, -1.75, -1.0),
        (0x90, -0.5, -0.0),
        (0x90, 3.5, 4.0),
    ] {
        cases.push(Fixture {
            expected: Outcome::F32Invocation(Ok(expected.to_bits())),
            bytes: modules(0x7d, 0x7d, opcode),
            export: "x",
            args: vec![WasmValue::F32(input.to_bits())],
        });
        cases.push(Fixture {
            expected: Outcome::F64Invocation(Ok(f64::from(expected).to_bits())),
            bytes: modules(0x7c, 0x7c, opcode + 0x0e),
            export: "x",
            args: vec![WasmValue::F64(f64::from(input).to_bits())],
        });
    }
    for (opcode, lhs, rhs, expected) in [
        (0x96, 0.0_f32, -0.0_f32, -0.0_f32),
        (0x96, -0.0, 0.0, -0.0),
        (0x97, 0.0, -0.0, 0.0),
        (0x97, -0.0, 0.0, 0.0),
        (0x96, -2.0, -3.0, -3.0),
        (0x97, 2.0, f32::INFINITY, f32::INFINITY),
    ] {
        let binary_module = |ty, op| {
            let mut bytes = b"\0asm\x01\0\0\0\x01\x07\x01\x60\x02".to_vec();
            bytes.extend([ty, ty, 1, ty]);
            bytes.extend(b"\x03\x02\x01\0\x07\x05\x01\x01x\0\0\x0a\x09\x01\x07\0\x20\0\x20\x01");
            bytes.extend([op, 0x0b]);
            bytes
        };
        cases.push(Fixture {
            expected: Outcome::F32Invocation(Ok(expected.to_bits())),
            bytes: binary_module(0x7d, opcode),
            export: "x",
            args: vec![WasmValue::F32(lhs.to_bits()), WasmValue::F32(rhs.to_bits())],
        });
        cases.push(Fixture {
            expected: Outcome::F64Invocation(Ok(f64::from(expected).to_bits())),
            bytes: binary_module(0x7c, opcode + 0x0e),
            export: "x",
            args: vec![
                WasmValue::F64(f64::from(lhs).to_bits()),
                WasmValue::F64(f64::from(rhs).to_bits()),
            ],
        });
    }
    cases.into_iter()
}

fn with_data(segments: &[(i32, &[u8])], address: i32, expected: Outcome) -> Fixture {
    let mut bytes = singleton_module(1, &[0], &[0x20, 0, 0x28, 2, 0], Some(1));
    let mut payload = vec![u8::try_from(segments.len()).unwrap()];
    for &(offset, data) in segments {
        payload.extend([0, 0x41]); // Memory index zero, i32.const.
        payload.extend(sleb32(offset, 1));
        assert!(data.len() < 128);
        payload.extend([0x0b, data.len() as u8]);
        payload.extend_from_slice(data);
    }
    assert!(payload.len() < 128);
    bytes.extend([11, payload.len() as u8]);
    bytes.extend(payload);
    Fixture {
        expected,
        bytes,
        export: EXPORT,
        args: vec![address],
    }
}

fn with_elements(segments: &[(i32, &[u8])], expected: Outcome) -> Fixture {
    let mut bytes = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7f\x03\x02\x01\0\
                      \x04\x04\x01\x70\0\x02\x07\x05\x01\x01x\0\0"
        .to_vec();
    let mut payload = vec![u8::try_from(segments.len()).unwrap()];
    for &(offset, functions) in segments {
        payload.extend([0, 0x41]); // Table index zero, i32.const.
        payload.extend(sleb32(offset, 1));
        assert!(functions.len() < 128);
        payload.extend([0x0b, functions.len() as u8]);
        payload.extend_from_slice(functions);
    }
    assert!(payload.len() < 128);
    bytes.extend([9, payload.len() as u8]);
    bytes.extend(payload);
    bytes.extend([10, 6, 1, 4, 0, 0x41, 42, 0x0b]);
    Fixture {
        expected,
        bytes,
        export: "x",
        args: vec![],
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|&byte| [DIGITS[(byte >> 4) as usize], DIGITS[(byte & 15) as usize]])
        .map(char::from)
        .collect()
}

fn evaluate(fixture: &Fixture<WasmValue>) -> Result<Outcome, Box<dyn Error>> {
    let module = validate_module(&fixture.bytes)?;
    let instance = match module.instantiate() {
        Ok(instance) => instance,
        Err(WasmInstantiationError::DataSegmentOutOfBounds { segment_index }) => {
            return Ok(Outcome::DataSegmentOutOfBounds(segment_index));
        }
        Err(WasmInstantiationError::ElementSegmentOutOfBounds { segment_index }) => {
            return Ok(Outcome::ElementSegmentOutOfBounds(segment_index));
        }
        Err(WasmInstantiationError::StartTrap(trap)) => return Ok(Outcome::StartTrap(trap)),
        Err(error) => return Err(error.into()),
    };
    let expected_kind = fixture.expected.result_kind();
    match instance.invoke_with_values(fixture.export, &fixture.args, expected_kind) {
        Ok(Some(WasmValue::I32(value))) => Ok(Outcome::Invocation(Ok(value))),
        Ok(Some(WasmValue::I64(value))) => Ok(Outcome::I64Invocation(Ok(value))),
        Ok(Some(WasmValue::F32(bits))) => Ok(Outcome::F32Invocation(Ok(bits))),
        Ok(Some(WasmValue::F64(bits))) => Ok(Outcome::F64Invocation(Ok(bits))),
        Ok(None) => Ok(Outcome::VoidInvocation(Ok(()))),
        Err(WasmInvokeError::Trap(trap)) => Ok(match expected_kind {
            WasmResultKind::Void => Outcome::VoidInvocation(Err(trap)),
            WasmResultKind::I32 => Outcome::Invocation(Err(trap)),
            WasmResultKind::I64 => Outcome::I64Invocation(Err(trap)),
            WasmResultKind::F32 => Outcome::F32Invocation(Err(trap)),
            WasmResultKind::F64 => Outcome::F64Invocation(Err(trap)),
        }),
        Err(host_error) => Err(host_error.into()),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let cases: Vec<_> = fixtures()
        .into_iter()
        .map(Fixture::into_typed)
        .chain(typed_fixtures())
        .collect();
    let encoded: Vec<_> = cases.iter().map(Fixture::encode).collect();
    // Only generated modules reach the fixed driver, never files or user inputs.
    let output = Command::new("node")
        .env_remove("NODE_OPTIONS")
        .args(["-e", REFERENCE, "--"])
        .args(&encoded)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Node failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let stdout = std::str::from_utf8(&output.stdout)?;
    let mut lines = stdout.lines();
    let version = lines.next().ok_or("missing Node version")?;
    let count = lines.clone().count();
    if count != cases.len() {
        return Err(format!("Node returned {count} results for {} fixtures", cases.len()).into());
    }
    for ((fixture, encoded), reference) in cases.iter().zip(encoded).zip(lines) {
        let expected = fixture.expected;
        let actual = evaluate(fixture).map_err(|error| format!("fixture {encoded}: {error}"))?;
        let agrees = match actual {
            Outcome::Invocation(Ok(value)) => reference.parse::<i32>().ok() == Some(value),
            Outcome::I64Invocation(Ok(value)) => {
                reference
                    .strip_suffix('n')
                    .and_then(|text| text.parse::<i64>().ok())
                    == Some(value)
            }
            Outcome::F32Invocation(Ok(bits)) => {
                reference
                    .strip_prefix("f32=")
                    .and_then(|text| u32::from_str_radix(text, 16).ok())
                    == Some(bits)
            }
            Outcome::F64Invocation(Ok(bits)) => {
                reference
                    .strip_prefix("f64=")
                    .and_then(|text| u64::from_str_radix(text, 16).ok())
                    == Some(bits)
            }
            Outcome::VoidInvocation(Ok(())) => reference == "void",
            Outcome::Invocation(Err(_))
            | Outcome::I64Invocation(Err(_))
            | Outcome::F32Invocation(Err(_))
            | Outcome::F64Invocation(Err(_))
            | Outcome::VoidInvocation(Err(_)) => reference == "trap",
            Outcome::DataSegmentOutOfBounds(_)
            | Outcome::ElementSegmentOutOfBounds(_)
            | Outcome::StartTrap(_) => reference == "instantiation-error",
        };
        if !agrees || actual != expected {
            return Err(format!(
                "fixture {encoded}: expected {expected:?}, Swayze {actual:?}, Node {version} {reference}"
            )
            .into());
        }
    }
    println!(
        "Matched all {count} fixtures against Node {version} (integer, non-NaN float, or void host results, invocation traps, or instantiation errors)"
    );
    Ok(())
}
