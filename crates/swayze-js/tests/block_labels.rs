use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, parse_script, parse_script_with_limits,
};

#[test]
fn named_break_exits_a_block_across_nested_loop_and_block_tails() {
    let script = parse_script(
        "let n=0;
         done: {
             let increment=4;
             { for (let i=0; i<1; n=999) {
                 do {
                     if (true) { n+=increment; break done; }
                     n=100;
                 } while (true);
                 n=200;
             } n=300; }
             n=400;
         }
         n+=38;
         n",
    )
    .unwrap();
    assert_eq!(script.max_stack_depth(), 2);
    assert_eq!(script.evaluate(), Ok(42.0));
    assert_eq!(script.evaluate(), Ok(42.0));
}

#[test]
fn a_block_exit_stays_inside_the_loop_and_unlabeled_jumps_ignore_block_targets() {
    let source = "let trace=0;
         outer: for (let i=1; i<=5; i++) {
             step: {
                 inner: { break inner; trace=999; }
                 while (true) { trace=trace*10+i; break; }
                 if (i===1) { break step; }
                 if (i===2) { continue; }
                 if (i===3) { continue outer; }
                 break;
             }
             trace=trace*10+4;
         }
         trace";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(14234.0));
}

#[test]
fn labels_share_a_separate_namespace_and_may_be_reused_after_close() {
    let source = "let name=0;
         if (false) { name: { break name; } }
         else if (false) { name: while (true) { break name; } }
         else { name: do { break name; } while (true); }
         name: {
             Name: { break Name; }
             { name=42; break name; }
         }
         name: while (true) { inner: { break name; } }
         name: { let name=0; break name; }
         name";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(42.0));
    assert_eq!(parse_script("done:{}42").unwrap().instruction_count(), 1);
}

fn assert_error_at(source: &str, kind: ErrorKind, last: &str) {
    let error = parse_script(source).unwrap_err();
    let start = source.rfind(last).unwrap();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + last.len()
        },
        "{source}: {error}"
    );
}

#[test]
fn invalid_jumps_and_duplicates_are_checked_in_skipped_code() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, last) in [
        ("a:{if(false){{a:{}}}}42", Malformed, "a"),
        (
            "done:{break done; done:do{}while(false);}42",
            Malformed,
            "done",
        ),
        ("a:for(;;){if(false){a:{}}}42", Malformed, "a"),
        ("a:{}break a;42", Malformed, "a"),
        ("if(false){a:{}}else{break a;}42", Malformed, "a"),
        ("a:{break a;break unknown;}42", Malformed, "unknown"),
        ("a:{break;}42", Malformed, "break"),
        ("a:{continue;}42", Malformed, "continue"),
        ("a:{break a;let x=x;}42", Unsupported, "x"),
        ("a:{break a;const x=0;x=1;}42", Unsupported, "="),
        ("a:{break a;first;second;}42", Unsupported, "first"),
        ("a:{let x=0;break a;}x", Unsupported, "x"),
        ("a:b:{}42", Unsupported, "b"),
        ("a:if(true){}42", Unsupported, "if"),
        ("a:42", Unsupported, "42"),
    ] {
        assert_error_at(source, kind, last);
    }

    for source in [
        "done:{continue done;}42",
        "done:{break done;while(false){continue done;}}42",
        "outer:while(false){done:{if(false){continue done;}}}42",
    ] {
        assert_error_at(source, Malformed, "done");
        assert_eq!(
            parse_script(source).unwrap_err().message(),
            "continue target must label a loop"
        );
    }
}

#[test]
fn a_block_jump_requires_an_explicit_semicolon_and_no_line_break_before_its_name() {
    use ErrorKind::{Malformed, Unsupported};
    for lt in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        for trivia in [
            lt.to_owned(),
            format!("// comment{lt}"),
            format!("/*{lt}*/"),
        ] {
            let source = format!("done:{{break{trivia}done;}}42");
            assert_error_at(&source, Unsupported, "done");
            let source = format!("done:{{break/**/done{trivia};}}42");
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(42.0));
        }
    }
    assert_error_at("done:{break done}42", Unsupported, "}");
    assert_error_at("done:{break done", Unsupported, "");
    assert_error_at("done:{break done 42;}", Malformed, "42");
}

