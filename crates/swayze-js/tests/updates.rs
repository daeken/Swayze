use swayze_js::{
    Error, ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

const FORMS: [(&str, f64); 4] = [("++x", 1.0), ("x++", 1.0), ("--x", -1.0), ("x--", -1.0)];

fn assert_error_at(marked: &str, kind: ErrorKind) -> Error {
    let (prefix, rest) = marked.split_once('@').unwrap();
    let (token, suffix) = rest.split_once('@').unwrap();
    let source = format!("{prefix}{token}{suffix}");
    let error = parse_script(&source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source:?}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start: prefix.len(),
            end: prefix.len() + token.len(),
        },
        "{source:?}: {error}"
    );
    error
}

fn assert_error(marked: &str, kind: ErrorKind, message: &str) {
    let error = assert_error_at(marked, kind);
    assert!(error.message().contains(message), "{marked:?}: {error}");
}

#[test]
fn all_four_forms_work_in_statements_and_both_for_clauses() {
    for (form, step) in FORMS {
        let source = format!("let x=42;{form};x");
        let script = parse_script(&source).unwrap();
        assert_eq!(script.instruction_count(), 7);
        assert_eq!(script.max_stack_depth(), 2);
        assert_eq!(script.evaluate(), Ok(42.0 + step), "{source}");
        assert_eq!(script.evaluate(), Ok(42.0 + step), "{source}");

        let source = format!("let x=42;if(false){{{form};}}else{{{form};}}{form};x");
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(42.0 + 2.0 * step)
        );
        // The initializer runs once, continue runs the update, and break skips it.
        let source =
            format!("let x=42;let n=0;for({form};;{form}){{if(n){{break;}}n++;continue;}}x");
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(42.0 + 2.0 * step)
        );
        let source = format!("let x=42;for({form};false;{form}){{{form};}}x");
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(42.0 + step));
    }
}

