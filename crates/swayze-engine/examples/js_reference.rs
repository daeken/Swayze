//! Opt-in differential JS value smoke check: run tools/check-js-reference.sh.

use std::{error::Error, process::Command};
use swayze_engine::{JsValue, parse_script};

// Evaluate complete Scripts (including optional semicolons), not expressions
// rewritten by the harness. Print binary64 in big-endian hex; ignore NaN payloads.
const REFERENCE: &str = r#"
const { runInNewContext } = require('node:vm');
const bytes = Buffer.alloc(8);
console.log(process.version);
for (const [i, source] of process.argv.slice(1).entries()) {
    const value = runInNewContext(source, {}, { filename: `fixture-${i + 1}` });
    if (value === null) {
        console.log('null');
    } else if (value === undefined) {
        console.log('undefined');
    } else if (typeof value === 'boolean') {
        console.log(`boolean:${value}`);
    } else if (typeof value === 'number') {
        bytes.writeDoubleBE(value);
        console.log(`number:${Number.isNaN(value) ? 'nan' : bytes.toString('hex')}`);
    } else {
        throw new Error(`fixture ${i + 1} returned unsupported type ${typeof value}`);
    }
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
            "let x = 2; const step = 5; x = x * step; x + 1;",
            "let x = .5; x = -x; let y = x; x = 4; y - x",
            "let x = 2; x /* assignment */ = x + 5; x",
            "let x = -0; x = x * 2; 1 / x",
            "let x = 1; x = 0 / 0; x",
            "let x = 3; let y = x * 2; x = y - x; y = y / x; x - y",
            "-0 ? 2 : 3",
            "0 / 0 ? 2 : 3",
            "1 / -0 ? 2 : 3",
            "0 ? 1 : 0 ? 2 : 3",
            "1 ? 0 ? 2 : 3 : 4",
            "(0 ? 1 : 2) * 3 + (1 ? 4 : 5)",
            "2 - 2 ? 1 : -0",
            "let x = 2; const y = x ? 3 : 4; x = y - 3 ? x : y; x",
            "1 ? /* branch */ 0 : 2 ? 3 : 4",
            "true",
            "false;",
            "true ? false : 1",
            "false ? 1 : true",
            "(0 / 0) ? 1 : true",
            "let x = true; x = x ? -0 : false; x",
            "let x = 1; x = false; x",
            "const trueValue = false; trueValue ? 10 : 11",
            "true + true * true",
            "false - true",
            "-false",
            "+true",
            "true / false",
            "false % false",
            "1 === true",
            "1 !== true",
            "-0 === +0",
            "0 / 0 === 0 / 0",
            "0 / 0 !== 0 / 0",
            "1 / 0 === 1 / 0",
            "false <= -0",
            "true > false",
            "true >= 1 / 0",
            "0 / 0 < 1",
            "0 / 0 <= 1",
            "1 > 0 / 0",
            "1 >= 0 / 0",
            "1 + 1 < 3 === true ? 42 : -0",
            "3 > 2 > 1",
            "0 === false !== true",
            "let x = 1; x = x < 2; x === true",
            "!!0",
            "!(-0)",
            "!(0 / 0)",
            "!(1 / 0)",
            "-0 && true",
            "false && 7",
            "1 || false",
            "0 || false",
            "(0 / 0) && 7",
            "(0 / 0) || -0",
            "true && -0",
            "2 || 0 && 3",
            "0 && 2 || 3",
            "1 < 2 && 2 !== true ? 42 : -0",
            "0 || 1 ? false && 4 : true",
            "let x = 0; x = x || 2; x && !false",
            "let x = 0; if (true) { x = 42; } else { x = 2; } x",
            "let x = 0; if (-0) { x = 42; } else { x = false; } x",
            "let x = -0; if (0 / 0) { x = 42; } x",
            "let x = 1; if (x === 1 && true) { x = x + 1; x = x * 5; } x + 1",
            "let x = 1; if (false) {} else {} if (x) { x = true; } x",
            "let x = false; if ((1 ? 2 : 3) < 3) { x = true; } const answer = x ? 42 : 0; answer",
            "let x = 1; while (x < 40) { x = x * 2; } x",
            "let x = true; let n = 41; while (x) { x = false; n = n + 1; } n",
            "let x = -0; while (x || 0 / 0) { x = 42; } x",
            "let x = 4; let y = 0; while (x !== 0 && true) { y = y + x; x = x - 1; } y",
            "let x = 0; while (false) {} while (x < 2) { x = x + 1; } if (x) { x = x * 21; } x",
            "let x = 0; if (true) { if (false) { x = 9; } else { x = 42; } } else { x = 7; } x",
            "let x = 0; if (true) { if (false) { x = 9; } } else { x = 7; } x",
            "let x = 3; let y = 0; while (x) { if (x > 1) { y = y + x; } else { y = y * 2; } x = x - 1; } y",
            "let x = 3; let y = 0; let z = 0; while (x) { y = x; while (y) { z = z + 1; y = y - 1; } x = x - 1; } z",
            "let x = -0; if (false) { while (true) { x = 42; } } else { while (x) { x = false; } } x",
            "let x = 0; while (true) { x = 42; break; x = 9; } x",
            "let x = 4; let y = 0; while (x) { x = x - 1; if (x === 2) { continue; } y = y + x; } y",
            "let x = 2; let y = 0; while (x) { while (true) { y = y + x; break; } x = x - 1; } y",
            "let x = 0; while (x < 6) { x = x + 1; if (x < 3) { continue; } else { break; } } x",
            "let out = 0; if (true) { const item = 42; out = item; } else { let item = 1; out = item; } out",
            "let out = 0; if (false) { const item = 42; out = item; } else { let item = 2; item = item + 3; out = item; } out",
            "let i = 0; let out = 0; while (i < 4) { i = i + 1; if (i === 2) { continue; } const item = i * 2; out = out + item; } out",
            "let out = 0; while (true) { let item = 42; if (item) { out = item; break; } } out",
            "let x = 3; let out = 0; if (x) { let x = 40; x = x + 2; out = x; } out + x",
            "let out = 0; if (true) { if (true) { const x = 42; out = x; } const x = 3; out = out + x; } out",
            "let i = 2; let out = 0; while (i) { i = i - 1; if (true) { const i = 21; out = out + i; } } out",
            "null",
            "undefined",
            "null ?? undefined",
            "undefined ?? null",
            "null ?? undefined ?? 42",
            "false ?? true",
            "-0 ?? 42",
            "(0 / 0) ?? 42",
            "null + false",
            "undefined + 42",
            "null === undefined",
            "null <= false",
            "undefined >= undefined",
            "(false || null) ?? 42",
            "(undefined ?? true) && null",
            "null ?? false ? 1 : undefined",
            "let out = 0; if (true) { let undefined = 21; out = undefined ?? 42; } out",
            "let out = undefined; if (true) { const undefined = null; out = undefined; } out",
            "~undefined",
            "~(+null)",
            "4294967297.75 & -1",
            "-4294967297.75 ^ true",
            "9007199254740991 | 0",
            "1.7976931348623157e308 | 0",
            "(1 / 0) ^ (0 / 0)",
            "-1 >>> 0",
            "-2147483648 >> -1",
            "-2147483648 >>> -1",
            "1 << 65.9",
            "1 << 1 + 1 === 4",
            "1 | 2 ^ 3 & 1",
            "8 >> 1 < 5 && 9",
            "null ?? false | true",
            "false | true && 42",
            "(-0 ^ null) ?? 42",
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

fn evaluate(source: &str) -> Result<JsValue, Box<dyn Error>> {
    Ok(parse_script(source)?.evaluate_value()?)
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
        let actual = match value {
            JsValue::Number(value) if value.is_nan() => "number:nan".to_owned(),
            JsValue::Number(value) => format!("number:{:016x}", value.to_bits()),
            JsValue::Boolean(value) => format!("boolean:{value}"),
            JsValue::Null => "null".to_owned(),
            JsValue::Undefined => "undefined".to_owned(),
        };
        if actual != reference {
            return Err(format!(
                "fixture {} {source:?}: Swayze {actual}, Node {version} {reference}",
                i + 1
            )
            .into());
        }
    }
    println!(
        "Matched all {count} fixtures against Node {version} (exact value tags and payloads, ignoring NaN payload bits)"
    );
    Ok(())
}
