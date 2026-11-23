use swayze_js::{
    ErrorKind, EvalError, EvalScratch, Limit, Limits, PrimitiveValue, Span, Value, parse_script,
    parse_script_with_limits,
};

fn assert_string(source: &str, expected: &str) {
    let script = parse_script(source).unwrap_or_else(|e| panic!("{source}: {e}"));
    let expected: Vec<_> = expected.encode_utf16().collect();
    let result = Ok(PrimitiveValue::String(expected.as_slice()));
    assert_eq!(script.evaluate_primitive(), result, "{source}");
    assert_eq!(
        script.evaluate_primitive_with_scratch(&mut EvalScratch::new()),
        result,
        "{source}"
    );
    assert_eq!(
        script.evaluate(),
        Err(EvalError::NonNumericResult),
        "{source}"
    );
    assert_eq!(
        script.evaluate_value(),
        Err(EvalError::NonScalarResult),
        "{source}"
    );
}

fn assert_parse_error(prefix: &str, site: &str, suffix: &str, kind: ErrorKind, message: &str) {
    let source = format!("{prefix}{site}{suffix}");
    let error = parse_script(&source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start: prefix.len(),
            end: prefix.len() + site.len()
        },
        "{source}"
    );
    assert_eq!(error.message(), message, "{source}");
}

#[test]
fn exact_utf16_primitive_spellings_use_both_string_apis() {
    for (source, expected) in [
        ("typeof x", "undefined"), // Exact expression and expected UTF-16 of the adapted Test262 seed.
        ("typeof (((x)))", "undefined"),
        ("typeof\n/* comment */ ((x))", "undefined"),
        ("typeof undefined", "undefined"),
        ("typeof void 0", "undefined"),
        ("let x; typeof x", "undefined"),
        ("typeof -0", "number"),
        ("typeof (0/0)", "number"),
        ("typeof (1/0)", "number"),
        ("typeof true", "boolean"),
        ("typeof false", "boolean"),
        ("typeof ''", "string"),
        ("typeof ('a' + 'b')", "string"),
        ("typeof typeof missing", "string"),
        ("typeof null", "object"),
        (
            "let typeof0 = 1, typeof_ = true, Typeof = null, $typeof = ''; typeof typeof0 === 'number' && typeof Typeof === 'object' ? typeof typeof_ : typeof $typeof",
            "boolean",
        ),
    ] {
        assert_string(source, expected);
    }
}

#[test]
fn only_a_direct_read_with_optional_pure_groups_can_be_unresolvable() {
    for (prefix, suffix) in [
        ("", ""),
        ("typeof (1, ", ")"),
        ("typeof ((0, (1, ((", ")))))"),
        ("typeof (", ", 1)"),
        ("typeof (false ? 1 : ", ")"),
        ("typeof (true ? 1 : ", ")"), // Static despite skipped alternate.
        ("typeof (true ? ", " : 1)"),
        ("typeof (true || ", ")"),
        ("typeof (false && ", ")"),
        ("typeof (0 ?? ", ")"),
        ("typeof ('a' + ", ")"),
        ("typeof (1 ** ", ")"),
        ("typeof ", ".length"),
        ("typeof ((", ")).length"),
        ("typeof +((", "))"),
        ("typeof void ((", "))"),
        ("typeof ((++", "))"),
        ("typeof ((", "++))"),
        ("typeof (", " = 0)"),
        ("let n; typeof (n = (", "))"),
        ("let n; typeof (n ||= (", "))"),
        ("false && typeof (0, ", ")"),
        ("typeof missing, ", ""), // Only the first access can be patched.
    ] {
        assert_parse_error(
            prefix,
            "missing",
            suffix,
            ErrorKind::Unsupported,
            "unbound name is outside the subset",
        );
    }
    // A value-producing enclosing expression does not disqualify an inner direct operand.
    assert_string("(0, true && typeof (((missing))) )", "undefined");
    assert_string("true ? typeof missing : typeof other", "undefined");
    assert_string("true ? typeof missing : typeof 0", "undefined");
    assert_string("typeof (0, typeof missing)", "string");
    assert_string("let n; typeof (n = typeof missing)", "string");
}

#[test]
fn lexical_bindings_win_before_either_unbound_read_fallback() {
    assert_string("let x = true; typeof (((x)))", "boolean");
    assert_string(
        "let x = true, out; { const x = null; out = typeof x; } out",
        "object",
    );
    assert_string(
        "let out; { const undefined = 42; out = typeof undefined; } out",
        "number",
    );
    assert_string(
        "let out; { let x = 1; } { out = typeof x; } out",
        "undefined",
    );
    assert_string(
        "let out; for (let x = ''; !out; ) { out = typeof x; } out",
        "string",
    );
    assert_string("false && typeof missing; typeof undefined", "undefined");
    for (prefix, name, suffix) in [
        ("let x = typeof (", "x", "); x"),
        ("const y = typeof ", "x", ", x = 1; y"),
        (
            "if (false) { typeof ",
            "undefined",
            "; let undefined = 1; } 0",
        ),
        ("true ? 0 : typeof ", "x", "; const x = 1; 0"),
        ("for (let x = typeof ", "x", "; false;) {} 0"),
    ] {
        assert_parse_error(
            prefix,
            name,
            suffix,
            ErrorKind::Unsupported,
            "access before lexical binding initialization is outside the subset",
        );
    }
    assert_parse_error(
        "const x = 1; false && typeof (x ",
        "=",
        " 2)",
        ErrorKind::Unsupported,
        "assignment to const is outside the subset",
    );
}