#[test]
fn statement_sequences_store_before_the_suffix_in_root_and_braced_scopes() {
    for (leading, step) in FORMS {
        for (following, offset) in [("x++", 0.0), ("++x", 1.0)] {
            let sequence = format!("{leading},y=x,z={following},x+=y;");
            for statement in [sequence.clone(), format!("{{{sequence}}}")] {
                let source = format!("let x=3,y=0,z=0;{statement}x*100+y*10+z");
                let first = 3.0 + step;
                let expected = (2.0 * first + 1.0) * 100.0 + first * 10.0 + first + offset;
                let script = parse_script(&source).unwrap();
                assert_eq!(script.evaluate(), Ok(expected), "{source}");
                assert_eq!(script.evaluate(), Ok(expected), "{source}");
            }
        }
    }
    for (source, expected) in [
        ("let x=0,y=0; x++, y += x; y", 1.0),
        ("let x=20,y; {let x=2; --x,y=x++,++x;} y*100+x", 120.0),
        ("let x=0,y=0; if(false){++x,y=x;} x*10+y", 0.0),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn statement_update_sequences_require_a_nonempty_suffix_and_explicit_semicolon() {
    use ErrorKind::{Malformed, Unsupported};
    for source in [
        "let x=0; ++x,/*💡*/@;@ x",
        "let x=0; x--,/*💡*/@,@ x; x",
        "let x=0; {x++,0,/*💡*/@;@} x",
    ] {
        assert_error(source, Malformed, "expected an operand");
    }
    assert_error(
        "let x=0; x++,/*💡*/@@",
        Malformed,
        "expected an operand before end of source",
    );
    assert_error(
        "let x=0; {--x,/*💡*/@}@ x",
        Unsupported,
        "braces are not expression tokens",
    );
    for source in [
        "let x=0; ++x,x/*💡*/@@",
        "let x=0; x++,null/*💡*/@@",
        "let x=0; {++x,x/*💡*/@}@ x",
        "let x=0; {x--,null/*💡*/@}@ x",
        "let x=0; x++,null/*💡\u{2028}*/@x@",
    ] {
        assert_eq!(
            assert_error_at(source, Unsupported).message(),
            "increments and decrements require explicit semicolons",
            "{source}",
        );
    }
    for source in ["let x=0; x++,x;/*💡*/@@", "let x=0; {--x,null;}/*💡*/@@"] {
        assert_error(source, Unsupported, "a final expression is required");
    }
}

#[test]
fn statement_update_sequences_check_all_targets_statically() {
    use ErrorKind::Unsupported;
    for (source, message) in [
        ("if(false){++/*💡*/@absent@,0;} 0", "unbound name"),
        ("@undefined@--,0;0", "unbound name"),
        ("const x=0; @--@x,missing; x", "assignment to const"),
        (
            "if(false){const x=0; x/*💡*/@++@,0;}0",
            "assignment to const",
        ),
        ("let x=0; { @x@++,0; let x; } x", "initialization"),
        ("let x=0; if(false){++x,@y@=2;let y;} x", "initialization"),
        ("let x=0; if(false){x--,@missing@=2;}x", "unbound name"),
        (
            "let x=0; {const y=0; --x,false && y@++@;} x",
            "assignment to const",
        ),
    ] {
        assert_error(source, Unsupported, message);
    }
}

#[test]
fn statement_leading_updates_keep_their_unsupported_boundary_spans() {
    use ErrorKind::Unsupported;
    for (source, message) in [
        ("let x=0; x@++@(); x", "outside the subset"),
        ("let x=0; { @++@x !1; } x", "outside the subset"),
        ("let x=0; @++@x ~1; x", "outside the subset"),
        ("let x=0; { x@++@ void 1; } x", "outside the subset"),
        ("let x=0; x++/*💡\n*/@(@); x", "explicit semicolons"),
        ("let x=0; @--@(x), x; x", "identifier"),
        ("let x=0; { (x)@--@, x; } x", "identifier"),
        ("let x=0; ++x@.@p, x; x", "token"),
        ("let x=0; { x@[@0]++, x; } x", "token"),
        ("let x=0; x@++@++, x; x", "outside the subset"),
    ] {
        assert_error(source, Unsupported, message);
    }
    // Grouping the leading update with the operator or putting it after
    // the first comma still uses ordinary value-producing expression rules.
    for source in [
        "let x=0; (x++ + 1)",
        "let x=0,y; 0,y=x++ + 1; y",
        "let x=-1,y; ++x,y=++x ** 2; y",
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(1.0),
            "{source}"
        );
    }
}

#[test]
fn for_sequences_run_after_leading_updates_in_order_and_follow_loop_jumps() {
    for init in ["++x", "x++"] {
        for update in ["--x", "x--"] {
            let source = format!(
                "let x=1,y,n=0;
                 for({init},y=x; n<2; {update},y=y*10+x,++n){{
                     if(n){{break;}}continue;
                 }}
                 x*1000+y*10+n"
            );
            // Init sets x and y to 2. Continue runs all three updater operands
            // (x=1, y=21, n=1); break skips them all on the next iteration.
            let script = parse_script(&source).unwrap();
            assert_eq!(script.evaluate(), Ok(1211.0), "{source}");
            assert_eq!(script.evaluate(), Ok(1211.0), "{source}");
        }
    }
    for (source, expected) in [
        ("let x=0,y;for(++x,y=0;y<2;x++,++y){}x*10+y", 32.0),
        ("let x=1,y;for(x++,y=x++ ** 2;false;){}x*10+y", 34.0),
        ("let x=0,y;for(let i=x++,end=2;i<end;++i,y=i){}x*10+y", 12.0),
        ("let x=0;for(++\nx\n, x/**/++\n;false;){}x", 2.0),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn postfix_comma_updater_respects_comment_line_breaks_and_loop_jumps() {
    for (jump, expected) in [("continue", 12.0), ("break", 0.0)] {
        let source = format!("let i=0,seen=0;for(;i<1;i/*💡*/++,seen=i+1){{{jump};}}i*10+seen");
        // Continue runs the postfix store before the suffix reads i; break
        // leaves both bindings untouched.
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
        assert_error(
            &source.replace("/*💡*/++", "/*💡\n*/@++@"),
            ErrorKind::Unsupported,
            "a line terminator cannot precede a postfix update",
        );
    }
}

#[test]
fn for_update_sequence_delimiters_and_targets_keep_their_diagnostics() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, message) in [
        (
            "let x;for(++x,@;@false;){}x",
            Malformed,
            "expected an operand",
        ),
        (
            "let x;for(;false;x--,@)@{}x",
            Malformed,
            "expected an operand",
        ),
        (
            "let x;for(x++,/*💡*/@@",
            Malformed,
            "expected an operand before end of source",
        ),
        (
            "let x;for(;false;--x,/*💡*/@@",
            Malformed,
            "expected an operand before end of source",
        ),
        (
            "let x;for(--x,@,@null;false;){}x",
            Malformed,
            "expected an operand",
        ),
        ("let x;for(x--,null@)@{}x", Malformed, "between for clauses"),
        (
            "let x;for(;false;++x,null@;@){}x",
            Malformed,
            "after the for update",
        ),
        (
            "let x;for(++x @in@ x){}x",
            Unsupported,
            "outside the subset",
        ),
        (
            "let x;for(x++ @of@ x){}x",
            Unsupported,
            "outside the subset",
        ),
        (
            "let x;for(x++,x @in@ x){}x",
            Unsupported,
            "outside the subset",
        ),
        (
            "let x;for(++x,x @of@ x){}x",
            Unsupported,
            "outside the subset",
        ),
        (
            "let x;for(@++@(x),null;false;){}x",
            Unsupported,
            "identifier",
        ),
        (
            "let x;for(;false;(x)@--@,null){}x",
            Unsupported,
            "identifier",
        ),
        ("let x;for(++x@.@p,null;false;){}x", Unsupported, "token"),
        (
            "const x=0;for(;false;x@++@,missing){}x",
            Unsupported,
            "assignment to const",
        ),
        (
            "let x;for(;false;++x,@missing@=2){let missing;}x",
            Unsupported,
            "unbound name",
        ),
        (
            "let x;for(let i=x++, @null@;false;){}x",
            Unsupported,
            "binding name",
        ),
    ] {
        assert_error(source, kind, message);
    }
}

#[test]
fn nearest_bindings_are_updated_and_body_bindings_are_fresh() {
    let source = "let x=0;let trace=0;
        for(++x;x<4;x++){let x=7;++x;trace=trace*10+x;continue;}
        trace*10+x";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(8884.0));
    assert_eq!(
        parse_script("let x=1; if(true){let undefined=true; undefined++; x+=undefined;}x")
            .unwrap()
            .evaluate(),
        Ok(3.0)
    );
}

#[test]
fn numeric_updates_use_number_coercion_and_binary64_rounding() {
    for (source, expected) in [
        ("let x=true; ++x; x", 2.0),
        ("let x=false; x--; x", -1.0),
        ("let x=null; x++; x", 1.0),
        ("let x=-0; ++x; x", 1.0),
        ("let x=-0; --x; x", -1.0),
        ("let x=1; x--; x", 0.0),
        ("let x=-1; x++; x", 0.0),
        ("let x=1e309; --x; x", f64::INFINITY),
        ("let x=-1e309; ++x; x", f64::NEG_INFINITY),
        ("let x=9007199254740991; ++x; x", 9007199254740992.0),
        ("let x=9007199254740992; x++; x", 9007199254740992.0),
        ("let x=9007199254740992; x--; x", 9007199254740991.0),
        ("let x=9007199254740994; ++x; x", 9007199254740996.0),
        ("let x=9007199254740994; --x; x", 9007199254740992.0),
        ("let x=-9007199254740992; --x; x", -9007199254740992.0),
    ] {
        let Value::Number(actual) = parse_script(source).unwrap().evaluate_value().unwrap() else {
            panic!("expected a Number for {source}");
        };
        assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
    }
    for (form, _) in FORMS {
        for value in ["undefined", "0/0"] {
            let source = format!("let x={value};{form};x");
            assert!(parse_script(&source).unwrap().evaluate().unwrap().is_nan());
        }
    }
}

#[test]
fn resolves_only_initialized_let_targets_after_syntax_in_source_order() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, message) in [
        ("++@absent@;later", "unbound name"),
        ("@undefined@--;0", "unbound name"),
        ("while(false){@local@++;}0", "unbound name"),
        ("for(;false;--@x@){let x=0;}0", "unbound name"),
        ("const x=0;x@++@;0", "assignment to const"),
        ("const x=0;@--@x;0", "assignment to const"),
        ("const x=0;for(x@--@;false;){}0", "assignment to const"),
        (
            "let x=0;if(false){const x=0;@++@x;}0",
            "assignment to const",
        ),
        ("let x=0;if(false){++@x@;const x=0;}0", "initialization"),
        (
            "let x=0;if(false){for(;false;@x@++){}let x=1;}0",
            "initialization",
        ),
        ("for(;;){break;@x@--;let x=1;}0", "initialization"),
        ("if(false){let x=0;x++;}--@x@;0", "unbound name"),
    ] {
        assert_error(source, Unsupported, message);
    }
    assert_error("missing++;let x=@;@0", Malformed, "operand");
    assert_error("const x=0;++x;0;extra + @@", Malformed, "operand");
    assert_error("const x=0;x++;@@", Unsupported, "final expression");
}

