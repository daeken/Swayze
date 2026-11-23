//! Opt-in slice runner, not the Test262 harness. Its only corpus input is a
//! reviewed manifest in the current superproject commit and the pinned Git tree.
//!
//! TSV rows: `ROLE raw PATH`, `ROLE raw-negative PATH`, or
//! `ROLE adapted PATH CALL ACTUAL EXPECTED [MESSAGE]`. Fields are tab-separated;
//! byte spans are zero-based and half-open (`START:END`). Review each whole blob;
//! the call span includes the final semicolon. `required` rows must pass their
//! gated variant; `frontier` may be unsupported but must not fail.

use std::{
    ffi::OsStr,
    io::Read,
    ops::Range,
    path::Path,
    process::{Command, ExitCode, Stdio},
};
use swayze_engine::{
    JsErrorKind, JsEvalError, JsEvalScratch, JsLimit, JsParseError, JsPrimitiveValue, JsValue,
    parse_script, parse_script_for_completion,
};

const PIN: &str = "2a1ade303f72d3de80565c921d685222405c32ce";
const MANIFEST: &str = "tools/test262-cases.tsv";
const SUBMODULE: &str = "vendor/test262";
const MAX_CASES: usize = 32;
const MAX_BYTES: usize = 8 * 1024; // Per blob, including the manifest.

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

enum Mode {
    Raw,
    RawNegative,
    Adapted(Assertion),
}

struct Case<'a> {
    path: &'a str,
    required: bool,
    mode: Mode,
}

