use swayze_js::{
    ErrorKind, EvalError, EvalScratch, Limit, Limits, PrimitiveValue, Span, Value, parse_script,
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

fn assert_parse_error(source: &str, limits: Limits, kind: ErrorKind, start: usize, end: usize) {
    let error = parse_script_with_limits(source, limits).unwrap_err();
    assert_eq!(error.kind(), kind, "{source:?}: {error}");
    assert_eq!(error.span(), Span { start, end }, "{source:?}: {error}");
    assert!(source.get(start..end).is_some());
}

fn assert_error(source: &str, kind: ErrorKind, start: usize, end: usize) {
    assert_parse_error(source, Limits::default(), kind, start, end);
}

#[test]
fn frontier_spelling_and_script_owned_utf16() {
    assert_units("``", &[]);
    for source in [
        "`NoSubstitutionTemplate` === 'NoSubstitutionTemplate'",
        "`\u{180e}` === \"\\u180E\"",
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate_value(),
            Ok(Value::Boolean(true))
        );
    }
    let script = {
        let source = String::from("`\0🦀\\uD800\\u{000DFFF}\\u{10FFFF}`");
        parse_script(&source).unwrap()
    };
    assert_eq!(script.instruction_count(), 1);
    assert_eq!(script.max_stack_depth(), 1);
    let Ok(PrimitiveValue::String(first)) = script.evaluate_primitive() else {
        panic!("expected Script-owned String")
    };
    let mut scratch = EvalScratch::new();
    let Ok(PrimitiveValue::String(second)) = script.evaluate_primitive_with_scratch(&mut scratch)
    else {
        panic!("expected Script-owned String with scratch")
    };
    assert_eq!(first, &[0, 0xd83e, 0xdd80, 0xd800, 0xdfff, 0xdbff, 0xdfff]);
    assert!(std::ptr::eq(first, second));
    assert_eq!(script.evaluate_value(), Err(EvalError::NonScalarResult));
    assert_eq!(script.evaluate(), Err(EvalError::NonNumericResult));
}

#[test]
fn cooked_line_endings_and_continuations() {
    assert_units(
        "`a\r\nb\rc\nd\u{2028}e\u{2029}f`",
        &[97, 10, 98, 10, 99, 10, 100, 0x2028, 101, 0x2029, 102],
    );
    assert_units("`a\\\r\n\\\r\\\n\\\u{2028}\\\u{2029}\nb`", &[97, 10, 98]);
    assert_units(
        r"`\b\f\n\r\t\v\0\x0d\u000d\u{D}`",
        &[8, 12, 10, 13, 9, 11, 0, 13, 13, 13],
    );
    assert_units(
        r#"`'"\'\"\q\😀\0９`"#,
        &[39, 34, 39, 34, 113, 0xd83d, 0xde00, 0, 0xff19],
    );
    let text = "/* literal */ // literal {} $x $ {";
    assert_units(
        &format!("`{text}`"),
        &text.encode_utf16().collect::<Vec<_>>(),
    );
}

#[test]
fn interpolation_only_starts_at_unescaped_dollar_brace() {
    for (source, text) in [
        (r"`\`\${x}`", "`${x}"),
        (r"`\\\${x}`", r"\${x}"),
        (r"`$\{x}`", "${x}"),
        (r"`\u0024{x}\u{60}`", "${x}`"),
    ] {
        assert_units(source, &text.encode_utf16().collect::<Vec<_>>());
    }
    for source in [
        "`${`",
        "`é${host()}ignored`",
        r"`\`$\{ok}${bad}`",
        r"`\\${bad}`",
        r"false && `\${ok}${bad}`",
        "if (false) { `${bad}`; } 42",
    ] {
        let start = source.rfind("${").unwrap();
        assert_error(source, ErrorKind::Unsupported, start, start + 2);
    }
}

