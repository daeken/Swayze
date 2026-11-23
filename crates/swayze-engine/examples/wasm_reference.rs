//! Opt-in differential Wasm smoke check: run tools/check-wasm-reference.sh.

use std::{error::Error, process::Command};
use swayze_engine::{WasmInstantiationError, WasmInvokeError, WasmTrap, validate_module};

const EXPORT: &str = "answer";
const REFERENCE: &str = r#"
if (process.versions.node.split('.')[0] !== '22') {
    throw new Error(`expected Node.js v22, got ${process.version}`);
}
const fixtures = process.argv.slice(1);
console.log(process.version);
for (const fixture of fixtures) {
    const [hex, nameBytes, ...args] = fixture.split(':');
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
        const value = invoke(...args.map(Number));
        if (value === undefined) {
            console.log('void');
        } else {
            if ((value | 0) !== value) throw new Error(`expected signed i32 for ${hex}`);
            console.log(value);
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
    VoidInvocation(Result<(), WasmTrap>),
    DataSegmentOutOfBounds(usize),
}

struct Fixture {
    expected: Outcome,
    bytes: Vec<u8>,
    export: &'static str,
    args: Vec<i32>,
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

    fn encode(&self) -> String {
        use std::fmt::Write;
        let mut encoded = format!("{}:{}", hex(&self.bytes), hex(self.export.as_bytes()));
        for arg in &self.args {
            write!(&mut encoded, ":{arg}").unwrap();
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
    cases
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

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|&byte| [DIGITS[(byte >> 4) as usize], DIGITS[(byte & 15) as usize]])
        .map(char::from)
        .collect()
}

fn evaluate(fixture: &Fixture) -> Result<Outcome, Box<dyn Error>> {
    let module = validate_module(&fixture.bytes)?;
    let instance = match module.instantiate() {
        Ok(instance) => instance,
        Err(WasmInstantiationError::DataSegmentOutOfBounds { segment_index }) => {
            return Ok(Outcome::DataSegmentOutOfBounds(segment_index));
        }
        Err(error) => return Err(error.into()),
    };
    if matches!(fixture.expected, Outcome::VoidInvocation(_)) {
        return match instance.invoke_void_with_args(fixture.export, &fixture.args) {
            Ok(()) => Ok(Outcome::VoidInvocation(Ok(()))),
            Err(WasmInvokeError::Trap(trap)) => Ok(Outcome::VoidInvocation(Err(trap))),
            Err(host_error) => Err(host_error.into()),
        };
    }
    match instance.invoke_i32_with_args(fixture.export, &fixture.args) {
        Ok(value) => Ok(Outcome::Invocation(Ok(value))),
        Err(WasmInvokeError::Trap(trap)) => Ok(Outcome::Invocation(Err(trap))),
        Err(host_error) => Err(host_error.into()),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let cases = fixtures();
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
            Outcome::VoidInvocation(Ok(())) => reference == "void",
            Outcome::Invocation(Err(_)) | Outcome::VoidInvocation(Err(_)) => reference == "trap",
            Outcome::DataSegmentOutOfBounds(_) => reference == "instantiation-error",
        };
        if !agrees || actual != expected {
            return Err(format!(
                "fixture {encoded}: expected {expected:?}, Swayze {actual:?}, Node {version} {reference}"
            )
            .into());
        }
    }
    println!(
        "Matched all {count} fixtures against Node {version} (i32 or void results, invocation traps, or instantiation errors)"
    );
    Ok(())
}
