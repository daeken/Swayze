use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, parse_script, parse_script_with_limits,
};

#[test]
fn else_binds_only_to_the_if_whose_then_arm_just_closed() {
    let source = "let x=0;
                  if (true) {
                      if (false) {}
                  } /*outer*/ else { while (true) {} }
                  if (false) {
                      if (true) { while (true) {} } else { while (true) {} }
                  } else {
                      if (false) { while (true) {} } else { x=4; }
                      if (true) { x=x*10+2; }
                  }
                  x";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(42.0));

    // Neither a while body nor an else arm may take an else of its own.
    for (source, needle) in [
        ("if(true){while(false){}else{}}42", "else"),
        ("while(false){if(true){}else{}else{}}42", "else"),
        ("while(false){if(false){}else if(true){}}42", "if"),
    ] {
        let start = source.rfind(needle).unwrap();
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported, "{source}");
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + needle.len()
            },
            "{source}"
        );
    }
}

#[test]
fn inner_loops_restart_at_their_own_tests_on_every_outer_iteration() {
    let source = "let out=0; let outer=2; let inner=0;
                  while (outer) {
                      inner=2;
                      while (inner) {
                          if (outer === inner) {
                              if (true) { out=out*10+outer; } else { while (true) {} }
                          } else { out=out*10+inner+2; }
                          inner=inner-1;
                      }
                      outer=outer-1;
                  }
                  out";
    let script = parse_script(source).unwrap();
    assert_eq!(script.evaluate(), Ok(2341.0));
    assert_eq!(script.evaluate(), Ok(2341.0));
}

#[test]
fn checks_every_nested_block_even_if_never_reached() {
    use ErrorKind::{Malformed, Unsupported};
    for (body, kind) in [
        ("missing=1;", Unsupported),
        ("later=1;", Unsupported),
        ("x=later;", Unsupported),
        ("c=1;", Unsupported),
        ("let x=1;", Unsupported),
        ("const y=1; let y=2;", Malformed),
        ("break", Unsupported),
        ("continue\n", Unsupported),
        ("{x=1;}", Unsupported),
        ("1;", Unsupported),
        ("x=1", Unsupported),
        ("x=1\nx=2;", Unsupported),
        ("x=;", Malformed),
        ("if(true)x=1;", Malformed),
        ("while(false)x=1;", Malformed),
        ("if(true){};", Unsupported),
        ("while(false){};", Unsupported),
    ] {
        let source = format!(
            "let x=0; const c=2;
             if(true){{}}else{{while(false){{if(false){{}}else{{{body}}}}}}}
             let later=1; x"
        );
        assert_eq!(parse_script(&source).unwrap_err().kind(), kind, "{source}");
    }
    let source = "while(false){if(true){}else{";
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.kind(), Malformed);
    assert_eq!(
        error.span(),
        Span {
            start: source.len(),
            end: source.len()
        }
    );
}

#[test]
fn fixed_statement_cap_counts_both_control_kinds_but_not_if_arms_twice() {
    let mut prefix = String::new();
    for _ in 0..64 {
        prefix.push_str("while(false){if(false){}else{");
    }
    let closes = "}".repeat(128);
    let at_cap = format!("{prefix}break;continue;{closes}");
    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    // Each test uses its own expression budget and neither jump needs a frame.
    // Frames are reusable, so two separate chains at the cap fit in one script.
    assert_eq!(
        parse_script_with_limits(&format!("{at_cap}{at_cap}42"), limits)
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
    for (extra, keyword) in [("if(false){}", "if"), ("while(false){}", "while")] {
        let error = parse_script_with_limits(
            &format!("{prefix}{extra}{closes}42"),
            Limits {
                max_nesting_depth: usize::MAX,
                ..limits
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
        assert_eq!(
            error.span(),
            Span {
                start: prefix.len(),
                end: prefix.len() + keyword.len()
            }
        );
        assert!(error.message().contains("statement nesting"));
    }

    let source = "while(false){if(true?true:false){}}42";
    let error = parse_script_with_limits(source, limits).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    let start = source.find('?').unwrap();
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + 1
        }
    );
}

#[test]
fn nested_loops_share_the_exact_dispatch_budget() {
    // Each of the 909 outer iterations dispatches 18 instructions: 2 for
    // the outer test, 2 to reset y, 7 for one inner iteration, 2 for its
    // falsy test, 4 to decrement x, and 1 for the outer back edge. Add 4
    // dispatches to initialize both bindings, 2 for the final outer test,
    // 1 load, and the unary ops.
    for (prefixes, expected) in [
        (14, Ok(0.0)),                               // 16,383 dispatches
        (15, Ok(0.0)),                               // 16,384 dispatches
        (16, Err(EvalError::DispatchLimitExceeded)), // 16,385 needed
    ] {
        let source = format!(
            "let x=909;let y=0;while(x){{y=1;while(y){{y=y-1;}}x=x-1;}}{}x",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn unreachable_nested_instructions_still_count_toward_the_prepared_cap() {
    let limits = Limits {
        max_tokens: 4200,
        ..Limits::default()
    };
    for (extra, expected) in [(0, Ok(42.0)), (1, Err(EvalError::LimitExceeded))] {
        let source = format!(
            "let x=42;while(false){{if(true){{x={}0;}}}}x",
            "- ".repeat(4086 + extra)
        );
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 4096 + extra);
        assert_eq!(script.evaluate(), expected);
    }
}