#[test]
fn later_binding_blocks_the_same_typeof_read_on_live_and_dead_paths() {
    for condition in ['1', '0'] {
        assert_parse_error(
            &format!("let x = true; if ({condition}) {{ typeof ((("),
            "x",
            "))); let x = 0; } x",
            ErrorKind::Unsupported,
            "access before lexical binding initialization is outside the subset",
        );
    }
}

#[test]
fn ordinary_unary_suffix_and_power_precedence_and_grammar() {
    for (source, expected) in [
        ("typeof 'hi'.length", "number"),
        ("typeof (('hi').length)", "number"),
        ("typeof (2 ** 3)", "number"),
        (
            "(typeof missing) + '!' === 'undefined!' ? typeof 1 : typeof false",
            "number",
        ),
    ] {
        assert_string(source, expected);
    }
    for (source, expected) in [
        ("(typeof 'hi').length", Value::Number(6.0)),
        ("typeof 1 === 'number'", Value::Boolean(true)),
        ("typeof missing ? 1 : 0", Value::Number(1.0)),
        ("!typeof !false", Value::Boolean(false)),
        ("void typeof 0", Value::Undefined),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate_value(),
            Ok(expected),
            "{source}"
        );
    }
    for source in ["2 ** typeof missing", "(typeof 2) ** 3", "+typeof false"] {
        assert!(
            parse_script(source).unwrap().evaluate().unwrap().is_nan(),
            "{source}"
        );
    }
    for prefix in [
        "typeof 2 ",
        "typeof (2) ",
        "typeof missing ",
        "2 ** typeof 1 ",
    ] {
        assert_parse_error(
            prefix,
            "**",
            " 2",
            ErrorKind::Malformed,
            "a unary base of exponentiation requires parentheses",
        );
    }
    for (prefix, site, suffix, kind, message) in [
        (
            "typeof",
            "",
            "",
            ErrorKind::Malformed,
            "expected an operand before end of source",
        ),
        (
            "0 ",
            "typeof",
            " 1",
            ErrorKind::Malformed,
            "expected a binary operator",
        ),
        (
            "let ",
            "typeof",
            "; 0",
            ErrorKind::Unsupported,
            "reserved or excluded name is outside the subset",
        ),
        (
            "typeof x",
            "(",
            ")",
            ErrorKind::Unsupported,
            "call expressions are outside the arithmetic subset",
        ),
        (
            "typeof ",
            "globalThis",
            "",
            ErrorKind::Unsupported,
            "reserved or excluded name is outside the subset",
        ),
    ] {
        assert_parse_error(prefix, site, suffix, kind, message);
    }
    assert_eq!(
        parse_script("typeof (undefined.length)")
            .unwrap()
            .evaluate_primitive(),
        Err(EvalError::UnsupportedPropertyReceiver)
    );
    assert_string("true || typeof (undefined.length); typeof 0", "number");
}

#[test]
fn reached_operands_are_evaluated_once_and_skipped_operands_not_at_all() {
    let script = parse_script(
        "let n = 0;\n         true && typeof n++ === 'number' &&\n         typeof ++n === 'number' &&\n         typeof (n += 1) === 'number' &&\n         (false ? typeof n++ : typeof n) === 'number' &&\n         (true || typeof ++n) && n === 3",
    ).unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
}

#[test]
fn type_spellings_are_ordinary_strings_through_bindings_and_value_operations() {
    let script = parse_script(
        "let s = typeof missing;\n         s === 'undefined' && s == ('undef' + 'ined') && 'string' < s &&\n         s != undefined && s != 0 && (s ?? 0) === s &&\n         s.length === 9 && ('😀' + s).length === 11",
    ).unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
    for (source, expected) in [
        ("(typeof missing) + 42", "undefined42"),
        ("null + (typeof null)", "nullobject"),
        ("(typeof 'a') + (typeof true)", "stringboolean"),
        ("let x = 'a' + 'b'; x += typeof missing; x", "abundefined"),
        ("(typeof missing) + ('a' + 'b')", "undefinedab"),
    ] {
        let script = parse_script(source).unwrap();
        let units: Vec<_> = expected.encode_utf16().collect();
        assert_eq!(
            script.evaluate_primitive(),
            Err(EvalError::RuntimeStringResultRequiresScratch)
        );
        assert_eq!(
            script.evaluate_primitive_with_scratch(&mut EvalScratch::new()),
            Ok(PrimitiveValue::String(&units)),
            "{source}"
        );
    }
}

