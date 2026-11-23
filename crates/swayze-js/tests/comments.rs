use swayze_js::{ErrorKind, Limit, Limits, Span, parse_script, parse_script_with_limits};

fn evaluate(source: &str) -> f64 {
    parse_script(source).unwrap().evaluate().unwrap()
}

#[test]
fn comments_are_trivia_at_script_and_token_boundaries() {
    assert_eq!(
        evaluate(
            "/* lead */const/**/x/* name */=/* equals */(/* open */8/* close */)/* init */;\n\
                  // declaration done\nconst y=x// still in the initializer\n+4;/* read */y/**//2;/* tail */"
        ),
        6.0
    );
    assert_eq!(evaluate("1+/**/+2"), 3.0);
    assert_eq!(evaluate("1-/**/-2"), 3.0);
    assert_eq!(evaluate("2*/**/3"), 6.0);
    assert_eq!(evaluate("2/**/**3"), 8.0);
    assert_eq!(evaluate("8// not division\n/2"), 4.0);
    assert_eq!(evaluate("8/ /* divisor */2"), 4.0);
    assert_eq!(evaluate("/* outer /* inner */ 2/***/"), 2.0);
    assert_eq!(evaluate("1/* // and /* are ordinary content */+2"), 3.0);
}

#[test]
fn every_ecmascript_line_terminator_ends_line_comments_and_affects_diagnostics() {
    for terminator in ['\n', '\r', '\u{2028}', '\u{2029}'] {
        assert_eq!(evaluate(&format!("1// ignored{terminator}+2")), 3.0);
        for trivia in [
            format!("// ignored{terminator}/**/"),
            format!("/*{terminator}*//**/"),
        ] {
            let error = parse_script(&format!("1{trivia}2")).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Unsupported);
            assert_eq!(
                error.message(),
                "expression statements require explicit semicolons"
            );
            assert_eq!(
                error.span(),
                Span {
                    start: 1 + trivia.len(),
                    end: 2 + trivia.len()
                }
            );

            for next in ["x", "2", "const y=2; x"] {
                let error = parse_script(&format!("const x=1{trivia}{next}")).unwrap_err();
                assert_eq!(error.kind(), ErrorKind::Unsupported);
                assert_eq!(
                    error.message(),
                    "const declarations require explicit semicolons"
                );
            }
            assert_eq!(evaluate(&format!("const x=1{trivia}+2; x")), 3.0);
            assert_eq!(evaluate(&format!("const x=1{trivia}; x")), 1.0);
            assert_eq!(
                parse_script(&format!("(1{trivia}2)")).unwrap_err().kind(),
                ErrorKind::Malformed
            );
        }
    }
    assert_eq!(evaluate("1// ignored\r\n+2"), 3.0);
    assert_eq!(
        evaluate("1// not line breaks: \u{0085}\u{000B}\u{000C}+2"),
        1.0
    );
    for trivia in ["/**/", "/*\u{0085}\u{000B}\u{000C}*/"] {
        for prefix in ["1", "const x=1"] {
            let error = parse_script(&format!("{prefix}{trivia}2")).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Malformed);
            assert_eq!(error.message(), "expected a binary operator");
        }
    }
}

#[test]
fn eof_ends_line_comments_but_not_block_comments() {
    for source in ["1//", "1; // arbitrary text /*", "const x=1; x//🚧"] {
        assert_eq!(evaluate(source), 1.0);
    }
    for (source, kind, message) in [
        (
            "//",
            ErrorKind::Unsupported,
            "empty scripts are outside the arithmetic subset",
        ),
        (
            "/**/ // no expression",
            ErrorKind::Unsupported,
            "empty scripts are outside the arithmetic subset",
        ),
        (
            "1+// no operand",
            ErrorKind::Malformed,
            "expected an operand before end of source",
        ),
        (
            "const x=1// no semicolon",
            ErrorKind::Unsupported,
            "const declarations require explicit semicolons",
        ),
        (
            "const x=1; /* no final expression */",
            ErrorKind::Unsupported,
            "a final expression is required after statements",
        ),
        (
            "let x=1; x=2// no semicolon",
            ErrorKind::Unsupported,
            "assignments require explicit semicolons",
        ),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), kind, "{source:?}");
        assert_eq!(error.message(), message);
        assert_eq!(
            error.span(),
            Span {
                start: source.len(),
                end: source.len()
            }
        );
    }
    for source in [
        "/*",
        "/*/",
        "/**",
        "1/* 🚧",
        "1;/*\u{2029}",
        "()/* unterminated",
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed, "{source:?}");
        assert_eq!(error.message(), "unterminated block comment");
        assert_eq!(
            error.span(),
            Span {
                start: source.find("/*").unwrap(),
                end: source.len()
            }
        );
    }
}

#[test]
fn comments_do_not_join_tokens_or_enable_other_syntax() {
    for source in [
        "1/**/2",
        "./**/5",
        "1./**/5",
        "1e/**/2",
        "1e+/**/2",
        "1/**/e2",
        "0/**/x10",
        "co/**/nst x=1; x",
        "const f/**/oo=1; foo",
        "const foo=1; fo/**/o",
    ] {
        assert!(parse_script(source).is_err(), "{source:?}");
    }
    for source in [
        "/**//2/",
        "8 / /**/ /2/",
        "8/**//=2",
        "1/**/++2",
        "1;/* comment */2\n3",
        "1; // comment\u{2028}const x=1;",
        "1/**/\nvar x=2",
        "#!/usr/bin/env node\n1",
        "1;/* comment */#extra",
        "<!-- not trivia\n1",
        "1\n--> not trivia",
        "1/*\n*/--> not trivia",
        "1;/**/<!-- not trivia",
    ] {
        assert_eq!(
            parse_script(source).unwrap_err().kind(),
            ErrorKind::Unsupported,
            "{source:?}"
        );
    }
    let error = parse_script("8 / /**/ /2/").unwrap_err();
    assert_eq!(
        error.message(),
        "regular expression literals are outside the arithmetic subset"
    );
    assert_eq!(error.span(), Span { start: 9, end: 10 });
}

#[test]
fn comment_bytes_use_the_work_and_source_budgets_but_not_the_token_budget() {
    let source = "/*🔑*/1//é";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 1,
        max_work: source.len() + 2, // One literal byte for conversion and one instruction.
        max_nesting_depth: 0,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 1);
    assert_eq!(script.evaluate(), Ok(1.0));

    for (max_work, start, end) in [
        (0, 0, 1),
        (1, 1, 2),
        (5, 2, 6),
        (6, 6, 7),
        (7, 7, 8),
        (11, 9, 10),
        (12, 10, 11),
        (14, 11, 13),
    ] {
        let error = parse_script_with_limits(source, Limits { max_work, ..limits }).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
        assert_eq!(error.span(), Span { start, end });
    }
    let error = parse_script_with_limits(
        source,
        Limits {
            max_tokens: 0,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Tokens));
    assert_eq!(error.span(), Span { start: 8, end: 9 });

    let error = parse_script_with_limits(
        source,
        Limits {
            max_source_bytes: source.len() - 1,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::SourceBytes));
    assert_eq!(error.span(), Span { start: 11, end: 13 });
}

#[test]
fn long_unterminated_comments_stop_at_the_work_ceiling() {
    let source = format!("/*{}", "🔑".repeat(5000));
    let error = parse_script_with_limits(
        &source,
        Limits {
            max_work: 42,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(error.span(), Span { start: 42, end: 46 });
}
