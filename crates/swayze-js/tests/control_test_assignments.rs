use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn tests_write_in_the_enclosing_scope_and_loops_retest_after_continue() {
    for (source, expected) in [
        (
            "let x=9,y,hit=0,skipped=0;
             { if (x=0) { hit=1; } else if (x=y=3) { let x=10; hit=x+y; }
               else if (skipped=4) { hit=99; } }
             x*1000+y*100+hit*10+skipped",
            3430.0,
        ),
        (
            "let n=4,s=0,t;
             { while (t=n=n-1) { let n=10; s+=t+n; continue; s=100; } }
             s*10+n",
            360.0,
        ),
        (
            "let n=3,t,s=0;
             do { let n=40; s+=n; continue; s=999; } while (t=n=n-1);
             s*10+t",
            1200.0,
        ),
        ("let x=0; do { break; } while (x=1); x", 0.0),
        (
            "let x=4,sum=0; for (; x=x-1;) { sum+=x; continue; } sum*10+x",
            60.0,
        ),
        (
            "let n=99,s=0,u=0,t;
             { for (let n=4; t=n=n-1; u++) { let n=10; s+=t+n; continue; s=999; } }
             n*10000+s*100+u*10+t",
            993630.0,
        ),
    ] {
        let script = parse_script(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
    }
}

#[test]
fn truthiness_does_not_coerce_the_stored_value_and_rhs_runs_once() {
    for (rhs, stored, entered, effect) in [
        ("false", "x===false && y===false", false, 0),
        ("null", "x===null && y===null", false, 0),
        ("void (side=1)", "x===undefined && y===undefined", false, 1),
        ("-0", "1/x===-1/0 && 1/y===-1/0", false, 0),
        ("0/0", "x!==x && y!==y", false, 0),
        ("true", "x===true && y===true", true, 0),
        ("-1/0", "x===-1/0 && y===-1/0", true, 0),
        ("++side", "x===1 && y===1", true, 1),
    ] {
        let source = format!(
            "let x,y,ran=false,side=0;
             if (x/*☃*/=y=true ? {rhs} : side=99) {{ ran=true; }}
             ran==={entered} && ({stored}) && side==={effect}"
        );
        let script = parse_script(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            script.evaluate_value(),
            Ok(Value::Boolean(true)),
            "{source}"
        );
    }
    let source = "let x,y,n=0; if (x=y=false ? n=99 : (true || (n=100))) { n++; }
                  x===true && y===true && n===1";
    assert_eq!(
        parse_script(source).unwrap().evaluate_value(),
        Ok(Value::Boolean(true))
    );
}

#[test]
fn test_assignment_can_run_before_or_after_comma() {
    for source in [
        "let x = 7; if (x = 0, true) { x += 42; } x",
        "let x = 7; if (true, x = 0) { x += 42; } x + 42",
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(42.0),
            "{source}"
        );
    }
}

#[test]
fn comma_discards_the_completed_assignment_then_uses_the_last_value() {
    for source in [
        "let x=1,hit=0; if (x=0, true) { hit++; } x*10+hit",
        "let x=1,hit=0; if (false) {} else if (x=0, true) { hit++; } x*10+hit",
        "let x=1,hit=0; while (x=0, hit===0) { hit++; } x*10+hit",
        "let x=1,hit=0; do { hit++; } while (x=7, (x=0), false); x*10+hit",
        "let x=1,hit=0; for (; x=0, hit===0;) { hit++; } x*10+hit",
        "let x=1,hit=0; if (x=(1,0), x===0, true) { hit++; } x*10+hit",
        "let x=1,hit=0; if (x=(x-=1), true) { hit++; } x*10+hit",
        "let x=1,hit=0; if (x=0, false ? 0 : hit=1) {} x*10+hit",
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(1.0),
            "{source}"
        );
    }
}

