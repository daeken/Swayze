use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn stores_finish_before_later_operands_in_root_and_block_scopes() {
    for (source, expected) in [
        ("let x=0,y=1; x=3,y+=x; y", 4.0),
        ("let x=8,y; x/=x=2,y=x; y*10+x", 44.0),
        ("let x=8,y=2; x+=y+=1,x; x*10+y", 113.0),
        ("let x=10,y; { let x=8; x/=x=2, y=x, x++; } y*100+x", 410.0),
        (
            "let x=1,y=2,z=3; x=y=true ? z+=4 : z=99, y=false ? x=99 : z*=2; x*100+y*10+z",
            854.0,
        ),
        ("let x=1,y; x=x++,y=++x; x*10+y", 22.0),
        ("let x=8,y; x/=(x=3,x=2), y=x; y", 4.0),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
    }
}

#[test]
fn logical_skip_and_take_both_run_the_suffix_with_the_stored_value() {
    for (op, skip, skipped, take) in [
        ("&&=", "false", Value::Boolean(false), "true"),
        ("||=", "true", Value::Boolean(true), "false"),
        ("??=", "false", Value::Boolean(false), "null"),
        ("??=", "0", Value::Number(0.0), "undefined"),
    ] {
        for (initial, result, hits) in [(skip, skipped, 0.0), (take, Value::Null, 1.0)] {
            let source =
                format!("let x={initial},y,hits=0,seen; x{op}(hits+=1,null),y=x,seen=hits; ");
            for (tail, expected) in [("y", result), ("seen", Value::Number(hits))] {
                let script = parse_script(&format!("{source}{tail}")).unwrap();
                assert_eq!(script.evaluate_value(), Ok(expected), "{source}{tail}");
            }
        }
    }
}

#[test]
fn leading_logical_assignment_runs_the_suffix_without_supplying_script_completion() {
    let sequence = "let x=0,y=0; x ||= 4, y += x;";
    let script = parse_script(&format!("{sequence} y")).unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Number(4.0)));
    assert_marked_error(
        &format!("{sequence}@@"),
        ErrorKind::Unsupported,
        "a final expression is required",
    );

    // Keep x at 3, not 4, but still add it to y in the suffix.
    for source in [
        "let x=3,y=0; x ||= 4, y += x; y",
        "let x=3,y=0; { x ||= 4, y += x; } y",
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate_value(), Ok(Value::Number(3.0)), "{source}");
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
            end: before.len() + at.len(),
        },
        "{source}: {error}"
    );
    assert!(error.message().contains(message), "{source}: {error}");
}

#[test]
fn separators_require_operands_and_do_not_interrupt_a_consequent_or_declaration() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, message) in [
        ("let x; x=/*α*/@,@x;x", "expected an operand"),
        ("let x; x=true,/*α*/@,@x;x", "expected an operand"),
        ("let x; x=true, x=false,/*α*/@;@x", "expected an operand"),
        ("let x=true; { x ||= 4,/*α*/@;@ } x", "expected an operand"),
        ("let x; x=true,/*α*/@@", "expected an operand"),
        (
            "let x; x=true ? x=1/*α*/@,@2 : 3; x",
            "expected ':' before ','",
        ),
        (
            "let x; x=0,true ? x=1/*α*/@,@2 : 3; x",
            "expected ':' before ','",
        ),
    ] {
        assert_marked_error(source, Malformed, message);
    }
    assert_marked_error(
        "let x; let y=x=1, z/*α*/@+=@2; x",
        Unsupported,
        "cannot initialize declarations",
    );
    for source in [
        "let x; @++@x,1;x",
        "let x; x@--@,1;x",
        "let x; { @--@x,1; } x",
        "let x; { x@++@,1; } x",
    ] {
        assert_marked_error(
            source,
            Unsupported,
            "outside the subset at the start of a statement",
        );
    }
}

#[test]
fn standalone_sequence_needs_a_semicolon_and_does_not_supply_the_final_expression() {
    for source in [
        "let x; x=1,x/*α*/@@",
        "let x; x=1,null/*\u{2028}*/@x@",
        "let x; x=1,null\n@let@ y; x",
        "let x; {x+=1,null/*α*/@}@ x",
    ] {
        assert_marked_error(
            source,
            ErrorKind::Unsupported,
            "assignments require explicit semicolons",
        );
    }
    for source in ["let x; x=1,x;/*α*/@@", "let x; {x||=1,x;}/*α*/@@"] {
        assert_marked_error(
            source,
            ErrorKind::Unsupported,
            "a final expression is required",
        );
    }
}

