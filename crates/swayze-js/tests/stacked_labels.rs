use swayze_js::{ErrorKind, Limit, Limits, Span, parse_script, parse_script_with_limits};

#[test]
fn every_block_alias_breaks_past_nested_statements() {
    for target in ["out", "middle", "last"] {
        let source = format!(
            "let n=0;
             out: middle: last: {{
                 let step=4;
                 {{ for(let i=0;i<1;n=999) {{
                     do {{
                         if(false) {{ n=999; }} else if(true) {{ n+=step; break {target}; }}
                         n=999;
                     }} while(true);
                 }} n=999; }}
                 n=999;
             }}
             n+=38;
             n"
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.max_stack_depth(), 2);
        assert_eq!(script.evaluate(), Ok(42.0));
    }
}

#[test]
fn every_loop_alias_breaks_and_continues_to_the_same_edges() {
    for (skip, exit) in [("outer", "alias"), ("alias", "outer")] {
        for (limit, result) in [(2, 12.0), (4, 123.0)] {
            let source = format!(
                "let n=0; let trace=0;
                 outer: alias: while(n<{limit}) {{
                     n++;
                     inner: step: {{ while(true) {{
                         trace=trace*10+n;
                         if(n<3) {{ continue {skip}; }}
                         break {exit};
                     }} trace=999; }}
                     trace=999;
                 }}
                 trace"
            );
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(result));
        }

        for (test, result) in [("i<3", 12.0), ("i<4", 123.0)] {
            let source = format!(
                "let trace=0;
                 outer: alias: for(let i=1;{test};i++) {{
                     {{ do {{
                         trace=trace*10+i;
                         if(i<3) {{ continue {skip}; }}
                         break {exit};
                     }} while(true); }}
                     trace=999;
                 }}
                 trace"
            );
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(result));
        }

        for (limit, result) in [(2, 2.0), (4, 3.0)] {
            let source = format!(
                "let n=0;
                 outer: alias: do {{
                     for(;;) {{ {{
                         n++;
                         if(n===3) {{ break {exit}; }}
                         if(n===1) {{ continue {skip}; }}
                         continue {exit};
                     }} }}
                 }} while(n<{limit});
                 n"
            );
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(result));
        }
    }
}

#[test]
fn aliases_share_neither_bindings_nor_lifetimes_with_other_statements() {
    let source = "let name=0;
         if(false) { name: reuse: { break name; } }
         else if(false) { reuse: name: while(false) { continue reuse; } }
         else { reuse: name: do { break name; } while(false); }
         name: reuse: {
             Name: other: { while(true) { break; } break Name; }
             name=42;
             break reuse;
             name=999;
         }
         reuse: name: { let name=0; break name; }
         name";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(42.0));
}

fn span(start: usize, len: usize) -> Span {
    Span {
        start,
        end: start + len,
    }
}

fn last_span(source: &str, needle: &str) -> Span {
    span(source.rfind(needle).unwrap(), needle.len())
}

#[test]
fn duplicate_diagnostics_point_at_the_second_name_not_its_colon() {
    for (source, duplicate) in [
        ("same:same:{}42", "same"),
        ("outer:alias:last:alias:while(false){}42", "alias"),
        (
            "outer:alias:{if(false){alias:fresh:do{}while(false);}}42",
            "alias",
        ),
        ("outer:alias:while(false){{fresh:outer:{}}}42", "outer"),
        ("outer:alias:for(;;){break alias;inner:alias:{}}42", "alias"),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Malformed, "{source}: {error}");
        assert_eq!(error.message(), "duplicate active label");
        assert_eq!(error.span(), last_span(source, duplicate));
    }
}

#[test]
fn prefix_lookahead_keeps_trivia_ternaries_and_stores_distinct() {
    let source = "let x=1;
         x/**/+=1;
         x/**/++;
         x?x:0;
         x/*\n*/: // before the alias
         y/**/: for(x?x:0;x?false:x;x?x:0) { x?x:0; }
         $done\n: _alias: { break/**/_alias/*\n*/; }
         x/**/?x:0;";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(3.0));
}