#[test]
fn respects_ecmascript_line_terminators_without_asi() {
    use ErrorKind::Unsupported;
    for newline in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        for trivia in [
            newline.to_owned(),
            format!("/*💡{newline}*/"),
            format!("//💡{newline}"),
        ] {
            for op in ["++", "--"] {
                for marked in [
                    format!("let x=0;x{trivia}@{op}@;x"),
                    format!("let x=0;if(true){{x{trivia}@{op}@;}}x"),
                    format!("let x=0;x{trivia}@{op}@,null;x"),
                    format!("let x=0;{{x{trivia}@{op}@,null;}}x"),
                    format!("let x=0;++x,x{trivia}@{op}@;x"),
                    format!("let x=0;for(x{trivia}@{op}@;false;){{}}x"),
                    format!("let x=0;for(;false;x{trivia}@{op}@){{}}x"),
                    format!("let x=0;for(x{trivia}@{op}@,null;false;){{}}x"),
                    format!("let x=0;for(;false;x{trivia}@{op}@,null){{}}x"),
                    format!("let x=0;for(x{trivia}@{op}@ + 0;false;){{}}x"),
                    format!("let x=0;for(;false;x{trivia}@{op}@ ? 0 : 1){{}}x"),
                    format!("let x=0;x{trivia}@{op}@x;x"),
                ] {
                    assert_error(&marked, Unsupported, "line terminator");
                }
            }
            let source = format!("let x=0;++{trivia}x;x++{trivia};x");
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(2.0));
            let source =
                format!("let x=0;++{trivia}x{trivia},null{trivia};{{x/*💡*/++{trivia},null;}}x");
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(2.0));
            let source = format!(
                "let x=0,y=0;for(x/**/++{trivia}+0;y<2;x--{trivia}?++{trivia}y:0){{}}x*10+y"
            );
            // Postfix may be followed by a line break before either continuation.
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(-18.0));
        }
    }
    let source = "let $x_1=0;if(false){}$x_1/*💡*/\u{00a0}++;--/**/$x_1;$x_1";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(0.0));
    assert_eq!(
        parse_script("let x=0;for(x/**/++;x<3;++/**/x){}x")
            .unwrap()
            .evaluate(),
        Ok(3.0)
    );
    assert_error("let x=0;++x/*💡*/@@", Unsupported, "explicit semicolons");
    assert_error(
        "let x=0;x++/*\u{2028}*/@x@",
        Unsupported,
        "explicit semicolons",
    );
    assert_error(
        "let x=0;if(true){x--@}@0",
        Unsupported,
        "explicit semicolons",
    );
    assert_error(
        "let x=0;for(x++@)@{}x",
        ErrorKind::Malformed,
        "between for clauses",
    );
    assert_error(
        "let x=0;for(;false;--x@;@){}x",
        ErrorKind::Malformed,
        "after the for update",
    );
}

