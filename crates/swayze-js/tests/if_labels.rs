use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn each_alias_breaks_from_each_arm_past_all_later_tests_and_bodies() {
    for target in ["$done", "_alias"] {
        for gate in 1..=4 {
            let source = format!(
                "let $done={gate}; let trace=0;
                 $done/*\n*/: _alias: if ($done===1) {{
                     let $done=10; trace=$done; break {target}; trace=999;
                 }} else if ($done===2) {{
                     let $done=20;
                     for(let i=0;i<1;trace=999) {{ do {{
                         trace=$done; break {target};
                     }} while(true); }}
                     trace=999;
                 }} else if ($done===3) {{
                     let $done=30; {{ if(true) {{ trace=$done; break {target}; }} }} trace=999;
                 }} else {{
                     let $done=40; trace=$done; break {target}; trace=999;
                 }}
                 trace+$done"
            );
            let script = parse_script(&source).unwrap();
            assert_eq!(script.max_stack_depth(), 2);
            assert_eq!(script.evaluate(), Ok(f64::from(gate * 11)));
        }
    }
}

#[test]
fn nested_targets_restore_the_outer_label_and_nearest_loop() {
    let source = "let trace=0;
         repeat: for(let i=1;i<=5;i++) {
             done: if(true) {
                 inner: if(false) {} else { break inner; trace=999; }
                 marker: { break marker; trace=999; }
                 while(true) { trace=trace*10+i; break; }
                 if(i===1) { break done; }
                 if(i===2) { continue; }
                 if(i===3) { continue repeat; }
                 break;
             }
             trace=trace*10+4;
         }
         trace";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(14234.0));

    let source = "let n=0;
         block: { skip: if(true) { break block; } n=999; }
         loop_: do { skip: if(true) { break loop_; } n=999; } while(true);
         if(true) { skip: if(true) { break skip; } n+=42; }
         n";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(42.0));
}

#[test]
fn all_aliases_release_at_the_final_arm_and_arm_local_labels_release_earlier() {
    let source = "let n=0;
         a:b:if(false) { sibling: if(false) {} }
         else if(false) { sibling: { break sibling; } }
         else { sibling: while(false) {} n++; break b; }
         b:a:if(true) { n++; break a; }
         a:b:if(false) {} else if(true) { n++; break b; }
         a:{ b:do { n+=39; break a; } while(true); }
         n";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(42.0));
}

fn last_span(source: &str, name: &str) -> Span {
    let start = source.rfind(name).unwrap();
    Span {
        start,
        end: start + name.len(),
    }
}

#[test]
fn invalid_targets_and_cross_kind_duplicates_point_at_the_name() {
    for (source, name, message) in [
        ("done:done:if(false){}42", "done", "duplicate active label"),
        (
            "block:{{fresh:block:if(false){}}}42",
            "block",
            "duplicate active label",
        ),
        (
            "loop_:while(false){loop_:if(false){}}42",
            "loop_",
            "duplicate active label",
        ),
        (
            "done:if(true){}else if(false){done:{}}42",
            "done",
            "duplicate active label",
        ),
        (
            "done:alias:if(true){}else if(false){}else{alias:do{}while(false);}42",
            "alias",
            "duplicate active label",
        ),
        (
            "done:if(false){done:if(false){}}42",
            "done",
            "duplicate active label",
        ),
        (
            "done:if(false){continue done;}42",
            "done",
            "continue target must label a loop",
        ),
        (
            "while(false){done:alias:if(false){}else if(false){{continue alias;}}}42",
            "alias",
            "continue target must label a loop",
        ),
        (
            "done:if(true){}else{break done;continue done;}42",
            "done",
            "continue target must label a loop",
        ),
        (
            "done:alias:if(true){break done;}break alias;42",
            "alias",
            "jump target is not an active label",
        ),
        (
            "done:if(true){}else if(false){}break done;42",
            "done",
            "jump target is not an active label",
        ),
        (
            "done:alias:if(true){}else{}break alias;42",
            "alias",
            "jump target is not an active label",
        ),
        (
            "if(true){done:if(false){}}else{break done;}42",
            "done",
            "jump target is not an active label",
        ),
        (
            "done:if(true){break;}42",
            "break",
            "break and continue require an enclosing loop",
        ),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed, "{source}: {error}");
        assert_eq!(error.message(), message, "{source}");
        assert_eq!(error.span(), last_span(source, name), "{source}");
    }
}