#[test]
fn skipped_rhs_and_suffix_writes_are_statically_checked_in_their_own_scope() {
    for (source, message) in [
        ("let x=true; x||=@missing@,x; x", "unbound name"),
        ("let x; if(false){x=1,@missing@=2;} x", "unbound name"),
        (
            "let x; {@x@=1,x; let x;} x",
            "before lexical binding initialization",
        ),
        (
            "let x; {x=1,@y@=2; let y;} x",
            "before lexical binding initialization",
        ),
        (
            "let x; {const y=true; x=1,y@||=@2;} x",
            "assignment to const",
        ),
    ] {
        assert_marked_error(source, ErrorKind::Unsupported, message);
    }
}

fn assert_limit(source: &str, limits: Limits, limit: Limit, start: usize) {
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

#[test]
fn suffix_uses_shared_token_work_and_pending_operator_limits() {
    let source = "let x;x=true,x=null,x;x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 14,
        max_nesting_depth: 1,
        // Ten emissions, the suffix assignment's name marker and pending push,
        // and two units for each of the four target/name resolutions.
        max_work: source.len() + 20,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 10);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Null));
    for (limit, limits, start) in [
        (
            Limit::Tokens,
            Limits {
                max_tokens: 13,
                ..limits
            },
            source.len() - 1,
        ),
        (
            Limit::Work,
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            source.len() - 1,
        ),
        (
            Limit::NestingDepth,
            Limits {
                max_nesting_depth: 0,
                ..limits
            },
            source.rfind('=').unwrap(),
        ),
    ] {
        assert_limit(source, limits, limit, start);
    }
    // Leave just enough work to lex each separator, but not to emit the
    // consuming first store or either suffix discard.
    for (start, spent) in [
        (source.find(',').unwrap(), 3),
        (source.rfind(',').unwrap(), 8),
        (source.rfind(';').unwrap(), 10),
    ] {
        assert_limit(
            source,
            Limits {
                max_work: start + 1 + spent,
                ..limits
            },
            Limit::Work,
            start,
        );
    }
}

#[test]
fn no_comma_simple_eager_and_logical_stores_keep_exact_legacy_accounting() {
    let source = "let x;x=true;x+=true;x||=false;x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 16,
        max_nesting_depth: 0,
        // Thirteen emissions and four two-unit lookups; no pending markers.
        max_work: source.len() + 21,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 13);
    assert_eq!(script.max_stack_depth(), 2);
    assert_eq!(script.evaluate(), Ok(2.0));
    assert_limit(
        source,
        Limits {
            max_tokens: 15,
            ..limits
        },
        Limit::Tokens,
        source.len() - 1,
    );
    assert_limit(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        Limit::Work,
        source.len() - 1,
    );
}

#[test]
fn prepared_and_reached_suffix_instructions_use_shared_inclusive_caps() {
    // Four instructions through the consuming store, 2,044 literal/discard
    // pairs, three or four instructions for the last suffix operand and its
    // discard, then one final read.
    for (tail, count, expected) in [
        ("!false", 4096, Ok(Value::Null)),
        ("!!false", 4097, Err(EvalError::LimitExceeded)),
    ] {
        let source = format!("let x;x=null,{}{tail};x", "false,".repeat(2044));
        let script = parse_script_with_limits(
            &source,
            Limits {
                max_tokens: 8192,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(script.instruction_count(), count);
        assert_eq!(script.max_stack_depth(), 1);
        assert_eq!(script.evaluate_value(), expected);
    }
    // Four setup dispatches, 12 per iteration on skip or 14 on take,
    // four for the last test, and one for the final read. Padding
    // reaches 16,384 exactly, including one suffix discard per iteration.
    for (initial, iterations, padding) in [("true", 1364, 7), ("false", 1169, 9)] {
        for extra in [0, 1] {
            let source = format!(
                "let i=0,x={initial};while(i<{iterations}){{x||=false,i+=1;}}{}i",
                "+ ".repeat(padding + extra),
            );
            let expected = if extra == 0 {
                Ok(f64::from(iterations))
            } else {
                Err(EvalError::DispatchLimitExceeded)
            };
            assert_eq!(parse_script(&source).unwrap().evaluate(), expected);
        }
    }
}
