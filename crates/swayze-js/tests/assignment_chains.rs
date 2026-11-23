use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn chains_preserve_the_rhs_value_and_existing_statement_boundaries() {
    for (source, expected) in [
        ("let x; let y = x = 3; y + x", 6.0),
        ("let x; let y = x = 3, z = 4; x*100 + y*10 + z", 334.0),
        ("let x,y; x = y = 1; x*10 + y", 11.0),
        ("let x,y; (x=y=42) + x + y", 126.0),
        ("let x=0; let y=x=++x; y*10+x", 11.0),
        ("let x=8,y; (x /= y = (x = 2))*100 + y*10+x", 424.0),
        ("let x=8,y; x += y = 2; x*10+y", 102.0),
        ("let y; for(let x, z=x=y=3; z===3; x=y=4) { z=0; } y", 4.0),
        ("let x,y; for(x=y=3; false;) {} x*10+y", 33.0),
    ] {
        let script = parse_script(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
    }
    let script = parse_script(
        "let x,y; { let undefined; y = x = undefined = -0; }
         1/x === -1/0 && 1/y === -1/0",
    )
    .unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
}

#[test]
fn conditional_rhs_runs_only_the_selected_arm_once_before_storing() {
    for (test, expected) in [("true", 3310.0), ("false", 4401.0)] {
        let source =
            format!("let x,y,n=3,m=3; x=y={test} ? n++ : ++m; x*1000+y*100+(n-3)*10+(m-3)");
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(expected));
    }
}

#[test]
fn only_the_selected_arm_writes_and_its_rhs_runs_once() {
    for (test, expected) in [("true", 22010.0), ("false", 20201.0)] {
        let source = format!(
            "let x,y,z,n=1,m=1; x={test} ? y=x=++n : z=x=++m;\n             x*10000+(y??0)*1000+(z??0)*100+(n-1)*10+(m-1)"
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), Ok(expected));
        assert_eq!(script.evaluate(), Ok(expected));
    }
    for (source, expected) in [
        ("let x,y; true ? x=y=null : x=false", Value::Null),
        ("let x,y; false ? x=true : x=y=undefined", Value::Undefined),
        ("let x,y; false ? x=null : x=y=false", Value::Boolean(false)),
        (
            "let x,y; 1/(true ? x=y=-0 : x=0) === -1/0 && 1/x === 1/y",
            Value::Boolean(true),
        ),
    ] {
        assert_eq!(parse_script(source).unwrap().evaluate_value(), Ok(expected));
    }
}

#[test]
fn nested_conditionals_close_only_the_assignments_in_the_finished_arm() {
    for (first, second, third, expected) in [
        ("true", "true", "false", 22200.0),
        ("true", "false", "false", 33030.0),
        ("false", "true", "true", 40404.0),
        ("false", "true", "false", 50055.0),
    ] {
        let source = format!(
            "let out,x=0,y=0,z=0,w=0;\n             out = {first} ? x = {second} ? y = 2 : z = 3 : w = {third} ? y = 4 : z = 5;\n             out*10000+x*1000+y*100+z*10+w"
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(expected));
    }
    for (source, expected) in [
        ("let x=0; true ? x=1 : true ? x=2 : x=3; x", 1.0),
        ("let x=0; false ? x=1 : false ? x=2 : x=3; x", 3.0),
        (
            "let x=0,y=0,z=0;
             true ? x=false ? y=1 : y=false ? z=2 : z=3 : x=9;
             x*100+y*10+z",
            333.0,
        ),
        (
            "let x, y, z; z = true ? x = false ? 1 : y = 2 : 99; x * 100 + y * 10 + z",
            222.0,
        ),
        ("let x,y; (10 + (true ? x=1 : y=2))", 11.0),
        ("let x=10,y; (x /= false ? y=1 : y=2)*10+y", 52.0),
    ] {
        assert_eq!(parse_script(source).unwrap().evaluate(), Ok(expected));
    }
}