#[test]
fn a_label_uses_only_its_blocks_statement_frame_and_no_expression_nesting() {
    let wrap = |inside| format!("outer:{{{}{inside}{}}}42", "{".repeat(126), "}".repeat(126));
    let limits = Limits {
        max_nesting_depth: 0,
        ..Limits::default()
    };
    let source = wrap("inner:{break outer;}");
    assert_eq!(
        parse_script_with_limits(&source, limits)
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
    let source = wrap("inner:{overflow:{}}");
    let error = parse_script_with_limits(&source, limits).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    let start = source.find("overflow").unwrap();
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + "overflow".len()
        }
    );
}

#[test]
fn mixed_target_traversal_and_block_break_fixups_are_metered_exactly() {
    let source = "a0:{while(false){{a1:{break a0;}}}}0";
    // Source bytes, 1 decimal byte, 1 group, 4 frames, 5 emissions, 1 break
    // fixup, 4 units to check a1 (loop + a0), and 7 to find a0 (a1 + loop
    // + a0). The unlabeled bare block isn't visited by either search.
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 20,
        max_work: source.len() + 23,
        ..Limits::default()
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 5);
    assert_eq!(script.evaluate(), Ok(0.0));
    for (limits, limit) in [
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
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        assert_eq!(
            error.span(),
            Span {
                start: source.len() - 1,
                end: source.len()
            }
        );
    }
    let inner = source.find("a1").unwrap();
    let jump = source.rfind("a0").unwrap();
    for (start, before_search, work, after_search) in [
        (inner, inner + 4 + 6, 4, inner + 3),
        (jump, jump + 2 + 11, 7, jump + 2),
    ] {
        for units in 0..=work {
            let error = parse_script_with_limits(
                source,
                Limits {
                    max_work: before_search + units,
                    ..limits
                },
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
            let expected = if units < work {
                Span {
                    start,
                    end: start + 2,
                }
            } else {
                Span {
                    start: after_search,
                    end: after_search + 1,
                }
            };
            assert_eq!(error.span(), expected);
        }
    }

    let source = "a:{break a;break a;}0";
    let close = source.find('}').unwrap();
    // Through the closing brace, before fixups: bytes, 1 frame, 4 units
    // for target searches, 2 emissions. Each fixup then charges at `}`.
    let before_fixups = close + 1 + 7;
    for units in 0..=2 {
        let error = parse_script_with_limits(
            source,
            Limits {
                max_work: before_fixups + units,
                ..Limits::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
        let start = if units < 2 { close } else { close + 1 };
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + 1
            }
        );
    }
}

#[test]
fn block_labels_add_no_instructions_and_only_reached_jumps_dispatch() {
    let jumps = "break a;".repeat(4095);
    for (extra, count, result) in [
        ("", 4096, Ok(42.0)),
        ("break a;", 4097, Err(EvalError::LimitExceeded)),
    ] {
        let source = format!("a:{{{jumps}{extra}}}42");
        let script = parse_script_with_limits(
            &source,
            Limits {
                max_tokens: 12_300,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(script.instruction_count(), count);
        assert_eq!(script.evaluate(), result);
    }

    // Two to initialize, 12 per iteration (including the named break), one
    // final load, plus 0, 1 or 2 unary instructions: 16,383, 16,384, 16,385.
    for (prefix, result) in [
        ("", Ok(1365.0)),
        ("+ ", Ok(1365.0)),
        ("+ + ", Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!(
            "let n=0;while(true){{done:{{n++;break done;n=999;}}if(n===1365){{break;}}}}{prefix}n"
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), result);
    }
}
