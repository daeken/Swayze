use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn retests_after_ordered_assignments_and_uses_fresh_slots() {
    let script = parse_script(
        "let x = 0; let n = 3;
         while (n > 0) { x = x * 10 + n; n = n - 1; }
         const snapshot = x;
         if (n === 0) { x = x + 1; }
         while (n) { x = 0; }
         snapshot * 10 + x",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(3532.0));
    assert_eq!(script.evaluate(), Ok(3532.0));
}

#[test]
fn tests_use_number_and_boolean_truthiness_and_full_expressions() {
    for (initial, expected) in [
        ("false", false),
        ("true", true),
        ("+0", false),
        ("-0", false),
        ("0 / 0", false),
        ("1 / -0", true),
        ("-5e-324", true),
    ] {
        let source = format!(
            "let flag = {initial}; let ran = false;
             while (flag && (true || false) ? !ran : false) {{ ran = true; flag = 0; }}
             ran"
        );
        assert_eq!(
            parse_script(&source).unwrap().evaluate_value(),
            Ok(Value::Boolean(expected)),
            "{source}"
        );
    }
    assert_eq!(
        parse_script("while (false) {} 42").unwrap().evaluate(),
        Ok(42.0)
    );
}

#[test]
fn keyword_boundaries_and_trivia() {
    for suffix in ["x", "0", "_", "$"] {
        let name = format!("while{suffix}");
        let source =
            format!("let {name}=true; while/**/({name}) // test\u{2028} {{{name}=false;}} {name};");
        assert_eq!(
            parse_script(&source).unwrap().evaluate_value(),
            Ok(Value::Boolean(false))
        );
    }
    let error = parse_script("let while = 2; 1").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert_eq!(error.span(), Span { start: 4, end: 9 });
    for suffix in ["é", "\u{200D}", "\\u0061"] {
        let error = parse_script(&format!("while{suffix}(false) {{}} 1")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_eq!(
            error.span(),
            Span {
                start: 5,
                end: 5 + suffix.chars().next().unwrap().len_utf8()
            }
        );
    }
}

#[test]
fn rejects_malformed_delimiters_missing_final_expression_and_trailing_input() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind) in [
        ("while false {} 1", Malformed),
        ("while () {} 1", Malformed),
        ("while (false {} 1", Malformed),
        ("while ((false) {} 1", Malformed),
        ("while (false;) {} 1", Malformed),
        ("while (false) 1", Malformed),
        ("let x=0; while (false) x=1; x", Malformed),
        ("while (false) {", Malformed),
        ("while (false) {}", Unsupported),
        ("while (false) {} else {} 1", Unsupported),
        ("1; while (false) {}", Unsupported),
        ("while (false) {} 1; 2;;", Unsupported),
        ("while (false) {} 1; /*", Malformed),
        ("let x=0\nwhile (false) {} x", Unsupported),
    ] {
        assert_eq!(parse_script(source).unwrap_err().kind(), kind, "{source}");
    }
}

#[test]
fn checks_the_whole_body_even_when_the_test_is_initially_false() {
    use ErrorKind::{Malformed, Unsupported};
    for (body, kind) in [
        ("missing=1;", Unsupported),
        ("later=1;", Unsupported),
        ("x=later;", Unsupported),
        ("c=1;", Unsupported),
        ("let y=;", Malformed),
        ("const y=1", Unsupported),
        ("x", Unsupported),
        ("x=1\n", Unsupported),
        ("x=1\nx=2;", Unsupported),
        ("x=;", Malformed),
        ("x=1+;", Malformed),
        ("x=(x=1);", Unsupported),
        ("c**=1;", Unsupported),
        ("break outer;", Malformed),
        ("continue outer;", Malformed),
        ("{x=1}", Unsupported),
    ] {
        let source = format!("let x=0; const c=2; while (false) {{{body}}} let later=1; x");
        assert_eq!(parse_script(&source).unwrap_err().kind(), kind, "{source}");
    }
}

#[test]
fn a_nonterminating_empty_loop_hits_the_typed_dispatch_limit() {
    let script = parse_script("while (true) {} 42").unwrap();
    assert_eq!(script.instruction_count(), 4);
    assert_eq!(script.evaluate(), Err(EvalError::DispatchLimitExceeded));
    assert_eq!(
        script.evaluate_value(),
        Err(EvalError::DispatchLimitExceeded)
    );
}

#[test]
fn dispatch_limit_is_inclusive_and_shared_by_all_loops_in_a_call() {
    // 7 dispatches per successful iteration + 2 for initialization + 2 for
    // the final falsy test + 1 for the final load + one per unary operator.
    for (prefixes, expected) in [
        (5, Ok(0.0)),                               // 16,383
        (6, Ok(0.0)),                               // 16,384
        (7, Err(EvalError::DispatchLimitExceeded)), // 16,385
    ] {
        let source = format!(
            "let x=2339; while (x) {{x=x-1;}} {}x",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }

    let one_loop = "let x=1200; while (x) {x=x-1;} x";
    let two_loops = "let x=1200; while (x) {x=x-1;} x=1200; while (x) {x=x-1;} x";
    assert_eq!(parse_script(one_loop).unwrap().evaluate(), Ok(0.0));
    assert_eq!(
        parse_script(two_loops).unwrap().evaluate(),
        Err(EvalError::DispatchLimitExceeded)
    );
}

#[test]
fn skipped_loop_body_is_subject_to_token_and_prepared_instruction_ceilings() {
    let limits = Limits {
        max_tokens: 4200,
        ..Limits::default()
    };
    for (extra, expected) in [(0, Ok(42.0)), (1, Err(EvalError::LimitExceeded))] {
        let source = format!(
            "let x=42; while (false) {{x={}0;}} x",
            "- ".repeat(4088 + extra)
        );
        assert_eq!(
            parse_script(&source).unwrap_err().kind(),
            ErrorKind::LimitExceeded(Limit::Tokens)
        );
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 4096 + extra);
        assert_eq!(script.evaluate(), expected);
    }
}