#[test]
fn semicolons_and_static_scope_checks_still_apply() {
    use ErrorKind::Unsupported;
    for (source, kind, at) in [
        ("done:if(true){break done}42", Unsupported, "}"),
        (
            "done:if(true){}else{break/*\n*/done;}42",
            Unsupported,
            "done",
        ),
        (
            "done:if(true){break done;let hidden=0;}else if(hidden){}42",
            Unsupported,
            "hidden",
        ),
        (
            "done:if(true){}else{break done;let local=local;}42",
            Unsupported,
            "local",
        ),
        ("labelOnly:if(false){}labelOnly", Unsupported, "labelOnly"),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), kind, "{source}: {error}");
        assert_eq!(error.span(), last_span(source, at), "{source}");
    }
    let source = "done:if (true) {break/**/done\n; }42";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(42.0));
}

#[test]
fn aliases_add_no_frames_and_else_if_still_uses_the_shared_frame_cap() {
    let aliases = (0..140).map(|i| format!("l{i}:")).collect::<String>();
    let head = format!(
        "while(true){{{{{aliases}if(false){{}}{}else ",
        "else if(false){}".repeat(125)
    );
    let tail = "}break;}42";
    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    let source = format!("{head}{{break l0;}}{tail}");
    assert_eq!(
        parse_script_with_limits(&source, limits)
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
    for (arm, at) in [
        ("if(true){}", "if"),
        ("{overflow:alias:if(true){}}", "overflow"),
    ] {
        let source = format!("{head}{arm}{tail}");
        let error = parse_script_with_limits(&source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
        assert_eq!(error.span(), last_span(&source, at));
    }
}

#[test]
fn if_prefix_lookup_emission_and_each_final_break_fixup_are_charged() {
    let source = "aa:bb:if(true){break aa;}else if(false){break bb;}else{break aa;}true";
    // Beyond scanned bytes: 14 for existing chain work, 4 to check bb,
    // 15 for the three target searches, 3 break emissions, 3 break fixups,
    // and 1 final Boolean emission. There is no per-arm label validation.
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 30,
        max_nesting_depth: 1,
        max_work: source.len() + 40,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 10);
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
    for (smaller, limit) in [
        (
            Limits {
                max_source_bytes: source.len() - 1,
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
        let error = parse_script_with_limits(source, smaller).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        let span = match limit {
            Limit::SourceBytes => Span {
                start: source.len() - 1,
                end: source.len(),
            },
            Limit::Tokens => {
                let start = source.len() - "true".len();
                Span {
                    start,
                    end: start + 1,
                }
            }
            _ => last_span(source, "true"),
        };
        assert_eq!(error.span(), span);
    }
    // Leave enough work for bytes through the last brace and both ordinary
    // if join fixups, but none for its pending breaks. Each costs one at `}`.
    let before_break_fixups = limits.max_work - "true".len() - 1 - 3;
    for units in 0..=3 {
        let error = parse_script_with_limits(
            source,
            Limits {
                max_work: before_break_fixups + units,
                ..limits
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
        let start = source.len() - "true".len() - usize::from(units < 3);
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
fn if_breaks_count_toward_prepared_and_reached_dispatch_ceilings() {
    let limits = Limits {
        max_tokens: 4200,
        ..Limits::default()
    };
    for (extra, expected) in [
        (0, Ok(Value::Boolean(true))),
        (1, Err(EvalError::LimitExceeded)),
    ] {
        let source = format!(
            "done:if(true){{break done;}}else if({}false){{break done;}}else{{break done;}}true",
            "!".repeat(4086 + extra)
        );
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 4096 + extra);
        assert_eq!(script.evaluate_value(), expected);
    }
    // Each iteration dispatches 2 for the while test, 2 for the first if
    // test, 4 for decrement, 1 for break, and 1 for the while back edge.
    // Initialization, the final while test and result load cost 5 more.
    for (prefixes, expected) in [(9, Ok(0.0)), (10, Err(EvalError::DispatchLimitExceeded))] {
        let source = format!(
            "let n=1637;while(n){{done:if(true){{n--;break done;}}else if(true){{}}}}{}n",
            "+ ".repeat(prefixes)
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), expected);
    }
}
