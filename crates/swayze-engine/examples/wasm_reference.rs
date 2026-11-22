//! Opt-in differential Wasm i32 smoke check: run tools/check-wasm-reference.sh.

use std::{error::Error, process::Command};
use swayze_engine::{WasmInvokeError, WasmTrap, validate_module};

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
    const invoke = new WebAssembly.Instance(module).exports[name];
    try {
        const value = invoke(...args.map(Number));
        if ((value | 0) !== value) throw new Error(`expected signed i32 for ${hex}`);
        console.log(value);
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

fn singleton_module(parameter_count: u8, locals: &[u8], expression: &[u8]) -> Vec<u8> {
    let mut module = b"\0asm\x01\0\0\0".to_vec();
    module.extend([1, 5 + parameter_count, 1, 0x60, parameter_count]);
    module.extend(std::iter::repeat_n(0x7f, usize::from(parameter_count)));
    module.extend([1, 0x7f, 3, 2, 1, 0]); // One i32 result; one function of type 0.
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

struct Fixture {
    expected: Result<i32, WasmTrap>,
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
            expected,
            bytes: singleton_module(u8::try_from(args.len()).unwrap(), locals, expression),
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
            expected: Ok(expected),
            bytes: multi.to_vec(),
            export,
            args: vec![arg],
        });
    }
    cases
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|&byte| [DIGITS[(byte >> 4) as usize], DIGITS[(byte & 15) as usize]])
        .map(char::from)
        .collect()
}

fn evaluate(fixture: &Fixture) -> Result<Result<i32, WasmTrap>, Box<dyn Error>> {
    match validate_module(&fixture.bytes)?
        .instantiate()
        .invoke_i32_with_args(fixture.export, &fixture.args)
    {
        Ok(value) => Ok(Ok(value)),
        Err(WasmInvokeError::Trap(trap)) => Ok(Err(trap)),
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
            Ok(value) => reference.parse::<i32>().ok() == Some(value),
            Err(_) => reference == "trap",
        };
        if !agrees || actual != expected {
            return Err(format!(
                "fixture {encoded}: expected {expected:?}, Swayze {actual:?}, Node {version} {reference}"
            )
            .into());
        }
    }
    println!("Matched all {count} fixtures against Node {version} (i32 results or runtime traps)");
    Ok(())
}
