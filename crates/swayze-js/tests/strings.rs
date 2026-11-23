use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, PrimitiveValue, Span, Value, parse_script,
    parse_script_with_limits,
};

fn assert_units(source: &str, expected: &[u16]) {
    let script = parse_script(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    assert_eq!(
        script.evaluate_primitive(),
        Ok(PrimitiveValue::String(expected)),
        "{source:?}"
    );
}

fn assert_scalar(source: &str, expected: Value) {
    let script = parse_script(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    assert_eq!(
        script.evaluate_primitive(),
        Ok(PrimitiveValue::Scalar(expected)),
        "{source:?}"
    );
    assert_eq!(script.evaluate_value(), Ok(expected), "{source:?}");
}

fn assert_error(source: &str, kind: ErrorKind, start: usize, end: usize) -> &'static str {
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source:?}: {error}");
    assert_eq!(error.span(), Span { start, end }, "{source:?}: {error}");
    assert!(source.get(start..end).is_some());
    error.message()
}

#[test]
fn exact_utf16_and_borrowed_primitive_result() {
    assert_units("''", &[]);
    assert_units(
        "'\0é😀\u{2028}\u{2029}'",
        &[0, 0xe9, 0xd83d, 0xde00, 0x2028, 0x2029],
    );
    assert_units(r#"'"\'\\'"#, &[34, 39, 92]);
    assert_units(r#""'\"\\""#, &[39, 34, 92]);
    assert_units(
        r"'\b\f\n\r\t\v\0\x8f\u0123\q\😀\0９'",
        &[
            8, 12, 10, 13, 9, 11, 0, 0x8f, 0x123, 113, 0xd83d, 0xde00, 0, 0xff19,
        ],
    );
    assert_units(
        r"'\uD800\u{000dfff}\u{10FFFF}\u{0}\uDE00'",
        &[0xd800, 0xdfff, 0xdbff, 0xdfff, 0, 0xde00],
    );
    assert_units("'a\\\n\\\r\\\r\n\\\u{2028}\\\u{2029}b'", &[97, 98]);
    assert_units(
        "'/*not a comment*/ // not one either'",
        &"/*not a comment*/ // not one either"
            .encode_utf16()
            .collect::<Vec<_>>(),
    );

    // The prepared script outlives its source; each result borrows the same pool.
    let script = {
        let source = String::from(r"'\uD800'");
        parse_script(&source).unwrap()
    };
    let Ok(PrimitiveValue::String(first)) = script.evaluate_primitive() else {
        panic!("expected String")
    };
    let Ok(PrimitiveValue::String(second)) = script.evaluate_primitive() else {
        panic!("expected String")
    };
    assert_eq!(first, &[0xd800]);
    assert!(std::ptr::eq(first, second));
    assert_eq!(script.evaluate_value(), Err(EvalError::NonScalarResult));
    assert_eq!(script.evaluate(), Err(EvalError::NonNumericResult));
}

#[test]
fn malformed_and_annex_b_escape_spans_are_utf8_boundaries() {
    for (source, start, end) in [
        ("'é", 0, 3),
        ("'é\\", 3, 4),
        ("'é\r\n'", 3, 4),
        ("'é\n'", 3, 4),
        ("'é\\x😀'", 3, 9),
        ("'é\\u000é'", 3, 10),
        (r"'\x'", 1, 4),
        (r"'\u{", 1, 4),
        (r"'\u{}'", 1, 5),
        (r"'\u{110000}'", 1, 11),
        ("'\\u{A😀}'", 1, 9),
        (r"'\u{1_2}'", 1, 6),
    ] {
        assert_error(source, ErrorKind::Malformed, start, end);
    }
    for body in [r"\1", r"\77", r"\8", r"\9", r"\00", r"\08", r"\09"] {
        let source = format!("'é{body}'");
        let end = if body.starts_with(r"\0") { 6 } else { 5 };
        assert_error(&source, ErrorKind::Unsupported, 3, end);
    }
}

#[test]
fn strict_directive_depends_on_raw_spelling_and_root_prologue() {
    for source in [
        "'use strict'",
        "\"use strict\";",
        "'use strict' /* EOF */",
        "'first'; /* trivia */ 'use strict'; 42",
        r"'use\x20strict'; 'use strict'; 42",
    ] {
        let start = source
            .find("'use strict'")
            .or_else(|| source.find("\"use strict\""))
            .unwrap();
        assert_error(source, ErrorKind::Unsupported, start, start + 12);
        assert!(
            parse_script(source)
                .unwrap_err()
                .message()
                .contains("Use Strict")
        );
    }
    let strict: Vec<_> = "use strict".encode_utf16().collect();
    for source in [
        r"'use\x20strict'",
        r"'\u0075se strict'",
        "'use\\\n strict'",
        "('use strict')",
        "'use strict' /* trivia */\n || 1",
        "'use strict'\n ? 'use strict' : ''",
        "; 'use strict'",
        "1; 'use strict'",
        "('first'); 'use strict'",
        "let x; 'use strict'",
        "{} 'use strict'",
        "{ 'use strict'; } 'use strict'",
        "'first', 1; 'use strict'",
        "'first' === ''; 'use strict'",
    ] {
        assert_units(source, &strict);
    }
    assert_scalar("'use strict'\n, 42", Value::Number(42.0));
    assert_scalar("'use strict'\n=== 'use strict'", Value::Boolean(true));
    assert_scalar(
        "{ 'use strict'; } if (true) { 'use strict'; } 42",
        Value::Number(42.0),
    );
    assert_eq!(
        parse_script("'use strict'\n+ 1").unwrap().evaluate(),
        Err(EvalError::UnsupportedStringOperation)
    );
    assert_error("'use strict'\n42", ErrorKind::Unsupported, 13, 15); // no ASI
}

#[test]
fn literal_byte_cap_includes_quotes_and_cannot_split_utf8() {
    let limit = ErrorKind::LimitExceeded(Limit::StringLiteralBytes);
    let exact = format!("'{}é'", "a".repeat(4092));
    assert_eq!(exact.len(), 4096);
    let script = parse_script(&exact).unwrap();
    assert_eq!(script.instruction_count(), 1);
    let Ok(PrimitiveValue::String(units)) = script.evaluate_primitive() else {
        panic!("expected String")
    };
    assert_eq!(units.len(), 4093);
    assert_eq!(units.last(), Some(&0xe9));

    let prefix = "/*😀*/ ";
    let source = format!("{prefix}'{}😀'", "a".repeat(4093));
    assert_eq!(
        assert_error(&source, limit, prefix.len() + 4094, prefix.len() + 4098),
        "quoted string literal byte ceiling exceeded (including quotes)",
    );
    assert_error(&format!("'{}'", "a".repeat(4095)), limit, 4096, 4097);
    assert_error(&format!("'{}", "a".repeat(4096)), limit, 4096, 4097);
    assert_error(&format!("'{}\r", "a".repeat(4095)), limit, 4096, 4097);
    assert_error(&format!("'{}\\x😀", "a".repeat(4093)), limit, 4096, 4100);
    assert_error(&format!("'\\u{{{}", "F".repeat(4093)), limit, 4096, 4097);
    let leading_zeros = format!("'\\u{{{}41}}'", "0".repeat(4088));
    assert_eq!(leading_zeros.len(), 4096);
    assert_units(&leading_zeros, &[65]);
    assert_error(
        &format!("'{}", "a".repeat(4095)),
        ErrorKind::Malformed,
        0,
        4096,
    );
}

#[test]
fn unit_cap_is_aggregate_inclusive_and_counts_skipped_literals() {
    let limit = ErrorKind::LimitExceeded(Limit::StringLiteralUnits);
    // 16,376 ASCII units in skipped arms, then 6 ASCII units and one surrogate pair.
    let prefix = format!(
        "if (false) {{ {} }} ",
        format!("'{}';", "a".repeat(4094)).repeat(4)
    );
    assert_units(
        &format!("{prefix}'123456😀'"),
        &[49, 50, 51, 52, 53, 54, 0xd83d, 0xde00],
    );
    assert_units(&format!("{prefix}'123456😀', ''"), &[]);
    let escaped = format!("{prefix}'1234567\\u{{10000}}'");
    assert_error(&escaped, limit, prefix.len() + 8, prefix.len() + 17);
    let over = format!("{prefix}'123456😀z'");
    assert_eq!(
        assert_error(&over, limit, prefix.len() + 11, prefix.len() + 12),
        "aggregate UTF-16 code unit ceiling across all string literals in one script exceeded",
    );
}

#[test]
fn one_token_and_one_instruction_plus_source_bytes_and_decoded_units() {
    let source = "'a😀\\x00\\\r\n'";
    let exact = Limits {
        max_source_bytes: source.len(),
        max_tokens: 1,
        max_nesting_depth: 0,
        max_work: source.len() + 4 + 1,
    };
    let script = parse_script_with_limits(source, exact).unwrap();
    assert_eq!(script.instruction_count(), 1);
    assert_eq!(script.max_stack_depth(), 1);
    for (limits, limit, span) in [
        (
            Limits {
                max_work: exact.max_work - 1,
                ..exact
            },
            Limit::Work,
            Span {
                start: 0,
                end: source.len(),
            },
        ),
        (
            Limits {
                max_tokens: 0,
                ..exact
            },
            Limit::Tokens,
            Span { start: 0, end: 1 },
        ),
        (
            Limits {
                max_source_bytes: 3,
                ..exact
            },
            Limit::SourceBytes,
            Span { start: 2, end: 6 },
        ),
        (
            Limits {
                max_work: 6,
                ..exact
            },
            Limit::Work,
            Span { start: 2, end: 6 },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        assert_eq!(error.span(), span);
    }
    // The literal cached after the semicolon is not rescanned or recharged.
    let latter = Limits {
        max_source_bytes: 7,
        max_tokens: 3,
        max_work: 12,
        ..exact
    };
    assert_eq!(
        parse_script_with_limits("'a';'b'", latter)
            .unwrap()
            .instruction_count(),
        3
    );
}

#[test]
fn strings_move_through_bindings_and_control_flow() {
    assert_units(
        "let a, b; const c = 'yes'; (a = b = c), b",
        &[121, 101, 115],
    );
    assert_units(
        "let x = 'old'; { const y = '\\uD800'; x = y; } x",
        &[0xd800],
    );
    assert_units("let x = 'no'; x = 3; x = ''; x ??= +''; x", &[]);
    assert_units("let x = ''; x ||= 'yes'; (x ||= +''); x", &[121, 101, 115]);
    assert_units("let x = ''; (x &&= +''); x", &[]);
    assert_units("let x = 'x'; (x &&= 'yes')", &[121, 101, 115]);
    assert_units("'' && +''", &[]);
    assert_units("'' ?? +''", &[]);
    assert_units("null ?? ''", &[]);
    assert_units("'x' || +''", &[120]);
    assert_scalar("!'' && !!'\\0' && !!'\\uD800'", Value::Boolean(true));
    assert_scalar("void '\\uD800'", Value::Undefined);
    assert_scalar("'' ? +'' : 42", Value::Number(42.0));
    assert_scalar(
        "let x = 'x', n = 0; while (x) { x = ''; n++; } for (; '';) { n++; } do { n++; } while (''); if ('x') { n++; } n",
        Value::Number(3.0),
    );
}

#[test]
fn equality_uses_units_not_ranges_or_unicode_normalization() {
    for (source, expected) in [
        (r"'😀' === '\uD83D\uDE00'", true),
        (r"'\uD800' == '\u{d800}'", true),
        (r"'\uD800' !== '\uFFFD'", true),
        ("'é' == 'e\u{301}'", false),
        ("'a' == 'b'", false),
        ("'' != ''", false),
        ("'a' != 'aa'", true),
        ("'' === 0", false),
        ("false !== ''", true),
        ("'' === null", false),
        ("'' == null", false),
        ("null == ''", false),
        ("'' == undefined", false),
        ("undefined != ''", true),
        ("'' === undefined", false),
    ] {
        assert_scalar(source, Value::Boolean(expected));
    }
}

#[test]
fn unsupported_operations_fail_only_when_dispatched() {
    for expr in [
        "+''",
        "-''",
        "~''",
        "'a' + 'b'",
        "1 + ''",
        "'' - 1",
        "1 * ''",
        "'' / 1",
        "'' % 1",
        "1 ** ''",
        "'' & 1",
        "1 ^ ''",
        "'' | 1",
        "'' << 1",
        "1 >> ''",
        "'' >>> 1",
        "'' < 'x'",
        "'' <= 1",
        "1 > ''",
        "1 >= ''",
        "'' == 0",
        "0 != ''",
        "'' != false",
        "true == '1'",
        "void ('' + '')",
    ] {
        let script = parse_script(expr).unwrap();
        assert_eq!(
            script.evaluate_primitive(),
            Err(EvalError::UnsupportedStringOperation),
            "{expr}"
        );
        assert_scalar(&format!("false && ({expr})"), Value::Boolean(false));
    }
    for update in ["x++", "++x", "x--", "--x"] {
        for source in [
            format!("let x = ''; {update}; 0"),
            format!("let x = ''; ({update})"),
        ] {
            assert_eq!(
                parse_script(&source).unwrap().evaluate(),
                Err(EvalError::UnsupportedStringOperation)
            );
        }
    }
    for op in [
        "+=", "-=", "*=", "/=", "%=", "**=", "&=", "|=", "^=", "<<=", ">>=", ">>>=",
    ] {
        for source in [
            format!("let x = ''; x {op} 1; 0"),
            format!("let x = 1; (x {op} '')"),
        ] {
            assert_eq!(
                parse_script(&source).unwrap().evaluate(),
                Err(EvalError::UnsupportedStringOperation)
            );
        }
    }
    assert_scalar(
        "let x = ''; if ('') { x++; x += 1; } 42",
        Value::Number(42.0),
    );
    assert_scalar(
        "let x = ''; for (; ''; ++x) { x *= 2; } 42",
        Value::Number(42.0),
    );
    assert!(parse_script(r"true || '\x00' + '\u{}'").is_err());
    assert!(parse_script("true || '' + missing").is_err());
    assert!(parse_script("const x = ''; false && ++x").is_err());
}

#[test]
fn equality_pair_budget_is_inclusive_shared_and_fresh_per_eval() {
    let suffix = "a".repeat(1023);
    let prelude = format!(
        "const x = 'a{suffix}', y = 'b{suffix}'; let i = 0; while (i < 64) {{ x != y; i++; }} "
    );
    let pair_budget = EvalError::StringComparisonLimitExceeded;
    // 64 reached loose inequalities charge 1,024 pairs each even though the
    // first unit mismatches. No remaining pairs, but different lengths are free.
    assert_scalar(&format!("{prelude}x !== ''"), Value::Boolean(true));
    assert_units(
        &format!("{prelude}'' || x"),
        &format!("a{suffix}").encode_utf16().collect::<Vec<_>>(),
    );
    assert_scalar(
        &format!("{prelude}false && (x == x)"),
        Value::Boolean(false),
    );
    let exhausted = parse_script(&format!("{prelude}'z' === 'z'")).unwrap();
    assert_eq!(exhausted.evaluate_primitive(), Err(pair_budget));
    assert_eq!(exhausted.evaluate_value(), Err(pair_budget));
    assert_eq!(exhausted.evaluate(), Err(pair_budget));
    let inclusive = parse_script(&format!(
        "const x = 'a{suffix}'; let i = 0; while (i < 64) {{ x === x; i++; }} i"
    ))
    .unwrap();
    assert_eq!(inclusive.evaluate(), Ok(64.0));
    assert_eq!(inclusive.evaluate(), Ok(64.0));
}
