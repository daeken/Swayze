//! Opt-in differential Wasm i32 smoke check: run tools/check-wasm-reference.sh.

use std::{error::Error, process::Command};
use swayze_engine::validate_module;

const EXPORT: &str = "answer";
const REFERENCE: &str = r#"
if (process.versions.node.split('.')[0] !== '22') {
    throw new Error(`expected Node.js v22, got ${process.version}`);
}
const [name, ...fixtures] = process.argv.slice(1);
console.log(process.version);
for (const hex of fixtures) {
    const module = new WebAssembly.Module(Buffer.from(hex, 'hex'));
    const value = new WebAssembly.Instance(module).exports[name]();
    if ((value | 0) !== value) throw new Error(`expected signed i32 for ${hex}`);
    console.log(value);
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

fn singleton_module(expression: &[u8]) -> Vec<u8> {
    let mut module = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7f\x03\x02\x01\0".to_vec();
    let name_len = EXPORT.len() as u8;
    module.extend([7, name_len + 4, 1, name_len]); // Export section, one name.
    module.extend(EXPORT.bytes());
    module.extend([0, 0]); // Function export, index 0.

    // These tiny bodies fit single-byte section and body lengths.
    assert!(expression.len() + 4 < 128);
    let body_len = expression.len() as u8 + 2;
    module.extend([10, body_len + 2, 1, body_len, 0]); // One body, no locals.
    module.extend_from_slice(expression);
    module.push(0x0b); // end
    module
}

fn fixtures() -> Vec<(i32, Vec<u8>)> {
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
            cases.push((value, singleton_module(&expression)));
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
            cases.push((a.wrapping_sub(op(b, c)), singleton_module(&expression)));
        }
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

fn evaluate(bytes: &[u8]) -> Result<i32, Box<dyn Error>> {
    Ok(validate_module(bytes)?.instantiate().invoke_i32(EXPORT)?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let cases = fixtures();
    let encoded: Vec<_> = cases.iter().map(|(_, bytes)| hex(bytes)).collect();
    // Only generated modules reach the fixed driver, never files or user inputs.
    let output = Command::new("node")
        .env_remove("NODE_OPTIONS")
        .args(["-e", REFERENCE, "--", EXPORT])
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
    for (((expected, bytes), hex), reference) in cases.iter().zip(encoded).zip(lines) {
        let actual = evaluate(bytes).map_err(|error| format!("module {hex}: {error}"))?;
        if reference.parse::<i32>()? != actual || actual != *expected {
            return Err(format!(
                "module {hex}: expected {expected}, Swayze {actual}, Node {version} {reference}"
            )
            .into());
        }
    }
    println!("Matched all {count} fixtures against Node {version} (signed i32)");
    Ok(())
}
