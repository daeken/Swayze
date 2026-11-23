use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn compound_operators_reuse_binary_coercion_and_preserve_number_bits() {
    for (expression, assignment, expected) in [
        ("true", "+=2*3", 7.0),
        ("false", "-=true", -1.0),
        ("null", "*=-2", -0.0),
        ("-0", "/=true", -0.0),
        ("-6", "%=(false || 3)", -0.0),
        ("-0", "**=(true ? 3 : 2)", -0.0),
        ("-1", "&=4294967298.9", 2.0),
        ("6", "^=(null ?? 3)", 5.0),
        ("undefined", "|=true", 1.0),
        ("true", "<<=31", -2147483648.0),
        ("-4", ">>=33", -2.0),
        ("-1", ">>>=33", 2147483647.0),
        ("-0", "|=null", 0.0),
        ("8", "/=true ? (null ?? 2) : 1", 4.0),
        ("2", "*=(x+1)", 6.0),
    ] {
        let source = format!("let x={expression}; x/* name */{assignment}; x");
        let script = parse_script(&source).unwrap();
        let Value::Number(actual) = script.evaluate_value().unwrap() else {
            panic!("expected Number: {source}");
        };
        assert_eq!(actual.to_bits(), f64::to_bits(expected), "{source}");
    }
    assert!(
        parse_script("let x=0;x*=undefined;x")
            .unwrap()
            .evaluate()
            .unwrap()
            .is_nan()
    );
    assert_eq!(
        parse_script("let x=8;x/* before *//=-0;x")
            .unwrap()
            .evaluate(),
        Ok(f64::NEG_INFINITY)
    );
}

#[test]
fn compound_for_clauses_respect_order_jumps_and_scope() {
    for (source, expected) in [
        (
            "let i=0;let trace=0;
             if (true) {
                 const step=1;
                 for (i+=step; i<5; i+=step) {
                     if (i===2) {continue;}
                     trace*=10;
                     trace+=i;
                     if (i===3) {break;}
                 }
             }
             trace*10+i",
            133.0,
        ),
        (
            "let i=0;let sum=0;
             if (true) {
                 const step=1;
                 for (i+=step; i<=3; i+=step) {
                     let i=7;
                     let step=2;
                     i<<=step;
                     step*=10;
                     sum+=i+step;
                     continue;
                 }
             }
             sum*10+i",
            1444.0,
        ),
        ("let i=8;for(i>>=1;false;i*=2){i+=10;}i", 4.0),
        (
            "const x=1;let out=0;while(!out){let x=2;x*=3;out+=x;}out*10+x",
            61.0,
        ),
        (
            "let out=0;if(true){let undefined=2;undefined+=3;out=undefined;}out",
            5.0,
        ),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
        assert_eq!(script.evaluate(), Ok(expected), "{source}");
    }
}

fn assert_diagnostic(marked: &str, kind: ErrorKind, message: &str) {
    let (prefix, marked) = marked.split_once('@').unwrap();
    let (token, suffix) = marked.split_once('@').unwrap();
    let source = format!("{prefix}{token}{suffix}");
    let error = parse_script(&source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start: prefix.len(),
            end: prefix.len() + token.len(),
        },
        "{source}: {error}"
    );
    assert!(error.message().contains(message), "{source}: {error}");
}

#[test]
fn target_is_resolved_before_rhs_reads_and_never_skips_a_shadow() {
    for (source, message) in [
        ("@x@+=missing;42", "unbound name"),
        ("@undefined@+=1;42", "unbound name"),
        ("const x=0;x@+=@missing;42", "assignment to const"),
        (
            "let x=0;if(false){const x=1;x@>>>=@1;}42",
            "assignment to const",
        ),
        (
            "let x=0;if(false){@x@+=1;const x=2;}42",
            "before lexical binding initialization",
        ),
        (
            "let i=0;if(false){for(@i@+=1;false;){}let i=2;}42",
            "before lexical binding initialization",
        ),
        ("const i=0;for(;false;i@&=@1){}42", "assignment to const"),
        ("let i=0;for(;false;i+=@x@){let x=1;}42", "unbound name"),
        (
            "let i=0;for(;;){break;i+=@x@;let x=2;}42",
            "before lexical binding initialization",
        ),
    ] {
        assert_diagnostic(source, ErrorKind::Unsupported, message);
    }
}

