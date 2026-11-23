use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

fn assert_value(source: &str, expected: Value) {
    let script = parse_script(source).unwrap_or_else(|error| panic!("{source:?}: {error}"));
    assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
}

#[test]
fn nullish_truthiness_and_comparisons() {
    use Value::{Boolean, Null, Number, Undefined};
    for source in ["null", "undefined"] {
        assert_value(&format!("!{source}"), Boolean(true));
        assert_value(&format!("{source} ? 1 : 2"), Number(2.0));
        assert_value(&format!("{source} || 3"), Number(3.0));
        assert_value(&format!("let x=4; if ({source}) {{x=5;}} x"), Number(4.0));
        assert_value(
            &format!("let x=6; while ({source}) {{x=7; break;}} x"),
            Number(6.0),
        );
        assert_value(&format!("{source} === {source}"), Boolean(true));
        assert_value(&format!("{source} !== {source}"), Boolean(false));
        for other in ["0", "false"] {
            assert_value(&format!("{source} === {other}"), Boolean(false));
            assert_value(&format!("{other} !== {source}"), Boolean(true));
        }
    }
    assert_value("null && true", Null);
    assert_value("undefined && true", Undefined);
    assert_value("null === undefined", Boolean(false));
    assert_value("undefined !== null", Boolean(true));
    for source in ["null < 1", "false <= null", "null > -1", "null >= -0"] {
        assert_value(source, Boolean(true));
    }
    for op in ["<", "<=", ">", ">="] {
        assert_value(&format!("undefined {op} null"), Boolean(false));
        assert_value(&format!("null {op} undefined"), Boolean(false));
    }
}

#[test]
fn coalescing_preserves_selected_tags_and_is_not_a_truthiness_test() {
    use Value::{Boolean, Null, Number, Undefined};
    for (source, result) in [
        ("null ?? 5", Number(5.0)),
        ("undefined ?? true", Boolean(true)),
        ("false ?? true", Boolean(false)),
        ("0 ?? 9", Number(0.0)),
        ("1/0 ?? 9", Number(f64::INFINITY)),
        ("null ?? undefined", Undefined),
        ("undefined ?? null", Null),
        ("null ?? undefined ?? false ?? 3", Boolean(false)),
        ("(null ?? 2) + (undefined ?? 3)", Number(5.0)),
        ("null/* one */??// two\n .5", Number(0.5)),
        ("let x=null ?? undefined; x=x ?? false; x", Boolean(false)),
    ] {
        assert_value(source, result);
    }
    let negative_zero = parse_script("-0 ?? 1").unwrap().evaluate().unwrap();
    assert_eq!(negative_zero.to_bits(), (-0.0_f64).to_bits());
    assert!(matches!(
        parse_script("0/0 ?? 1").unwrap().evaluate_value(),
        Ok(Number(n)) if n.is_nan()
    ));
}

#[test]
fn coalescing_precedence_and_grouping_respect_ternary_boundaries() {
    use Value::{Boolean, Number};
    for (source, result) in [
        ("null ?? 1 === 2", Boolean(false)),
        ("false ?? 1 === 2", Boolean(false)),
        ("null === undefined ?? 7", Boolean(false)),
        ("null ?? 1 < 2", Boolean(true)),
        ("0 ?? 1 ? 2 : 3", Number(3.0)),
        ("null ?? 0 ? 2 : 3", Number(3.0)),
        ("null ?? (0 ? 2 : 3)", Number(3.0)),
        ("3 ?? 0 ? 2 : 4", Number(2.0)),
        ("(null ?? 0) || 7", Number(7.0)),
        ("null ?? (0 || 7)", Number(7.0)),
        ("(0 || null) ?? 7", Number(7.0)),
        ("false || (0 ?? 7)", Number(0.0)),
        ("(undefined ?? 1) && 7", Number(7.0)),
        ("undefined ?? (1 && 7)", Number(7.0)),
        ("(1 && null) ?? 7", Number(7.0)),
        ("1 && (0 ?? 7)", Number(0.0)),
        ("true || false ? null ?? 8 : 9", Number(8.0)),
        ("null ?? false ? 8 : 0 || 9", Number(9.0)),
        ("true ? null ?? 8 : 0 || 9", Number(8.0)),
        ("false ? false && 8 : null ?? 9", Number(9.0)),
        ("(true ? null ?? false : 3 || 4) ?? 7", Boolean(false)),
        (
            "true ? (0 || 1) ? null ?? 2 : 3 && 4 : null ?? 5",
            Number(2.0),
        ),
        ("(1 || (true ? null ?? 2 : 3)) && 4", Number(4.0)),
        (
            "let x=null; if (x ?? true) {x=undefined ?? 6;} x",
            Number(6.0),
        ),
    ] {
        assert_value(source, result);
    }
}

#[test]
fn unparenthesized_mixing_is_malformed_even_in_skipped_paths() {
    for op in ["&&", "||"] {
        for (source, conflict) in [
            (format!("null??false{op}7"), op),
            (format!("true{op}false??7"), "??"),
            (format!("(null??(false){op}7)"), op),
            (format!("null??1/* trivia */===2{op}7"), op),
            (format!("true{op}!false===true??7"), "??"),
            (format!("true ? 1 : null??false{op}7"), op),
            (format!("false ? true{op}false??7 : 1"), "??"),
            (format!("if (false) {{let x=true{op}false??7;}} 1"), "??"),
        ] {
            let start = source.rfind(conflict).unwrap();
            let error = parse_script(&source).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Malformed, "{source}: {error}");
            assert_eq!(
                error.span(),
                Span {
                    start,
                    end: start + 2
                },
                "{source}"
            );
        }
    }
    for source in [
        "null??1&&2||3",
        "true||false&&null??1",
        "true&&false||null??1",
    ] {
        assert_eq!(
            parse_script(source).unwrap_err().kind(),
            ErrorKind::Malformed,
            "{source}"
        );
    }
}