#[test]
fn comma_ownership_is_decided_after_the_conditional_joins() {
    assert_marked_error(
        "let test=true,x,y=7,z=2; (test ? x = 1@,@ y : z)",
        ErrorKind::Malformed,
        "expected ':' before ','",
    );
    for (source, expected) in [
        ("let x,y; const v=x=false ? y=1 : y=2,z=x+y; z+v", 6.0),
        ("let x,y; true ? x=(1,2) : y=3, x", 2.0),
        ("let x,y; false ? x=1 : y=2, y", 2.0),
        ("let x; true ? 0 : x=1, x=2, x", 2.0),
        ("let test=true,x,y=7,z=2; (test ? x = 1 : z, x + y)", 8.0),
        (
            "let x; for(let y=false ? x=1 : x=2,z=y; z===x; ) { break; } x",
            2.0,
        ),
        ("let x=0; if (true ? x=1 : x=2, x===1) { x+=10; } x", 11.0),
        (
            "let x,y; for (x=false ? y=1 : y=2; x<3; x=true ? y=3 : y=4) {} x+y",
            6.0,
        ),
    ] {
        assert_eq!(parse_script(source).unwrap().evaluate(), Ok(expected));
    }
}

#[test]
fn grouped_logical_assignment_skips_or_takes_the_entire_chain() {
    for (initial, expected) in [("true", 17.0), ("false", 99.0)] {
        let source = format!("let g={initial},y=7; const v=(g ||= y = 9); v*10+y");
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(expected));
    }
    assert_marked_error(
        "let g=true; const y=7; (g ||= y/*☃*/@=@missing)",
        ErrorKind::Unsupported,
        "assignment to const",
    );
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
fn new_targets_resolve_in_source_order_and_obey_existing_lexical_checks() {
    for (source, message) in [
        (
            "let x; { const y=0; x = y /*☃*/@=@ missing; } x",
            "assignment to const",
        ),
        (
            "let x; { let q = x = @y@ = 3, y; } x",
            "before lexical binding initialization",
        ),
        (
            "let x=1; { let z = @x@ = 3; let x; } x",
            "before lexical binding initialization",
        ),
        ("let x; let y = x = @undefined@ = 3; y", "unbound name"),
        ("let x; let y = @missing@ = x = 3; y", "unbound name"),
        ("const x=0; x @=@ missing = 3; x", "assignment to const"),
    ] {
        assert_marked_error(source, ErrorKind::Unsupported, message);
    }
}

#[test]
fn both_skipped_arms_still_validate_write_targets_in_source_order() {
    for (source, message) in [
        (
            "const x=0; false ? x/*☃*/@=@ missing : 0",
            "assignment to const",
        ),
        (
            "const x=0; true ? 0 : x/*☃*/@=@ missing",
            "assignment to const",
        ),
        ("true ? 0 : @undefined@ = missing", "unbound name"),
        ("false ? @missing@ = 1 : absent = 2", "unbound name"),
        (
            "let x=0; { const y=false ? @x@ = 1 : 0; let x; } x",
            "before lexical binding initialization",
        ),
        (
            "let x=0; { const y=true ? 0 : @x@ = 1; let x; } x",
            "before lexical binding initialization",
        ),
    ] {
        assert_marked_error(source, ErrorKind::Unsupported, message);
    }
}

#[test]
fn empty_and_malformed_arms_fail_at_the_owning_delimiter() {
    for (source, message) in [
        ("let x; true ? @:@ x=1", "expected an operand"),
        ("let x; true ? x=1 :@@", "expected an operand"),
        ("let x; true ? x= @:@ 1", "expected an operand"),
        ("let x; true ? 1 : x=@;@", "expected an operand"),
        ("let x; true ? x=1 : x=2 @:@ 3", "without a matching '?'"),
        ("let x; true ? x=false ? x=1 : x=2@@", "expected ':'"),
        ("let x; true ? (false ? x=1@)@ : x=2", "expected ':'"),
        (
            "let x; let y=true ? x=1@,@ z=2 : x=3; y",
            "expected ':' before ','",
        ),
    ] {
        assert_marked_error(source, ErrorKind::Malformed, message);
    }
}