#[test]
fn rejects_other_targets_operators_and_assignment_expressions() {
    use ErrorKind::{Malformed, Unsupported};
    for op in ["++", "--"] {
        assert_diagnostic(&format!("let x=1;x@{op}@2;x"), Unsupported, "subset");
    }
    for (source, kind, message) in [
        ("let x=1;x@.@p+=2;x", Unsupported, "token"),
        ("let x=1;x@[@0]+=2;x", Unsupported, "token"),
        ("let x=1;(x)@+=@2;x", Unsupported, "assignment expressions"),
        ("let x=1;x+=x@*=@2;x", Unsupported, "assignment expressions"),
        (
            "let x=1;for(;x@+=@1;){}x",
            Unsupported,
            "assignment expressions",
        ),
        ("let x@+=@1;x", Unsupported, "declarations"),
        (
            "let x=1;x >>>/**/ @=@2;x",
            Unsupported,
            "assignment expressions",
        ),
        ("let x=1;x>>>=@=@2;x", Unsupported, "assignment expressions"),
        ("let x=1;x+=2@@", Unsupported, "explicit semicolons"),
        ("let x=1;x+=2\n@x@", Unsupported, "explicit semicolons"),
        (
            "let x=1;if(true){x+=2@}@x",
            Unsupported,
            "explicit semicolons",
        ),
        ("let x=1;x+=2;@@", Unsupported, "final expression"),
        ("let x=1;x+=@;@x", Malformed, "operand"),
        ("let x=1;for(x+=0@)@{}x", Malformed, "between for clauses"),
    ] {
        assert_diagnostic(source, kind, message);
    }
}

#[test]
fn logical_assignments_select_and_preserve_primitive_values() {
    use Value::{Boolean, Null, Number, Undefined};
    for (initial, and, or, nullish) in [
        ("true", Number(42.0), Boolean(true), Boolean(true)),
        ("false", Boolean(false), Number(42.0), Boolean(false)),
        ("2", Number(42.0), Number(2.0), Number(2.0)),
        ("-0", Number(-0.0), Number(42.0), Number(-0.0)),
        ("0/0", Number(f64::NAN), Number(42.0), Number(f64::NAN)),
        ("null", Null, Number(42.0), Number(42.0)),
        ("undefined", Undefined, Number(42.0), Number(42.0)),
    ] {
        for (op, expected) in [("&&=", and), ("||=", or), ("??=", nullish)] {
            let source = format!("let x={initial};x{op}42;x");
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
    for (source, value) in [
        ("let x=1;x&&=null;x", Null),
        ("let x=false;x||=undefined;x", Undefined),
        ("let x=null;x??=false;x", Boolean(false)),
    ] {
        assert_eq!(parse_script(source).unwrap().evaluate_value(), Ok(value));
    }
}

#[test]
fn logical_for_clauses_run_in_order_and_target_the_enclosing_scope() {
    for (source, value) in [
        (
            "let i=0;let trace=0;
             for(i||=1;i<5;i||=3){
                 let j=undefined;
                 j??=i;
                 trace=trace*10+j;
                 if(i===1){i=0;continue;}
                 if(i===3){i=4;}else{break;}
             }
             trace*10+i",
            1344.0,
        ),
        (
            "const x=9;let out=0;
             if(true){
                 let x=null;
                 for(x??=1;x<2;x&&=2){let x=0;x||=7;out=x;continue;}
                 out=out*10+x;
             }
             out*10+x",
            729.0,
        ),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate(), Ok(value), "{source}");
        assert_eq!(script.evaluate(), Ok(value), "{source}");
    }
}

#[test]
fn inactive_logical_assignments_still_validate_rhs_and_selected_targets() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, message) in [
        (
            "const x=0;x@&&=@missing;42",
            Unsupported,
            "assignment to const",
        ),
        ("const x=1;x@||=@2;42", Unsupported, "assignment to const"),
        (
            "let x=1;if(false){const x=false;x@??=@2;}42",
            Unsupported,
            "assignment to const",
        ),
        (
            "let x=0;if(false){@x@&&=2;const x=1;}42",
            Unsupported,
            "before lexical binding initialization",
        ),
        ("@undefined@??=1;42", Unsupported, "unbound name"),
        ("let x=1;x||=@missing@;x", Unsupported, "unbound name"),
        ("let x=1;x||=@;@x", Malformed, "operand"),
        (
            "let i=0;for(;false;i&&=@j@){let j=1;}i",
            Unsupported,
            "unbound name",
        ),
        (
            "let i=0;for(;;){break;i&&=@j@;let j=1;}i",
            Unsupported,
            "before lexical binding initialization",
        ),
    ] {
        assert_diagnostic(source, kind, message);
    }
}

#[test]
fn logical_assignment_does_not_participate_in_rhs_precedence() {
    for (source, expected) in [
        ("let x=0;x||=null??2+3*4;x", 14.0),
        ("let x=null;x??=false||true&&7;x", 7.0),
        ("let x=1;x&&=null??false?2:x+8;x", 9.0),
        ("let x=undefined;x??=null??(false||7);x", 7.0),
    ] {
        assert_eq!(parse_script(source).unwrap().evaluate(), Ok(expected));
    }
    for source in [
        "let x=0;x&&=null??false@||@7;x",
        "let x=1;x||=false&&null@??@7;x",
    ] {
        assert_diagnostic(source, ErrorKind::Malformed, "cannot be mixed");
    }
}