impl Case<'_> {
    fn role(&self) -> &'static str {
        if self.required {
            "required"
        } else {
            "frontier"
        }
    }

    fn mode(&self) -> &'static str {
        match self.mode {
            Mode::Raw => "raw",
            Mode::RawNegative => "raw-negative",
            Mode::Adapted(_) => "adapted",
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
        let required = match fields.next()? {
            "required" => true,
            "frontier" => false,
            _ => return None,
        };
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
        let mode = match mode {
            "raw" => Mode::Raw,
            "raw-negative" => Mode::RawNegative,
            "adapted" => Mode::Adapted(Assertion {
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
        cases.push(Case {
            path,
            required,
            mode,
        });
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NegativePhase {
    Parse,
    Resolution,
    Runtime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorType {
    Error,
    AggregateError,
    EvalError,
    RangeError,
    ReferenceError,
    SyntaxError,
    TypeError,
    UriError,
}

#[derive(Debug, PartialEq, Eq)]
struct Negative {
    phase: NegativePhase,
    error_type: ErrorType,
}

// Deliberately not general YAML. Only this reviewed cohort's frontmatter shape
// is accepted: LF framing, exact raw flags, a two-space phase/type map, and a
// small set of inert annotation forms. No aliases, tags or includes.
fn negative_metadata(source: &str) -> Result<Negative, Diagnostic> {
    const FRAME: Diagnostic = "invalid or ambiguous raw-negative frontmatter";
    const FIELD: Diagnostic = "invalid, duplicate or unsupported raw-negative metadata field";
    enum Block {
        None,
        Text(Option<usize>),
        Negative,
    }

    if source.len() > MAX_BYTES
        || source.matches("/*---").count() != 1
        || source.matches("---*/").count() != 1
    {
        return Err(FRAME);
    }
    let (before, rest) = source.split_once("/*---\n").ok_or(FRAME)?;
    let (header, after) = rest.split_once("\n---*/").ok_or(FRAME)?;
    if (!before.is_empty() && !before.ends_with('\n'))
        || (!after.is_empty() && !after.starts_with('\n'))
        || header.contains("*/")
        || header
            .chars()
            .any(|c| c != '\n' && (c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')))
    {
        return Err(FRAME);
    }

    let plain_word = |word: &str| {
        word.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && word
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    let [
        mut flags,
        mut negative,
        mut description,
        mut info,
        mut esid,
        mut features,
    ] = [false; 6];
    let mut block = Block::None;
    let mut phase = None;
    let mut error_type = None;
    for line in header.split('\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(child) = line.strip_prefix("  ") {
            match &mut block {
                Block::Text(indent) => {
                    // A block's first content line fixes its minimum indent;
                    // never swallow a dedented key as annotation text.
                    let n = line.len() - line.trim_start_matches(' ').len();
                    if n == line.len() || n < *indent.get_or_insert(n) {
                        return Err(FIELD);
                    }
                }
                Block::Negative => match child.split_once(": ") {
                    Some(("phase", value)) if phase.is_none() => {
                        phase = Some(match value {
                            "parse" => NegativePhase::Parse,
                            "resolution" => NegativePhase::Resolution,
                            "runtime" => NegativePhase::Runtime,
                            _ => return Err(FIELD),
                        });
                    }
                    Some(("type", value)) if error_type.is_none() => {
                        error_type = Some(match value {
                            "Error" => ErrorType::Error,
                            "AggregateError" => ErrorType::AggregateError,
                            "EvalError" => ErrorType::EvalError,
                            "RangeError" => ErrorType::RangeError,
                            "ReferenceError" => ErrorType::ReferenceError,
                            "SyntaxError" => ErrorType::SyntaxError,
                            "TypeError" => ErrorType::TypeError,
                            "URIError" => ErrorType::UriError,
                            _ => return Err(FIELD),
                        });
                    }
                    _ => return Err(FIELD),
                },
                Block::None => return Err(FIELD),
            }
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(FIELD)?;
        let (seen, next) = match (name, value) {
            ("flags", " [raw]") => (&mut flags, Block::None),
            ("negative", "") => (&mut negative, Block::Negative),
            ("description", " |" | " >" | " |-" | " >-") => (&mut description, Block::Text(None)),
            ("info", " |" | " >" | " |-" | " >-") => (&mut info, Block::Text(None)),
            ("esid", " pending") => (&mut esid, Block::None),
            ("esid", word) if word.starts_with(" sec-") && plain_word(&word[1..]) => {
                (&mut esid, Block::None)
            }
            ("features", list) => {
                let list = list
                    .strip_prefix(" [")
                    .and_then(|list| list.strip_suffix(']'))
                    .ok_or(FIELD)?;
                // The reviewed form uses short lists of distinct plain feature names.
                let mut names = [""; 16];
                if !list.is_empty() {
                    for (index, name) in list.split(", ").enumerate() {
                        if index == names.len()
                            || !plain_word(name)
                            || names[..index].contains(&name)
                        {
                            return Err(FIELD);
                        }
                        names[index] = name;
                    }
                }
                (&mut features, Block::None)
            }
            _ => return Err(FIELD),
        };
        if std::mem::replace(seen, true) {
            return Err(FIELD);
        }
        block = next;
    }
    if !flags {
        return Err("raw-negative requires exactly flags: [raw]");
    }
    Ok(Negative {
        phase: phase.ok_or("raw-negative requires a supported negative.phase")?,
        error_type: error_type.ok_or("raw-negative requires a supported negative.type")?,
    })
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

// Only the comment/metadata surroundings needed by the first cohort. The
// trusted review still establishes the exact call and operand boundaries.
fn comment_prefix(prefix: &str) -> Option<()> {
    let (before, rest) = prefix.split_once("/*---\n")?;
    let (header, after) = rest.split_once("\n---*/")?;
    let line_comment_or_empty = |line: &str| {
        let line = line.trim_ascii();
        !line.contains(['\r', '\u{2028}', '\u{2029}'])
            && (line.is_empty() || line.starts_with("//"))
    };
    (!header.contains("*/")
        && (before.is_empty() || before.ends_with('\n'))
        && before.lines().all(line_comment_or_empty)
        && after.starts_with('\n')
        && after.lines().all(line_comment_or_empty))
    .then_some(())
}

// A shape check only: Swayze, not the host's decimal parser, determines the
// value. Isolated expectations cannot read any bindings or have side effects.
fn is_primitive_literal(source: &str) -> bool {
    let decimal = source.strip_prefix('-').unwrap_or(source);
    is_quoted_literal(source)
        || matches!(source, "true" | "false" | "null")
        || (decimal.as_bytes().first().is_some_and(u8::is_ascii_digit)
            && source.parse::<f64>().is_ok())
}

impl Assertion {
    // Structural anchors for reviewed spans, not an argument splitter or a JS
    // lexer. The pinned blob review must establish a top-level terminal call,
    // the exact argument boundaries, and no dependence on the harness.
    fn scripts(&self, source: &str) -> Option<(String, String)> {
        let prefix = source.get(..self.call.start)?;
        let suffix = source.get(self.call.end..)?;
        comment_prefix(prefix)?;
        if !prefix.rsplit('\n').next()?.trim_ascii().is_empty()
            || !suffix.trim_ascii().is_empty()
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
        if !is_primitive_literal(expected) {
            return None;
        }
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
        // Preserve all bytes outside the call and evaluate the actual once.
        // The isolated expectation and optional message are pure literals;
        // grouping prevents either from accidentally becoming a directive.
        Some((
            format!("{prefix}({actual});{suffix}"),
            format!("{message}({expected});"),
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
    fn parse_error(error: JsParseError) -> Outcome {
        match error.kind() {
            JsErrorKind::Unsupported => Outcome::Unsupported(error.message()),
            JsErrorKind::LimitExceeded(JsLimit::Allocation) => Outcome::Infra(error.message()),
            JsErrorKind::LimitExceeded(_) => Outcome::Unsupported(error.message()),
            JsErrorKind::Malformed => Outcome::Fail(error.message()),
        }
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
        let mut actual_scratch = JsEvalScratch::new();
        let Some(expected_source) = sources.1 else {
            let script = parse_script_for_completion(sources.0).map_err(parse_error)?;
            script
                .execute_with_scratch(&mut actual_scratch)
                .map_err(eval_error)?;
            return Ok(Outcome::Pass);
        };
        let actual = parse_script(sources.0).map_err(parse_error)?;
        let lhs = actual
            .evaluate_primitive_with_scratch(&mut actual_scratch)
            .map_err(eval_error)?;
        let expected = parse_script(expected_source).map_err(parse_error)?;
        let mut expected_scratch = JsEvalScratch::new();
        let rhs = expected
            .evaluate_primitive_with_scratch(&mut expected_scratch)
            .map_err(eval_error)?;
        if !same_value(lhs, rhs) {
            return Err(Outcome::Fail("SameValue mismatch"));
        }
        Ok(Outcome::Pass)
    })();
    result.unwrap_or_else(|outcome| outcome)
}

// Baseline has no typed parse errors. Wire only the engine's typed accessor
// here, never a kind, message, span or pattern from the guest source.
fn recognized_negative_type(_error: &JsParseError) -> Option<ErrorType> {
    None
}

fn classify_negative_error(
    expected: ErrorType,
    kind: JsErrorKind,
    detail: Diagnostic,
    recognized: Option<ErrorType>,
) -> Outcome {
    match kind {
        JsErrorKind::LimitExceeded(JsLimit::Allocation) => Outcome::Infra(detail),
        JsErrorKind::LimitExceeded(_) => Outcome::Unsupported(detail),
        JsErrorKind::Unsupported | JsErrorKind::Malformed if recognized == Some(expected) => {
            Outcome::Pass
        }
        JsErrorKind::Unsupported | JsErrorKind::Malformed if recognized.is_some() => {
            Outcome::Fail("wrong typed parse error")
        }
        JsErrorKind::Unsupported => Outcome::Unsupported(detail),
        JsErrorKind::Malformed => Outcome::Fail(detail),
    }
}

fn parse_negative(source: &str, expected: ErrorType) -> Outcome {
    // Exact original UTF-8 blob. No evaluator, scratch, rewrite or retry, even
    // if parsing succeeds and the source contains a poison statement.
    match parse_script_for_completion(source) {
        Ok(_) => Outcome::Fail("expected a typed parse error, but parsing succeeded"),
        Err(error) => classify_negative_error(
            expected,
            error.kind(),
            error.message(),
            recognized_negative_type(&error),
        ),
    }
}

fn raw_negative(source: &str) -> (&'static str, Outcome) {
    match negative_metadata(source) {
        Err(detail) => ("metadata", Outcome::Infra(detail)),
        Ok(Negative {
            phase: NegativePhase::Parse,
            error_type,
        }) => ("parse", parse_negative(source, error_type)),
        Ok(Negative {
            phase: NegativePhase::Resolution,
            ..
        }) => (
            "resolution/not-run",
            Outcome::Unsupported("negative resolution phase outside Swayze slice"),
        ),
        Ok(Negative {
            phase: NegativePhase::Runtime,
            ..
        }) => (
            "runtime/not-run",
            Outcome::Unsupported("negative runtime phase outside Swayze slice"),
        ),
    }
}

#[derive(Default)]
struct Totals {
    pass: usize,
    adapted_pass: usize,
    raw_pass: usize,
    raw_negative_pass: usize,
    unsupported: usize,
    fail: usize,
    infra: usize,
    required_pass: usize,
    required_unsupported: usize,
}

impl Totals {
    fn report(&mut self, case: &Case<'_>, variant: &str, outcome: Outcome) {
        let gated = case.required && variant != "strict/not-run";
        let (class, detail) = match outcome {
            Outcome::Pass => {
                self.pass += 1;
                match case.mode {
                    Mode::Raw => self.raw_pass += 1,
                    Mode::RawNegative => self.raw_negative_pass += 1,
                    Mode::Adapted(_) => self.adapted_pass += 1,
                }
                self.required_pass += usize::from(gated);
                ("pass", "")
            }
            Outcome::Unsupported(detail) => {
                self.unsupported += 1;
                self.required_unsupported += usize::from(gated);
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
        let detail = if detail.is_empty() {
            "".to_owned()
        } else {
            format!(": {detail}")
        };
        println!(
            "{class} {} {}/{variant} {}{detail}",
            case.role(),
            case.mode(),
            case.path
        );
    }

    fn status(&self) -> u8 {
        if self.infra != 0 {
            2
        } else if self.fail != 0 || self.required_unsupported != 0 || self.required_pass == 0 {
            1
        } else {
            0
        }
    }
}

fn run_case(case: &Case<'_>, source: &str, totals: &mut Totals) {
    if matches!(case.mode, Mode::RawNegative) {
        let (variant, outcome) = raw_negative(source);
        totals.report(case, variant, outcome);
        return;
    }
    let variants = match metadata(source) {
        Ok(variants) => variants,
        Err(detail) => {
            totals.report(case, "metadata", Outcome::Unsupported(detail));
            return;
        }
    };
    match (&case.mode, variants) {
        (Mode::RawNegative, _) => unreachable!("raw-negative uses only the parse-negative path"),
        (Mode::Raw, Variants::Raw) => totals.report(case, "script", evaluate((source, None))),
        (Mode::Raw, _) | (Mode::Adapted(_), Variants::Raw) => {
            totals.report(
                case,
                "metadata",
                Outcome::Unsupported("mode does not match flags"),
            );
        }
        (Mode::Adapted(assertion), variants) => {
            if matches!(variants, Variants::Sloppy | Variants::Both) {
                let outcome = assertion.scripts(source).map_or(
                    Outcome::Infra("reviewed assertion spans or anchors do not match"),
                    |(actual, expected)| evaluate((&actual, Some(&expected))),
                );
                totals.report(case, "sloppy", outcome);
            }
            if matches!(variants, Variants::Strict | Variants::Both) {
                // Default strict is explicitly not run, never a required
                // variant. A strict-only required row cannot be satisfied.
                let variant = if variants == Variants::Strict && case.required {
                    "strict-only/not-run"
                } else {
                    "strict/not-run"
                };
                totals.report(
                    case,
                    variant,
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
    println!(
        "required-pass={} required-unsupported={}",
        totals.required_pass, totals.required_unsupported
    );
    println!(
        "adapted-pass={} raw-pass={} raw-negative-pass={}",
        totals.adapted_pass, totals.raw_pass, totals.raw_negative_pass
    );
    if totals.required_pass == 0 {
        println!("no passing required variants");
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

fn self_test_raw_negative() {
    let row = "frontier\traw-negative\ttest/synthetic.js";
    let cases = manifest(row).expect("raw-negative is an explicit manifest mode");
    let case = &cases[0];
    assert_eq!(case.mode(), "raw-negative");
    assert!(manifest(&format!("{row}\t0:1")).is_none());
    assert!(manifest(&format!("{row}\nrequired\traw\ttest/synthetic.js")).is_none());

    let header = "flags: [raw]\nnegative:\n  phase: parse\n  type: SyntaxError";
    // This script parses, but evaluating the property would be unsupported.
    // The parse-negative result must be failure, not an evaluation outcome.
    let script = |header| format!("/*---\n{header}\n---*/\nfalse.length;");
    let source = script(header);
    assert_eq!(
        raw_negative(&source),
        (
            "parse",
            Outcome::Fail("expected a typed parse error, but parsing succeeded")
        )
    );
    assert!(matches!(
        raw_negative(&source.replace("false.length;", "let = ;")),
        ("parse", Outcome::Fail(_))
    ));
    assert!(matches!(
        raw_negative(&source.replace("false.length;", "@")),
        ("parse", Outcome::Unsupported(_))
    ));
    for (phase, label) in [
        ("resolution", "resolution/not-run"),
        ("runtime", "runtime/not-run"),
    ] {
        let unsupported = source
            .replace("phase: parse", &format!("phase: {phase}"))
            .replace("type: SyntaxError", "type: ReferenceError");
        let (variant, outcome) = raw_negative(&unsupported);
        assert_eq!(variant, label);
        assert!(matches!(outcome, Outcome::Unsupported(_)));
    }

    for bad in [
        script("flags: [raw]"),
        script("negative:\n  phase: parse\n  type: SyntaxError"),
        source.replace("[raw]", "[raw, noStrict]"),
        source.replace("[raw]", "['raw']"),
        source.replace("  phase: parse\n", ""),
        source.replace("  type: SyntaxError\n", ""),
        source.replace("phase: parse", "phase: early"),
        source.replace("phase: parse", "phase: parse # comment"),
        source.replace("type: SyntaxError", "type: OtherError"),
        source.replace("phase: parse", "phase: parse\n  phase: runtime"),
        source.replace("type: SyntaxError", "type: SyntaxError\n  type: TypeError"),
        source.replace("type: SyntaxError", "type: SyntaxError\n  extra: value"),
        source.replace("  phase:", "\tphase:"),
        source.replace("  phase:", "   phase:"),
        source.replace("flags: [raw]", "flags: [raw]\nflags: [raw]"),
        source.replace("negative:", "negative:\nnegative:"),
        source.replace("negative:", "includes: []\nnegative:"),
        source.replace("negative:", "unknown: true\nnegative:"),
        source.replace("negative:", "features: [hashbang, hashbang]\nnegative:"),
        source.replace("negative:", "esid: pending\nesid: sec-test\nnegative:"),
        source.replace("negative:", "'negative':"),
        source.replace(
            "negative:",
            "description: >\n    first line\n  includes: []\nnegative:",
        ),
        source.replace("flags: [raw]", "description: |\n  flags: [raw]"),
        source.replace("flags: [raw]", "description: |\n  */\nflags: [raw]"),
        source.replace("---*/", "---*/ false.length;"),
        source.replace('\n', "\r\n"),
        format!("{source}\n{source}"),
        format!("{source}\n// {}", "x".repeat(MAX_BYTES)),
    ] {
        assert!(
            matches!(raw_negative(&bad), ("metadata", Outcome::Infra(_))),
            "{bad}"
        );
    }
    let mut totals = Totals::default();
    run_case(case, &script("flags: [raw]"), &mut totals);
    assert_eq!((totals.infra, totals.status()), (1, 2));
    for mode in [
        Mode::Raw,
        Mode::Adapted(Assertion {
            call: 1..2,
            actual: 1..2,
            expected: 1..2,
            message: None,
        }),
    ] {
        let mut totals = Totals::default();
        run_case(
            &Case {
                path: case.path,
                required: false,
                mode,
            },
            &source,
            &mut totals,
        );
        assert_eq!(
            (totals.pass, totals.unsupported, totals.fail, totals.infra),
            (0, 1, 0, 0)
        );
    }

    // Exercise the matching policy with synthetic typed evidence; the baseline
    // production seam never derives evidence from coarse diagnostics.
    let classify = |kind, recognized| {
        classify_negative_error(ErrorType::SyntaxError, kind, "synthetic error", recognized)
    };
    for kind in [JsErrorKind::Unsupported, JsErrorKind::Malformed] {
        assert_eq!(classify(kind, Some(ErrorType::SyntaxError)), Outcome::Pass);
        assert_eq!(
            classify(kind, Some(ErrorType::TypeError)),
            Outcome::Fail("wrong typed parse error")
        );
    }
    assert_eq!(
        classify(JsErrorKind::Unsupported, None),
        Outcome::Unsupported("synthetic error")
    );
    assert_eq!(
        classify(JsErrorKind::Malformed, None),
        Outcome::Fail("synthetic error")
    );
    assert_eq!(
        classify(
            JsErrorKind::LimitExceeded(JsLimit::Allocation),
            Some(ErrorType::SyntaxError)
        ),
        Outcome::Infra("synthetic error")
    );
    assert_eq!(
        classify(
            JsErrorKind::LimitExceeded(JsLimit::Tokens),
            Some(ErrorType::SyntaxError)
        ),
        Outcome::Unsupported("synthetic error")
    );
    let mut totals = Totals::default();
    totals.report(
        case,
        "parse",
        classify(JsErrorKind::Unsupported, Some(ErrorType::SyntaxError)),
    );
    assert_eq!(
        (
            totals.pass,
            totals.adapted_pass,
            totals.raw_pass,
            totals.raw_negative_pass
        ),
        (1, 0, 0, 1)
    );
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
    assert!(manifest("required\traw\ttest/../bad.js\n").is_none());
    assert!(manifest("required\traw\ttest/x.js\nfrontier\traw\ttest/x.js\n").is_none());
    assert!(manifest("required\traw\ttest/x.js\textra\n").is_none());
    assert!(manifest("").unwrap().is_empty());
    assert_eq!(Totals::default().status(), 1);
    self_test_raw_negative();
    assert!(metadata("/*---\nnegative:\n  phase: parse\n---*/").is_err());
    assert!(metadata("/*---\nincludes: [test.js]\n---*/").is_err());
    assert!(metadata("/*---\nflags: [async]\n---*/").is_err());
    assert_eq!(metadata("/*---\nflags: [raw]\n---*/"), Ok(Variants::Raw));
    let raw = Case {
        path: "test/raw-synthetic.js",
        required: true,
        mode: Mode::Raw,
    };
    let mut totals = Totals::default();
    run_case(
        &raw,
        "#! kept in source\n/*---\nflags: [raw]\n---*/\nlet x = 1;",
        &mut totals,
    );
    assert_eq!(
        (totals.status(), totals.raw_pass, totals.required_pass),
        (0, 1, 1)
    );
    assert!(matches!(
        evaluate(("#! comment\u{2028}let = ;", None)),
        Outcome::Fail(_)
    ));
    assert_eq!(
        evaluate(("#! comment\n{ (0).length; }", None)),
        Outcome::Unsupported("evaluation outside Swayze slice")
    );
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

    let prefix = "/*---\ndescription: synthetic\n---*/\n";
    let call = "assert.sameValue(-0, -0, 'literal');";
    let source = format!("{prefix}{call}\n");
    let offset = prefix.len();
    let assertion = Assertion {
        call: offset..offset + call.len(),
        actual: offset + 17..offset + 19,
        expected: offset + 21..offset + 23,
        message: Some(offset + 25..offset + 34),
    };
    assert!(!is_primitive_literal("x"));
    assert!(!is_primitive_literal("1+1"));
    assert!(comment_prefix("/*---\ndescription: synthetic\n---*/\nlet x = 2;\n").is_none());
    assert_eq!(
        evaluate(("('use strict');", Some("('use strict');"))),
        Outcome::Pass
    );
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

    let mut case = Case {
        path: "test/synthetic.js",
        required: true,
        mode: Mode::Adapted(assertion),
    };
    let baseline = || Totals {
        pass: 1,
        required_pass: 1,
        ..Totals::default()
    };
    let mut totals = Totals::default();
    run_case(&case, &source, &mut totals);
    assert_eq!(
        (totals.status(), totals.required_pass, totals.unsupported),
        (0, 1, 1)
    );

    let unsupported = source.replacen("-0,", "xx,", 1);
    let mut totals = baseline();
    run_case(&case, &unsupported, &mut totals);
    assert_eq!((totals.status(), totals.required_unsupported), (1, 1));

    case.required = false;
    let mut totals = baseline();
    run_case(&case, &unsupported, &mut totals);
    assert_eq!(
        (
            totals.status(),
            totals.required_unsupported,
            totals.unsupported
        ),
        (0, 0, 2)
    );

    for actual in ["+0,", "1+,"] {
        let mut totals = baseline();
        run_case(&case, &source.replacen("-0,", actual, 1), &mut totals);
        assert_eq!((totals.status(), totals.fail), (1, 1));
    }
    let mut totals = baseline();
    run_case(&case, &format!("{source}4;"), &mut totals);
    assert_eq!((totals.status(), totals.infra), (2, 1));
}