#[test]
fn stacked_prefixes_do_not_expand_statement_or_jump_syntax() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, needle) in [
        ("a:b:/**/", Malformed, ""),
        ("{a:b:}42", Malformed, "}"),
        ("a:b:c", Unsupported, "c"),
        ("a:b:c=1;42", Unsupported, "c"),
        ("a:b:c?1:2", Unsupported, "c"),
        ("a:b:;42", Unsupported, ";"),
        ("a:b:let x=42;x", Unsupported, "let"),
        ("a:b:if(true){}42", Unsupported, "if"),
        ("a:b\u{e9}:{}42", Unsupported, "\u{e9}"),
        ("a:b\\u0030:{}42", Unsupported, "\\"),
        ("a:eval:{}42", Unsupported, "eval"),
        ("a:b:while(false) break b;42", Malformed, "break"),
        ("a:b:do{}while(false)\n42", Unsupported, "42"),
        ("a:b:{break/*\n*/b;}42", Unsupported, "b"),
        ("a:b:while(false){continue\n b;}42", Unsupported, "b"),
        ("a:b:{break b}42", Unsupported, "}"),
        ("a:b:while(false){continue b}42", Unsupported, "}"),
        ("outer:while(false){a:b:{continue b;}}42", Malformed, "b"),
        ("a:b:{break;}42", Malformed, "break"),
        ("a:b:{}break a;42", Malformed, "a"),
        ("a:b:{c:d:{}break d;}42", Malformed, "d"),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), kind, "{source}: {error}");
        assert_eq!(error.span(), last_span(source, needle), "{source}");
    }
}

#[test]
fn many_aliases_use_one_frame_and_no_expression_depth() {
    let prefix = (0..140).map(|i| format!("l{i}:")).collect::<String>();
    let limits = Limits {
        max_nesting_depth: 0,
        ..Limits::default()
    };
    let braces = ("{".repeat(127), "}".repeat(127));
    let source = format!("{}{}{{break l0;}}{}42", braces.0, prefix, braces.1);
    assert_eq!(
        parse_script_with_limits(&source, limits)
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
    let source = format!(
        "{}{}{{overflow:alias:{{}}}}{}42",
        braces.0, prefix, braces.1
    );
    let error = parse_script_with_limits(&source, limits).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(error.span(), last_span(&source, "overflow"));
}

#[test]
fn stacked_label_accounting_pins_prefix_visits_and_target_searches() {
    let source = "a0:b0:{while(false){{c0:d:{break a0;}}}}0";
    // Beyond scanned bytes: 4 frames, 1 group, 5 emissions, 1 fixup, 1
    // decimal byte. Check b0: 1 validation + 1 visit + 2 bytes. Check c0:
    // 1 loop + 2 visits + 4 bytes. Check d: 1 validation + 4 visits, no
    // same-length names. Find a0: 5 visits + 6 bytes. Skip the bare block.
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 24,
        max_work: source.len() + 39,
        ..Limits::default()
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 5);
    assert_eq!(script.evaluate(), Ok(0.0));
    for (smaller, limit) in [
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
        let error = parse_script_with_limits(source, smaller).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        assert_eq!(error.span(), last_span(source, "0"));
    }

    let outer_brace = source.find('{').unwrap();
    let inner_brace = source.find("d:{").unwrap() + 2;
    let jump = last_span(source, "a0");
    for (name, before, cost, after) in [
        (
            last_span(source, "b0"),
            outer_brace + 1,
            4,
            span(outer_brace, 1),
        ),
        (
            last_span(source, "c0"),
            inner_brace + 1 + 10,
            7,
            last_span(source, "d"),
        ),
        (
            last_span(source, "d"),
            inner_brace + 1 + 17,
            5,
            span(inner_brace, 1),
        ),
        (jump, jump.end + 23, 11, last_span(source, ";")),
    ] {
        for units in 0..=cost {
            let error = parse_script_with_limits(
                source,
                Limits {
                    max_work: before + units,
                    ..limits
                },
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
            assert_eq!(error.span(), if units < cost { name } else { after });
        }
    }
}