#[test]
fn standalone_dispatch_still_requires_a_complete_update() {
    use ErrorKind::Unsupported;
    for (source, message) in [
        ("let x=0;for(x@++@=1;false;){}x", "expressions"),
        ("let x=0;for(;false;@--@x !1){}x", "expressions"),
        ("let x=0;x@++@=1;x", "expressions"),
        ("let x=0;x@--@=1;x", "expressions"),
        ("let x=0;@++@+x;x", "identifier"),
        ("let x=0;@--@(x);x", "identifier"),
        ("@++@true;0", "identifier"),
        ("@--@1;0", "identifier"),
        ("1@++@;0", "identifier"),
        ("let x=0;(x)@--@;x", "identifier"),
        ("let x=0;++x@.@p;x", "token"),
        ("let x=0;x@[@0]--;x", "token"),
        ("let x=0;@--@>x", "identifier"),
        ("1;++@x@;0", "unbound name"),
    ] {
        assert_error(source, Unsupported, message);
    }
    assert_eq!(
        parse_script("let x=0;x+=3;x-=1; x +/**/+x -/**/-x")
            .unwrap()
            .evaluate(),
        Ok(6.0)
    );
}

#[test]
fn token_and_work_budgets_include_trivia_and_only_one_target_lookup() {
    let source = "let x=false;x/*💡*/++;x";
    let last = Span {
        start: source.len() - 1,
        end: source.len(),
    };
    let emoji = source.find('💡').unwrap();
    // Source bytes, 7 emitted instructions, and one candidate + one byte per
    // lookup (one update target and one final read). No extra trivia scan.
    let limits = Limits {
        max_tokens: 9,
        max_work: source.len() + 11,
        max_nesting_depth: 0,
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(1.0)
    );
    for (limits, ceiling, span) in [
        (
            Limits {
                max_tokens: 8,
                ..limits
            },
            Limit::Tokens,
            last,
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            last,
        ),
        // Two declaration instructions have been emitted when the lexer gets here.
        (
            Limits {
                max_work: emoji + 2 + 3,
                ..limits
            },
            Limit::Work,
            Span {
                start: emoji,
                end: emoji + 4,
            },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(ceiling));
        assert_eq!(error.span(), span);
    }
}

#[test]
fn statement_update_suffix_shares_inclusive_parse_budgets() {
    for (leading, step) in FORMS {
        let source = format!("let x=false;{leading}/*💡*/,x=(x),x;x");
        let last = source.len() - 1;
        let comma = source.find(',').unwrap();
        let open = source.find('(').unwrap();
        let limits = Limits {
            max_source_bytes: source.len(),
            max_tokens: 17,
            max_nesting_depth: 2,
            // Twelve emissions, the suffix assignment's name charge, two
            // pending pushes, and two units per lookup at five access sites.
            max_work: source.len() + 25,
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 12, "{source}");
        assert_eq!(script.max_stack_depth(), 2, "{source}");
        assert_eq!(script.evaluate(), Ok(step), "{source}");
        for (limits, limit, start) in [
            (
                Limits {
                    max_source_bytes: last,
                    ..limits
                },
                Limit::SourceBytes,
                last,
            ),
            (
                Limits {
                    max_tokens: 16,
                    ..limits
                },
                Limit::Tokens,
                last,
            ),
            (
                Limits {
                    max_tokens: 7,
                    ..limits
                },
                Limit::Tokens,
                comma,
            ),
            (
                Limits {
                    max_nesting_depth: 1,
                    ..limits
                },
                Limit::NestingDepth,
                open,
            ),
            (
                Limits {
                    max_work: limits.max_work - 1,
                    ..limits
                },
                Limit::Work,
                last,
            ),
            // Lex through the comma once and emit two declaration and three
            // update instructions, leaving no work for the store at the comma.
            (
                Limits {
                    max_work: comma + 1 + 5,
                    ..limits
                },
                Limit::Work,
                comma,
            ),
        ] {
            let error = parse_script_with_limits(&source, limits).unwrap_err();
            assert_eq!(
                error.kind(),
                ErrorKind::LimitExceeded(limit),
                "{source}: {error}"
            );
            assert_eq!(
                error.span(),
                Span {
                    start,
                    end: start + 1
                },
                "{source}: {error}"
            );
        }
    }
}

