use swayze_js::{ErrorKind, EvalError, Limits, Span, parse_script, parse_script_with_limits};

#[test]
fn inner_loop_jumps_leave_outer_execution_and_loop_targets_intact() {
    let script = parse_script(
        "let outer=3; let inner=0; let trace=0;
         while (outer) {
             outer=outer-1;
             inner=3;
             while (inner) {
                 inner=inner-1;
                 if (inner===2) {
                     if (true) { continue; }
                 } else {
                     trace=trace*10+outer+1;
                     break;
                 }
                 trace=999;
             }
             if (outer===2) { continue; }
             trace=trace*10+4;
             if (outer===1) { break; }
             trace=999;
         }
         trace=trace*10+outer;
         trace",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(3241.0));
    assert_eq!(script.evaluate(), Ok(3241.0));
}

#[test]
fn continue_keeps_prior_writes_skips_later_writes_and_restarts_the_full_test() {
    let script = parse_script(
        "let n=3; let total=0;
         while (n>0 && total<20) {
             total=total*10+n;
             n=n-1;
             continue;
             total=999;
         }
         total*10+n",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(321.0));
}

#[test]
fn loop_jumps_are_whole_keywords_and_require_explicit_semicolons() {
    for keyword in ["break", "continue"] {
        for suffix in ["0", "_", "$"] {
            let name = format!("{keyword}{suffix}");
            let source = format!("let {name}=1;while({name}){{{name}=0;}}{name}");
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(0.0));
        }
        assert_error_at(
            &format!("let {keyword}=1;0"),
            ErrorKind::Unsupported,
            keyword,
        );
        assert_error_at(&format!("({keyword})"), ErrorKind::Unsupported, keyword);
        for suffix in ["é", "\\u0061"] {
            let error = parse_script(&format!("while(false){{{keyword}{suffix};}}0")).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Unsupported);
            let start = "while(false){".len() + keyword.len();
            assert_eq!(
                error.span(),
                Span {
                    start,
                    end: start + suffix.chars().next().unwrap().len_utf8()
                }
            );
        }
    }
    assert_eq!(
        parse_script(
            "let x=1;while(x){x=0;continue// retest\u{2028};}while(true){break/*exit\n*/;}42"
        )
        .unwrap()
        .evaluate(),
        Ok(42.0)
    );
}

fn assert_error_at(source: &str, kind: ErrorKind, needle: &str) {
    let start = source.rfind(needle).unwrap();
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + needle.len()
        },
        "{source}"
    );
}

#[test]
fn out_of_loop_jumps_are_early_errors_even_in_inactive_if_arms() {
    for keyword in ["break", "continue"] {
        for source in [
            format!("{keyword};42"),
            format!("if(false){{{keyword};}}42"),
            format!("if(true){{while(false){{}}}}else{{{keyword};}}42"),
            format!("while(false){{}}{keyword};42"),
            format!("42;{keyword};"),
        ] {
            assert_error_at(&source, ErrorKind::Malformed, keyword);
        }
    }
}

#[test]
fn rejects_invalid_and_out_of_slice_syntax_in_loop_paths_that_never_run() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, needle) in [
        ("let x=0;while(true){break; x=+;} x", Malformed, ";"),
        ("while(true){continue; while(false {}}42", Malformed, "{"),
        (
            "while(false){if(false){break outer;}}42",
            Unsupported,
            "outer",
        ),
        (
            "while(false){if(true){}else{continue/**/outer;}}42",
            Unsupported,
            "outer",
        ),
        (
            "while(false){continue/*\u{2029}*/outer;}42",
            Unsupported,
            "outer",
        ),
        ("while(false){break\n}42", Unsupported, "}"),
        ("while(false){continue 1;}42", Malformed, "1"),
        ("while(false){label: break;}42", Unsupported, ":"),
        ("switch(1){break;}42", Unsupported, "switch"),
        ("while(true){break;}42; 1", Unsupported, "1"),
    ] {
        assert_error_at(source, kind, needle);
    }
    for source in ["while(true){break;}", "while(true){continue;}"] {
        // Neither jump replaces the required final expression.
        assert_eq!(parse_script(source).unwrap_err().kind(), Unsupported);
    }
}

#[test]
fn jump_dispatches_share_the_exact_budget() {
    // 7 dispatches per iteration that continues, 2 to initialize, 2 for the
    // final falsy test, 3 for the next loop through break, and 1 final load.
    for (prefixes, expected) in [
        (2, Ok(0.0)),                               // 16,383
        (3, Ok(0.0)),                               // 16,384
        (4, Err(EvalError::DispatchLimitExceeded)), // 16,385
    ] {
        let source = format!(
            "let x=2339;while(x){{x=x-1;continue;}}while(true){{break;}}{}x",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
    assert_eq!(
        parse_script("while(true){continue;}0").unwrap().evaluate(),
        Err(EvalError::DispatchLimitExceeded)
    );
}

#[test]
fn unreached_jumps_still_count_toward_the_prepared_ceiling() {
    let jumps = "break;continue;".repeat(2046);
    let limits = Limits {
        max_tokens: 8193,
        ..Limits::default()
    };
    for (extra, expected) in [("", Ok(42.0)), ("break;", Err(EvalError::LimitExceeded))] {
        let source = format!("while(false){{{jumps}{extra}}}42");
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(
            script.instruction_count(),
            if extra.is_empty() { 4096 } else { 4097 }
        );
        assert_eq!(script.evaluate(), expected);
    }
}