#[test]
fn nullish_punctuation_uses_maximal_munch_without_exposing_unsupported_operators() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, start, end) in [
        ("??null", Malformed, 0, 2),
        ("null??", Malformed, 6, 6),
        ("null???1:2", Malformed, 6, 7),
        ("null??;", Malformed, 6, 7),
        ("null?/**/?1:2", Malformed, 9, 10),
        ("null??=1", Unsupported, 4, 7),
        ("undefined?.x", Unsupported, 9, 11),
        ("null==undefined", Unsupported, 4, 6),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), kind, "{source}: {error}");
        assert_eq!(error.span(), Span { start, end }, "{source}");
    }
    assert_value("null?.1:2", Value::Number(2.0));
}

#[test]
fn undefined_is_a_name_with_an_immutable_unbound_read_default() {
    use Value::{Boolean, Null, Number, Undefined};
    for (source, result) in [
        ("undefined", Undefined),
        ("let undefined=8; undefined=0; undefined??7", Number(0.0)),
        ("const undefined=false; undefined", Boolean(false)),
        (
            "let undefined=1; if(true){let undefined=2; undefined=3;} undefined",
            Number(1.0),
        ),
        (
            "let undefined=0; if (true) {if(true){undefined=4;}} undefined",
            Number(4.0),
        ),
        (
            "let x=3; if (undefined) {let undefined=1; x=2;} x",
            Number(3.0),
        ),
        (
            "let x=0; if(false){let undefined=1;}else{x=undefined;} x",
            Undefined,
        ),
        ("if (true) {const undefined=1;} undefined", Undefined),
        ("let undefined=null; undefined", Null),
        (
            "const undefined0=1; const null$=2; undefined0 + null$",
            Number(3.0),
        ),
    ] {
        assert_value(source, result);
    }
    for (source, start, end) in [
        ("undefined=1; undefined", 0, 9),
        ("if(false){undefined=1;} 2", 10, 19),
        ("const undefined=undefined; 0", 16, 25),
        ("const x=undefined; let undefined=1; x", 8, 17),
        (
            "let undefined=1; if(false){let x=0??undefined; let undefined=2;} undefined",
            36,
            45,
        ),
        (
            "let undefined=1; if(false){undefined=2; let undefined=3;} undefined",
            27,
            36,
        ),
        ("0??missing", 3, 10),
        ("Undefined", 0, 9),
        ("undefined0", 0, 10),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported, "{source}: {error}");
        assert_eq!(error.span(), Span { start, end }, "{source}");
    }
    let write = parse_script("const undefined=1; undefined=2; 3").unwrap_err();
    assert_eq!(write.kind(), ErrorKind::Unsupported);
    assert_eq!(write.span(), Span { start: 28, end: 29 });
    let duplicate = parse_script("let undefined=1; const undefined=2; 3").unwrap_err();
    assert_eq!(duplicate.kind(), ErrorKind::Malformed);
    assert_eq!(duplicate.span(), Span { start: 23, end: 32 });
}

#[test]
fn coalescing_preserves_exact_work_token_and_nesting_limits() {
    let source = "null??undefined";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 3,
        max_nesting_depth: 0,
        max_work: source.len() + 4, // Three emissions and one pending push.
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Undefined));
    assert_eq!(script.instruction_count(), 3);
    assert_eq!(script.max_stack_depth(), 1);
    for (limits, kind, span) in [
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            Span { start: 6, end: 15 },
        ),
        (
            Limits {
                max_tokens: 2,
                ..limits
            },
            Limit::Tokens,
            Span { start: 6, end: 7 },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(kind));
        assert_eq!(error.span(), span);
    }
}

#[test]
fn skipped_rhs_still_counts_toward_the_instruction_ceiling() {
    let chain = format!("+0{}", "??null".repeat(2047));
    let at_cap = parse_script(&chain).unwrap();
    assert_eq!(at_cap.instruction_count(), 4096);
    assert_eq!(at_cap.max_stack_depth(), 1);
    assert_eq!(at_cap.evaluate(), Ok(0.0));
    let over_cap = parse_script_with_limits(
        &chain.replacen("??null", "??+null", 1),
        Limits {
            max_tokens: 4097,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(over_cap.instruction_count(), 4097);
    assert_eq!(over_cap.evaluate_value(), Err(EvalError::LimitExceeded));
}

#[test]
fn coalescing_dispatches_only_the_reached_instructions() {
    let run = |lhs, final_prefix_count| {
        let prefix = "+ ".repeat(final_prefix_count);
        let source = format!("let n=1637; while (n) {{n=n-1; let x={lhs}??+null;}} {prefix}n");
        parse_script(&source).unwrap().evaluate()
    };
    // Initialization and final test cost four total, then 10 per loop when the
    // RHS is skipped, then ten for the final result: 4 + 1637*10 + 10 = 16384.
    assert_eq!(run("0", 9), Ok(0.0));
    assert_eq!(run("0", 10), Err(EvalError::DispatchLimitExceeded));
    assert_eq!(run("null", 9), Err(EvalError::DispatchLimitExceeded));
}
