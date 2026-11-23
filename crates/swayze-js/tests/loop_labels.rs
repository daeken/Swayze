use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn named_for_jumps_run_or_skip_the_update_across_inner_loops_and_scopes() {
    let script = parse_script(
        "let i=0, trace=0;
         outer: for (i=1; i<=4; i++) {
             let outer=i;
             { do {
                 while (true) {
                     trace=trace*10+outer;
                     if (outer<3) { let i=99; continue outer; }
                     break outer;
                 }
             } while (true); }
             trace=999;
         }
         trace*10+i",
    )
    .unwrap();
    // The first two iterations jump to the outer update; the third exits without it.
    assert_eq!(script.evaluate(), Ok(1233.0));
    assert_eq!(script.evaluate(), Ok(1233.0));
}

#[test]
fn named_do_continue_runs_its_trailing_test_in_the_enclosing_scope() {
    let script = parse_script(
        "let n=0, go=true;
         _again$9: do {
             let go=false;
             n++;
             for (;;) {
                 if (n===1) { continue _again$9; }
                 if (n===2) { while (true) { continue _again$9; } }
                 while (true) { break _again$9; }
             }
             n=99;
         } while (go && n<2);
         n",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(2.0));
    assert_eq!(script.max_stack_depth(), 2);

    // If a named break ran the do test this loop would exhaust the dispatch budget.
    let script = parse_script("a:do{while(true){break a;}}while(true);42").unwrap();
    assert_eq!(script.evaluate(), Ok(42.0));
}

#[test]
fn named_while_and_for_without_updates_continue_at_their_tests() {
    for head in ["while(n<3)", "for(;n<3;)"] {
        let source = format!(
            "let n=0, hits=0; outer:{head}{{
                 n++;
                 inner:do{{break;}}while(true);
                 hits++;
                 inner:do{{continue outer;}}while(true);
                 n=99;
             }}n*10+hits"
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(33.0));
    }
}

#[test]
fn labels_have_their_own_case_sensitive_namespace_and_disjoint_reuse() {
    let script = parse_script(
        "let a=0;
         if(false){a:while(true){break a;}}
         else if(false){a:for(;;){break a;}}
         else {a:do{while(true){break a;}}while(true);}
         a:for(let a=1;a<2;a++){
             A:while(true){break A;}
             {let a=40; a+=2; if(a===42){break a;}}
         }
         undefined:while(true){break undefined;}
         a ? a : undefined",
    )
    .unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Undefined));

    // A jump can name the surrounding loop even before a same-name binding initializes.
    assert_eq!(
        parse_script("a:while(true){{break a;let a=1;}}42")
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
}

#[test]
fn statement_prefix_lookahead_does_not_consume_ternary_colons_or_stores() {
    let script = parse_script(
        "let x=1;
         x/**/+=1;
         x/**/++;
         ++x;
         x ? x : 0;
         x/*\n*/: // label and colon may be separated by a line terminator
         for (x ? x : 0; x ? false : x; x ? x : 0) { x ? x : 0; }
         x/**/? x : 0;",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(4.0));
    assert_eq!(parse_script("let x=42;x").unwrap().evaluate(), Ok(42.0));
    for source in ["let x=1;(x:while(false){})", "let x=1;for(x:;false;){}42"] {
        // A colon in a parenthesized expression or for clause is not a label.
        assert_error_at(source, ErrorKind::Malformed, ":");
    }
}

fn assert_error(source: &str, kind: ErrorKind, start: usize, len: usize) {
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + len
        },
        "{source}: {error}"
    );
}

fn assert_error_at(source: &str, kind: ErrorKind, last: &str) {
    assert_error(source, kind, source.rfind(last).unwrap(), last.len());
}

#[test]
fn invalid_labels_and_static_targets_pin_error_spans() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, last) in [
        ("a:while(false){{a:do{}while(false);}}42", Malformed, "a:"),
        (
            "a:for(;;){if(true){}else if(false){a:while(false){}}}42",
            Malformed,
            "a:",
        ),
        ("a:while(false){}break a;42", Malformed, "a;"),
        ("break missing;late:while(false){}42", Malformed, "missing"),
        (
            "a:while(true){break;continue missing;}42",
            Malformed,
            "missing",
        ),
        (
            "if(false){continue later;}later:while(false){}42",
            Malformed,
            "later;",
        ),
        (
            "if(false){a:do{}while(false);}else{break a;}42",
            Malformed,
            "a;",
        ),
        ("A:while(false){break a;}42", Malformed, "a;"),
        ("while(false){let a=0;break a;}42", Malformed, "a;"),
        ("a:while(false){} a", Unsupported, "a"),
        ("a:while(true){break a;let x=x;}42", Unsupported, "x"),
        (
            "a:do{let x=1;while(true){continue a;}}while(x);42",
            Unsupported,
            "x",
        ),
        ("a:while(false){continue a 42;}0", Malformed, "42"),
        ("a:while(false) break a;42", Malformed, "break"),
        ("a:for(;;) ;42", Malformed, ";"),
        ("a:do ;while(false);42", Malformed, ";while"),
        ("while(true){a:}42", Malformed, "}"),
        ("a: /**/", Malformed, ""),
        ("a:if(true){}42", Unsupported, "if"),
        ("aé:while(false){}42", Unsupported, "é"),
        ("a\\u0030:while(false){}42", Unsupported, "\\"),
        ("eval:while(false){}42", Unsupported, "eval"),
        ("a:while(false){break a\u{e9};}42", Unsupported, "é"),
    ] {
        // Some needles include trailing text to pick out one occurrence.
        let start = source.rfind(last).unwrap();
        let len = match last {
            "a:" | "a;" | ";while" => 1,
            "later;" => 5,
            _ => last.len(),
        };
        assert_error(source, kind, start, len);
    }
}

