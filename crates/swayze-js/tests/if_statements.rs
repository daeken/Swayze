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
fn else_if_selects_the_first_truthy_test_or_the_optional_else() {
    for (first, second, third, fallback, expected) in [
        ("true", "true", "true", true, 1.0),
        ("null", "42", "true", true, 2.0),
        ("null", "undefined", "!0", false, 3.0),
        ("null", "0", "false", true, 4.0),
        ("null", "0", "false", false, 0.0),
    ] {
        let fallback = if fallback { "else { x=4; }" } else { "" };
        let source = format!(
            "let x=0; if ({first}) {{ x=1; }} else /*\n*/ if ({second}) {{ x=2; }}
             else // comment\u{2028} if ({third}) {{ x=3; }} {fallback} x"
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(expected));
    }
}

#[test]
fn else_if_tests_see_the_enclosing_scope_and_arms_have_disjoint_shadows() {
    for (gate, expected) in [(1, 10.0), (2, 20.0), (3, 30.0), (4, 40.0)] {
        let source = format!(
            "const key=0; let result=0;
             {{ let key={gate};
                if (key===1) {{ const key=10; result=key; }}
                else if (key===2) {{ let key=20; result=key; }}
                else if (key===3) {{ const key=30; result=key; }}
                else {{ let key=40; result=key; }}
             }} result + key"
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(expected));
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
        ("if (true) {} else if false {} 1", Malformed),
        ("if (true) {} else if (false) ; 1", Malformed),
        ("let x=0; if (true) {} else if (false) x=1; x", Malformed),
        ("if (true) {} else if (false)", Malformed),
        ("if (true) {} else if (false) {} else 1", Malformed),
        ("if (true) {", Malformed),
        ("if (true) {}", Unsupported),
        ("let x=0; if (true) {x=2;}", Unsupported),
        ("if (true) {}; else {} 1", Unsupported),
        ("else {} 1", Unsupported),
        ("if (true) {} else {} else {} 1", Unsupported),
        ("if (true) {} else if (false) {}; else {} 1", Unsupported),
        ("if (true) {} else if (false) {} else", Malformed),
        ("1; if (true) {}", Unsupported),
        ("if (true) {} 1; 2", Unsupported),
        ("let x=0; if (true) {} x; x=2;", Unsupported),
        ("let x=0; if (true) {x=2\n} x", Unsupported),
        ("let x=0\nif (true) {} x", Unsupported),
        ("let x=0; if (true) {} /*", Malformed),
    ] {
        assert_eq!(parse_script(source).unwrap_err().kind(), kind, "{source}");
    }
    // An unbraced nested `if` is still disallowed unless it follows `else`.
    let source = "if (false) {} else if (true) if (false) {} 1";
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
    assert!(error.message().contains("require braces"));
}

#[test]
fn validates_all_bodies_even_when_untaken() {
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
            format!("if (true) {{}} else if (true) {{{body}}} else {{}}"),
            format!("if (true) {{}} else if (false) {{}} else {{{body}}}"),
        ] {
            let source = format!("let x=0; const c=2; {statement} let later=1; x");
            assert_eq!(parse_script(&source).unwrap_err().kind(), kind, "{source}");
        }
    }
}

#[test]
fn validates_untaken_else_if_tests_and_rejects_arm_local_accesses() {
    for name in ["missing", "prior", "own", "later"] {
        let prefix = "if (true) { let prior=1; } else if (";
        let source = format!("{prefix}{name}) {{ let own=1; }} let later=1; 42");
        let error = parse_script(&source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported, "{source}");
        assert_eq!(
            error.span(),
            Span {
                start: prefix.len(),
                end: prefix.len() + name.len(),
            },
            "{source}"
        );
    }
    // The same isolation holds between two consecutive else-if arms.
    let source = "if (true) {} else if (false) { let prior=1; } else if (prior) {} 42";
    let start = source.rfind("prior").unwrap();
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + "prior".len(),
        }
    );

    for (source, name) in [
        (
            "if (true) {} else if (false) { let hidden=1; } hidden",
            "hidden",
        ),
        (
            "let x=1; if (true) {} else if (false) { x=2; let x=3; } x",
            "x=2",
        ),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported, "{source}");
        let start = source.rfind(name).unwrap();
        assert_eq!(error.span().start, start, "{source}");
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
        "if (true) {} else if ((false)) {} 1",
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