#[test]
fn statement_update_suffix_shares_inclusive_prepared_and_dispatch_caps() {
    // Two setup instructions, four for the leading update, 2,043 pairs
    // of literal/discard, three or four for the final suffix operand and its
    // discard, then the final load.
    for (operand, count, expected) in [
        ("!false", 4096, Ok(1.0)),
        ("!!false", 4097, Err(EvalError::LimitExceeded)),
    ] {
        let source = format!("let x=0;x++,{}{operand};x", "false,".repeat(2043));
        let script = parse_script_with_limits(
            &source,
            Limits {
                max_tokens: 8192,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(script.instruction_count(), count);
        assert_eq!(script.max_stack_depth(), 2);
        assert_eq!(script.evaluate(), expected);
    }

    // Two setup dispatches, 11 per taken iteration with the suffix or nine
    // without it, four for the last test, and a final load. Padding reaches
    // 16,384 exactly in each case.
    for (statement, iterations, padding) in [("++i,null;", 1488, 9), ("i++;", 1819, 6)] {
        for extra in [0, 1] {
            let source = format!(
                "let i=0;while(i<{iterations}){{{statement}}}{}i",
                "+ ".repeat(padding + extra)
            );
            let expected = if extra == 0 {
                Ok(f64::from(iterations))
            } else {
                Err(EvalError::DispatchLimitExceeded)
            };
            assert_eq!(
                parse_script(&source).unwrap().evaluate(),
                expected,
                "{source}"
            );
        }
    }
}

#[test]
fn skipped_updates_count_toward_exact_prepared_instruction_ceiling() {
    // Two instructions for the declaration, two for the test, four per
    // skipped update, plus one final load and the unary operators.
    for (prefixes, expected) in [(3, Ok(0.0)), (4, Err(EvalError::LimitExceeded))] {
        let source = format!(
            "let x=false;if(false){{{}}}{}x",
            "++x;".repeat(1022),
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.instruction_count(), 4093 + prefixes);
        assert_eq!(script.max_stack_depth(), 2);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn for_updates_count_toward_exact_dispatch_ceiling() {
    // 6 setup dispatches, 11 per continuing iteration (including 4 for the
    // update), 4 for the final test, then 1 final load and the unary operators.
    for (prefixes, expected) in [
        (4, Ok(1489.0)),
        (5, Ok(1489.0)),
        (6, Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!(
            "let i=0;for(i++;i<1489;++i){{continue;}}{}i",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.instruction_count(), 19 + prefixes);
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
    let script = parse_script("let i=9007199254740992;for(;;i++){continue;}i").unwrap();
    assert_eq!(script.evaluate(), Err(EvalError::DispatchLimitExceeded));
}

#[test]
fn grouped_updates_yield_and_store_numbers_without_losing_old_number_bits() {
    for (init, old, incremented, decremented) in [
        ("=true", 1.0, 2.0, 0.0),
        ("=false", 0.0, 1.0, -1.0),
        ("=null", 0.0, 1.0, -1.0),
        ("", f64::NAN, f64::NAN, f64::NAN),
        ("=0/0", f64::NAN, f64::NAN, f64::NAN),
        ("=0", 0.0, 1.0, -1.0),
        ("=-0", -0.0, 1.0, -1.0),
        ("=-1", -1.0, 0.0, -2.0),
        ("=1e309", f64::INFINITY, f64::INFINITY, f64::INFINITY),
        (
            "=-1e309",
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ),
        (
            "=9007199254740992",
            9007199254740992.0,
            9007199254740992.0,
            9007199254740991.0,
        ),
    ] {
        for (form, new, result) in [
            ("++x", incremented, incremented),
            ("--x", decremented, decremented),
            ("x++", incremented, old),
            ("x--", decremented, old),
        ] {
            for (tail, expected) in [("", result), ("; x", new)] {
                let source = format!("let x{init}; ({form}){tail}");
                let Value::Number(actual) =
                    parse_script(&source).unwrap().evaluate_value().unwrap()
                else {
                    panic!("expected a Number: {source}");
                };
                if expected.is_nan() {
                    assert!(actual.is_nan(), "{source}");
                } else {
                    assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
                }
            }
        }
    }
}

#[test]
fn postfix_preserves_the_old_coerced_number_and_stores_the_new_number() {
    for source in [
        "let x = -0; const old = (x++); 1 / old === -1 / 0 && x === 1",
        "let x = true; const old = (x++); old === 1 && x === 2",
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(
            script.evaluate_value(),
            Ok(Value::Boolean(true)),
            "{source}"
        );
    }
}

#[test]
fn skipped_grouped_updates_leave_a_valid_let_unchanged() {
    let source = "let x = true; false && (x++); true ? x : (--x)";
    let script = parse_script(source).unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
}

#[test]
fn expression_updates_run_once_in_order_with_expressions_and_control() {
    for (source, expected) in [
        ("let x=1; (x++ + 1)*10 + x", 22.0),
        ("let x=1; 1 + ++x", 3.0),
        ("let x=1; (x++*1000 + ++x*100 + x--*10 + x)", 1332.0),
        ("let x=2; x + x+++1 + x---1 + x", 9.0),
        ("let x=4; (x-=x++)*10+x", 0.0),
        ("let x=4; x-=x++; x", 0.0),
        ("let x=0, y=++x, z=x++; x*100+y*10+z", 211.0),
        ("let x=0, y; y = x++; y*10+x", 1.0),
        ("let x=0, y=1; (y&&=++x)*10+x", 11.0),
        ("let x=0, y=1; (y||=++x)*10+x", 10.0),
        ("let x=0; 0, x++, ++x, x", 2.0),
        ("let x=0; false ? x++ : --x", -1.0),
        ("let x=0; (x++ || ++x)", 2.0),
        ("let x=0; undefined ?? ++x", 1.0),
        (
            "let x=0; false && ++x; true || x++; 0 ?? --x; true ? x : x--",
            0.0,
        ),
        ("let x=0; !++x; void x++; x", 2.0),
        (
            "let x=0; if (x++) { x = 42; } else if (++x) { 1 + x++; } x",
            3.0,
        ),
        ("let x=3; while (--x) {} do {} while (x--); x", -1.0),
        (
            "let x=0,s=0; for(0, ++x; x++<8; 0, ++x) { s+=x; continue; } s*100+x",
            2010.0,
        ),
        (
            "let x=1; for(let x=3; x<4; 0, x++) { let x=20; 0, x--; } x",
            1.0,
        ),
        ("const x=2; let y; { let x=3; (y=x++*10+x); } y*10+x", 342.0),
        (
            "let y; { let undefined=true; y=undefined++*10+undefined; } y",
            12.0,
        ),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
    }
}

#[test]
fn expression_updates_require_one_bare_name_and_do_not_relax_assignment_grammar() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, message) in [
        ("let x=0;1 + (x)@++@", "single identifier"),
        ("let x=0;let y=@++@(x);y", "single identifier"),
        ("let x=0;let y=@++@--x;y", "single identifier"),
        ("let x=0;let y=++x@++@;y", "single identifier"),
        ("let x=0;let y=x++@--@;y", "single identifier"),
        ("let x=0;let y=++x@.@p;y", "token"),
        ("let x=0;let y=x@[@0]++;y", "token"),
        ("let x=0;let y=--x@(@);y", "call expressions"),
        ("(null@++@)", "single identifier"),
        ("let x=0;let y=x++@=@2;y", "assignment expressions"),
        ("let x=0;let y=++x@+=@2;y", "assignment expressions"),
        ("let x=0;if(0,x++@=@1){}0", "assignment expressions"),
        ("let x=0;(0, ++x@=@1)", "assignment expressions"),
    ] {
        assert_error(source, Unsupported, message);
    }
    for source in ["let x=0;(x++/*💡*/@@", "let x=0;(--x@@"] {
        assert_error(source, Malformed, "expected a closing parenthesis");
    }
    assert_error("let x=0;let y=x++@@", Unsupported, "explicit semicolon");
}

#[test]
fn expression_update_targets_are_validated_even_when_not_evaluated() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, message) in [
        ("const x=0; false && x/*💡*/@++@", "assignment to const"),
        (
            "const x=0; if(true){}else if(@--@x){}0",
            "assignment to const",
        ),
        ("for(const x=0;false;0, x@--@){}0", "assignment to const"),
        (
            "let x=0; { true || ++/*💡*/@x@; const x=1; } x",
            "initialization",
        ),
        ("let x = false ? @x@++ : 0; x", "initialization"),
        ("let y=--@x@,x=1; y", "initialization"),
        ("true || @undefined@++", "unbound name"),
        ("false && ++/*💡*/@absent@; later", "unbound name"),
        ("for(;false;0, @x@--){let x=0;}0", "unbound name"),
        ("do{break;}while(@x@++);let x=0;0", "initialization"),
        ("const y=0; (y@=@absent++)", "assignment to const"),
        ("let y=0; false && (y&&=++@absent@)", "unbound name"),
    ] {
        assert_error(source, Unsupported, message);
    }
    assert_error("false && missing++; let x=@;@0", Malformed, "operand");
}

#[test]
fn only_postfix_forbids_a_line_terminator_before_its_operator() {
    for newline in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        for trivia in [
            newline.to_owned(),
            format!("/*💡{newline}*/"),
            format!("//💡{newline}"),
        ] {
            for op in ["++", "--"] {
                for source in [
                    format!("let x=1; (x{trivia}@{op}@)"),
                    format!("let x=1; let y=x{trivia}@{op}@x;y"),
                    format!("let x=1; if (x{trivia}@{op}@){{}}x"),
                ] {
                    assert_error(&source, ErrorKind::Unsupported, "line terminator");
                }
            }
            let source =
                format!("let x=1; 10 * ++{trivia}x{trivia} + x/*💡*/\u{00a0}--{trivia} + x");
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(23.0));
        }
    }
}

