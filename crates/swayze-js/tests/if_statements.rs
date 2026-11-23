use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn runs_only_the_selected_assignments_in_source_order() {
    for (test, expected) in [("true", 428.0), ("false", 20.0)] {
        let script = parse_script(&format!(
            "let x = 0; let y = 8;
             if ({test}) {{ x = 40; x = x + 2; }} else {{ x = 2; y = 0; }}
             const snapshot = x;
             snapshot * 10 + y"
        ))
        .unwrap();
        assert_eq!(script.evaluate(), Ok(expected));
    }
}

#[test]
fn missing_else_and_empty_branches_do_not_write() {
    for (source, expected) in [
        ("let x=1; if (true) {x=5;} x", 5.0),
        ("let x=1; if (false) {x=5;} x", 1.0),
        ("let x=1; if (true) {} else {x=5;} x", 1.0),
        ("let x=1; if (false) {} else {x=5;} x", 5.0),
        ("let x=1; if (false) {x=5;} else {} x", 1.0),
        ("if (true) {} if (false) {} else {} 1;", 1.0),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn uses_fresh_slots_for_repeated_evaluation() {
    let script = parse_script(
        "let x = 40; let ran = false;
         if (!ran) { x = x + 2; ran = true; } else { x = 0; }
         if (ran) { x = x + 1; }
         x",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(43.0));
    assert_eq!(script.evaluate(), Ok(43.0));
}

#[test]
fn tests_use_existing_truthiness_and_expression_precedence() {
    for (test, expected) in [
        ("+0", false),
        ("-0", false),
        ("0 / 0", false),
        ("1 / -0", true),
        ("-5e-324", true),
        ("!(1 + 2 * 3 < 7) && 5 === 5 || false", true),
        ("true || false && false", true),
        ("true || false ? 0 : 1", false),
        ("(false || 3) && (true ? !0 : false)", true),
    ] {
        let source = format!(
            "let x=0; if ({test}) {{x = true && true;}} else {{x = true ? false : true;}} x"
        );
        assert_eq!(
            parse_script(&source).unwrap().evaluate_value(),
            Ok(Value::Boolean(expected)),
            "{test}"
        );
    }
}

#[test]
fn keyword_boundaries_and_trivia() {
    for keyword in ["if", "else"] {
        for suffix in ["x", "0", "_", "$"] {
            let name = format!("{keyword}{suffix}");
            let source =
                format!("let {name}=0; if/**/(true) {{ {name}=42; }} // not an else\n {name}");
            assert_eq!(
                parse_script(&source).unwrap().evaluate(),
                Ok(42.0),
                "{source}"
            );
        }
        let error = parse_script(&format!("let {keyword}=0; 1")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_eq!(
            error.span(),
            Span {
                start: 4,
                end: 4 + keyword.len()
            }
        );
        for suffix in ["é", "\u{200D}", "\\u0061"] {
            let prefix = if keyword == "if" { "" } else { "if(true){} " };
            let start = prefix.len() + keyword.len();
            let error = parse_script(&format!("{prefix}{keyword}{suffix}")).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Unsupported);
            assert_eq!(
                error.span(),
                Span {
                    start,
                    end: start + suffix.chars().next().unwrap().len_utf8()
                }
            );
        }
    }
    let source = "let x=0; if (false) {} /*\n*/else//comment\u{2028}{x=2;} x";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(2.0));
}

#[test]
fn delimiters_and_the_required_final_expression() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind) in [
        ("if true {} 1", Malformed),
        ("if () {} 1", Malformed),
        ("if (true {} 1", Malformed),
        ("if ((true) {} 1", Malformed),
        ("if (true;) {} 1", Malformed),
        ("if (true) 1", Malformed),
        ("let x=0; if (true) x=1; x", Malformed),
        ("let x=0; if (true) {} else x=1; x", Malformed),
        ("if (true) {} else", Malformed),
        ("if (true) {", Malformed),
        ("if (true) {}", Unsupported),
        ("let x=0; if (true) {x=2;}", Unsupported),
        ("if (true) {}; 1", Unsupported),
        ("else {} 1", Unsupported),
        ("if (true) {} else {} else {} 1", Unsupported),
        ("1; if (true) {}", Unsupported),
        ("if (true) {} 1; 2", Unsupported),
        ("let x=0; if (true) {} x; x=2;", Unsupported),
        ("let x=0; if (true) {x=2\n} x", Unsupported),
        ("let x=0\nif (true) {} x", Unsupported),
        ("let x=0; if (true) {} /*", Malformed),
    ] {
        assert_eq!(parse_script(source).unwrap_err().kind(), kind, "{source}");
    }
    let source = "if (false) {} else if (true) {} 1";
    let error = parse_script(source).unwrap_err();
    let start = source.rfind("if").unwrap();
    assert_eq!(error.kind(), Unsupported);
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + 2
        }
    );
    assert!(error.message().contains("else if"));
}

#[test]
fn validates_both_bodies_even_when_untaken() {
    use ErrorKind::{Malformed, Unsupported};
    for (body, kind) in [
        ("missing=1;", Unsupported),
        ("later=1;", Unsupported),
        ("x=later;", Unsupported),
        ("c=1;", Unsupported),
        ("let y=y;", Unsupported),
        ("const y=1", Unsupported),
        ("1;", Unsupported),
        ("x;", Unsupported),
        (";", Unsupported),
        ("x=1", Unsupported),
        ("x=;", Malformed),
        ("x=1+;", Malformed),
        ("x=(x=1);", Unsupported),
        ("x||=later;", Unsupported),
        ("function f() {}", Unsupported),
    ] {
        for statement in [
            format!("if (false) {{{body}}}"),
            format!("if (true) {{}} else {{{body}}}"),
        ] {
            let source = format!("let x=0; const c=2; {statement} let later=1; x");
            assert_eq!(parse_script(&source).unwrap_err().kind(), kind, "{source}");
        }
    }
}

#[test]
fn condition_parentheses_and_untaken_expressions_share_the_nesting_limit() {
    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    assert!(parse_script_with_limits("let x=1; if (false) {x=(0);} x", limits).is_ok());
    for source in [
        "if ((true)) {} 1",
        "if (true ? false : true) {} 1",
        "let x=1; if (false) {x=((0));} x",
    ] {
        assert_eq!(
            parse_script_with_limits(source, limits).unwrap_err().kind(),
            ErrorKind::LimitExceeded(Limit::NestingDepth),
            "{source}"
        );
    }
}

#[test]
fn instructions_in_both_branches_count_toward_the_evaluation_ceiling() {
    let limits = Limits {
        max_tokens: 4200,
        ..Limits::default()
    };
    for test in [true, false] {
        for (extra, expected) in [(0, Ok(42.0)), (1, Err(EvalError::LimitExceeded))] {
            let skipped = format!("x={}1;", "- ".repeat(4086 + extra));
            let (consequent, alternate) = if test {
                ("x=42;", skipped.as_str())
            } else {
                (skipped.as_str(), "x=42;")
            };
            let source = format!("let x=0; if ({test}) {{{consequent}}} else {{{alternate}}} x");
            let script = parse_script_with_limits(&source, limits).unwrap();
            assert_eq!(script.instruction_count(), 4096 + extra);
            assert_eq!(script.evaluate(), expected);
        }
    }
}
