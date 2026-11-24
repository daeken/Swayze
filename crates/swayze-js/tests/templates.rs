use swayze_js::{
    ErrorKind, EvalError, EvalScratch, Limit, Limits, PrimitiveValue, Span, Value, parse_script,
    parse_script_with_limits,
};

fn assert_units(source: &str, expected: &[u16]) {
    let script = parse_script(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    assert_eq!(
        script.evaluate_primitive_with_scratch(&mut EvalScratch::new()),
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
    for (source, text) in [
        (r"`\`$\{ok}${1}`", "`${ok}1"),
        (r"`\\${1}`", r"\1"),
        (r"`${1}\${x}${2}$\{y}`", "1${x}2${y}"),
    ] {
        assert_units(source, &text.encode_utf16().collect::<Vec<_>>());
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
        ("false && `${1}` ", r"`\x"),
        ("false && `${name ", r"`\8"),
        ("true ? 42 : `${++x ", r"`\8"),
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
        "`${1}`",
        "false && `${1}`",
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
        "`use ${'strict'}`",
        "'ordinary'; `use ${'strict'}`",
        "`${'ordinary'}`; 'use strict'",
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
fn exact_frontier_cases_and_flat_lowering() {
    for (source, text, count) in [
        ("`foo ${`bar ${5} baz`} qux`", "foo bar 5 baz qux", 5),
        ("`${0} ${`bar ${5} baz`} qux`", "0 bar 5 baz qux", 7),
        ("`${0} ${1} ${`bar ${5} baz`} qux`", "0 1 bar 5 baz qux", 9),
    ] {
        assert_units(source, &text.encode_utf16().collect::<Vec<_>>());
        let script = parse_script(source).unwrap();
        assert_eq!(script.instruction_count(), count);
        assert_eq!(script.max_stack_depth(), 3);
    }
    for (source, units) in [("`${''}`", &[][..]), ("`${''}${''}a`", &[97])] {
        assert_units(source, units);
        let script = parse_script(source).unwrap();
        assert_eq!(script.max_stack_depth(), 2);
        assert_eq!(
            script.evaluate_primitive(),
            Err(EvalError::RuntimeStringResultRequiresScratch)
        );
        assert_eq!(script.evaluate_value(), Err(EvalError::NonScalarResult));
        assert_eq!(script.evaluate(), Err(EvalError::NonNumericResult));
    }
}

#[test]
fn raw_continuations_and_primitive_to_string() {
    let text = "a\n } // comment?\n/* still text */   undefined\n🌊`$d";
    assert_units(
        "`a\r\n${ /* } `${ */ ''\n } } // comment?\r/* still text */   ${void 1}\\\r\n\r\n🌊\\`\\$d`",
        &text.encode_utf16().collect::<Vec<_>>(),
    );
    for (source, text) in [
        (
            "`${true},${false},${null},${undefined}`",
            "true,false,null,undefined",
        ),
        ("`${-0},${0/0},${1/0},${-1/0}`", "0,NaN,Infinity,-Infinity"),
        (
            "`${1e-7},${1e-6},${1e20},${1e21},${5e-324}`",
            "1e-7,0.000001,100000000000000000000,1e+21,5e-324",
        ),
        (
            "`${0x20}:${1_00.0}:${typeof (missing)}`",
            "32:100:undefined",
        ),
        ("`${'😀' + 'x'}|${`x${1}y`}`", "😀x|x1y"),
    ] {
        assert_units(source, &text.encode_utf16().collect::<Vec<_>>());
    }
    assert_units(
        r"`\uDC00${'\0\uD800'}\uD801${'\uDFFF'}\0`",
        &[0xdc00, 0, 0xd800, 0xd801, 0xdfff, 0],
    );
}

#[test]
fn substitution_expression_boundaries_and_evaluation_order() {
    for source in [
        "typeof `${typeof (missing)}`.length === 'number'",
        "typeof (`${2}`) + '!' === 'string!'",
        "`${3}` ** `${23}`.length === 9",
        "1 + `${1}` + 2 === '112'",
        "`${1 + 2 * 3}x${(1, 2)}` === '7x2'",
        "`${null ?? 'x'}${false || `${true && 1}`}` === 'x1'",
        "let x = 0, y; `${x = y = 1, ++x}${x++}${x = x + y}:${x}` === '224:4'",
        "let x = 0; `${x += (x = 2)}${(x = 3, x++)}${true ? x = 8 : x = 9}${x}` === '2388'",
        "let x = 1; const out = `${x ||= (1).length}${x &&= (x = 2)}${x}`; out === '122'",
        "let x = 'a'; const out = `${x += `${x = 'b'}`}${x}`; out === 'abab'",
        "let x; let a = `${x = 1, x++}`, b = `${x}`; a + b === '12'",
        "let x = 0; if (`${x = 2, ''}`) { x = 0; } else { `${x += 1}`; } x === 3",
        "let x = 0; for (`${x = 1}`; x < 2; `${x += 1}`) {} x === 2",
        "let x = 0; false && `${x = 1}`; true ? x === 0 : `${x = 2}`",
        "`${true ? 'ok' : (1).length}${false && (1).length}` === 'okfalse'",
        "false && `${(1).length}` || true",
    ] {
        assert_eq!(
            parse_script(source)
                .unwrap_or_else(|e| panic!("{source:?}: {e}"))
                .evaluate_value(),
            Ok(Value::Boolean(true)),
            "{source}"
        );
    }
    let source = "typeof `${missing}`";
    let start = source.find("missing").unwrap();
    assert_error(source, ErrorKind::Unsupported, start, start + 7);
    let source = "`${typeof (missing, missing)}`";
    let start = source.find("missing").unwrap();
    assert_error(source, ErrorKind::Unsupported, start, start + 7);
    assert_error("typeof `${1}` ** 2", ErrorKind::Malformed, 14, 16);
    assert_error("`${1}`.length.length", ErrorKind::Unsupported, 13, 14);
}

#[test]
fn invalid_substitution_stops_before_raw_continuation() {
    for (body, offending) in [
        (r"`${}\8`", "}"),
        (r"`${1 + }\8`", "}"),
        (r"`${(1}\8`", "}"),
        (r"`${(x = 1}\8`", "}"),
        (r"`${true ? 1}\8`", "}"),
        (r"`${1}${}\8`", "}\\"),
        (r"`${1, }\8`", "}"),
        (r"`${1; 2}\8`", ";"),
        (r"`${1) }\8`", ")"),
        (r"`${1}\8`", "\\8"),
        (r"`${1}\8${2}`", "\\8"),
        (r"`${`\8`}`", "\\8"),
        (r"`${`\9${1}`}`", "\\9"),
        (r"`${`${1}\8${2}`}`", "\\8"),
        (r"`${`${1}\9`}`", "\\9"),
        (r"`${`${1}\xg`}z`", "\\xg"),
    ] {
        let start = body.find(offending).unwrap();
        let length = offending.trim_end_matches('\\').len();
        for prefix in ["", "false && ", "true ? 0 : "] {
            assert_error(
                &format!("{prefix}{body}"),
                ErrorKind::Malformed,
                prefix.len() + start,
                prefix.len() + start + length,
            );
        }
    }
    for (source, start) in [("`${", 3), ("`${1", 4), ("`${1}raw", 4)] {
        assert_error(source, ErrorKind::Malformed, start, source.len());
    }
    for (source, token) in [
        ("false && `${missing}`", "missing"),
        ("false && `${host()}`", "("),
        ("false && `${{}`", "{}"),
        ("false && `${/x/}`", "/"),
        ("false && `${1n}`", "n"),
        ("false && `${1?.length}`", "?."),
    ] {
        let start = if token == "{}" {
            source.find("${").unwrap() + 2
        } else {
            source.find(token).unwrap()
        };
        let length = if token == "{}" { 1 } else { token.len() };
        assert_error(source, ErrorKind::Unsupported, start, start + length);
    }
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
        ErrorKind::LimitExceeded(Limit::Work),
        0,
        5,
    );
}

#[test]
fn exact_substitution_tokens_work_and_shared_nesting() {
    let source = "`a${1} b${2}c`";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 5, // Head, 1, Middle, 2, Tail; '}' is not a second token.
        max_nesting_depth: 1,
        // Source bytes, cooked units, decimal conversion, one marker, five emissions.
        max_work: source.len() + 4 + 2 + 1 + 5,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 5);
    assert_eq!(script.max_stack_depth(), 2);
    for tokens in [2, 4] {
        let brace = source.match_indices('}').nth(tokens / 2 - 1).unwrap().0;
        assert_parse_error(
            source,
            Limits {
                max_tokens: tokens,
                ..limits
            },
            ErrorKind::LimitExceeded(Limit::Tokens),
            brace,
            brace + 1,
        );
    }
    assert_parse_error(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        ErrorKind::LimitExceeded(Limit::Work),
        11,
        source.len(),
    );
    assert_parse_error(
        source,
        Limits {
            max_nesting_depth: 0,
            ..limits
        },
        ErrorKind::LimitExceeded(Limit::NestingDepth),
        0,
        4,
    );

    for (source, span) in [
        ("`${`${1}`}`", 3..6),
        ("(`${1}`)", 1..4),
        ("`${(1)}`", 3..4),
        ("`${true?1:2}`", 7..8),
        ("let x; `${x=1}`", 11..12),
    ] {
        let limits = Limits {
            max_nesting_depth: 1,
            ..Limits::default()
        };
        assert_parse_error(
            source,
            limits,
            ErrorKind::LimitExceeded(Limit::NestingDepth),
            span.start,
            span.end,
        );
        assert!(
            parse_script_with_limits(
                source,
                Limits {
                    max_nesting_depth: 2,
                    ..limits
                }
            )
            .is_ok()
        );
    }
    let nested = format!("{}0{}", "`${".repeat(128), "}`".repeat(128));
    let script = parse_script(&nested).unwrap();
    assert_eq!(script.max_stack_depth(), 129);
    assert_eq!(script.instruction_count(), 257);
    assert_eq!(
        script.evaluate_primitive_with_scratch(&mut EvalScratch::new()),
        Ok(PrimitiveValue::String(&[48]))
    );
    let too_deep = format!("`${{{nested}}}`");
    assert_error(
        &too_deep,
        ErrorKind::LimitExceeded(Limit::NestingDepth),
        384,
        387,
    );
}

#[test]
fn exact_segment_bytes_and_runtime_result_limits() {
    let head = "a".repeat(4093);
    let tail = "b".repeat(4094);
    let at = format!("`{head}${{'b'}}`");
    assert_units(&at, &format!("{head}b").encode_utf16().collect::<Vec<_>>());
    assert_units(&format!("`${{''}}{tail}`"), &vec![98; 4094]);
    let skipped = format!("false && `{head}${{1}}{head}${{2}}{tail}`");
    assert_eq!(
        parse_script(&skipped).unwrap().evaluate_value(),
        Ok(Value::Boolean(false))
    );
    let bytes = ErrorKind::LimitExceeded(Limit::StringLiteralBytes);
    assert_error(&format!("`{head}x${{1}}`"), bytes, 4096, 4097);
    assert_error(&format!("`${{1}}{tail}x`"), bytes, 4100, 4101);
    assert_error(&format!("`${{1}}{head}x${{2}}`"), bytes, 4100, 4101);
    assert_error(&format!("`${{1}}{tail}😀`"), bytes, 4099, 4103);
    for following in ["\\\r\n", "\r\n", "\\xA0"] {
        let part = "b".repeat(4096 - following.len());
        assert_error(&format!("`${{1}}{part}{following}`"), bytes, 4100, 4101);
    }
    let prefix = format!("false && `{head}${{'{tail}'}}{head}${{2}}{tail}`; ");
    // The four pieces above have 16374 units. The surrogate pair reaches
    // the shared cap; empty ranges after it still validate.
    assert_units(
        &format!("{prefix}`1234${{''}}56${{''}}78😀${{''}}`"),
        &"12345678😀".encode_utf16().collect::<Vec<_>>(),
    );
    let over_pool = format!("{prefix}`1234${{''}}567${{''}}89😀`");
    let start = over_pool.find('😀').unwrap();
    assert_error(
        &over_pool,
        ErrorKind::LimitExceeded(Limit::StringLiteralUnits),
        start,
        start + 4,
    );

    for source in [
        format!("`{head}${{0}}b${{(1).length}}`"),
        format!("`${{'x'}}{tail}`"),
        format!("`{head}${{'xx'}}`"),
        format!("`{head}${{1e21}}`"),
    ] {
        assert_eq!(
            parse_script(&source).unwrap().evaluate_value(),
            Err(EvalError::StringConcatenationLengthLimitExceeded)
        );
    }
}

#[test]
fn empty_template_append_still_spends_dispatch_fuel() {
    let source = "let i = 0; while (i++ < 1819) { `${''}`; } + + + + + + 42";
    // 2 init + 1819 * (4 test + 4 body + 1 back edge) + 4 test + 7 final.
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(42.0));
    assert_eq!(
        parse_script(&format!("{source} + 0")).unwrap().evaluate(),
        Err(EvalError::DispatchLimitExceeded)
    );
}

#[test]
fn append_budget_counts_every_prefix_copy_before_the_next_substitution() {
    // 1 + ... + 255 = 32640; prefill 128 to reach exactly 32768.
    let prefix = format!("'{}' + ''; `{}`; ", "x".repeat(128), "${'x'}".repeat(255));
    assert_units(&format!("{prefix}`${{''}}${{''}}`"), &[]);
    let over = format!("{prefix}`${{'x'}}${{(1).length}}`");
    let script = parse_script(&over).unwrap();
    let mut scratch = EvalScratch::new();
    assert_eq!(
        script.evaluate_primitive_with_scratch(&mut scratch),
        Err(EvalError::StringConcatenationBudgetExceeded)
    );
    let flat = format!("`{}${{''}}${{(1).length}}`", "${'x'}".repeat(255));
    assert_eq!(
        parse_script(&flat).unwrap().evaluate_value(),
        Err(EvalError::StringConcatenationBudgetExceeded)
    );
    assert_eq!(
        parse_script("`a${2}b`")
            .unwrap()
            .evaluate_primitive_with_scratch(&mut scratch),
        Ok(PrimitiveValue::String(&[97, 50, 98]))
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