#[test]
fn a_line_break_after_the_update_does_not_prevent_a_continuation() {
    use ErrorKind::Unsupported;
    for source in [
        "let x = 2; (x/*\n*/@++@ ** 2) + x",
        "let x = 2; x/*\n*/@++@ + 1; x",
    ] {
        assert_error(
            source,
            Unsupported,
            "a line terminator cannot precede a postfix update",
        );
    }
    for (source, expected) in [
        ("let x=2; x++\n+1", 3.0),
        ("let x=2; ++x\n**2", 9.0),
        ("let x = 2; x++/*\n*/ + 1; x", 3.0),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
    for newline in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        for trivia in [format!("/*💡{newline}*/"), format!("//💡{newline}")] {
            let source = format!("let x=2; --{trivia}x{trivia}? x : 0");
            assert_eq!(
                parse_script(&source).unwrap().evaluate(),
                Ok(1.0),
                "{source}"
            );
            let source = format!("let x=2; x{trivia}--{trivia}**2");
            assert_error(
                &source.replacen("--", "@--@", 1),
                Unsupported,
                "line terminator",
            );
            let source = format!("let x=2; x--{trivia}**2");
            assert_eq!(
                parse_script(&source).unwrap().evaluate(),
                Ok(4.0),
                "{source}"
            );
        }
    }
}

