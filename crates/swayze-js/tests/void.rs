use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

fn assert_error_at(marked: &str, len: usize, kind: ErrorKind) {
    let start = marked.find('@').unwrap();
    let source = marked.replacen('@', "", 1);
    let error = parse_script(&source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + len
        },
        "{source}"
    );
}

#[test]
fn void_returns_the_undefined_tag_independent_of_operand_or_shadowing() {
    for source in [
        "void 42",
        "void null",
        "void false",
        "void undefined",
        "void void 0",
        "void/* trivia\n */\n0",
        "let result; {const undefined = 42; result = void undefined;} result",
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate_value(), Ok(Value::Undefined), "{source}");
        assert_eq!(
            script.evaluate(),
            Err(EvalError::NonNumericResult),
            "{source}"
        );
    }
    // Coercion is performed by the outer unary plus, not by void or evaluate().
    assert!(
        parse_script("+void false")
            .unwrap()
            .evaluate()
            .unwrap()
            .is_nan()
    );
}

#[test]
fn void_is_a_right_associative_prefix_tighter_than_binary_and_conditional_operators() {
    use Value::{Boolean, Number, Undefined};
    for (source, expected) in [
        ("!void !true", Boolean(true)),
        ("void !false", Undefined),
        ("void 2 === undefined", Boolean(true)),
        ("void 1 ? 2 : 42", Number(42.0)),
        ("void (1 ? 2 : 42)", Undefined),
        ("void 1, 42", Number(42.0)),
        ("void (1, 42)", Undefined),
        ("void 1 ?? 42", Number(42.0)),
        ("void (1 ?? 42)", Undefined),
        ("null ?? void 1", Undefined),
        ("true || void 1", Boolean(true)),
        ("false && void 1", Boolean(false)),
        ("void (true || false)", Undefined),
        ("const x = void 1, y = x ?? 42; y", Number(42.0)),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate_value(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn lexer_matches_the_exact_keyword_and_rejects_it_as_a_binding_name() {
    assert_eq!(
        parse_script("let void0 = 40, void_ = 1, Void = 1; void0 + void_ + Void")
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
    assert_error_at("let @void; 0", 4, ErrorKind::Unsupported);
    assert_error_at(
        "for (const @void = 0; false;) {} 0",
        4,
        ErrorKind::Unsupported,
    );
}

#[test]
fn void_does_not_bypass_grammar_or_binding_validation() {
    use ErrorKind::{Malformed, Unsupported};
    for (marked, len, kind) in [
        ("void@", 0, Malformed),
        ("1 @void 0", 4, Malformed),
        ("let x = 1\n@void x", 4, Unsupported),
        ("{@void 0;} 42", 4, Unsupported),
        ("void 0; @42", 2, Unsupported),
        ("void @typeof 0", 6, Unsupported),
        ("void @delete x", 6, Unsupported),
        ("void @\"x\"", 1, Unsupported),
        ("false && void @;", 1, Malformed),
        ("void 0 ?? false @|| true", 2, Malformed),
        ("void @missing", 7, Unsupported),
        ("true || void @missing", 7, Unsupported),
        ("0 ?? void @missing", 7, Unsupported),
        ("const x = void @x; x", 1, Unsupported),
        (
            "let x = 0; if (false) {let y = void @x, x = 42;} x",
            1,
            Unsupported,
        ),
    ] {
        assert_error_at(marked, len, kind);
    }
}

#[test]
fn void_costs_one_token_one_pending_push_and_one_emission() {
    let source = "void 0";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 2,
        max_nesting_depth: 0,
        max_work: source.len() + 1 + 1 + 2, // One conversion byte, one push, two emissions.
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 2);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Undefined));
    for (limits, kind, span) in [
        (
            Limits {
                max_tokens: 1,
                ..limits
            },
            Limit::Tokens,
            Span { start: 5, end: 6 },
        ),
        (
            Limits {
                max_work: 4,
                ..limits
            },
            Limit::Work,
            Span { start: 0, end: 4 },
        ), // Push.
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            Span { start: 0, end: 4 },
        ), // Emission.
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(kind));
        assert_eq!(error.span(), span);
    }
}

#[test]
fn pending_void_chain_is_bounded_without_spending_nesting_budget() {
    let caps = Limits {
        max_nesting_depth: 0,
        ..Limits::default()
    };
    let source = "void ".repeat(4095) + "0";
    let script = parse_script_with_limits(&source, caps).unwrap();
    assert_eq!(script.instruction_count(), 4096);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Undefined));

    let over = format!("void {source}");
    let error = parse_script_with_limits(&over, caps).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Tokens));
    assert_eq!(
        error.span(),
        Span {
            start: over.len() - 1,
            end: over.len()
        }
    );
    let script = parse_script_with_limits(
        &over,
        Limits {
            max_tokens: 4097,
            ..caps
        },
    )
    .unwrap();
    assert_eq!(script.instruction_count(), 4097);
    assert_eq!(script.evaluate_value(), Err(EvalError::LimitExceeded));
}

#[test]
fn reached_void_and_its_operand_each_dispatch_once_but_skipped_code_does_not_dispatch() {
    let run = |tail: &str| {
        parse_script(&format!("let n=1637; while (void n, n) {{n-=1;}} {tail}"))
            .unwrap()
            .evaluate_value()
    };
    // Initialization (2), 1,637 iterations (5 test + 4 decrement + 1 back edge each),
    // and the last test (5) total 16,377 dispatches, leaving exactly seven for the tail.
    assert_eq!(run("void void void void void void n"), Ok(Value::Undefined));
    assert_eq!(
        run("void void void void void void void n"),
        Err(EvalError::DispatchLimitExceeded)
    );
    assert_eq!(run("+ + + + + true || void n"), Ok(Value::Number(1.0)));
    assert_eq!(
        run("+ + + + + false || void n"),
        Err(EvalError::DispatchLimitExceeded)
    );
    assert_eq!(run("+ + + false || void n"), Ok(Value::Undefined));
}
