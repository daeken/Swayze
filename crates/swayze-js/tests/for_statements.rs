use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, parse_script, parse_script_with_limits,
};

#[test]
fn initializes_once_and_orders_test_body_and_update() {
    let script = parse_script(
        "let i=99; let trace=0;
         for (i=(null ?? 1); i<4 && trace<20; i=i+(trace ? 1 : 10)) {
             trace=trace*10+i;
         }
         trace*10+i",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(1234.0));
    assert_eq!(script.evaluate(), Ok(1234.0));

    for test in ["false", "0", "-0", "0/0", "null", "undefined"] {
        let source = format!("let i=99;for(i=3;{test};i=4){{i=5;}}i");
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(3.0));
    }
}

#[test]
fn each_clause_can_be_omitted() {
    for statement in [
        "for (;i<3;i=i+1) {}",
        "for (i=1;;i=i+1) {if(i===3){break;}}",
        "for (i=1;i<3;) {i=i+1;}",
        "for (;;i=i+1) {if(i===3){break;}continue;}",
        "for (;i<3;) {i=i+1;continue;}",
        "for (i=3;;) {break;}",
        "for (/* init */;/* test */;/* update */) {i=3;break;}",
    ] {
        let source = format!("let i=1;{statement}i");
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(3.0),
            "{source}"
        );
    }
}

#[test]
fn loop_jumps_select_the_innermost_for_or_while_across_if_blocks() {
    let script = parse_script(
        "let i=9; let j=0; let w=0; let trace=0;
         while (true) {
             w=w+1;
             for (i=1;i<4;i=i+1) {
                 j=0;
                 while (true) {j=j+1;if(j===1){continue;}break;}
                 if (i===2) {continue;}
                 trace=trace*10+i+j;
                 if (i===3) {break;}
             }
             if (w===1) {continue;}
             trace=trace*10+i;
             break;
         }
         trace*10+w",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(353532.0));

    let script = parse_script(
        "let i=0; let j=0; let n=0;
         for (;i<3;i=i+1) {
             for (j=0;j<3;j=j+1) {
                 if (j===0) {continue;}
                 n=n+1;
                 if (i===1) {break;}
             }
             if (i<2) {continue;}
             break;
         }
         n*100+i*10+j",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(523.0));
}

#[test]
fn clauses_see_enclosing_bindings_not_fresh_body_bindings() {
    let script = parse_script(
        "let i=0; let sum=0;
         if (true) {
             const step=1;
             for (i=step;i<step+3;i=i+step) {
                 const i=7;
                 let step=2;
                 sum=sum+i+step;
                 step=step+100;
                 continue;
             }
         }
         sum*10+i",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(274.0));
}

fn assert_error_at(source: &str, kind: ErrorKind, marked: &str) {
    // A prefix before '|' disambiguates identical tokens at different sites.
    let (prefix, needle) = marked.split_once('|').unwrap_or(("", marked));
    let start = source.rfind(&[prefix, needle].concat()).unwrap() + prefix.len();
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
fn checks_inactive_headers_and_bodies_with_lexical_diagnostics() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, needle) in [
        ("for(a=0;false;){let a=1;}42", Unsupported, "for(|a"),
        ("for(;flag;){let flag=0;}42", Unsupported, ";|flag"),
        (
            "let i=0;for(;false;i=only){let only=0;}42",
            Unsupported,
            "i=|only",
        ),
        ("for(;false;missing=0){}42", Unsupported, "missing"),
        ("const i=0;for(i=1;false;){}42", Unsupported, "="),
        ("const i=0;for(;false;i=1){}42", Unsupported, "="),
        (
            "let x=0;if(false){for(x=1;false;){}let x=2;}42",
            Unsupported,
            "for(|x",
        ),
        ("let i=0;for(;false;i=i+1){let a=+;}42", Malformed, ";"),
        ("for(;false;){break; x=0;}42", Unsupported, "x"),
        ("for(;false;){const a=0;let a=1;}42", Malformed, "let |a"),
        ("for(;false;){let local=1;}local", Unsupported, "local"),
        ("for(;;){break;}continue;42", Malformed, "continue"),
    ] {
        assert_error_at(source, kind, needle);
    }
}

#[test]
fn requires_braces_delimiters_and_assignment_clauses() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, needle) in [
        ("for ;", Malformed, ";"),
        ("for(){}42", Malformed, ")"),
        ("for(;;;){}42", Malformed, ";;|;"),
        ("for(0;false;){}42", Unsupported, "0"),
        ("for(let i=0;false;){}42", Unsupported, "let"),
        ("for(i in j){}42", Unsupported, "in"),
        ("for(i of j){}42", Unsupported, "of"),
        ("for(i**=1;false;){}42", Unsupported, "**="),
        ("for(i=0,j=1;false;){}42", Unsupported, ","),
        ("for(;i=1;){}42", Unsupported, "="),
        ("for(;false;i++){}42", Unsupported, "false;|i"),
        ("for(;false;1+2){}42", Unsupported, "1"),
        ("for(;false;i+1){}42", Unsupported, "+"),
        ("for(;false;i=0,i=1){}42", Unsupported, ","),
        ("for(i=;false;){}42", Malformed, "i=|;"),
        ("for(i=0\ntrue;){}42", Malformed, "true"),
        ("for(i=0){}42", Malformed, ")"),
        ("for(;true){}42", Malformed, ")"),
        ("for(;true;i=)", Malformed, ")"),
        ("for(;true;i=1;){}42", Malformed, "i=1|;"),
        ("for(;true;i=1", Malformed, ""),
        ("for(;true;i=1{}42", Malformed, "{"),
        ("for(;;)42", Malformed, "42"),
        ("for(;;){break;}", Unsupported, ""),
        ("for(;;){break;};42", Unsupported, ";"),
        ("42;for(;;){}", Unsupported, "for"),
        ("let for=0;42", Unsupported, "for"),
        ("(for)", Unsupported, "for"),
    ] {
        assert_error_at(source, kind, needle);
    }
    assert_eq!(
        parse_script("let for0=1;let for_=2;let for$=3;for(;for0;){for0=0;}for0+for_+for$")
            .unwrap()
            .evaluate(),
        Ok(5.0)
    );
}