#[test]
fn leading_update_expressions_obey_precedence_and_keep_primitive_results() {
    for (source, expected) in [
        ("let x=2; x+++1", Value::Number(3.0)),
        ("let x=2; x---1;", Value::Number(1.0)),
        ("let x=2; --x + 2 * x", Value::Number(3.0)),
        ("let x=2; x++ ** 2", Value::Number(4.0)),
        ("let x=1; ++x ** 2 ** 3", Value::Number(256.0)),
        ("let x=2; x-- ** -1 + x", Value::Number(1.5)),
        ("let x=true; x++ === 1 && x === 2", Value::Boolean(true)),
        ("let x=0; x++ ? void --x : null", Value::Null),
        ("let x=0; ++x ? void x : null", Value::Undefined),
        ("let x=2; x++ + 1, x += 3", Value::Number(6.0)),
        ("let x=2; x++ + 1, null;", Value::Null),
        ("let x=0; x++ && --x, x", Value::Number(1.0)),
        ("let x=3; x-- ?? ++x, x", Value::Number(2.0)),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
        assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
    }
    for (source, expected) in [
        ("let x=-0; x++ && 1", -0.0_f64),
        ("let x=-0; x-- / 2", -0.0),
        ("let x=1; --x / 2", 0.0),
    ] {
        let actual = parse_script(source).unwrap().evaluate().unwrap();
        assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
    }
}

#[test]
fn leading_update_expressions_have_ordinary_completion_and_error_rules() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, expected) in [
        ("let x=2; x++ + 100; x", 3.0),
        ("let x=2; { ++x ** 2, x+=4; } x", 7.0),
        ("let x=2; if(false){x++ + 1;} x", 2.0),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
    for (source, kind, message) in [
        ("let x=0; x++ + /*💡*/@@", Malformed, "expected an operand"),
        (
            "let x=0; ++x ? null @,@ 0",
            Malformed,
            "expected ':' before ','",
        ),
        ("let x=0; ++x ?? 1 @||@ 2", Malformed, "cannot be mixed"),
        (
            "let x=0; {x++ + 1/*💡*/@}@ x",
            Unsupported,
            "expression statements require explicit semicolons",
        ),
        (
            "let x=0; {x++ + 1;}/*💡*/@@",
            Unsupported,
            "a final expression is required",
        ),
        (
            "let x=0; ++x;/*💡*/@@",
            Unsupported,
            "a final expression is required",
        ),
        (
            "let x=0; x++ + 1; let y=0;/*💡*/@@",
            Unsupported,
            "a final expression is required",
        ),
        (
            "let x=0; {++x ** 2\n@let@ y=0;} x",
            Unsupported,
            "expression statements require explicit semicolons",
        ),
        (
            "let x=0; x++ + 1\n@x@",
            Unsupported,
            "expression statements require explicit semicolons",
        ),
        (
            "let x=0; x++ + x@=@2",
            Unsupported,
            "assignment expressions",
        ),
        ("@missing@++ + 1", Unsupported, "unbound name"),
        (
            "if(false){const x=0; @++@x ** 2;} 0",
            Unsupported,
            "assignment to const",
        ),
        (
            "let x=0; { @x@-- + 1; let x; } x",
            Unsupported,
            "initialization",
        ),
        ("missing++ + @;@ 0", Malformed, "expected an operand"),
    ] {
        assert_error(source, kind, message);
    }
}

#[test]
fn leading_update_continuation_is_read_and_charged_once() {
    for (form, expected) in [("x++", 1.0), ("--x", 0.0)] {
        let source = format!("let x=false;{form}/*💡*/+true");
        let name = source.rfind('x').unwrap();
        let plus = source.rfind('+').unwrap();
        let limits = Limits {
            max_source_bytes: source.len(),
            max_tokens: 9,
            max_nesting_depth: 0,
            // Source bytes, five emissions, one pending push, and one name lookup.
            max_work: source.len() + 8,
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 5, "{source}");
        assert_eq!(script.max_stack_depth(), 2, "{source}");
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
        for (limits, limit, start) in [
            (
                Limits {
                    max_tokens: 7,
                    ..limits
                },
                Limit::Tokens,
                plus,
            ),
            // The delimiter lookahead is read after the two declaration
            // emissions but before charging the expression update at its name.
            (
                Limits {
                    max_work: plus + 1 + 2,
                    ..limits
                },
                Limit::Work,
                name,
            ),
            (
                Limits {
                    max_work: plus + 1 + 3,
                    ..limits
                },
                Limit::Work,
                plus,
            ),
            (
                Limits {
                    max_work: limits.max_work - 1,
                    ..limits
                },
                Limit::Work,
                name,
            ),
        ] {
            let error = parse_script_with_limits(&source, limits).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{source}");
            assert_eq!(
                error.span(),
                Span {
                    start,
                    end: start + 1
                },
                "{source}"
            );
        }
    }
}

