use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn root_and_braced_statements_discard_supported_expressions() {
    use Value::{Boolean, Null, Number, Undefined};
    for (source, expected) in [
        ("1; 2", Number(2.0)),
        ("0o7;0x1", Number(1.0)),
        ("true; ; {} null; // final expression", Null),
        ("true, false ? 1 : null; (2 + 1) === 3;", Boolean(true)),
        ("1; let x=40; x; ++x; x===41; x+=1; x", Number(42.0)),
        ("let x=40; {x; x++; x+=1; void x;} x", Number(42.0)),
        ("null; {false; const x=42; x;} const y=void 1; y", Undefined),
        (
            "let x=0; if(false){null;}else if(true){true; x=42;}else{false;} x",
            Number(42.0),
        ),
        (
            "let n=1; while(n){n; n--;} for(;n<2;n++){null,n;} do{{void n;}}while(false); n",
            Number(2.0),
        ),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
    }
    let script = parse_script("1; false;").unwrap();
    assert_eq!(script.evaluate(), Err(EvalError::NonNumericResult));
}

fn assert_error(marked: &str, width: usize, kind: ErrorKind, message: &str) {
    let (before, after) = marked.split_once('@').unwrap();
    let source = [before, after].concat();
    let error = parse_script(&source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start: before.len(),
            end: before.len() + width
        },
        "{source}"
    );
    assert!(error.message().contains(message), "{source}: {error}");
}

#[test]
fn only_tokens_after_the_root_semicolon_force_a_discard() {
    let script = parse_script("42; /* first ; */ \n // second ;").unwrap();
    assert_eq!(script.instruction_count(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Number(42.0)));
    assert_error("42;;@", 0, ErrorKind::Unsupported, "final expression");
}

#[test]
fn requires_explicit_boundaries_and_a_root_final_expression() {
    use ErrorKind::{Malformed, Unsupported};
    for source in [
        "1; {} /* trivia */@",
        "1; let x=2; // only statements@",
        "{1;}@",
    ] {
        assert_error(source, 0, Unsupported, "final expression");
    }
    for (source, width) in [
        ("1/*\u{2028}*/@2", 1),
        ("{1@} 2", 1),
        ("{1\n@} 2", 1),
        ("{1/*\u{2028}*/@} 2", 1),
        ("{undefined/* trailing trivia */@", 0),
        ("{1\n@let x=2;} 3", 3),
    ] {
        assert_error(source, width, Unsupported, "explicit semicolons");
    }
    assert_error(
        "{1; /* trailing trivia */@",
        0,
        Malformed,
        "expected a statement or '}'",
    );
    assert_error(
        "1; @/* never closed",
        15,
        Malformed,
        "unterminated block comment",
    );
    assert_error("{(1@;);} 2", 1, Malformed, "semicolon inside");
    assert_error("{true ? 1@;} 2", 1, Malformed, "expected ':'");
}

#[test]
fn statement_expressions_do_not_enable_other_syntax() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, width, message) in [
        ("let x=1; {x, x@=2;} x", 1, "assignment expressions"),
        (
            "let x=1; x + @++x; x",
            2,
            "increment and decrement expressions",
        ),
        (
            "let x=1; {(x@++);} x",
            2,
            "increment and decrement expressions",
        ),
        ("1; @\"use strict\"; 2", 1, "outside"),
        ("{@function f(){}; } 2", 8, "outside"),
        ("{(@{x: 1});} 2", 1, "braces are not expression tokens"),
    ] {
        assert_error(source, width, Unsupported, message);
    }
    // A leading '{' starts a block, so its content cannot be an object literal.
    assert_error("{1@: 2;} 3", 1, Malformed, "without a matching '?'");
}

#[test]
fn assignment_rhs_still_requires_parentheses_for_comma_sequences() {
    let script = parse_script("let x=0; (1,2); {1,2; x=(1,42);} x").unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Number(42.0)));
    for source in ["let x=0; 1,2; x=1@,42; x", "let x=0; {1,2; x=1@,42;} x"] {
        assert_error(
            source,
            1,
            ErrorKind::Unsupported,
            "assignment RHS requires parentheses",
        );
    }
}

#[test]
fn validates_discarded_expressions_and_resolves_later_declarations_before_checking_tdz() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, width) in [
        ("@later; let later=1; 2", 5),
        ("let x=1; {@x+x; let x=2;} x", 1),
        ("let x=1; if(false){true || @x; let x=2;} x", 1),
        ("for(;;){break; @undefined; const undefined=1;} 2", 9),
    ] {
        assert_error(source, width, Unsupported, "initialization");
    }
    assert_error(
        "while(false){true ? 1 : @missing;} 2",
        7,
        Unsupported,
        "unbound name",
    );
    assert_error("do{break; 1+@;}while(false); 2", 1, Malformed, "operand");
}

#[test]
fn exact_work_and_token_budgets_count_each_lookahead_and_discard_once() {
    let source = "{undefined/*💡*/;}true;/*💡*/null;";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 8,
        max_nesting_depth: 0,
        // One frame, five emissions, and one scope hop for `undefined`.
        max_work: source.len() + 7,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Null));
    assert_eq!(script.instruction_count(), 5);
    assert_eq!(script.max_stack_depth(), 1);

    let body_semicolon = source.find(';').unwrap();
    let root_semicolon = source.find("true;").unwrap() + "true".len();
    for (limit, limits, start, width) in [
        (
            Limit::Tokens,
            Limits {
                max_tokens: 7,
                ..limits
            },
            source.len() - 1,
            1,
        ),
        (
            Limit::Work,
            Limits {
                max_work: body_semicolon + 1 + 2,
                ..limits
            },
            body_semicolon,
            1,
        ),
        // Root lookahead scans the next token before charging the discard.
        (
            Limit::Work,
            Limits {
                max_work: source.len() - 1 + 4,
                ..limits
            },
            root_semicolon,
            1,
        ),
        (
            Limit::Work,
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            1,
            9,
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + width
            }
        );
    }
}

#[test]
fn skipped_discards_count_toward_the_exact_prepared_ceiling() {
    // Two test instructions, 2,046 literal/discard pairs, and two or three
    // final expression instructions. The branch skips every pair.
    for (tail, instructions, expected) in [
        ("void false", 4096, Ok(Value::Undefined)),
        ("void void false", 4097, Err(EvalError::LimitExceeded)),
    ] {
        let source = format!("if(false){{{}}}{tail}", "true;".repeat(2046));
        let script = parse_script_with_limits(
            &source,
            Limits {
                max_tokens: 4101,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(script.instruction_count(), instructions);
        assert_eq!(script.max_stack_depth(), 1);
        assert_eq!(script.evaluate_value(), expected);
    }
}

#[test]
fn reached_discards_cost_one_dispatch_each_at_the_inclusive_cap() {
    // Nine dispatches per iteration (two test, two expression statement,
    // four update, one back edge); two for initialization and two for exit.
    for (voids, expected) in [
        (7, Ok(Value::Undefined)),
        (8, Ok(Value::Undefined)),
        (9, Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!(
            "let n=1819;while(n){{true;n--;}}{}null",
            "void ".repeat(voids)
        );
        assert_eq!(parse_script(&source).unwrap().evaluate_value(), expected);
    }
}
