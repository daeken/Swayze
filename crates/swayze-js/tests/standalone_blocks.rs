use swayze_js::{
    Error, ErrorKind, EvalError, Limit, Limits, Span, parse_script, parse_script_with_limits,
};

#[test]
fn root_and_nested_blocks_keep_separate_lexical_bindings() {
    let script = parse_script(
        ";; const x=1; let out=0;
         { let x=2; { const x=3; ; out=x; } ++x; out=out*10+x; }
         ; { const x=4; ; out=out*10+x; }
         out*10+x;",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(3341.0));

    // The nearest earlier declaration can also be inside a later-declaring ancestor.
    let source = "let out=0; { { const x=3; out=x; } let x=4; out=out*10+x; } out";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(34.0));
}

#[test]
fn blocks_and_empty_statements_are_allowed_in_braced_bodies() {
    let source = "let out=0;
                  if(false){;{out=1;}}else{;{out=4;};};
                  while(false){;{out=1;};};
                  for(;;){;{break;};};
                  do{;{out+=2;};}while(false);;
                  out";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(6.0));
}

// `|` marks the start of the expected diagnostic.
fn error_at(marked: &str, len: usize, kind: ErrorKind) -> Error {
    let (before, after) = marked.split_once('|').unwrap();
    let error = parse_script(&format!("{before}{after}")).unwrap_err();
    assert_eq!(error.kind(), kind, "{marked}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start: before.len(),
            end: before.len() + len,
        },
        "{marked}: {error}"
    );
    error
}

#[test]
fn nearest_binding_is_checked_even_before_initialization_or_in_skipped_code() {
    use ErrorKind::{Malformed, Unsupported};
    for source in [
        "let x=1;{let x=|x;}x",
        "let x=1;{{|x++;}let x=2;}x",
        "let x=1;if(false){{|x=3;const x=2;}}x",
    ] {
        let error = error_at(source, 1, Unsupported);
        assert!(
            error
                .message()
                .contains("before lexical binding initialization")
        );
    }
    let error = error_at("let x=1;{const x=2;{x|+=3;}}x", 2, Unsupported);
    assert!(error.message().contains("assignment to const"));
    error_at("{let x=1;}{let y=|x;}0", 1, Unsupported);
    error_at("{let x=1;}|x", 1, Unsupported);
    error_at("{let x=1;const |x=2;}0", 1, Malformed);
}

#[test]
fn jumps_cross_bare_blocks_to_the_innermost_loop_and_reentry_initializes_slots() {
    let script = parse_script(
        "let sum=0; let i=0;
         for(;i<3;i++) {{
             const seed=i+1; let local=seed;
             do { {{ local+=10; break; }} } while(true);
             sum+=local;
             if(i===1) {{ continue; }}
             { sum+=100; }
         }}
         sum*10+i",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(2363.0));
    assert_eq!(script.evaluate(), Ok(2363.0));

    // A continue from a nested block in a do loop lands at its trailing test.
    let source = "let n=0; do {{{ n++; if(n<3){{continue;}} n=100; }}} while(n<2); n";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(2.0));

    error_at("{{|break;}}0", 5, ErrorKind::Malformed);
    error_at("if(false){{|continue;}}0", 8, ErrorKind::Malformed);
}

#[test]
fn empty_statements_do_not_supply_or_follow_the_final_expression() {
    for source in ["|", "// trivia\n|"] {
        let error = error_at(source, 0, ErrorKind::Unsupported);
        assert_eq!(
            error.message(),
            "empty scripts are outside the arithmetic subset"
        );
    }
    for source in [";|", "{}|", ";{;{}}; /* trivia */|", "42;;|", "42;{}|"] {
        let error = error_at(source, 0, ErrorKind::Unsupported);
        assert_eq!(
            error.message(),
            "a final expression is required after statements"
        );
    }
    error_at("42\n|{}", 1, ErrorKind::Unsupported);
    error_at("{;|", 0, ErrorKind::Malformed);
}