#[test]
fn syntax_errors_on_reached_and_skipped_paths() {
    for (body, start, end) in [
        ("`é", 0, 3),
        ("`é\\", 3, 4),
        ("`é\\x😀`", 3, 9),
        ("`é\\u000é`", 3, 10),
        (r"`\x`", 1, 4),
        (r"`\u{", 1, 4),
        (r"`\u{}`", 1, 5),
        (r"`\u{110000}`", 1, 11),
        (r"`\u{1_2}`", 1, 6),
        (r"`\1`", 1, 3),
        (r"`\77`", 1, 3),
        (r"`\8`", 1, 3),
        (r"`\9`", 1, 3),
        (r"`\00`", 1, 4),
        (r"`\08`", 1, 4),
        (r"`\09`", 1, 4),
    ] {
        for prefix in ["", "false && ", "true ? 0 : ", "let x = 1; x ||= "] {
            assert_error(
                &format!("{prefix}{body}"),
                ErrorKind::Malformed,
                prefix.len() + start,
                prefix.len() + end,
            );
        }
    }
    let source = "false && (`safe`, unknown)";
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.span(), Span { start: 18, end: 25 });
    assert!(error.message().contains("unbound"));
}

#[test]
fn tagged_templates_and_calls_remain_unsupported() {
    for (prefix, body) in [
        ("1 ", "`plain`"),
        ("1\n", "`unterminated"),
        ("(1) /*😀*/ ", "`${sub}`"),
        ("let x = 1; x /*\r\n*/ ", r"`\x`"),
        ("let x = 1; x++ ", "`tagged`"),
        ("let x = 1; ++x ", "`tagged`"),
        ("let x = 1; false && ++x ", "`tagged`"),
        ("let x = 1; false && typeof x ", "`tagged`"),
        ("let x = 1; for (; false; x ", "`tagged`) {} 42"),
        ("let x = 1; for (; false; x++ ", "`tagged`) {} 42"),
        ("false && 'str'.length ", r"`\8`"),
        ("false && `a` ", "`b`"),
    ] {
        assert_error(
            &format!("{prefix}{body}"),
            ErrorKind::Unsupported,
            prefix.len(),
            prefix.len() + 1,
        );
    }
    for prefix in [
        "`a`",
        "false && `a`",
        "true ? 42 : (`a`)",
        "`a`.length /*\n*/",
    ] {
        assert_error(
            &format!("{prefix}()"),
            ErrorKind::Unsupported,
            prefix.len(),
            prefix.len() + 1,
        );
    }
}

#[test]
fn templates_stop_and_never_enter_the_directive_prologue() {
    for source in [
        "`use strict`",
        "`use strict`;",
        "'ordinary'; `use strict`",
        "`ordinary`; 'use strict'",
        r"`\u006frdinary`; 'use strict'",
    ] {
        assert_units(source, &"use strict".encode_utf16().collect::<Vec<_>>());
    }
    assert_error("'use strict'; `ok`", ErrorKind::Unsupported, 0, 12);
    assert_error("`use strict`\n'other'", ErrorKind::Unsupported, 13, 20);
    assert_error("`use strict`\n`other`", ErrorKind::Unsupported, 13, 14);
}

#[test]
fn expression_positions_and_precedence_share_primitive_string_path() {
    for source in [
        "typeof `😀` /*s*/ . /*p*/ length === 'number'",
        "typeof (`x`) === 'string'",
        "typeof `x` + `!` === 'string!'",
        "`3` ** `23`.length === 9",
        "`a\r\nb` === 'a\\nb'",
        "false ? `a` : `` || `fallback` === 'fallback'",
        "if (false) {} `after if`.length === 8",
        "let x = `a`, y; x += `b`; y = x = `c`; y === 'c'",
        "let hit = false; for (`init`; `test`; `update`) { hit = true; break; } hit",
        "let hit = false; if (`test`) { `stmt`; hit = true; } hit",
        "let x = 0; x++, `suffix`; ((x = `3`), x) === '3'",
    ] {
        assert_eq!(
            parse_script(source)
                .unwrap_or_else(|e| panic!("{source:?}: {e}"))
                .evaluate_value(),
            Ok(Value::Boolean(true)),
            "{source:?}"
        );
    }
    assert_error("typeof `x` ** 2", ErrorKind::Malformed, 11, 13);
    assert_error("`a`.length.length", ErrorKind::Unsupported, 10, 11);
}

