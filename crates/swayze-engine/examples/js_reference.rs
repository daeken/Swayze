//! Opt-in differential Number smoke check: run tools/check-js-reference.sh.

use std::{error::Error, process::Command};
use swayze_engine::parse_script;

// Evaluate complete Scripts (including optional semicolons), not expressions
// rewritten by the harness. Print binary64 in big-endian hex; ignore NaN payloads.
const REFERENCE: &str = r#"
const { runInNewContext } = require('node:vm');
const bytes = Buffer.alloc(8);
console.log(process.version);
for (const [i, source] of process.argv.slice(1).entries()) {
    const value = runInNewContext(source, {}, { filename: `fixture-${i + 1}` });
    if (typeof value !== 'number') throw new Error(`fixture ${i + 1} is not a Number`);
    bytes.writeDoubleBE(value);
    console.log(Number.isNaN(value) ? 'nan' : bytes.toString('hex'));
}
"#;

fn fixtures() -> Vec<String> {
    let mut cases = Vec::new();
    // All ordered pairs of operators, with default, left and right grouping.
    for (a, b, c) in [
        ("20", "3", "4"),
        ("0", "1", "(0 - 1)"),
        ("9007199254740992", "1", "9007199254740992"),
        ("134217729", "134217727", "134217728"),
    ] {
        for op in ['+', '-', '*', '/', '%'] {
            for next in ['+', '-', '*', '/', '%'] {
                cases.extend([
                    format!("{a} {op} {b} {next} {c}"),
                    format!("({a} {op} {b}) {next} {c};"),
                    format!("{a} {op} ({b} {next} {c})"),
                ]);
            }
        }
    }

    cases.extend(
        [
            "\u{feff}\t1\u{00a0}+\u{2028}2\r\n; ",
            "9007199254740991",                              // 2^53 - 1
            "9007199254740993",                              // ties to even, down
            "9007199254740995",                              // ties to even, up
            "18446744073709551615",                          // u64::MAX rounds to 2^64
            "18446744073709551616",                          // above u64::MAX
            "134217729 * 134217727 - 134217728 * 134217728", // round before subtract
            "0 * (0 - 1)",
            "(0 * (0 - 1)) * (0 - 1)",
            "+ - + 42",
            "-0",
            "- -0",
            "1 - -2",
            "-(2 + 3) * -4",
            "1 / -0",
            "0 / 0",
            "-4 % 2",
            "-7 % 3",
            "7 % -3",
            "(7 / 2) % (3 / 2)",
            "1 % 0",
            "(1 / 0) % 3",
            "7 % (1 / 0)",
            ".5 + 1.",
            "1.e2 + .25e+2",
            "0.1 + 0.2",
            "-1E-99999",
            "5e-324",
            "2.4703282292062327e-324",
            "2.4703282292062328e-324",
            "2.225073858507201e-308",
            "2.2250738585072014e-308",
            "1.7976931348623157e308",
            "1.7976931348623159e308",
            "9007199254740993.0",
            "0e99999999999999999999999999",
            "const x = 2; const y = x * 5; y + 1;",
            "const x = 1; const X = 4; x - X",
            "const const0 = .5; const $x = const0 * 1e2; $x / const0;",
            "const x = -0; const y = x * 2; 1 / y",
            "const x = 1 / 0; const y = x - x; y",
            "const x = 9007199254740992; const y = x + 1; y - x",
            "const x\n=\n3; const y = x + 2; y * (x - 1)",
            "const x = 6; const unused = x / 0; x",
            "const x = 2; x",
            "const x = 3; x",
            "/* before */ const x/* binding */=2; x/**/+3; // trailing",
            "1 // line comment\r\n + 2",
            "1 // unicode\u{2028} * 3",
            "1 // unicode\u{2029} - 3",
            "1 + /* \n 👋 */ 2",
            "8 / /* divisor */ 2",
            "8 / // divisor follows\n 2",
            "2 /****/+ 3",
            "2 /* /* nested opener ignored */ + 3",
        ]
        .map(str::to_owned),
    );

    // 2^1024 - 2^970 is the 309-digit midpoint that rounds to infinity.
    let prefix = concat!(
        "17976931348623158079372897140530341507993413271003782693617377898044496829276475",
        "094664901797758720709633028641669288791094655554785194040263065748867150582068",
        "190890200070838367627385484581771153176447573027006985557136695962284291481986",
        "083493647529271907416844436551070434271155969950809304288017790417449779",
    );
    let finite = format!("{prefix}1");
    let huge = format!("1{}", "0".repeat(309));
    cases.extend([
        format!("1{}e-350", "0".repeat(350)),
        format!(".{}1e350", "0".repeat(349)),
        finite.clone(),
        format!("{prefix}2"),
        huge.clone(),
        format!("{finite} + {finite}"),
        format!("(0 - 2) * {finite}"),
        format!("{huge} - {huge}"),
        format!("0 * {huge}"),
        format!("{huge} + (0 - {huge})"),
    ]);
    cases
}

fn evaluate(source: &str) -> Result<f64, Box<dyn Error>> {
    Ok(parse_script(source)?.evaluate()?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let cases = fixtures();
    // Only the fixed driver and generated fixtures reach Node; no shell, stdin,
    // CLI inputs or corpus. `--` separates fixtures from Node options.
    let output = Command::new("node")
        .env_remove("NODE_OPTIONS")
        .args(["-e", REFERENCE, "--"])
        .args(&cases)
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
    for (i, (source, reference)) in cases.iter().zip(lines).enumerate() {
        let value =
            evaluate(source).map_err(|error| format!("fixture {} {source:?}: {error}", i + 1))?;
        let actual = if value.is_nan() {
            "nan".to_owned()
        } else {
            format!("{:016x}", value.to_bits())
        };
        if actual != reference {
            return Err(format!(
                "fixture {} {source:?}: Swayze {actual}, Node {version} {reference}",
                i + 1
            )
            .into());
        }
    }
    println!("Matched all {count} fixtures against Node {version} (f64 bits or NaN)");
    Ok(())
}