#[test]
fn for_header_and_body_share_existing_parse_ceilings() {
    let source = "for(;;){}0";
    let limits = Limits {
        max_tokens: 8,
        max_nesting_depth: 1,
        max_work: 17,
        ..Limits::default()
    };
    assert!(parse_script_with_limits(source, limits).is_ok());
    for (limits, ceiling, span) in [
        (
            Limits {
                max_tokens: 7,
                ..limits
            },
            Limit::Tokens,
            Span { start: 9, end: 10 },
        ),
        (
            Limits {
                max_work: 16,
                ..limits
            },
            Limit::Work,
            Span { start: 9, end: 10 },
        ),
        (
            Limits {
                max_nesting_depth: 0,
                ..limits
            },
            Limit::NestingDepth,
            Span { start: 3, end: 4 },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(ceiling));
        assert_eq!(error.span(), span);
    }

    let source = "for(;(true?0:1);){}42";
    let limits = Limits {
        max_nesting_depth: 2,
        ..Limits::default()
    };
    let error = parse_script_with_limits(source, limits).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(&source[error.span().start..error.span().end], "?");
    assert!(
        parse_script_with_limits(
            source,
            Limits {
                max_nesting_depth: 3,
                ..limits
            }
        )
        .is_ok()
    );

    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    for depth in [128, 129] {
        let source = format!("{}break;{}0", "for(;;){".repeat(depth), "}".repeat(depth));
        let parsed = parse_script_with_limits(&source, limits);
        if depth == 128 {
            assert_eq!(parsed.unwrap().max_stack_depth(), 1);
        } else {
            let error = parsed.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
            assert_eq!(
                error.span(),
                Span {
                    start: 1024,
                    end: 1027
                }
            );
        }
    }
}

#[test]
fn meters_header_clauses_jumps_and_resolution_even_on_skipped_paths() {
    let source = "let i=0;for(i=1;false;i=2){break;}i";
    // In addition to the source bytes: three decimal conversions, one header
    // and one frame, 13 op emissions, one break patch, and three lookups of i
    // (each costs one candidate visit plus one compared byte).
    let limits = Limits {
        max_work: source.len() + 25,
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(1.0)
    );
    let error = parse_script_with_limits(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(
        error.span(),
        Span {
            start: source.len() - 1,
            end: source.len()
        }
    );
}

#[test]
fn skipped_update_and_body_count_toward_the_inclusive_prepared_ceiling() {
    let limits = Limits {
        max_tokens: 8192,
        ..Limits::default()
    };
    for (prefixes, expected) in [(4085, Ok(42.0)), (4086, Err(EvalError::LimitExceeded))] {
        let source = format!(
            "let i=0;for(;false;i={}0){{break;}}42",
            "+ ".repeat(prefixes)
        );
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 11 + prefixes);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn for_dispatches_share_the_inclusive_execution_ceiling() {
    // 11 dispatches per continuing iteration, 2 for setup, 4 for the last
    // (falsy) test, and 1 for the final load before the extra unary operators.
    for (prefixes, expected) in [
        (8, Ok(1488.0)),
        (9, Ok(1488.0)),
        (10, Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!(
            "let i=0;for(;i<1488;i=i+1){{continue;}}{}i",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
    for source in ["for(;;){}0", "let i=0;for(;;i=i+1){continue;}0"] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Err(EvalError::DispatchLimitExceeded)
        );
    }
}