#[test]
fn statement_positions_do_not_expand_expression_grammar_or_insert_semicolons() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, len, kind) in [
        ("{42|}0", 1, Unsupported),
        ("let x=1;{{x+1|}}0", 1, Unsupported),
        ("let x=1;{x=2\n|}x", 1, Unsupported),
        ("{let x=1\n|}0", 1, Unsupported),
        ("let x=|{};x", 1, Unsupported),
        ("1+|;0", 1, Malformed),
        ("while(false)|;0", 1, Malformed),
        ("for(;false;)|;0", 1, Malformed),
        ("do|;while(false);0", 1, Malformed),
        ("if(false){};|else{}0", 4, Unsupported),
        ("do{}|;while(false);0", 1, Malformed),
    ] {
        error_at(source, len, kind);
    }
}

#[test]
fn bare_blocks_share_the_exact_active_frame_cap_but_not_expression_depth() {
    let blocks = "{".repeat(128);
    let closes = "}".repeat(128);
    let chain = format!("{blocks};{closes}");
    let limits = Limits {
        max_nesting_depth: 0,
        ..Limits::default()
    };
    let script = parse_script_with_limits(&format!("{chain}{chain}42"), limits).unwrap();
    assert_eq!(script.instruction_count(), 1);
    assert_eq!(script.evaluate(), Ok(42.0));

    let mixed = format!("while(false){{{}", "{".repeat(127));
    assert_eq!(
        parse_script(&format!("{mixed};{closes}42"))
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
    for (prefix, extra, len) in [
        (&blocks, "{}", 1),
        (&mixed, "{}", 1),
        (&blocks, "if(false){}", 2),
    ] {
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
                end: prefix.len() + len
            }
        );
        assert!(error.message().contains("statement nesting"));
    }
}

#[test]
fn exact_lexing_and_frame_work_budgets_include_braces_and_empty_statements() {
    let source = ";{;{;;};};42;";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 12,
        // Source bytes, two frames, two decimal conversion bytes, one emission.
        max_work: source.len() + 5,
        ..Limits::default()
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 1);
    assert_eq!(script.evaluate(), Ok(42.0));
    for (limit, limits) in [
        (
            Limit::SourceBytes,
            Limits {
                max_source_bytes: limits.max_source_bytes - 1,
                ..limits
            },
        ),
        (
            Limit::Tokens,
            Limits {
                max_tokens: limits.max_tokens - 1,
                ..limits
            },
        ),
        (
            Limit::Work,
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        assert_eq!(
            error.span(),
            Span {
                start: source.len() - 1,
                end: source.len()
            }
        );
    }

    // At this point both the empty-statement token and the brace have been scanned.
    let error = parse_script_with_limits(
        source,
        Limits {
            max_work: 2,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(error.span(), Span { start: 1, end: 2 });
}

#[test]
fn delimiters_and_empty_statements_do_not_count_as_prepared_instructions_or_dispatches() {
    for (pluses, expected) in [(4093, Ok(42.0)), (4094, Err(EvalError::LimitExceeded))] {
        let source = format!("{{;const x={}0;{{;;}}}}42;", "+ ".repeat(pluses));
        let script = parse_script_with_limits(
            &source,
            Limits {
                max_tokens: 4200,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(script.instruction_count(), pluses + 3);
        assert_eq!(script.evaluate(), expected);
    }

    // Seven dispatches per iteration; two for the initializer, two for the
    // last (falsy) loop test, and one load plus the unary ops for the result.
    for (pluses, expected) in [(6, Ok(0.0)), (7, Err(EvalError::DispatchLimitExceeded))] {
        let source = format!(
            "let n=2339;while(n){{;{{;{{--n;;}};}};}}{}n",
            "+ ".repeat(pluses)
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), expected);
    }
}