#[test]
fn logical_assignment_punctuators_require_statement_targets_and_explicit_delimiters() {
    use ErrorKind::Unsupported;
    for (op, split) in [("&&=", "&&"), ("||=", "||"), ("??=", "??")] {
        for (source, message) in [
            (format!("let x=1;(x)@{op}@2;x"), "assignment expressions"),
            (format!("let x=1;(x@{op}@2)"), "assignment expressions"),
            (format!("let x=1;x=x@{op}@2;x"), "assignment expressions"),
            (
                format!("let x=1;for(;x@{op}@2;){{}}x"),
                "assignment expressions",
            ),
            (format!("let x@{op}@1;x"), "declarations"),
            (format!("let x=1;x@.@p{op}2;x"), "token"),
            (format!("let x=1;x{op}@=@2;x"), "assignment expressions"),
            (
                format!("let x=1;x{split}/**/@=@2;x"),
                "assignment expressions",
            ),
            (format!("let x=1;x{op}2\n@x@"), "explicit semicolons"),
        ] {
            assert_diagnostic(&source, Unsupported, message);
        }
    }
}

#[test]
fn compound_clauses_charge_each_token_emission_and_target_lookup() {
    let source = "let i=0;for(i+=1;false;i>>>=2){break;}i";
    // Source bytes, three decimal conversions, one header and one frame,
    // 17 instructions, a break patch, and three binding lookups at two units each.
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 22,
        max_nesting_depth: 1,
        max_work: source.len() + 29,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 17);
    assert_eq!(script.max_stack_depth(), 2);
    assert_eq!(script.evaluate(), Ok(1.0));
    for (limit, limits) in [
        (
            Limit::Tokens,
            Limits {
                max_tokens: 21,
                ..limits
            },
        ),
        (
            Limit::Work,
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        assert_eq!(
            error.span(),
            Span {
                start: source.len() - 1,
                end: source.len()
            }
        );
    }
}

#[test]
fn logical_clauses_charge_single_tokens_and_resolve_each_target_once() {
    let source = "let i=0;for(i||=1;false;i??=2){break;}i";
    // Source bytes, three decimal conversions, one header and one frame,
    // 17 instructions, a break patch, and three binding lookups at two units each.
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 22,
        max_nesting_depth: 1,
        max_work: source.len() + 29,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 17);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate(), Ok(1.0));
    for (limit, limits) in [
        (
            Limit::Tokens,
            Limits {
                max_tokens: 21,
                ..limits
            },
        ),
        (
            Limit::Work,
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        assert_eq!(
            error.span(),
            Span {
                start: source.len() - 1,
                end: source.len()
            }
        );
    }
}

#[test]
fn skipped_logical_rhs_counts_at_the_prepared_instruction_ceiling() {
    for (prefixes, expected) in [(4089, Ok(1.0)), (4090, Err(EvalError::LimitExceeded))] {
        let source = format!("let x=1;x||={}2;x", "+ ".repeat(prefixes));
        let limits = Limits {
            max_tokens: 8192,
            ..Limits::default()
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 7 + prefixes);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn skipped_logical_rhs_and_store_cost_no_dispatches_in_update() {
    // Eight dispatches for the declarations and taken init, 1259 iterations at
    // thirteen each (including the update's load and branch, but no RHS or
    // store), four for the falsy test, one final load, and the final prefixes.
    for (prefixes, expected) in [
        (3, Ok(1.0)),
        (4, Ok(1.0)),
        (5, Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!(
            "let x=0;let i=0;for(x||=1;i<1259;x||=+ + + + 2){{i+=1;continue;}}{}x",
            "+ ".repeat(prefixes),
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn skipped_compound_update_counts_at_the_prepared_instruction_ceiling() {
    for (prefixes, expected) in [(4083, Ok(42.0)), (4084, Err(EvalError::LimitExceeded))] {
        let source = format!(
            "let i=0;for(;false;i+={}0){{break;}}42",
            "+ ".repeat(prefixes)
        );
        let limits = Limits {
            max_tokens: 8192,
            ..Limits::default()
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 13 + prefixes);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn compound_init_and_repeated_updates_count_at_the_dispatch_ceiling() {
    // Six setup dispatches, 1488 continuing iterations at 11 each, four
    // for the falsy test, and one final load, plus the unary operators.
    for (prefixes, expected) in [
        (4, Ok(1489.0)),
        (5, Ok(1489.0)),
        (6, Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!(
            "let i=0;for(i+=1;i<1489;i+=1){{continue;}}{}i",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
}