fn assert_marked_error(source: &str, kind: ErrorKind, message: &str) {
    let (before, rest) = source.split_once('@').unwrap();
    let (at, after) = rest.split_once('@').unwrap();
    let error = parse_script(&format!("{before}{at}{after}")).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start: before.len(),
            end: before.len() + at.len()
        },
        "{source}: {error}"
    );
    assert!(error.message().contains(message), "{source}: {error}");
}

#[test]
fn test_assignment_prefixes_require_a_bare_name() {
    for source in [
        "let x; if (false) {} else if (!x @||=@ 1) {} x",
        "let x,y; if (x=(y) @+=@ 1) {} x",
        "let x; if (true, x+1 @+=@ 1) {} x",
        "let x; for (; x=0, !x @=@ 1;) {} x",
        "let x; if ((x) @=@ 1) {} x",
        "let x; while (true && x @=@ 1) {} x",
        "let x; do {} while (!x @=@ 1); x",
        "let x; for (; x+1 @=@ 1;) {} x",
    ] {
        assert_marked_error(source, ErrorKind::Unsupported, "assignment expressions");
    }
    assert_marked_error(
        "let x; if (x=true) {}@@",
        ErrorKind::Unsupported,
        "a final expression is required",
    );
}

#[test]
fn malformed_tests_report_the_owning_delimiter() {
    for (source, message) in [
        ("let x; if(x=1/*💡*/@;@){}x", "semicolon inside"),
        (
            "let x; if(x=1@{@}x",
            "expected ')' after the statement test",
        ),
        ("let x; if(x=1@@", "expected a closing parenthesis"),
        ("let x; if(x=@)@{}x", "expected an operand"),
        ("let x; if(x=(@)@){}x", "empty parenthesized expression"),
        (
            "let x; if(x=true ? 1@)@{}x",
            "expected ':' before closing parenthesis",
        ),
        (
            "let x; if(x=true ? 1@,@2 : 3){}x",
            "expected ':' before ','",
        ),
        ("let x; if(x=1,@)@{}x", "expected an operand"),
        ("let x; for(;x=1@)@{}x", "expected ';' between for clauses"),
        ("let x; for(;x=1@@", "expected ';' between for clauses"),
        ("let x; for(;x=(1@;@2);){}x", "semicolon inside"),
        ("let x; for(;x=1, @;@){}x", "expected an operand"),
        ("let x; for(;x=true ? 1@;@){}x", "expected ':'"),
    ] {
        assert_marked_error(source, ErrorKind::Malformed, message);
    }
    assert_marked_error(
        "let x; do {} while(x=0) @x@",
        ErrorKind::Unsupported,
        "explicit trailing semicolon",
    );
}

#[test]
fn deferred_target_checks_use_source_order_and_the_nearest_declaration() {
    for (source, message) in [
        (
            "const x=0; if(true){} else if(x/*☃*/@=@ missing){} x",
            "assignment to const",
        ),
        (
            "let x; for(const n=0; x=n @=@ missing;){} x",
            "assignment to const",
        ),
        (
            "let x=7; { if (false) {} else if (@x@=1) {} let x; } x",
            "before lexical binding initialization",
        ),
        (
            "let x=7; { do { break; } while (@x@=1); const x=0; } x",
            "before lexical binding initialization",
        ),
        (
            "let x; do { let y; break; } while (x=@y@=1); x",
            "unbound name",
        ),
        (
            "let x; if(x=false ? @undefined@=1 : true){} x",
            "unbound name",
        ),
    ] {
        assert_marked_error(source, ErrorKind::Unsupported, message);
    }
}

