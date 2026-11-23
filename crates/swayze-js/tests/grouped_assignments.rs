use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn the_expression_and_binding_preserve_the_same_tagged_value() {
    for (rhs, expected) in [
        ("2 + 3", Value::Number(5.0)),
        ("-0", Value::Number(-0.0)),
        ("0 / 0", Value::Number(f64::NAN)),
        ("true", Value::Boolean(true)),
        ("false", Value::Boolean(false)),
        ("null", Value::Null),
        ("undefined", Value::Undefined),
    ] {
        for tail in ["", "; x"] {
            let source = format!("let x = 42; (x = {rhs}){tail}");
            let script = parse_script(&source).unwrap();
            match expected {
                Value::Number(expected) => {
                    let Value::Number(actual) = script.evaluate_value().unwrap() else {
                        panic!("expected a Number: {source}");
                    };
                    if expected.is_nan() {
                        assert!(actual.is_nan(), "{source}");
                    } else {
                        assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
                    }
                }
                _ => {
                    assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
                    assert_eq!(script.evaluate(), Err(EvalError::NonNumericResult));
                }
            }
        }
    }
}

#[test]
fn nested_writes_run_in_order_and_evaluate_the_rhs_only_once() {
    for (source, expected) in [
        ("let x; let y; const z=(x=(y=4)); x*100+y*10+z", 444.0),
        ("let x=1; (x=(x=2)+x) * 10 + x", 44.0),
        ("let a=0,x; 10+(x=((a=a+1),a+3)) + x*100 + a*1000", 1414.0),
        ("let x=0; ((x = true ? 2 : 3), x + 4)", 6.0),
        ("let x=1; (x=2)**3 + x", 10.0),
        ("let x=0; x += (x=2); x", 2.0),
        (
            "const k=9; let result; { let k; (result=(k=2)); } k*10+result",
            92.0,
        ),
        ("let x; { let undefined; (x=(undefined=3)); } x", 3.0),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
    }
}

