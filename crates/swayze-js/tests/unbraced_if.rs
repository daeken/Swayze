use swayze_js::{
    ErrorKind, EvalScratch, Limit, Limits, Span, parse_script, parse_script_for_completion,
    parse_script_for_completion_with_limits, parse_script_with_limits,
};

#[test]
fn nearest_else_wins_even_across_a_label_but_not_a_brace() {
    for (outer, inner, expected) in [(true, true, 1.0), (true, false, 2.0), (false, true, 3.0)] {
        let source =
            format!("let x=0; if({outer}) inner:if({inner}) x=1; /*\n*/else x=2; else {{x=3;}} x");
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(expected));
    }
    for (source, expected) in [
        ("let x=0;if(false)if(true)x=1;else x=2;x", 0.0),
        ("let x=0;if(false){if(true)x=1;}else x=2;x", 2.0),
        (
            "let x=2;if(false){let x=9;}else if(x===2) ++x; else x=99;x",
            3.0,
        ),
        (
            "let x=1;if(true) x++, x+=2; else x=99; if(true) ++x + 100; x",
            5.0,
        ),
        ("let x=0; if(true) while(x<2) {x++;} else x=99; x", 2.0),
        (
            "let x=0; if(false) ; else for(let i=0;i<2;i++) {x+=i;} x",
            1.0,
        ),
        ("let x=0; if(true) do {x++;} while(x<2); else x=99; x", 2.0),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn empty_and_discarded_arms_leave_no_script_result() {
    for source in [
        "if (true) ;",
        "if (false) ;;",
        "if(true)if(false) ; else 'a'+'b'; // EOF",
        "if(false) false.length; else 'use strict';",
        "let x; if(true) x=1;",
    ] {
        let script = parse_script_for_completion(source).unwrap();
        assert_eq!(script.execute_with_scratch(&mut EvalScratch::new()), Ok(()));
        let error = parse_script(source).unwrap_err();
        assert_eq!(
            error.message(),
            "a final expression is required after statements"
        );
        assert_eq!(
            error.span(),
            Span {
                start: source.len(),
                end: source.len()
            }
        );
    }
}

#[test]
fn jumps_keep_existing_target_and_label_lifetimes() {
    for (source, expected) in [
        (
            "let x=0; outer:while(x<5){x++; if(x<3) continue outer; if(x===3) break; x=99;} x",
            3.0,
        ),
        (
            "let x=0; done:alias:if(true) if(false) break done; else break alias; else x=99; done:if(true) x=2; x",
            2.0,
        ),
        (
            "let x=0; if(true) done:do{ x++; if(x<2) continue done; }while(x<3); else x=99;x",
            3.0,
        ),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
    for (source, at, message) in [
        (
            "a:if(false) a:if(true);",
            "a:if(true)",
            "duplicate active label",
        ),
        (
            "a:if(false);else continue a;",
            "a;",
            "continue target must label a loop",
        ),
        (
            "if(true) a:if(true); else; else break a;",
            "a;",
            "jump target is not an active label",
        ),
        (
            "if(false) break;",
            "break",
            "break and continue require an enclosing loop",
        ),
    ] {
        let error = parse_script_for_completion(source).unwrap_err();
        let start = source.rfind(at).unwrap();
        assert_eq!(error.kind(), ErrorKind::Malformed, "{source}");
        assert_eq!(error.message(), message, "{source}");
        assert_eq!(error.span().start, start, "{source}");
    }
}

#[test]
fn skipped_arms_are_checked_without_asi_or_declarations() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, at, len) in [
        ("if(false) missing;", Unsupported, "missing", 7),
        ("const x=1;if(true);else ++x;", Unsupported, "++", 2),
        ("if(false)if(true) 1+;", Malformed, ";", 1),
        ("if(false) let x;", Unsupported, "let", 3),
        ("if(true);else const x=0;", Unsupported, "const", 5),
        ("if(false) a:let x;", Unsupported, "let", 3),
        ("if(false) function f() {}", Unsupported, "function", 8),
        ("if(false) var x;", Unsupported, "var", 3),
        ("if(true) 'x'\nelse;", Unsupported, "else", 4),
        ("let x; if(true) x=1\nelse;", Unsupported, "else", 4),
        ("if(true) ; else", Malformed, "", 0),
        ("if(true) if(false); /*", Malformed, "/*", 2),
        ("if(true) while(false);", Malformed, ";", 1),
        ("if(true) for(;false;);", Malformed, ";", 1),
        ("if(true) do ; while(false);", Malformed, "; while", 1),
    ] {
        let error = parse_script_for_completion(source).unwrap_err();
        let start = source.rfind(at).unwrap();
        assert_eq!(error.kind(), kind, "{source}: {error}");
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
fn single_arms_do_not_add_scope_hops_or_duplicate_token_work() {
    let source = "let x;if(true)if(false)x;else;true";
    // 16 tokens, 2 (marker + frame + two instructions + false-edge fixup),
    // 2 for the alternate edge and fixup, 3 for the declaration and final
    // value, 2 for load/discard, and 2 for x lookup (one candidate and one
    // comparison byte; there are no parent hops through unbraced arms).
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 16,
        max_nesting_depth: 1,
        max_work: source.len() + 19,
    };
    assert_eq!(
        parse_script_with_limits(source, limits)
            .unwrap()
            .instruction_count(),
        10
    );
    for (limit, below) in [
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
        assert_eq!(
            parse_script_with_limits(source, below).unwrap_err().kind(),
            ErrorKind::LimitExceeded(limit)
        );
    }
}

#[test]
fn unbraced_arms_share_the_frame_ceiling_and_cache_eof_on_unwind() {
    let chain = "if(true)".repeat(128);
    let source = format!("{chain}; // cached EOF");
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 128 * 4 + 1,
        max_nesting_depth: 1,
        max_work: source.len() + 128 * 5,
    };
    let script = parse_script_for_completion_with_limits(&source, limits).unwrap();
    assert_eq!(script.execute_with_scratch(&mut EvalScratch::new()), Ok(()));
    for (extra, token_len) in [("if(true);", 2), ("label:while(false){}", 5)] {
        let source = format!("{chain}{extra}");
        let error = parse_script_for_completion_with_limits(
            &source,
            Limits {
                max_nesting_depth: 1,
                ..Limits::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
        assert_eq!(
            error.span(),
            Span {
                start: chain.len(),
                end: chain.len() + token_len
            }
        );
    }
}
