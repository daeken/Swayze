use swayze_js::{ErrorKind, Limit, Limits, Span, Value, parse_script, parse_script_with_limits};

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
fn only_direct_names_at_admitted_rhs_starts_can_begin_a_chain() {
    for source in [
        "let a=0,b=0,c=0; let x = a + b @=@ c; x",
        "let a; let x = -a @=@ 1; x",
        "let a; let x = true && a @=@ 1; x",
        "let a; let x = true ? a @=@ 1 : 0; x",
        "let a; let x = false ? 0 : a @=@ 1; x",
        "let a; let x = (0, a @=@ 1); x",
        "let a; if (a @=@ 1) {} a",
        "let a; let x = (a) @=@ 1; x",
        "let a; let x = a @||=@ 1; x",
        "let a; let x = a = a++ @=@ 1; x",
    ] {
        assert_marked_error(source, ErrorKind::Unsupported, "assignment expressions");
    }
    for (source, message) in [
        ("let x; (x=1@,@x)", "sequence in an assignment RHS"),
        ("let x,y; (x=y=1@,@x)", "sequence in an assignment RHS"),
        ("let x,y; x=y=1@,@x; x", "sequence in an assignment RHS"),
        (
            "let x,y; x=y=1@@",
            "assignments require explicit semicolons",
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
