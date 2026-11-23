//! Opt-in slice runner, not the Test262 harness. Its only corpus input is a
//! reviewed manifest in the current superproject commit and the pinned Git tree.
//!
//! TSV rows: `raw PATH` or `adapted PATH CALL ACTUAL EXPECTED [MESSAGE]`,
//! with tabs between fields and zero-based half-open byte spans `START:END`.
//! Review each whole blob; the call span includes the final semicolon.

use std::{
    ffi::OsStr,
    io::Read,
    ops::Range,
    path::Path,
    process::{Command, ExitCode, Stdio},
};
use swayze_engine::{
    JsErrorKind, JsEvalError, JsEvalScratch, JsLimit, JsPrimitiveValue, JsValue, parse_script,
};

const PIN: &str = "2a1ade303f72d3de80565c921d685222405c32ce";
const MANIFEST: &str = "tools/test262-cases.tsv";
const SUBMODULE: &str = "vendor/test262";
const MAX_CASES: usize = 32;
const MAX_BYTES: usize = 8 * 1024; // Per blob, including the manifest.
const ACTUAL: &str = "__swayze262_actual";
const EXPECTED: &str = "__swayze262_expected";

type Diagnostic = &'static str;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Pass,
    Unsupported(Diagnostic),
    Fail(Diagnostic),
    Infra(Diagnostic),
}

struct Assertion {
    call: Range<usize>,
    actual: Range<usize>,
    expected: Range<usize>,
    message: Option<Range<usize>>,
}

struct Case<'a> {
    path: &'a str,
    assertion: Option<Assertion>,
}

impl Case<'_> {
    fn mode(&self) -> &'static str {
        if self.assertion.is_some() {
            "adapted"
        } else {
            "raw"
        }
    }
}

fn span(text: &str) -> Option<Range<usize>> {
    let (start, end) = text.split_once(':')?;
    let (start, end) = (start.parse().ok()?, end.parse().ok()?);
    (start < end && end <= MAX_BYTES).then_some(start..end)
}

fn manifest(text: &str) -> Option<Vec<Case<'_>>> {
    let mut cases: Vec<Case<'_>> = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let mode = fields.next()?;
        let path = fields.next()?;
        if cases.len() == MAX_CASES
            || path.len() > 240
            || !path.starts_with("test/")
            || !path.ends_with(".js")
            || path.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
            || cases.iter().any(|case| case.path == path)
        {
            return None;
        }
        let assertion = match mode {
            "raw" => None,
            "adapted" => Some(Assertion {
                call: span(fields.next()?)?,
                actual: span(fields.next()?)?,
                expected: span(fields.next()?)?,
                message: match fields.next() {
                    Some(field) => Some(span(field)?),
                    None => None,
                },
            }),
            _ => return None,
        };
        if fields.next().is_some() {
            return None;
        }
        cases.push(Case { path, assertion });
    }
    Some(cases)
}

// No config from the invoking environment, replacement objects, lazy fetches,
// filters, pathspec magic, or unbounded stdout/stderr capture.
fn git(repo: &Path, args: &[&str], max: usize) -> Result<Vec<u8>, Diagnostic> {
    let mut child = Command::new("git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "--no-replace-objects",
            "--no-lazy-fetch",
            "--literal-pathspecs",
            "-c",
            "protocol.allow=never",
        ])
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "cannot launch Git")?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("piped Git stdout")
        .take((max + 1) as u64)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > max {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|_| "cannot wait for Git")?;
    if bytes.len() > max {
        Err("Git output exceeds byte cap")
    } else if read.is_err() || !status.success() {
        Err("cannot read Git object or revision")
    } else {
        Ok(bytes)
    }
}