#[test]
fn only_direct_names_at_admitted_rhs_starts_can_begin_a_chain() {
    for source in [
        "let a=0,b=0,c=0; let x = a + b @=@ c; x",
        "let a; let x = -a @=@ 1; x",
        "let a; let x = true && a @=@ 1; x",
        "let a; let x = true ? a @+=@ 1 : 0; x",
        "let a; let x = false ? 0 : a @||=@ 1; x",
        "let a; true ? 0 : +a @=@ 1",
        "let a; if (+a @=@ 1) {} a",
        "let a; let x = (a) @=@ 1; x",
        "let a; let x = a @||=@ 1; x",
        "let a; let x = a = a++ @=@ 1; x",
    ] {
        assert_marked_error(source, ErrorKind::Unsupported, "assignment expressions");
    }
    for (source, message) in [
        ("let x,y; x=y=1@,@x; x", "sequence in an assignment RHS"),
        (
            "let x,y; x=y=1@@",
            "assignments require explicit semicolons",
        ),
        (
            "let x; x=true ? x=1 : x=2@,@ x; x",
            "sequence in an assignment RHS",
        ),
        (
            "let x; x=true ? x=1 : x=2@@",
            "assignments require explicit semicolons",
        ),
        (
            "let x; x=true ? x=1 : x=2;@@",
            "a final expression is required",
        ),
        ("let x,y; x=y=1;@@", "a final expression is required"),
    ] {
        assert_marked_error(source, ErrorKind::Unsupported, message);
    }
    assert_marked_error(
        "let x; let y=x=@;@y",
        ErrorKind::Malformed,
        "expected an operand",
    );
    assert_marked_error(
        "let x; let y=x=true ? 1@;@y",
        ErrorKind::Malformed,
        "expected ':'",
    );
}

#[test]
fn a_chain_link_has_one_name_marker_one_pending_push_and_one_store() {
    let source = "let x;let y=x/*α*/=true;y";
    let operator = source.rfind('=').unwrap();
    let separator = source.rfind(';').unwrap();
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 11,
        max_nesting_depth: 1,
        // Six instructions, one name marker and one pending push, plus eight
        // lookup units (two at declaration of y, four for x, two for the read of y).
        max_work: source.len() + 16,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 6);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
    for (limits, limit, start) in [
        (
            Limits {
                max_nesting_depth: 0,
                ..limits
            },
            Limit::NestingDepth,
            operator,
        ),
        (
            Limits {
                max_work: operator + 6,
                ..limits
            },
            Limit::Work,
            operator,
        ),
        (
            Limits {
                max_work: separator + 8,
                ..limits
            },
            Limit::Work,
            separator,
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
                max_tokens: 10,
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
fn arm_prefixes_share_the_nesting_limit_and_keep_existing_work_charges() {
    let old = "let x;true?null:undefined";
    let old_limits = Limits {
        max_source_bytes: old.len(),
        max_tokens: 8,
        max_nesting_depth: 1,
        // Seven instruction charges (including the read), two arm markers, one lookup visit.
        max_work: old.len() + 10,
    };
    let script = parse_script_with_limits(old, old_limits).unwrap();
    assert_eq!(script.instruction_count(), 7);
    assert_eq!(script.max_stack_depth(), 1);
    let error = parse_script_with_limits(
        old,
        Limits {
            max_work: old_limits.max_work - 1,
            ..old_limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(
        error.span(),
        Span {
            start: old.find("undefined").unwrap(),
            end: old.len()
        }
    );

    let source = "let x;true?x/*α*/=null:x=undefined";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: old_limits.max_tokens + 4,
        // Each arm takes one extra slot, reused after ':'.
        max_nesting_depth: old_limits.max_nesting_depth + 1,
        // Each link adds a name charge, a push, a store and two lookup units.
        max_work: source.len() + (old_limits.max_work - old.len()) + 2 * 5,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 9);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Null));

    let operator = source.find('=').unwrap();
    let colon = source.find(':').unwrap();
    let undefined = source.find("undefined").unwrap();
    let eof = source.len();
    for (limits, limit, start, end) in [
        (
            Limits {
                max_nesting_depth: 1,
                ..limits
            },
            Limit::NestingDepth,
            operator,
            operator + 1,
        ),
        (
            Limits {
                max_work: operator + 7,
                ..limits
            },
            Limit::Work,
            operator,
            operator + 1,
        ),
        (
            Limits {
                max_work: colon + 9,
                ..limits
            },
            Limit::Work,
            colon,
            colon + 1,
        ),
        (
            Limits {
                max_work: eof + 14,
                ..limits
            },
            Limit::Work,
            eof,
            eof,
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            undefined,
            eof,
        ),
        (
            Limits {
                max_tokens: limits.max_tokens - 1,
                ..limits
            },
            Limit::Tokens,
            undefined,
            undefined + 1,
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{error}");
        assert_eq!(error.span(), Span { start, end }, "{error}");
    }
    // The alternate also needs its own link's slot and reports that '=' span.
    let alternate = "let x;true?null:x=null";
    let error = parse_script_with_limits(
        alternate,
        Limits {
            max_nesting_depth: 1,
            ..Limits::default()
        },
    )
    .unwrap_err();
    let start = alternate.find('=').unwrap();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + 1
        }
    );
}