#[test]
fn static_spellings_obey_the_exact_concat_length_and_append_fences() {
    let run = |source: &str| parse_script(source).unwrap().evaluate();
    let part = "x".repeat(4085); // +9 = 4,094 code units.
    assert_eq!(run(&format!("('{part}' + typeof x).length")), Ok(4094.0));
    let prefix = format!("let n=8; while(n--) {{ '{part}' + typeof x; }}"); // 32,752 appended.
    let exact = format!("{prefix} (typeof x + '1234567').length");
    assert_eq!(run(&exact), Ok(16.0));
    assert_eq!(
        run(&format!("{prefix} (typeof x + '12345678').length")),
        Err(EvalError::StringConcatenationBudgetExceeded)
    );
    let full = format!("{exact};");
    assert_eq!(run(&format!("{full} (typeof x).length")), Ok(9.0));
    assert_eq!(
        run(&format!("{full} ('' + typeof x).length")),
        Err(EvalError::StringConcatenationBudgetExceeded)
    );
    let too_long = "x".repeat(4086);
    assert_eq!(
        run(&format!("(typeof x + '{too_long}').length")),
        Err(EvalError::StringConcatenationLengthLimitExceeded)
    );
    // Length error takes priority even when the aggregate budget is exhausted.
    let full = format!(
        "const s = '{too_long}'; let n=8; while(n--) {{ s + '12345678'; }} typeof x + '1234567';"
    );
    assert_eq!(
        run(&format!("{full} (typeof x + s).length")),
        Err(EvalError::StringConcatenationLengthLimitExceeded)
    );
}

#[test]
fn parser_accounts_for_typeof_and_keeps_metering_lexical_resolution() {
    let source = "typeof x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 2,
        max_nesting_depth: 0,
        max_work: source.len() + 3,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 2);
    assert_eq!(script.max_stack_depth(), 1);
    for (limits, kind, start, end) in [
        (
            Limits {
                max_source_bytes: 7,
                ..limits
            },
            Limit::SourceBytes,
            7,
            8,
        ),
        (
            Limits {
                max_tokens: 1,
                ..limits
            },
            Limit::Tokens,
            7,
            8,
        ),
        (
            Limits {
                max_work: 6,
                ..limits
            },
            Limit::Work,
            0,
            6,
        ), // Pending operator push.
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            0,
            6,
        ), // Emission.
        (
            Limits {
                max_source_bytes: 12,
                ..limits
            },
            Limit::NestingDepth,
            7,
            8,
        ),
    ] {
        let source = if kind == Limit::NestingDepth {
            "typeof (x)"
        } else {
            source
        };
        let err = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::LimitExceeded(kind));
        assert_eq!(err.span(), Span { start, end });
    }
    // Four emissions, extra decimal conversion byte, pending push, resolution
    // visit and one equal-length name comparison; the name is not patched early.
    let source = "let y=0; typeof x";
    let work = source.len() + 8;
    let limits = Limits {
        max_work: work,
        ..Limits::default()
    };
    assert!(parse_script_with_limits(source, limits).is_ok());
    let err = parse_script_with_limits(
        source,
        Limits {
            max_work: work - 1,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(
        err.span(),
        Span {
            start: source.len() - 1,
            end: source.len()
        }
    );
}

#[test]
fn iterative_chains_and_reached_dispatch_respect_the_existing_caps() {
    let limits = Limits {
        max_nesting_depth: 0,
        ..Limits::default()
    };
    let source = "typeof ".repeat(4095) + "x";
    let script = parse_script_with_limits(&source, limits).unwrap();
    assert_eq!(script.instruction_count(), 4096);
    assert_eq!(script.max_stack_depth(), 1);
    let string: Vec<_> = "string".encode_utf16().collect();
    assert_eq!(
        script.evaluate_primitive(),
        Ok(PrimitiveValue::String(&string))
    );
    let over = format!("typeof {source}");
    assert_eq!(
        parse_script_with_limits(&over, limits).unwrap_err().kind(),
        ErrorKind::LimitExceeded(Limit::Tokens)
    );
    let script = parse_script_with_limits(
        &over,
        Limits {
            max_tokens: 4097,
            ..limits
        },
    )
    .unwrap();
    assert_eq!(script.instruction_count(), 4097);
    assert_eq!(script.evaluate_primitive(), Err(EvalError::LimitExceeded));
    // Init (2), 1,637 iterations (10 each), final test (5) leave seven dispatches.
    let run = |tail| {
        parse_script(&format!("let n=1637; while (typeof n, n) {{n-=1;}} {tail}"))
            .unwrap()
            .evaluate_value()
    };
    assert_eq!(
        run("!typeof typeof typeof typeof typeof x"),
        Ok(Value::Boolean(false))
    );
    assert_eq!(
        run("!!typeof typeof typeof typeof typeof x"),
        Err(EvalError::DispatchLimitExceeded)
    );
    assert_eq!(run("+ + + + + true || typeof x"), Ok(Value::Number(1.0)));
    assert_eq!(
        run("+ + + + + false || typeof x"),
        Err(EvalError::DispatchLimitExceeded)
    );
}