#[test]
fn old_and_new_test_costs_are_inclusive_with_exact_failure_spans() {
    let old = "let x;if(true){}x";
    let old_limits = Limits {
        max_source_bytes: old.len(),
        max_tokens: 10,
        max_nesting_depth: 1,
        // Five emissions, opening test marker, opening block, if fixup,
        // and two units to resolve the final read of x.
        max_work: old.len() + 10,
    };
    let script = parse_script_with_limits(old, old_limits).unwrap();
    assert_eq!(script.instruction_count(), 5);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Undefined));
    for (limits, limit, start) in [
        (
            Limits {
                max_tokens: old_limits.max_tokens - 1,
                ..old_limits
            },
            Limit::Tokens,
            old.len() - 1,
        ),
        (
            Limits {
                max_work: old_limits.max_work - 1,
                ..old_limits
            },
            Limit::Work,
            old.len() - 1,
        ),
        (
            Limits {
                max_nesting_depth: old_limits.max_nesting_depth - 1,
                ..old_limits
            },
            Limit::NestingDepth,
            old.find('(').unwrap(),
        ),
    ] {
        let error = parse_script_with_limits(old, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{error}");
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + 1
            },
            "{error}"
        );
    }

    let source = "let x;if(x/*α*/=x=true){}x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: old_limits.max_tokens + 4,
        max_nesting_depth: old_limits.max_nesting_depth + 2,
        // Each link adds one name charge, a pending push, a store and two lookup units.
        max_work: source.len() + 10 + 2 * 5,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 7);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
    let first_equals = source.find('=').unwrap();
    let close = source.find(')').unwrap();
    for (limits, limit, start) in [
        (
            Limits {
                max_nesting_depth: 2,
                ..limits
            },
            Limit::NestingDepth,
            source.rfind('=').unwrap(),
        ),
        (
            Limits {
                max_work: first_equals + 5,
                ..limits
            },
            Limit::Work,
            first_equals,
        ),
        (
            Limits {
                max_work: close + 9,
                ..limits
            },
            Limit::Work,
            close,
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            source.len() - 1,
        ),
        (
            Limits {
                max_tokens: limits.max_tokens - 1,
                ..limits
            },
            Limit::Tokens,
            source.len() - 1,
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{error}");
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + 1
            },
            "{error}"
        );
    }
}

#[test]
fn both_test_delimiters_release_long_chains_without_growing_the_operand_stack() {
    let depth = 1000;
    let links = "x=".repeat(depth - 1);
    for (prefix, suffix, instructions_without_stores) in [
        ("let x;if(", "false){}x", 5),
        ("let x;for(;", "false;){}x", 6),
    ] {
        let source = format!("{prefix}{links}{suffix}");
        let limits = Limits {
            max_nesting_depth: depth,
            ..Limits::default()
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(
            script.instruction_count(),
            depth - 1 + instructions_without_stores
        );
        assert_eq!(script.max_stack_depth(), 1);
        assert_eq!(script.evaluate_value(), Ok(Value::Boolean(false)));
        let error = parse_script_with_limits(
            &source,
            Limits {
                max_nesting_depth: depth - 1,
                ..limits
            },
        )
        .unwrap_err();
        let end = prefix.len() + links.len();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
        assert_eq!(
            error.span(),
            Span {
                start: end - 1,
                end
            }
        );
    }
}

#[test]
fn test_stores_count_toward_prepared_and_dispatch_ceilings() {
    // The whole else-if test is skipped, but its instructions count toward 4096.
    for (prefixes, expected) in [
        (4087, Ok(Value::Undefined)),
        (4088, Err(EvalError::LimitExceeded)),
    ] {
        let source = format!(
            "let x;if(true){{}}else if(x={}false){{}}x",
            "!".repeat(prefixes)
        );
        let script = parse_script_with_limits(
            &source,
            Limits {
                max_tokens: 5000,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(script.instruction_count(), prefixes + 9);
        assert_eq!(script.evaluate_value(), expected);
    }
    // Two initialization dispatches, 2729 tests with continue (six each),
    // a final falsy test (five), and the final read plus two or three unary ops.
    for (tail, expected) in [
        ("+ + x", Ok(0.0)),
        ("+ + + x", Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!("let x=2730;while(x=x-1){{continue;}}{tail}");
        assert_eq!(parse_script(&source).unwrap().evaluate(), expected);
    }
}