#[test]
fn exact_lex_and_emission_work_includes_trivia_and_both_crlf_bytes() {
    let prefix = "/*😀*/ ";
    let start = prefix.len();
    let source = format!("{prefix}`a\r\n\\\r\n😀`");
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 1,
        max_nesting_depth: 0,
        max_work: source.len() + 4 + 1,
    };
    let script = parse_script_with_limits(&source, limits).unwrap();
    assert_eq!(
        script.evaluate_primitive(),
        Ok(PrimitiveValue::String(&[97, 10, 0xd83d, 0xde00]))
    );
    assert_parse_error(
        &source,
        Limits {
            max_tokens: 0,
            ..limits
        },
        ErrorKind::LimitExceeded(Limit::Tokens),
        start,
        start + 1,
    );
    for (work, from, to) in [
        (start - 1, start - 1, start),              // trivia
        (start + 4, start + 3, start + 4),          // second raw CRLF byte
        (start + 5, start + 2, start + 4),          // normalized LF append
        (start + 8, start + 6, start + 7),          // second continued CRLF byte
        (limits.max_work - 1, start, source.len()), // instruction
    ] {
        assert_parse_error(
            &source,
            Limits {
                max_work: work,
                ..limits
            },
            ErrorKind::LimitExceeded(Limit::Work),
            from,
            to,
        );
    }
    let source = "'a'; `b`; `c`";
    let limits = Limits {
        max_tokens: 5,
        max_work: source.len() + 3 + 5,
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(source, limits)
            .unwrap()
            .instruction_count(),
        5
    );
    assert_parse_error(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        ErrorKind::LimitExceeded(Limit::Work),
        10,
        13,
    );
    let source = "`ab${unscanned}`";
    assert_parse_error(
        source,
        Limits {
            max_work: 6,
            ..Limits::default()
        },
        ErrorKind::LimitExceeded(Limit::Work),
        4,
        5,
    );
    assert_parse_error(
        source,
        Limits {
            max_work: 7,
            ..Limits::default()
        },
        ErrorKind::Unsupported,
        3,
        5,
    );
}

#[test]
fn exact_token_byte_and_shared_pool_limits() {
    let limit = ErrorKind::LimitExceeded(Limit::StringLiteralBytes);
    let ascii = "a".repeat(4094);
    let exact = format!("`{ascii}`");
    assert_units(&exact, &vec![97; 4094]);
    assert_error(&format!("`{ascii}a`"), limit, 4096, 4097);
    assert_error(&format!("/*x*/`{ascii}😀`"), limit, 4100, 4104);
    for tail in ["\r\n", "\\\r\n", "${", "\\x1"] {
        let body = "a".repeat(4096 - tail.len());
        assert_error(&format!("`{body}{tail}"), limit, 4096, 4097);
    }
    let exact = format!("`\\u{{{}41}}`", "0".repeat(4088));
    assert_eq!(exact.len(), 4096);
    assert_units(&exact, &[65]);

    let prefix = format!("if (false) {{ `{ascii}`; '{ascii}'; `{ascii}`; '{ascii}'; }} ");
    assert_units(&format!("{prefix}`123456😀`; ''"), &[]);
    assert_units(&format!("{prefix}'123456😀'; ``"), &[]);
    let limit = ErrorKind::LimitExceeded(Limit::StringLiteralUnits);
    let start = prefix.len() + 8;
    assert_error(&format!("{prefix}`1234567😀`"), limit, start, start + 4);
    assert_error(
        &format!("{prefix}`1234567\r\n`; 'a'"),
        limit,
        start + 6,
        start + 7,
    );
    assert_error(
        &format!("{prefix}'12345678'; `\r\n`"),
        limit,
        start + 5,
        start + 7,
    );
}
