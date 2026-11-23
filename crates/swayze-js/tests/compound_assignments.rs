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
    for op in ["&&=", "||=", "??=", "**=", "++", "--"] {
        assert_diagnostic(&format!("let x=1;x@{op}@2;x"), Unsupported, "subset");
    }
    for (source, kind, message) in [
        ("let x=1;x@.@p+=2;x", Unsupported, "token"),
        ("let x=1;x@[@0]+=2;x", Unsupported, "token"),
        ("let x=1;(x)@+=@2;x", Unsupported, "assignment expressions"),
        ("let x=1;(x@+=@2)", Unsupported, "assignment expressions"),
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
