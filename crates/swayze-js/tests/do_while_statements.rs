use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, parse_script, parse_script_with_limits,
};

#[test]
fn runs_at_least_once_and_repeats_with_existing_truthiness() {
    for (test, expected) in [
        ("false", 1.0),
        ("null", 1.0),
        ("undefined", 1.0),
        ("-0", 1.0),
        ("0/0", 1.0),
        ("true", 3.0),
        ("1/-0", 3.0),
        ("-5e-324", 3.0),
    ] {
        let source = format!("let n=0;do{{n+=1;}}while(n<3 ? {test} : false);n");
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
    }
}

#[test]
fn continue_retests_before_reentering_the_body_and_skips_its_declarations() {
    let script = parse_script(
        "let remaining=4; let total=0;
         do {
             remaining-=1;
             if (remaining===2 || !remaining) { continue; }
             const value=remaining;
             total=total*10+value;
         } while (remaining);
         total",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(31.0));
}

#[test]
fn loop_jumps_stay_with_the_innermost_loop_across_mixed_control_frames() {
    let script = parse_script(
        "let i=0; let trace=0;
         for (; i<3; i+=1) {
             let j=2;
             do {
                 if (j===2) { j-=1; continue; }
                 let k=2;
                 while (k) {
                     k-=1;
                     if (k) { continue; }
                     trace=trace*10+i+1;
                     break;
                 }
                 if (j===1) { break; }
                 trace=999;
             } while (j);
             if (i===0) { continue; }
             trace=trace*10+4;
             if (i===2) { break; }
         }
         trace*10+i",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(124342.0));
}

#[test]
fn trailing_test_resolves_in_the_enclosing_scope() {
    let script = parse_script(
        "let n=0; let total=0;
         if (true) {
             let n=3;
             do { const n=0; total+=1+n; } while (total<n);
         }
         n*10+total",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(3.0));

    use ErrorKind::Unsupported;
    for (source, prefix, name) in [
        (
            "do{let bodyOnly=false;}while(bodyOnly);0",
            "while(",
            "bodyOnly",
        ),
        (
            "do{let bodyOnly=false;}while(false);bodyOnly",
            "false);",
            "bodyOnly",
        ),
        (
            "do{let later=0;}while(later);let later=1;0",
            "while(",
            "later",
        ),
    ] {
        let start = source.find(prefix).unwrap() + prefix.len();
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), Unsupported, "{source}: {error}");
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + name.len()
            }
        );
    }
}

#[test]
fn requires_braces_and_a_complete_tail_with_explicit_semicolon() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind) in [
        ("do", Malformed),
        ("let x=0;do x=1;while(false);x", Malformed),
        ("do{", Malformed),
        ("do{}", Malformed),
        ("do{}for(;;){}0", Malformed),
        ("do{}while false;0", Malformed),
        ("do{}while();0", Malformed),
        ("do{}while(false;0", Malformed),
        ("do{}while(false)", Unsupported),
        ("do{}while(false)\n0", Unsupported),
        ("do{}while(false) 0", Unsupported),
        ("if(true){do{}while(false)}0", Unsupported),
        ("do{}while(false);", Unsupported),
        ("do{}while(false);0;;", Unsupported),
        ("do{}while(false);continue;0", Malformed),
        ("0;do{}while(false);", Unsupported),
    ] {
        assert_eq!(parse_script(source).unwrap_err().kind(), kind, "{source}");
    }
    for (source, start, end) in [
        ("do{}while(false)", 16, 16),
        ("if(true){do{}while(false)}0", 25, 26),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.span(), Span { start, end });
    }
}

#[test]
fn checks_both_parts_even_when_a_jump_or_branch_skips_them() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind) in [
        ("do{continue;let x=;}while(false);0", Malformed),
        ("do{break; x=1;}while(false);0", Unsupported),
        ("do{break; const x=1; x=2;}while(false);0", Unsupported),
        ("do{break outer;}while(false);0", Unsupported),
        ("do{break;}while(1+);0", Malformed),
        ("let x=0;do{break;}while(x=1);x", Unsupported),
        ("do{break;}while(false && missing);0", Unsupported),
        ("if(false){do{let x=0;}while(x++);}0", Unsupported),
    ] {
        assert_eq!(parse_script(source).unwrap_err().kind(), kind, "{source}");
    }
}

#[test]
fn recognizes_only_whole_keywords_and_accepts_trivia_in_the_tail() {
    for name in ["do0", "do_", "do$", "done"] {
        let source = format!(
            "let {name}=1;do/**/{{{name}=0;}}// body\u{2028}while/**/({name})/*test\n*/;{name}"
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(0.0));
    }
    for source in ["let do=0;1", "(do)"] {
        let start = source.find("do").unwrap();
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + 2
            }
        );
    }
}

#[test]
fn statement_frames_are_iterative_and_independent_of_test_nesting() {
    let nested = |n: usize| format!("{}{}42", "do{".repeat(n), "}while(false);".repeat(n));
    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(&nested(128), limits)
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
    let error = parse_script_with_limits(&nested(129), limits).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(
        error.span(),
        Span {
            start: 384,
            end: 386
        }
    );
    let error = parse_script_with_limits(
        "do{}while(false);0",
        Limits {
            max_nesting_depth: 0,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(error.span(), Span { start: 9, end: 10 });
}

#[test]
fn skipped_body_and_test_instructions_count_at_the_prepared_ceiling() {
    let limits = Limits {
        max_tokens: 8200,
        ..Limits::default()
    };
    for (extra, expected) in [(0, Ok(42.0)), (1, Err(EvalError::LimitExceeded))] {
        for source in [
            format!(
                "do{{break;{}}}while(false);42",
                "continue;".repeat(4091 + extra)
            ),
            format!("do{{break;}}while({}false);42", "!".repeat(4091 + extra)),
        ] {
            let script = parse_script_with_limits(&source, limits).unwrap();
            assert_eq!(script.instruction_count(), 4096 + extra);
            assert_eq!(script.evaluate(), expected);
        }
    }
}

#[test]
fn exact_fuel_counts_continue_and_back_edges_but_break_skips_the_test() {
    // 8 dispatches per truthy iteration (decrement, continue, test and back edge),
    // 7 for the last, 2 to initialize, 1 for break, and 1 plus prefixes for the result.
    for (prefixes, expected) in [
        (4, Ok(0.0)),                               // 16,383
        (5, Ok(0.0)),                               // 16,384
        (6, Err(EvalError::DispatchLimitExceeded)), // 16,385
    ] {
        let source = format!(
            "let n=2047;do{{n-=1;continue;}}while(n);do{{break;}}while(true);{}n",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
    let script = parse_script("do{}while(true);42").unwrap();
    assert_eq!(script.instruction_count(), 4);
    assert_eq!(
        script.evaluate_value(),
        Err(EvalError::DispatchLimitExceeded)
    );
}