fn tree_entry(repo: &Path, commit: &str, path: &str, kind: &str) -> Result<String, Diagnostic> {
    let bytes = git(repo, &["ls-tree", "--full-tree", commit, "--", path], 512)?;
    let entry = std::str::from_utf8(&bytes).map_err(|_| "non-ASCII Git tree entry")?;
    let oid = entry
        .strip_prefix(kind)
        .and_then(|line| line.strip_suffix(&format!("\t{path}\n")))
        .filter(|oid| oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("missing or unexpected Git tree entry")?;
    Ok(oid.to_owned())
}

fn blob(repo: &Path, commit: &str, path: &str) -> Result<String, Diagnostic> {
    let oid = tree_entry(repo, commit, path, "100644 blob ")?;
    String::from_utf8(git(repo, &["cat-file", "blob", &oid], MAX_BYTES)?)
        .map_err(|_| "blob is not UTF-8")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variants {
    Raw,
    Sloppy,
    Strict,
    Both,
}

// A fail-closed subset of the format, not a YAML parser. Each committed row
// still requires reviewing the entire blob for harness use and variant intent.
fn metadata(source: &str) -> Result<Variants, Diagnostic> {
    let (_, rest) = source
        .split_once("/*---\n")
        .ok_or("no supported frontmatter")?;
    let (header, _) = rest
        .split_once("\n---*/")
        .ok_or("no supported frontmatter")?;
    let mut flags = None;
    let mut block = false;
    for line in header.lines() {
        if line.trim_ascii().is_empty() || (block && line.starts_with(' ')) {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or("unsupported metadata layout")?;
        let value = value.trim_ascii();
        block = false;
        match name {
            "flags" if flags.is_none() => flags = Some(value),
            "description" | "info" => block = matches!(value, "|" | ">" | "|-" | ">-"),
            "author" | "esid" | "es5id" | "es6id" | "features" => (),
            _ => return Err("unsupported metadata key (e.g. includes or negative)"),
        }
    }
    match flags {
        Some("[raw]") => Ok(Variants::Raw),
        Some("[noStrict]") => Ok(Variants::Sloppy),
        Some("[onlyStrict]") => Ok(Variants::Strict),
        None | Some("[]") => Ok(Variants::Both),
        _ => Err("unsupported Test262 flags"),
    }
}

fn is_quoted_literal(source: &str) -> bool {
    let bytes = source.as_bytes();
    let [quote @ (b'\'' | b'"'), inner @ .., end] = bytes else {
        return false;
    };
    let mut escaped = false;
    for &byte in inner {
        if matches!(byte, b'\r' | b'\n') || (!escaped && byte == *quote) {
            return false;
        }
        escaped = !escaped && byte == b'\\';
    }
    !escaped && end == quote
}

impl Assertion {
    // Structural anchors for reviewed spans, not an argument splitter or a JS
    // lexer. The pinned blob review must establish a top-level terminal call,
    // the exact argument boundaries, and no dependence on the harness.
    fn scripts(&self, source: &str) -> Option<(String, String)> {
        let prefix = source.get(..self.call.start)?;
        let suffix = source.get(self.call.end..)?;
        if !prefix.rsplit('\n').next()?.trim_ascii().is_empty()
            || !suffix.trim_ascii().is_empty()
            || source.contains(ACTUAL)
            || source.contains(EXPECTED)
            || source
                .get(self.call.start..self.actual.start)?
                .trim_ascii_end()
                != "assert.sameValue("
            || source
                .get(self.actual.end..self.expected.start)?
                .trim_ascii()
                != ","
        {
            return None;
        }
        let actual = source.get(self.actual.clone())?;
        let expected = source.get(self.expected.clone())?;
        let (last_end, message) = if let Some(span) = &self.message {
            let message = source.get(span.clone())?;
            if source.get(self.expected.end..span.start)?.trim_ascii() != ","
                || !is_quoted_literal(message)
            {
                return None;
            }
            (span.end, format!("({message});"))
        } else {
            (self.expected.end, String::new())
        };
        if source.get(last_end..self.call.end)?.trim_ascii_start() != ");" {
            return None;
        }
        // The bindings cannot capture names in the admitted parser slice (it
        // declines escaped/non-ASCII identifiers). Both deterministic replays
        // run both operands in order, even when an operand has side effects.
        let common =
            format!("{prefix};const {ACTUAL}=({actual});const {EXPECTED}=({expected});{message}");
        Some((
            format!("{common}({ACTUAL});{suffix}"),
            format!("{common}({EXPECTED});{suffix}"),
        ))
    }
}

fn same_value(lhs: JsPrimitiveValue<'_>, rhs: JsPrimitiveValue<'_>) -> bool {
    use JsPrimitiveValue::{Scalar, String};
    match (lhs, rhs) {
        (Scalar(JsValue::Number(a)), Scalar(JsValue::Number(b))) => {
            a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
        }
        (Scalar(a), Scalar(b)) => a == b,
        (String(a), String(b)) => a == b,
        _ => false,
    }
}

fn evaluate(sources: (&str, Option<&str>)) -> Outcome {
    fn parse(source: &str) -> Result<swayze_engine::Script, Outcome> {
        parse_script(source).map_err(|error| match error.kind() {
            JsErrorKind::Unsupported => Outcome::Unsupported(error.message()),
            JsErrorKind::LimitExceeded(JsLimit::Allocation) => Outcome::Infra(error.message()),
            JsErrorKind::LimitExceeded(_) => Outcome::Unsupported(error.message()),
            JsErrorKind::Malformed => Outcome::Fail(error.message()),
        })
    }
    fn eval_error(error: JsEvalError) -> Outcome {
        match error {
            JsEvalError::UnsupportedStringOperation | JsEvalError::UnsupportedPropertyReceiver => {
                Outcome::Unsupported("evaluation outside Swayze slice")
            }
            JsEvalError::LimitExceeded
            | JsEvalError::DispatchLimitExceeded
            | JsEvalError::StringComparisonLimitExceeded
            | JsEvalError::StringNumericConversionLimitExceeded
            | JsEvalError::StringConcatenationLengthLimitExceeded
            | JsEvalError::StringConcatenationBudgetExceeded => {
                Outcome::Unsupported("evaluation cap")
            }
            JsEvalError::AllocationFailed => Outcome::Infra("evaluation allocation failed"),
            _ => Outcome::Fail("unexpected evaluation error"),
        }
    }
    let result = (|| {
        let actual = parse(sources.0)?;
        let mut actual_scratch = JsEvalScratch::new();
        let lhs = actual
            .evaluate_primitive_with_scratch(&mut actual_scratch)
            .map_err(eval_error)?;
        if let Some(source) = sources.1 {
            let expected = parse(source)?;
            let mut expected_scratch = JsEvalScratch::new();
            let rhs = expected
                .evaluate_primitive_with_scratch(&mut expected_scratch)
                .map_err(eval_error)?;
            if !same_value(lhs, rhs) {
                return Err(Outcome::Fail("SameValue mismatch"));
            }
        }
        Ok(Outcome::Pass)
    })();
    result.unwrap_or_else(|outcome| outcome)
}

#[derive(Default)]
struct Totals {
    pass: usize,
    unsupported: usize,
    fail: usize,
    infra: usize,
}

impl Totals {
    fn report(&mut self, case: &Case<'_>, variant: &str, outcome: Outcome) {
        let (class, detail) = match outcome {
            Outcome::Pass => {
                self.pass += 1;
                ("pass", "")
            }
            Outcome::Unsupported(detail) => {
                self.unsupported += 1;
                ("unsupported", detail)
            }
            Outcome::Fail(detail) => {
                self.fail += 1;
                ("fail", detail)
            }
            Outcome::Infra(detail) => {
                self.infra += 1;
                ("infra", detail)
            }
        };
        println!("{class} {}/{variant} {} {detail}", case.mode(), case.path);
    }

    fn status(&self) -> u8 {
        if self.infra != 0 {
            2
        } else if self.fail != 0 || self.pass == 0 {
            1
        } else {
            0
        }
    }
}

fn run_case(case: &Case<'_>, source: &str, totals: &mut Totals) {
    let variants = match metadata(source) {
        Ok(variants) => variants,
        Err(detail) => {
            totals.report(case, "metadata", Outcome::Unsupported(detail));
            return;
        }
    };
    match (&case.assertion, variants) {
        (None, Variants::Raw) => totals.report(case, "script", evaluate((source, None))),
        (None, _) | (Some(_), Variants::Raw) => {
            totals.report(
                case,
                "metadata",
                Outcome::Unsupported("mode does not match flags"),
            );
        }
        (Some(assertion), variants) => {
            if matches!(variants, Variants::Sloppy | Variants::Both) {
                let outcome = assertion.scripts(source).map_or(
                    Outcome::Infra("reviewed assertion spans or anchors do not match"),
                    |(actual, expected)| evaluate((&actual, Some(&expected))),
                );
                totals.report(case, "sloppy", outcome);
            }
            if matches!(variants, Variants::Strict | Variants::Both) {
                totals.report(
                    case,
                    "strict",
                    Outcome::Unsupported("strict Script outside Swayze slice"),
                );
            }
        }
    }
}

fn run() -> Result<u8, Diagnostic> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = root.join(SUBMODULE);
    let commit = git(&root, &["rev-parse", "--verify", "HEAD"], 41)?;
    let commit = std::str::from_utf8(&commit)
        .map_err(|_| "cannot resolve superproject HEAD")?
        .trim_ascii_end();
    if !repo.join(".git").exists() {
        return Err("initialize the pinned Test262 submodule first");
    }
    if tree_entry(&root, commit, SUBMODULE, "160000 commit ")? != PIN
        || git(&repo, &["rev-parse", "--verify", "HEAD"], 41)? != format!("{PIN}\n").as_bytes()
    {
        return Err("Test262 gitlink or checkout HEAD does not match runner pin");
    }
    let index = blob(&root, commit, MANIFEST)
        .map_err(|_| "cannot read bounded manifest from superproject HEAD")?;
    let cases = manifest(&index).ok_or("invalid or oversized manifest")?;
    let mut totals = Totals::default();
    println!(
        "Test262 {PIN}: {} reviewed files; adapted evidence is not an official Test262 pass",
        cases.len()
    );
    for case in &cases {
        match blob(&repo, PIN, case.path) {
            Ok(source) => run_case(case, &source, &mut totals),
            Err(detail) => totals.report(case, "load", Outcome::Infra(detail)),
        }
    }
    println!(
        "pass={} unsupported={} fail={} infra={}",
        totals.pass, totals.unsupported, totals.fail, totals.infra
    );
    if totals.pass == 0 {
        println!("no passing variants");
    }
    Ok(totals.status())
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    match (args.next().as_deref(), args.next()) {
        (None, None) => match run() {
            Ok(status) => status.into(),
            Err(error) => {
                eprintln!("infra: {error}");
                2.into()
            }
        },
        (Some(flag), None) if flag == OsStr::new("--self-test") => {
            self_test();
            println!("Test262 runner synthetic checks passed (no corpus loaded)");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("infra: usage: run_test262 [--self-test]");
            2.into()
        }
    }
}

// Opt-in synthetic runner checks, deliberately not a Cargo test target.
fn self_test() {
    use JsPrimitiveValue::{Scalar, String};
    use JsValue::{Null, Number, Undefined};

    assert!(same_value(
        Scalar(Number(f64::NAN)),
        Scalar(Number(-f64::NAN))
    ));
    assert!(!same_value(Scalar(Number(0.0)), Scalar(Number(-0.0))));
    assert!(!same_value(Scalar(Null), Scalar(Undefined)));
    assert!(!same_value(Scalar(Number(1.0)), String(&[49])));
    assert!(same_value(String(&[0xd800]), String(&[0xd800])));
    assert!(!same_value(String(&[0xd800]), String(&[0xfffd])));
    assert!(manifest("raw\ttest/../bad.js\n").is_none());
    assert!(manifest("raw\ttest/x.js\nraw\ttest/x.js\n").is_none());
    assert!(manifest("raw\ttest/x.js\textra\n").is_none());
    assert!(manifest("").unwrap().is_empty());
    assert_eq!(Totals::default().status(), 1);
    assert!(metadata("/*---\nnegative:\n  phase: parse\n---*/").is_err());
    assert!(metadata("/*---\nincludes: [test.js]\n---*/").is_err());
    assert!(metadata("/*---\nflags: [async]\n---*/").is_err());
    assert_eq!(metadata("/*---\nflags: [raw]\n---*/"), Ok(Variants::Raw));
    assert_eq!(
        metadata("/*---\nflags: [onlyStrict]\n---*/"),
        Ok(Variants::Strict)
    );
    assert_eq!(
        metadata("/*---\ndescription: >\n  two lines\n---*/"),
        Ok(Variants::Both)
    );
    assert_eq!(
        evaluate(("'\\uD800' + '';", Some("'\\uD800';"))),
        Outcome::Pass
    );
    assert_eq!(
        evaluate(("0;", Some("-0;"))),
        Outcome::Fail("SameValue mismatch")
    );
    assert_eq!(evaluate(("0 / 0;", Some("-(0 / 0);"))), Outcome::Pass);

    let prefix = "/*---\nflags: [noStrict]\n---*/\nlet x = 2;\n";
    let call = "assert.sameValue(++x, x, 'literal');";
    let source = format!("{prefix}{call}\n");
    let offset = prefix.len();
    let assertion = Assertion {
        call: offset..offset + call.len(),
        actual: offset + 17..offset + 20,
        expected: offset + 22..offset + 23,
        message: Some(offset + 25..offset + 34),
    };
    let (actual, expected) = assertion.scripts(&source).expect("exact reviewed spans");
    assert_eq!(evaluate((&actual, Some(&expected))), Outcome::Pass);
    assert!(assertion.scripts(&format!("{source}4;")).is_none());
    assert!(
        assertion
            .scripts(&source.replace("sameValue", "sameWrong"))
            .is_none()
    );
    assert!(
        assertion
            .scripts(&source.replace("'literal'", "('other')"))
            .is_none()
    );
}