#[test]
fn skipped_else_if_tests_also_count_toward_the_prepared_instruction_ceiling() {
    let limits = Limits {
        max_tokens: 4200,
        ..Limits::default()
    };
    for (extra, expected) in [(0, Ok(42.0)), (1, Err(EvalError::LimitExceeded))] {
        let source = format!(
            "let x=42;if(true){{}}else if({}false){{}}else if(false){{}}x",
            "!".repeat(4085 + extra)
        );
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 4096 + extra);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn else_if_dispatches_only_reached_tests_and_one_join_per_selected_arm() {
    // Each iteration costs 12: outer test (2), first and second if tests
    // (2 each), decrement (4), jump to the join (1), and back edge (1).
    // Initialization, the final loop test, and the result load cost 5 more.
    for (prefixes, expected) in [(11, Ok(0.0)), (12, Err(EvalError::DispatchLimitExceeded))] {
        let source = format!(
            "let n=1364;while(n){{if(false){{}}else if(true){{n--;}}else if(true){{}}else{{}}}}{}n",
            "+ ".repeat(prefixes)
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), expected);
    }
}

#[test]
fn else_if_counts_each_tested_arm_against_the_shared_frame_cap() {
    let chain = format!("if(false){{}}{}", "else if(false){}".repeat(127));
    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    for fallback in ["", "else{}"] {
        // The optional final else reuses the last if frame, and subsequent
        // statements can reuse all the frames after the chain joins.
        let source = format!("{chain}{fallback}{chain}{fallback}42");
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.evaluate(), Ok(42.0));
    }
    let prefix = format!("{chain}else ");
    for (source, start, len) in [
        (format!("{prefix}if(true){{}}42"), prefix.len(), 2),
        (
            format!("{chain}else{{{{}}}}42"),
            chain.len() + "else{".len(),
            1,
        ),
    ] {
        let error = parse_script_with_limits(&source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + len
            }
        );
        assert!(error.message().contains("statement nesting"));
    }
}

#[test]
fn else_if_shares_the_frame_cap_with_bare_blocks_and_loops() {
    // The outer while and a bare block leave room for 126 tested arms. The
    // final else reuses the deepest frame, and break still finds the while.
    let prefix = format!(
        "let x=0;while(true){{{{if(false){{}}{}else ",
        "else if(false){}".repeat(125)
    );
    let suffix = "}break;}x";
    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    let source = format!("{prefix}{{x=42;}}{suffix}");
    assert_eq!(
        parse_script_with_limits(&source, limits)
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
    for (arm, offset, len) in [
        ("if(true){}", 0, 2),
        ("{{}}", 1, 1),
        ("{while(false){}}", 1, 5),
    ] {
        let source = format!("{prefix}{arm}{suffix}");
        let error = parse_script_with_limits(&source, limits).unwrap_err();
        let start = prefix.len() + offset;
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + len
            },
            "{source}"
        );
    }
}

#[test]
fn if_work_budgets_include_every_jump_fixup() {
    // Beyond source bytes: each test uses a paren marker, frame, two
    // emissions and a false-edge fixup. Each alternate requires a jump to
    // the join and its fixup. The final Boolean costs one more emission.
    for (arms, fallback, work_beyond_source) in [
        (1, "", 6),
        (1, "else{}", 8),
        (3, "", 20),
        (3, "else{}", 22),
        (128, "else{}", 897),
    ] {
        let source = format!(
            "if(true){{}}{}{fallback}true",
            "else if(false){}".repeat(arms - 1)
        );
        let limits = Limits {
            max_source_bytes: source.len(),
            max_tokens: 6 + (arms - 1) * 7 + usize::from(!fallback.is_empty()) * 3 + 1,
            max_nesting_depth: 1,
            max_work: source.len() + work_beyond_source,
        };
        assert_eq!(
            parse_script_with_limits(&source, limits)
                .unwrap()
                .evaluate_value(),
            Ok(Value::Boolean(true))
        );
        let close = source.rfind('}').unwrap();
        let result_start = close + 1;
        // Just enough work to arrive at the final fixup, which has not run
        // yet. Without a final else, lookahead has already scanned `true`.
        let work_before_final_fixup =
            limits.max_work - 2 - if fallback.is_empty() { 0 } else { "true".len() };
        for (kind, span, limits) in [
            (
                Limit::Work,
                Span {
                    start: close,
                    end: close + 1,
                },
                Limits {
                    max_work: work_before_final_fixup,
                    ..limits
                },
            ),
            (
                Limit::Work,
                Span {
                    start: result_start,
                    end: source.len(),
                },
                Limits {
                    max_work: limits.max_work - 1,
                    ..limits
                },
            ),
            (
                Limit::Tokens,
                Span {
                    start: result_start,
                    end: result_start + 1,
                },
                Limits {
                    max_tokens: limits.max_tokens - 1,
                    ..limits
                },
            ),
        ] {
            let error = parse_script_with_limits(&source, limits).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(kind));
            assert_eq!(error.span(), span, "{arms} tests, fallback {fallback:?}");
        }
        if let Some(start) = source.find("else") {
            // At the first else the source through that token has been
            // scanned, with one marker, one frame and three emissions paid.
            let end = start + "else".len();
            let error = parse_script_with_limits(
                &source,
                Limits {
                    max_work: end + 5,
                    ..limits
                },
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
            assert_eq!(error.span(), Span { start, end });
        }
    }
}