#[test]
fn branches_join_with_value_and_skip_untaken_writes() {
    for (source, expected) in [
        ("let x=1; false && (x=2); x", Value::Number(1.0)),
        ("let x=1; true || (x=2); x", Value::Number(1.0)),
        ("let x=1; false ?? (x=2); x", Value::Number(1.0)),
        ("let x=1; 0 ? (x=2) : x", Value::Number(1.0)),
        ("let x; 10 + (false || (x=2)) + x", Value::Number(14.0)),
        ("let x; true && (x = null ?? false)", Value::Boolean(false)),
        ("let x; true ? (x = null) : (x = false)", Value::Null),
        ("let x; (x = false ? null : void 0)", Value::Undefined),
        ("let x; null ?? (x = false || true)", Value::Boolean(true)),
        ("let x=0; x===(x=1)", Value::Boolean(false)),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate_value(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn expression_statements_and_control_clauses_join_at_empty_stacks() {
    let source = "let x=0,sum=0;
        if ((x=false)) { (sum=99); }
        else if ((x=2)) { (sum=x); }
        else { (sum=98); }
        while ((x=x-1)) { (sum=sum+10); }
        do { (sum=sum+100); continue; } while ((x=0));
        for ((x=1); (x=x+1)<5; (x=x+1)) { (sum=sum+x); continue; }
        sum*10+x";
    let script = parse_script(source).unwrap();
    assert_eq!(script.max_stack_depth(), 2);
    assert_eq!(script.evaluate(), Ok(1186.0));
}

fn assert_marked_error(marked: &str, kind: ErrorKind, message: &str) {
    let (before, rest) = marked.split_once('@').unwrap();
    let (at, after) = rest.split_once('@').unwrap();
    let source = format!("{before}{at}{after}");
    let error = parse_script(&source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}");
    assert_eq!(
        error.span(),
        Span {
            start: before.len(),
            end: before.len() + at.len()
        },
        "{source}"
    );
    assert!(error.message().contains(message), "{source}: {error}");
}

#[test]
fn target_checks_are_deferred_and_apply_even_to_skipped_code() {
    let tdz = "before lexical binding initialization";
    let immutable = "assignment to const";
    for (marked, message) in [
        ("const x=0; false && (x/*☃*/@=@1)", immutable),
        ("let x=0; if (false) { const x=1; (x@=@2); } x", immutable),
        ("let a=0; const b=1; a &&= (b@=@2); a", immutable),
        ("const x=0; true ? 0 : (x@=@missing)", immutable),
        ("let x = false ? (@x@=2) : 0; x", tdz),
        ("let x=0; { true || (@x@=1); let x; } x", tdz),
        ("let q = (true || (@x@=1)), x=2; q", tdz),
        ("false && (@missing@=42)", "unbound name"),
        ("true || (@undefined@=42)", "unbound name"),
        ("let x; false && (x=@missing@)", "unbound name"),
    ] {
        assert_marked_error(marked, ErrorKind::Unsupported, message);
    }
    // Even a skipped update resolves in the header, not in the loop body.
    assert_eq!(
        parse_script("let x=0; for(;false;(x=1)) { let x; } x")
            .unwrap()
            .evaluate(),
        Ok(0.0)
    );
}

#[test]
fn rejects_targets_and_positions_outside_the_slice() {
    use ErrorKind::{Malformed, Unsupported};
    for (marked, kind, message) in [
        ("let x;(x@(@)=1)", Unsupported, "call expressions"),
        ("let x;((x)@=@1)", Unsupported, "assignment expressions"),
        ("let x;(!x@=@1)", Unsupported, "assignment expressions"),
        ("let x;(x+1@=@2)", Unsupported, "assignment expressions"),
        ("let x;(x@.@p=1)", Unsupported, "token is outside"),
        ("(null@=@1)", Unsupported, "assignment expressions"),
        ("let x;(x@+=@1)", Unsupported, "assignment expressions"),
        (
            "let x;(x@++@)",
            Unsupported,
            "increment and decrement expressions",
        ),
        ("let x,y;(x=y@=@42)", Unsupported, "assignment expressions"),
        ("let x;(0,x@=@42)", Unsupported, "assignment expressions"),
        (
            "let x;(x=true ? x@=@1 : 2)",
            Unsupported,
            "assignment expressions",
        ),
        ("let x;if(x@=@1){}x", Unsupported, "assignment expressions"),
        (
            "let x;while(x@=@1){}x",
            Unsupported,
            "assignment expressions",
        ),
        (
            "let x;do{}while(x@=@1);x",
            Unsupported,
            "assignment expressions",
        ),
        (
            "let x;for(;x@=@1;){}x",
            Unsupported,
            "assignment expressions",
        ),
        (
            "let x;(x=0@,@1)",
            Unsupported,
            "sequence in an assignment RHS",
        ),
        (
            "let x;(x=true?0:1@,@2)",
            Unsupported,
            "sequence in an assignment RHS",
        ),
        ("let x;(x=@)@", Malformed, "expected an operand"),
        (
            "let x;(x=1@;@",
            Malformed,
            "semicolon inside a parenthesized expression",
        ),
        ("let x;(x=1@@", Malformed, "expected a closing parenthesis"),
    ] {
        assert_marked_error(marked, kind, message);
    }
}

#[test]
fn lookahead_and_store_have_exact_token_and_work_budgets() {
    let source = "let x;(x/*α*/\u{2028}=false)";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 8,
        max_nesting_depth: 1,
        // Scanned bytes, two initializer instructions, the group marker and
        // assignment attachment, two expression instructions, and resolution
        // (one candidate visit + one-byte comparison).
        max_work: source.len() + 8,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 4);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(false)));

    let target = source.rfind('x').unwrap();
    for (limits, limit, start) in [
        (
            Limits {
                max_tokens: 7,
                ..limits
            },
            Limit::Tokens,
            source.len() - 1,
        ),
        (
            Limits {
                max_work: target + 1 + 3,
                ..limits
            },
            Limit::Work,
            target,
        ),
        (
            Limits {
                max_work: source.len() + 5,
                ..limits
            },
            Limit::Work,
            source.len() - 1,
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
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
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
fn grouped_identifier_lookahead_preserves_read_budgets() {
    let source = "let x;(x/*α*/\u{2028}==undefined)";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 8,
        max_nesting_depth: 1,
        // Five emissions, two pending entries and three lexical lookup units.
        max_work: source.len() + 10,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 5);
    assert_eq!(script.max_stack_depth(), 2);
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
    let start = source.rfind('x').unwrap();
    let error = parse_script_with_limits(
        source,
        Limits {
            max_work: start + 1 + 3,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + 1
        }
    );
}

#[test]
fn nested_writes_use_one_group_entry_each_and_one_operand() {
    let depth = 1000;
    let source = format!("let x;{}null{}", "(x=".repeat(depth), ")".repeat(depth));
    let limits = Limits {
        max_nesting_depth: depth,
        ..Limits::default()
    };
    let script = parse_script_with_limits(&source, limits).unwrap();
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.instruction_count(), depth + 3);
    assert_eq!(script.evaluate_value(), Ok(Value::Null));
    let error = parse_script_with_limits(
        &source,
        Limits {
            max_nesting_depth: depth - 1,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    let start = "let x;".len() + (depth - 1) * "(x=".len();
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + 1
        }
    );
}

#[test]
fn skipped_store_keep_counts_toward_prepared_instruction_limit() {
    let limits = Limits {
        max_tokens: 5000,
        ..Limits::default()
    };
    let prefix = "!".repeat(4090);
    let at_cap =
        parse_script_with_limits(&format!("let x; true || (x={prefix}false)"), limits).unwrap();
    assert_eq!(at_cap.instruction_count(), 4096);
    assert_eq!(at_cap.evaluate_value(), Ok(Value::Boolean(true)));
    let over_cap =
        parse_script_with_limits(&format!("let x; true || (x={prefix}!false)"), limits).unwrap();
    assert_eq!(over_cap.instruction_count(), 4097);
    assert_eq!(over_cap.evaluate_value(), Err(EvalError::LimitExceeded));
}

#[test]
fn each_reached_store_keep_dispatches_once() {
    // 2 initialization dispatches, 2046 taken iterations at 8 dispatches
    // each, 7 for the final (falsy) test, then 7 for the final expression.
    let at_cap = "let n=0; while ((n=n+1)<2047) {} + + + + + +n";
    let script = parse_script(at_cap).unwrap();
    assert_eq!(script.evaluate(), Ok(2047.0));
    assert_eq!(script.evaluate(), Ok(2047.0));
    let over_cap = "let n=0; while ((n=n+1)<2047) {} + + + + + + +n";
    assert_eq!(
        parse_script(over_cap).unwrap().evaluate(),
        Err(EvalError::DispatchLimitExceeded)
    );
}