#[test]
fn line_terminators_before_a_target_are_rejected_but_the_semicolon_may_follow_one() {
    use ErrorKind::Unsupported;
    for jump in ["break", "continue"] {
        for lt in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
            for trivia in [
                lt.to_owned(),
                format!("// comment{lt}"),
                format!("/*{lt}*/"),
            ] {
                let source = format!("target:while(false){{{jump}{trivia}target;}}42");
                assert_error_at(&source, Unsupported, "target");
                let source = format!("target:while(false){{{jump}/**/target{trivia};}}42");
                assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(42.0));
            }
        }
        assert_error_at(
            &format!("target:while(false){{{jump} target}}42"),
            Unsupported,
            "}",
        );
        assert_error_at(
            &format!("target:while(false){{{jump} target"),
            Unsupported,
            "",
        );
    }
}

#[test]
fn labels_share_the_exact_statement_frame_ceiling_and_add_no_expression_nesting() {
    let open = "a:while(false){b:for(let i=1;;){c:do{";
    let close = "}while(false);}}42";
    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    let source = format!(
        "{open}{}break a;{}{close}",
        "{".repeat(125),
        "}".repeat(125)
    );
    assert_eq!(
        parse_script_with_limits(&source, limits)
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
    let too_deep = format!(
        "{open}{}overflow:while(false){{}}{}{close}",
        "{".repeat(125),
        "}".repeat(125)
    );
    let error = parse_script_with_limits(&too_deep, limits).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    let start = too_deep.find("overflow").unwrap();
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + "overflow".len()
        }
    );
}

#[test]
fn label_tokens_and_scans_are_charged_exactly() {
    let source = "a0:while(false){while(false){a1:do{break a0;}while(false);}}0";
    // Source bytes, 1 decimal-conversion byte, 3 groups, 3 frames, 11 emissions,
    // 1 break fixup, 4 work for the duplicate check and 7 for target lookup.
    // Name checks visit unlabeled loops too; each equal-length comparison costs 2.
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 28,
        max_work: source.len() + 30,
        ..Limits::default()
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 11);
    assert_eq!(script.evaluate(), Ok(0.0));
    for (limits, kind) in [
        (
            Limits {
                max_source_bytes: limits.max_source_bytes - 1,
                ..limits
            },
            Limit::SourceBytes,
        ),
        (
            Limits {
                max_tokens: limits.max_tokens - 1,
                ..limits
            },
            Limit::Tokens,
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(kind));
        assert_eq!(
            error.span(),
            Span {
                start: source.len() - 1,
                end: source.len()
            }
        );
    }

    for (name, start, work_before_search, search_work, next_char) in [
        (
            "a1",
            source.find("a1").unwrap(),
            source.find("do{").unwrap() + 2 + 8,
            4,
            "{",
        ),
        (
            "a0",
            source.rfind("a0").unwrap(),
            source.rfind("a0").unwrap() + 2 + 13,
            7,
            ";",
        ),
    ] {
        for units in 0..=search_work {
            let error = parse_script_with_limits(
                source,
                Limits {
                    max_work: work_before_search + units,
                    ..limits
                },
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
            let expected = if units < search_work {
                Span {
                    start,
                    end: start + name.len(),
                }
            } else {
                let start = start + source[start..].find(next_char).unwrap();
                Span {
                    start,
                    end: start + 1,
                }
            };
            assert_eq!(error.span(), expected);
        }
    }

    // Unequal lengths still charge for the visited loop, but not for a byte comparison.
    let shorter = source.replace("a1", "b");
    let work = shorter.len() + 26;
    assert!(
        parse_script_with_limits(
            &shorter,
            Limits {
                max_work: work,
                ..limits
            }
        )
        .is_ok()
    );
    assert_eq!(
        parse_script_with_limits(
            &shorter,
            Limits {
                max_work: work - 1,
                ..limits
            }
        )
        .unwrap_err()
        .kind(),
        ErrorKind::LimitExceeded(Limit::Work)
    );
}

#[test]
fn labels_do_not_add_prepared_instructions_or_dispatches() {
    let jumps = "break outer;".repeat(4089);
    for (extra, instructions, result) in [
        ("", 4096, Ok(42.0)),
        ("break outer;", 4097, Err(EvalError::LimitExceeded)),
    ] {
        let source = format!("outer:while(false){{while(true){{{jumps}{extra}}}}}42");
        let script = parse_script_with_limits(
            &source,
            Limits {
                max_tokens: 12_300,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(script.instruction_count(), instructions);
        assert_eq!(script.evaluate(), result);
    }

    // Two to initialize, 13 per iteration (including its taken jump), one
    // final load, plus 0, 1 or 2 unary instructions: 16,383, 16,384, 16,385.
    for (prefix, result) in [
        ("", Ok(1260.0)),
        ("+ ", Ok(1260.0)),
        ("+ + ", Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!(
            "let x=0;outer:while(true){{while(true){{x++;if(x===1260){{break outer;}}continue outer;}}}}{prefix}x"
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), result);
    }
}