#[test]
fn skipped_arm_stores_count_toward_the_prepared_ceiling() {
    let limits = Limits {
        max_tokens: 5000,
        ..Limits::default()
    };
    let prefix = "!".repeat(4087);
    for (source, expected) in [
        (format!("let x;true?x=null:x={prefix}false"), Value::Null),
        (format!("let x;false?x={prefix}false:x=null"), Value::Null),
    ] {
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 4096);
        assert_eq!(script.evaluate_value(), Ok(expected));
        let over = source.replacen('!', "!!", 1);
        let script = parse_script_with_limits(&over, limits).unwrap();
        assert_eq!(script.instruction_count(), 4097);
        assert_eq!(script.evaluate_value(), Err(EvalError::LimitExceeded));
    }
}

#[test]
fn only_reached_arm_stores_count_toward_the_dispatch_ceiling() {
    // 2 initialization dispatches, iterations of 4 test + 2 conditional test +
    // 3 selected RHS + 1 store + (1 jump for the consequent) + 1 discard +
    // 1 loop back edge, 4 final loop test dispatches, and the tail expression.
    for (iterations, tail, conditional) in [
        (1259, 10, "true?n=n+1:n=n=n+10000"),
        (1364, 9, "false?n=n=n+10000:n=n+1"),
    ] {
        let body = format!("let n=0;while(n<{iterations}){{{conditional};}}");
        let evaluate = |prefix| {
            parse_script(&format!("{body}{}n", "+ ".repeat(prefix)))
                .unwrap()
                .evaluate()
        };
        assert_eq!(evaluate(tail), Ok(f64::from(iterations)));
        assert_eq!(evaluate(tail + 1), Err(EvalError::DispatchLimitExceeded));
    }
}

#[test]
fn flat_chains_share_the_nesting_budget_but_do_not_grow_the_operand_stack() {
    let depth = 1000;
    let limits = Limits {
        max_nesting_depth: depth,
        ..Limits::default()
    };
    // The first link shares the group record; the conditional adds a slot.
    // The next declarator's flat chain reuses all of those slots.
    let prefix = "let x;const y=(";
    let links = "x=".repeat(depth - 1);
    let flat = "x=".repeat(depth);
    let source = format!("{prefix}{links}true?null:0),z={flat}0;y");
    let script = parse_script_with_limits(&source, limits).unwrap();
    assert_eq!(script.instruction_count(), 2 * depth + 10);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Null));

    let error = parse_script_with_limits(
        &source,
        Limits {
            max_nesting_depth: depth - 1,
            ..limits
        },
    )
    .unwrap_err();
    let start = prefix.len() + links.len() + "true".len();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + 1
        }
    );

    let prefix = "let x;const y=";
    let source = format!("{prefix}{}null;y", "x=".repeat(depth + 1));
    let error = parse_script_with_limits(&source, limits).unwrap_err();
    let start = prefix.len() + 2 * depth + 1;
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + 1
        }
    );
}
