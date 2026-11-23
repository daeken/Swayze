use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn eager_prefixes_store_and_yield_the_same_number() {
    for (initial, assignment, expected) in [
        ("true", "+=2*3", 7.0),
        ("false", "-=true", -1.0),
        ("null", "*=-2", -0.0),
        ("8", "/=-0", f64::NEG_INFINITY),
        ("-6", "%=3", -0.0),
        ("2", "**=3**2", 512.0),
        ("-1", "&=4294967298.9", 2.0),
        ("6", "^=null ?? 3", 5.0),
        ("undefined", "|=true", 1.0),
        ("true", "<<=31", -2147483648.0),
        ("-4", ">>=33", -2.0),
        ("-1", ">>>=32", 4294967295.0),
    ] {
        for tail in ["result", "x"] {
            let source = format!("let x={initial}; const result=x/*☃*/{assignment}; {tail}");
            let actual = parse_script(&source).unwrap().evaluate().unwrap();
            assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
        }
    }
}

#[test]
fn mixed_chains_snapshot_old_values_and_finish_from_right_to_left() {
    for (source, expected) in [
        ("let x=8; let r=x/=x=2; r*10+x", 44.0),
        ("let x=2; let r=x += x = 5; x * 10 + r", 77.0),
        ("let x=8; let r=x-=x*=x=2; r*10+x", -88.0),
        ("let x=8,y=2; (x -= y <<= 1)*100+x*10+y", 444.0),
        ("let x=8,y=2; x /= y **= 2; x*10+y", 24.0),
        ("let x=8,y=0; let r=x-=y=++x; r*100+x*10+y", -101.0),
        (
            "const x=8; let out; { let x=4; out = x = x **= 2; } out+x",
            24.0,
        ),
        (
            "let i,n=1,s=0; for(i=n<<=1; i<5; i=n+=1){s+=i;continue;} s*10+i",
            95.0,
        ),
    ] {
        let script = parse_script(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
    }
}

#[test]
fn logical_prefixes_skip_or_take_the_rhs_and_pass_the_tag_to_outer_writes() {
    use Value::{Boolean, Null, Number, Undefined};
    for (op, initial, rhs, expected, take) in [
        ("&&=", "-0", "true", Number(-0.0), false),
        ("&&=", "true", "undefined", Undefined, true),
        ("||=", "2", "true", Number(2.0), false),
        ("||=", "0/0", "-0", Number(-0.0), true),
        ("??=", "false", "true", Boolean(false), false),
        ("??=", "null", "null", Null, true),
        ("??=", "0/0", "true", Number(f64::NAN), false),
    ] {
        let body = format!("let x={initial},n=0,out; const result=out=x{op}(n++,{rhs});");
        for (tail, expected) in [
            ("x", expected),
            ("out", expected),
            ("result", expected),
            ("n", Number(f64::from(u8::from(take)))),
        ] {
            let source = format!("{body} {tail}");
            let actual = parse_script(&source).unwrap().evaluate_value().unwrap();
            match (actual, expected) {
                (Number(actual), Number(expected)) if expected.is_nan() => {
                    assert!(actual.is_nan(), "{source}");
                }
                (Number(actual), Number(expected)) => {
                    assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
                }
                _ => assert_eq!(actual, expected, "{source}"),
            }
        }
    }
}

#[test]
fn logical_skips_resume_at_the_immediate_enclosing_instruction() {
    for (source, expected) in [
        // The skipped RHS must feed the enclosing += with y's old value.
        ("let x=2,y=3; let r=x += y ||= 9; x*100+r*10+y", 553.0),
        // The skipped consequent must still jump over the alternate's write.
        (
            "let x=2,alt=0; let r=true ? x ||= 9 : alt += 1; r*100+x*10+alt",
            220.0,
        ),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn ternary_arms_finish_before_the_join_and_join_before_an_outer_write() {
    for (first, y, z, expected) in [
        ("true", 2, 0, 8200.0),   // Consequent skips to the arm's closing jump.
        ("true", 0, 2, 13720.0),  // Consequent takes and writes.
        ("false", 0, 2, 8020.0),  // Alternate skips to the enclosing binary operation.
        ("false", 2, 0, 13277.0), // Alternate takes and writes through the nested conditional.
    ] {
        let source = format!(
            "let x=6,y={y},z={z},n=0;
             const v=x+= {first} ? y||=7 : z||=false ? n=9 : n+=7;
             v*1000 + y*100 + z*10 + n"
        );
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
    for (initial, expected) in [(1, 10.0), (0, 11.0)] {
        let source = format!("let x={initial},y=0; const v=x||=true ? y+=1 : y+=9; v*10+y");
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn control_tests_and_noninitial_sequence_operands_accept_compounds() {
    for (source, expected) in [
        ("let x=0; if (x||=2) { x++; } x", 3.0),
        ("let x; if(false){} else if (x??=2) { x++; } x", 3.0),
        ("let x=3,n=0; while(x-=1){n+=x; continue;} n*10+x", 30.0),
        (
            "let x=3,n=0; do {n+=x; continue;} while(x-=1); n*10+x",
            60.0,
        ),
        ("let n=0; for(let x=3; x-=1;){n+=x; continue;} n", 3.0),
        (
            "let x=1,n=0; if(x||=99, x+=2, false){} else {n++;} x*10+n",
            31.0,
        ),
        ("let x=8,y=2; const v=x/= y += 2, w=x+y; v*10+w", 26.0),
        ("let x=1,y=0; true ? x||=9 : y=8, y+=x, x*10+y", 11.0),
        ("let x=2; (x+=1, x*=2, x+1)", 7.0),
        ("let x=1; {0,x+=2;} x", 3.0),
        ("let x; for(0,x??=2;x;0,x-=1){continue;} x", 0.0),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
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
        }
    );
    assert!(error.message().contains(message), "{source}: {error}");
}

#[test]
fn skipped_targets_and_rhs_are_resolved_in_source_order() {
    for (source, message) in [
        (
            "const x=true; false ? x@||=@missing : 0",
            "assignment to const",
        ),
        ("const x=false; 0,x@&&=@missing", "assignment to const"),
        ("let x=true; const v=x||=@missing@+=1; v", "unbound name"),
        ("let x=true; const v=x||=@missing@; v", "unbound name"),
        (
            "let x=0; { let v=true ? 1 : @x@ **= 3; let x; } x",
            "before lexical binding initialization",
        ),
        ("if(true){} else if(@undefined@ ??= 1){} 0", "unbound name"),
        ("do {let x; break;} while(@x@+=1);0", "unbound name"),
    ] {
        assert_marked_error(source, ErrorKind::Unsupported, message);
    }
    assert_marked_error(
        "let x=true; const v=x||=false ?? true @||@ false; v",
        ErrorKind::Malformed,
        "cannot be mixed",
    );
}

#[test]
fn delimiters_and_leading_standalone_dispatch_keep_their_existing_roles() {
    for source in [
        "let x=0,y=2; x=y+=1@,@x; x",
        "let x=0; x ||= 1@,@2; x",
        "let x=0; for(x+=1@,@true;false;){} x",
        "let x=0; for(;false;x+=1@,@true){} x",
    ] {
        assert_marked_error(
            source,
            ErrorKind::Unsupported,
            "sequence in an assignment RHS",
        );
    }
    assert_marked_error(
        "let x=0; x+=1@@",
        ErrorKind::Unsupported,
        "assignments require explicit semicolons",
    );
    assert_marked_error(
        "let x=0; x+=1;@@",
        ErrorKind::Unsupported,
        "a final expression is required",
    );
    assert_marked_error(
        "let x=0; true ? x+=1@,@2 : 0",
        ErrorKind::Malformed,
        "expected ':' before ','",
    );
    assert_marked_error(
        "let x=0, y@+=@1; x",
        ErrorKind::Unsupported,
        "cannot initialize declarations",
    );
}

#[test]
fn compound_prefixes_have_inclusive_token_work_and_depth_budgets() {
    let source = "let x=true;null,x+=x||=false,x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 14,
        max_nesting_depth: 2,
        // 13 emissions (including the precharged final read), two name charges,
        // two pending pushes, and three two-unit lookups (one per target/read).
        max_work: source.len() + 13 + 2 + 2 + 3 * 2,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 13);
    assert_eq!(script.max_stack_depth(), 2);
    assert_eq!(script.evaluate(), Ok(2.0));
    for (limits, limit, start, width) in [
        (
            Limits {
                max_tokens: 13,
                ..limits
            },
            Limit::Tokens,
            source.len() - 1,
            1,
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            source.len() - 1,
            1,
        ),
        (
            Limits {
                max_nesting_depth: 1,
                ..limits
            },
            Limit::NestingDepth,
            source.find("||=").unwrap(),
            3,
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{error}");
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
fn long_eager_chains_use_one_pending_slot_and_saved_operand_per_link() {
    let depth = 512;
    let source = format!("let x=1;0,{}1", "x+=".repeat(depth));
    let limits = Limits {
        max_nesting_depth: depth,
        ..Limits::default()
    };
    let script = parse_script_with_limits(&source, limits).unwrap();
    assert_eq!(script.instruction_count(), 5 + 3 * depth);
    assert_eq!(script.max_stack_depth(), depth + 1);
    assert_eq!(script.evaluate(), Ok(513.0));
    let error = parse_script_with_limits(
        &source,
        Limits {
            max_nesting_depth: depth - 1,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    let start = source.rfind("+=").unwrap();
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + 2
        }
    );
}

#[test]
fn skipped_compound_rhs_still_counts_toward_prepared_ceiling() {
    let limits = Limits {
        max_tokens: 5000,
        ..Limits::default()
    };
    for (count, expected) in [
        (4096, Ok(Value::Boolean(true))),
        (4097, Err(EvalError::LimitExceeded)),
    ] {
        let source = format!("let x=true; null,x||={}false", "!".repeat(count - 8));
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), count);
        assert_eq!(script.max_stack_depth(), 1);
        assert_eq!(script.evaluate_value(), expected);
    }
}

#[test]
fn dispatch_budget_counts_only_reached_rhs_and_store() {
    // Four init dispatches, two final test dispatches, and 12 or 14 per
    // iteration, depending on whether the RHS literal and store are reached.
    for (initial, iterations, tail) in [("true", 1364, 9), ("false", 1169, 11)] {
        let body = format!("let x={initial};let n={iterations};while(n){{0,x||=false;n-=1;}}");
        let eval = |prefix| {
            parse_script(&format!("{body}{}n", "+ ".repeat(prefix)))
                .unwrap()
                .evaluate()
        };
        assert_eq!(eval(tail), Ok(0.0));
        assert_eq!(eval(tail + 1), Err(EvalError::DispatchLimitExceeded));
    }
}