#[test]
fn leading_update_expressions_share_the_prepared_and_dispatch_caps() {
    let limits = Limits {
        max_tokens: 5200,
        ..Limits::default()
    };
    for (prefixes, expected) in [(3, Ok(0.0)), (4, Err(EvalError::LimitExceeded))] {
        // Each skipped block item emits update, operand, add, and discard.
        let source = format!(
            "let x=0;if(false){{{}}}{}x",
            "++x+0;".repeat(1022),
            "+ ".repeat(prefixes)
        );
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 4093 + prefixes);
        assert_eq!(script.evaluate(), expected);
    }
    // Two setup dispatches, eight per iteration (one extra for the RHS on
    // the first), four for the final test, and the final load: 16,384.
    for (prefix, expected) in [
        ("", Ok(2047.0)),
        ("+", Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!("let i=0;while(i<2047){{i++ || 1;}}{prefix}i");
        assert_eq!(parse_script(&source).unwrap().evaluate(), expected);
    }
}

#[test]
fn grouped_updates_charge_the_group_one_emission_and_one_lookup() {
    for form in ["x/*💡*/++", "--/*💡*/x"] {
        let source = format!("let x=false;({form})");
        let span = |start, len| Span {
            start,
            end: start + len,
        };
        let target = span(source.rfind('x').unwrap(), 1);
        let open = span(source.find('(').unwrap(), 1);
        let close = span(source.len() - 1, 1);
        let limits = Limits {
            max_source_bytes: source.len(),
            max_tokens: 9,
            max_nesting_depth: 1,
            // Scan the source, emit three instructions, push a group marker,
            // and charge one candidate and one comparison byte for the target.
            max_work: source.len() + 6,
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 3);
        assert_eq!(script.max_stack_depth(), 1);
        assert_eq!(
            script.evaluate(),
            Ok(if form.starts_with('x') { 0.0 } else { -1.0 })
        );
        for (limits, limit, where_) in [
            (
                Limits {
                    max_source_bytes: source.len() - 1,
                    ..limits
                },
                Limit::SourceBytes,
                close,
            ),
            (
                Limits {
                    max_tokens: 8,
                    ..limits
                },
                Limit::Tokens,
                close,
            ),
            (
                Limits {
                    max_nesting_depth: 0,
                    ..limits
                },
                Limit::NestingDepth,
                open,
            ),
            (
                Limits {
                    max_work: target.end + 3,
                    ..limits
                },
                Limit::Work,
                target,
            ),
            (
                Limits {
                    max_work: limits.max_work - 1,
                    ..limits
                },
                Limit::Work,
                target,
            ),
        ] {
            let error = parse_script_with_limits(&source, limits).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{source}");
            assert_eq!(error.span(), where_, "{source}");
        }
    }
}

#[test]
fn ungrouped_operand_reads_and_updates_have_one_emission_and_one_lookup() {
    for (operand, token_count) in [("x/*💡*/", 7), ("x/*💡*/++", 8), ("--/*💡*/x", 8)] {
        let source = format!("let x=false;void {operand}");
        let start = source.rfind('x').unwrap();
        let target = Span {
            start,
            end: start + 1,
        };
        let last = if operand.ends_with("++") {
            source.len() - 2
        } else {
            start
        };
        let limits = Limits {
            max_source_bytes: source.len(),
            max_nesting_depth: 0,
            max_tokens: token_count,
            // Scan once, emit four instructions, push `void`, and resolve one name.
            max_work: source.len() + 7,
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 4);
        assert_eq!(script.max_stack_depth(), 1);
        assert_eq!(script.evaluate_value(), Ok(Value::Undefined));
        for (limits, limit, span) in [
            (
                Limits {
                    max_tokens: token_count - 1,
                    ..limits
                },
                Limit::Tokens,
                Span {
                    start: last,
                    end: last + 1,
                },
            ),
            (
                Limits {
                    max_work: target.end + 3,
                    ..limits
                },
                Limit::Work,
                target,
            ),
            (
                Limits {
                    max_work: limits.max_work - 1,
                    ..limits
                },
                Limit::Work,
                target,
            ),
        ] {
            let error = parse_script_with_limits(&source, limits).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{source}");
            assert_eq!(error.span(), span, "{source}");
        }
    }
}

#[test]
fn expression_updates_count_at_the_existing_prepared_and_dispatch_caps() {
    let limits = Limits {
        max_tokens: 5000,
        ..Limits::default()
    };
    for (unary, expected) in [(4089, Ok(0.0)), (4090, Err(EvalError::LimitExceeded))] {
        let source = format!("let x=false; false && x++; {}x", "+ ".repeat(unary));
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 7 + unary);
        assert_eq!(script.max_stack_depth(), 1);
        assert_eq!(script.evaluate(), expected);
    }

    // 2 initialization dispatches, 3275 taken iterations at 5 each,
    // 4 dispatches for the final test, then the final load and unary operators.
    for (unary, expected) in [(2, Ok(3276.0)), (3, Err(EvalError::DispatchLimitExceeded))] {
        let source = format!("let n=0; while(n++<3275){{}} {}n", "+ ".repeat(unary));
        let script = parse_script(&source).unwrap();
        assert_eq!(script.max_stack_depth(), 2);
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
}
