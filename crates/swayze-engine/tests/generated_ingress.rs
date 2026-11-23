//! Bounded constructive cases for the public ingress APIs, not a corpus check.

use swayze_engine::{
    JsErrorKind, JsLimit, JsLimits, JsSpan, WasmErrorKind, parse_script, parse_script_with_limits,
    validate_module,
};

const CASES: usize = 10;

fn scripts() -> impl Iterator<Item = (usize, String, i32)> {
    (1..=CASES).scan((String::from("5"), 5), |(expression, expected), depth| {
        let lhs = 7 * i32::try_from(depth).unwrap();
        *expression = format!("{lhs}\u{2003}-({expression})");
        *expected = lhs - *expected;
        Some((depth, format!("{expression}\u{00a0}"), *expected))
    })
}

#[test]
fn generated_scripts_evaluate_at_the_inclusive_source_limit() {
    for (depth, source, expected) in scripts() {
        let limits = JsLimits {
            max_source_bytes: source.len(),
            ..JsLimits::default()
        };
        let script = parse_script_with_limits(&source, limits)
            .unwrap_or_else(|error| panic!("depth {depth}: {error}"));
        // Each wrapper contributes one literal followed later by one subtraction.
        assert_eq!(script.instruction_count(), 2 * depth + 1, "{source:?}");
        assert_eq!(script.max_stack_depth(), depth + 1, "{source:?}");
        assert_eq!(
            script.evaluate().unwrap().to_bits(),
            f64::from(expected).to_bits(),
            "{source:?}"
        );

        // Put the ceiling in the middle of the final two-byte whitespace.
        let error = parse_script_with_limits(
            &source,
            JsLimits {
                max_source_bytes: source.len() - 1,
                ..limits
            },
        )
        .expect_err("source must fit the byte ceiling");
        assert_eq!(
            error.kind(),
            JsErrorKind::LimitExceeded(JsLimit::SourceBytes)
        );
        assert_eq!(
            error.span(),
            JsSpan {
                start: source.len() - '\u{00a0}'.len_utf8(),
                end: source.len(),
            },
            "{source:?}"
        );
    }
}

#[test]
fn generated_script_suffixes_report_byte_spans() {
    for (_, source, _) in scripts() {
        // One suffix ends inside a numeric token; the other starts a second
        // expression, which is outside the supported Script subset.
        for (suffix, kind, span_width) in [
            ("+1e+", JsErrorKind::Malformed, 0),
            (";\u{2003}0", JsErrorKind::Unsupported, 1),
        ] {
            let input = format!("{source}{suffix}");
            let error = parse_script(&input)
                .expect_err("must not accept a valid expression followed by this suffix");
            assert_eq!(error.kind(), kind, "{input:?}");
            assert_eq!(
                error.span(),
                JsSpan {
                    start: input.len() - span_width,
                    end: input.len(),
                },
                "{input:?}"
            );
        }
    }
}

const EXPORT: &str = "step";
const END: u8 = 0x0b;

struct WasmFixture {
    bytes: Vec<u8>,
    code_size_at: usize,
    body_size_at: usize,
}

fn wasm_fixture(steps: usize) -> WasmFixture {
    let mut bytes = b"\0asm\x01\0\0\0".to_vec();
    bytes.extend([1, 8, 1, 0x60, 3, 0x7f, 0x7f, 0x7f, 1, 0x7f]); // (i32, i32, i32) -> i32
    bytes.extend([3, 2, 1, 0]); // One function of type zero.
    let name_len = u8::try_from(EXPORT.len()).unwrap();
    bytes.extend([7, name_len + 4, 1, name_len]);
    bytes.extend_from_slice(EXPORT.as_bytes());
    bytes.extend([0, 0]); // Export function zero.

    let mut body = vec![0, 0x20, 0]; // No extra locals; local.get seed.
    for _ in 0..steps {
        body.extend([0x20, 1, 0x6c, 0x20, 2, 0x6a]); // acc = acc * factor + delta
    }
    body.push(END);
    // Both lengths (and the one-byte increases below) fit in one unsigned LEB byte.
    assert!(body.len() + 3 < 128);
    let body_size = u8::try_from(body.len()).unwrap();
    let code_size_at = bytes.len() + 1;
    let body_size_at = bytes.len() + 3;
    bytes.extend([10, body_size + 2, 1, body_size]); // Code section, one body.
    bytes.extend(body);
    WasmFixture {
        bytes,
        code_size_at,
        body_size_at,
    }
}

#[test]
fn generated_wasm_steps_evaluate_exactly() {
    for steps in 1..=CASES {
        let module = validate_module(&wasm_fixture(steps).bytes)
            .unwrap_or_else(|error| panic!("{steps} steps: {error}"));
        let instance = module.instantiate().unwrap();
        for (seed, factor) in [(7_i32, 3_i32), (i32::MAX - 5, -2)] {
            // With delta = factor - 1, acc + 1 is multiplied by factor at
            // every step. Use the closed form in the same wrapping ring.
            let expected = seed
                .wrapping_add(1)
                .wrapping_mul(factor.wrapping_pow(u32::try_from(steps).unwrap()))
                .wrapping_sub(1);
            assert_eq!(
                instance.invoke_i32_with_args(EXPORT, &[seed, factor, factor - 1]),
                Ok(expected),
                "{steps} steps, seed {seed}, factor {factor}"
            );
        }
    }
}

#[test]
fn generated_wasm_frame_errors_stay_at_the_owning_boundary() {
    for steps in 1..=CASES {
        let WasmFixture {
            bytes,
            code_size_at,
            body_size_at,
        } = wasm_fixture(steps);
        let code_size = usize::from(bytes[code_size_at]);
        let body_size = usize::from(bytes[body_size_at]);
        let end = bytes.len();
        let reject = |input: &[u8], offset, kind| {
            let error = validate_module(input).expect_err("inconsistent frame must be rejected");
            assert_eq!(error.kind, kind, "{steps} steps, bytes {input:02x?}");
            assert_eq!(error.offset, offset, "{steps} steps, {kind:?}");
        };

        let mut long_section = bytes.clone();
        long_section[code_size_at] += 1;
        reject(
            &long_section,
            end,
            WasmErrorKind::TruncatedSection {
                declared: code_size + 1,
                available: code_size,
            },
        );

        // A following empty-name custom section cannot supply the missing body byte.
        let mut long_body = bytes.clone();
        long_body[body_size_at] += 1;
        long_body.extend([0, 1, 0]);
        reject(
            &long_body,
            end,
            WasmErrorKind::TruncatedBody {
                declared: body_size + 1,
                available: body_size,
            },
        );

        // A correctly framed body still cannot contain bytes after its end.
        let mut trailing_body = bytes;
        trailing_body[code_size_at] += 1;
        trailing_body[body_size_at] += 1;
        trailing_body.push(END);
        reject(&trailing_body, end, WasmErrorKind::TrailingBodyBytes);
    }
}
