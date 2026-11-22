use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

fn assert_boolean(source: &str, expected: bool) {
    let script = parse_script(source).unwrap();
    assert_eq!(
        script.evaluate_value(),
        Ok(Value::Boolean(expected)),
        "{source}"
    );
    assert_eq!(
        script.evaluate(),
        Err(EvalError::NonNumericResult),
        "{source}"
    );
}

#[test]
fn strict_comparisons_compare_tags_and_values_without_coercion() {
    for (lhs, rhs, equal) in [
        ("true", "true", true),
        ("false", "false", true),
        ("true", "false", false),
        ("true", "1", false),
        ("false", "-0", false),
        ("1", "1.0", true),
        ("-2", "2", false),
        ("+0", "-0", true),
        ("0/0", "0/0", false),
        ("0/0", "false", false),
        ("1/0", "1e309", true),
        ("-1/0", "-1e309", true),
        ("1/0", "-1/0", false),
    ] {
        for (lhs, rhs) in [(lhs, rhs), (rhs, lhs)] {
            assert_boolean(&format!("{lhs}==={rhs}"), equal);
            assert_boolean(&format!("{lhs}!=={rhs}"), !equal);
        }
    }
    assert_boolean("const n=0/0; n!==n", true);
}

#[test]
fn relational_comparisons_convert_both_operands_and_handle_special_numbers() {
    for (lhs, rhs, [lt, le, gt, ge]) in [
        ("false", "true", [true, true, false, false]),
        ("true", "false", [false, false, true, true]),
        ("true", "1", [false, true, false, true]),
        ("0.5", "true", [true, true, false, false]),
        ("false", "-0", [false, true, false, true]),
        ("-0", "+0", [false, true, false, true]),
        ("+0", "-0", [false, true, false, true]),
        ("-1/0", "1/0", [true, true, false, false]),
        ("1/0", "1e309", [false, true, false, true]),
        ("-1/0", "-1e309", [false, true, false, true]),
        ("1/0", "true", [false, false, true, true]),
        ("0/0", "0/0", [false; 4]),
        ("0/0", "false", [false; 4]),
        ("true", "0/0", [false; 4]),
        ("0/0", "1e309", [false; 4]),
        ("-1", "0/0", [false; 4]),
    ] {
        for (op, expected) in [("<", lt), ("<=", le), (">", gt), (">=", ge)] {
            assert_boolean(&format!("{lhs}{op}{rhs}"), expected);
        }
    }
}

#[test]
fn comparisons_follow_precedence_and_reduce_left_to_right() {
    for (source, expected) in [
        ("2 + 2 < 3", false),
        ("3 < 2 + 2", true),
        ("2 * 3 >= 7 - 1", true),
        ("true === 2 < 1", false),
        ("2 < 1 === false", true),
        ("3 < 2 < 1", true),
        ("3 < (2 < 1)", false),
        ("2 < 1 <= 0", true),
        ("2 === 2 === true", true),
        ("2 === (2 === true)", false),
        ("true !== 1 === false", false),
        ("1 !== 1 !== false", false),
        ("(1 < 2) + 3 === 4", true),
        ("1 /* before */\n<=/**/2 === true", true),
    ] {
        assert_boolean(source, expected);
    }
}

#[test]
fn comparisons_integrate_with_conditionals_and_binding_statements() {
    for (source, expected) in [
        ("3 < 2 === false ? -7 : false", Value::Number(-7.0)),
        ("false !== 0 ? 1 < 2 : 1 > 2", Value::Boolean(true)),
        ("1 > (false ? -1 : true)", Value::Boolean(false)),
        (
            "true > 2 ? 7 : true >= 1 ? false : 8",
            Value::Boolean(false),
        ),
        (
            "let x=1; const wasNumber=x===1; x=x>=true; x=x===wasNumber ? x : 0; x!==1",
            Value::Boolean(true),
        ),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
    }
}

#[test]
fn rejects_unsupported_punctuators_as_whole_tokens() {
    for (op, width) in [
        ("==", 2),
        ("!=", 2),
        ("<<", 2),
        (">>", 2),
        (">>>", 3),
        ("<<=", 3),
        (">>=", 3),
        (">>>=", 4),
        (">>>>=", 3),
        ("<<==", 3),
        ("==>", 2),
        ("=>", 2),
        ("!", 1),
    ] {
        let prefix = "let x=1; x\u{00a0}";
        let source = format!("{prefix}{op}2");
        let error = parse_script(&source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported, "{source}");
        assert_eq!(
            error.span(),
            Span {
                start: prefix.len(),
                end: prefix.len() + width
            },
            "{source}"
        );
    }
}

#[test]
fn rejects_malformed_comparisons_and_does_not_join_punctuators_across_trivia() {
    for (source, kind, start, end) in [
        ("===1", ErrorKind::Malformed, 0, 3),
        ("1!==", ErrorKind::Malformed, 4, 4),
        ("1 < ;", ErrorKind::Malformed, 4, 5),
        ("1><2", ErrorKind::Malformed, 2, 3),
        ("1</**/<2", ErrorKind::Malformed, 6, 7),
        ("1</**/=2", ErrorKind::Unsupported, 6, 7),
        ("1==/**/=2", ErrorKind::Unsupported, 1, 3),
        ("1!/**/==2", ErrorKind::Unsupported, 1, 2),
        ("1====2", ErrorKind::Unsupported, 4, 5),
        ("1>==2", ErrorKind::Unsupported, 3, 4),
        ("true ? false : 1 >=", ErrorKind::Malformed, 19, 19),
        ("true ? false : 1 == 2", ErrorKind::Unsupported, 17, 19),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), kind, "{source}");
        assert_eq!(error.span(), Span { start, end }, "{source}");
    }
}

#[test]
fn comparison_tokens_charge_each_byte_and_each_push_and_emission_once() {
    for op in ["===", "!==", "<", "<=", ">", ">="] {
        let source = format!("true{op}false");
        let limits = Limits {
            max_tokens: 3,
            max_work: source.len() + 4,
            max_nesting_depth: 0,
            ..Limits::default()
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 3);
        assert_eq!(script.max_stack_depth(), 2);
        for (limits, limit, start, end) in [
            (
                Limits {
                    max_tokens: 2,
                    ..limits
                },
                Limit::Tokens,
                4 + op.len(),
                5 + op.len(),
            ),
            (
                Limits {
                    max_work: limits.max_work - 1,
                    ..limits
                },
                Limit::Work,
                4,
                4 + op.len(),
            ),
        ] {
            let error = parse_script_with_limits(&source, limits).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{source}");
            assert_eq!(error.span(), Span { start, end }, "{source}");
        }
    }

    let source = "let x=0;x!==false";
    let limits = Limits {
        max_tokens: 8,
        // Source bytes, one conversion byte, two binding lookup units,
        // five emitted instructions, and one pending operator push.
        max_work: source.len() + 9,
        ..Limits::default()
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
    let error = parse_script_with_limits(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(error.span(), Span { start: 9, end: 12 });

    // Even a rejected punctuator is one token and charges all its bytes.
    for (work, kind, start) in [
        (7, ErrorKind::Unsupported, 1),
        (6, ErrorKind::LimitExceeded(Limit::Work), 4),
    ] {
        let limits = Limits {
            max_tokens: 2,
            max_work: work,
            ..Limits::default()
        };
        let error = parse_script_with_limits("1>>>=2", limits).unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.span(), Span { start, end: 5 });
    }
}

#[test]
fn long_comparison_chains_are_iterative_and_count_even_in_a_skipped_arm() {
    let chain = "<true".repeat(2044);
    let limits = Limits {
        max_tokens: 4097,
        max_nesting_depth: 1,
        ..Limits::default()
    };
    let at_cap =
        parse_script_with_limits(&format!("true===true?false:-true{chain}"), limits).unwrap();
    assert_eq!(at_cap.instruction_count(), 4096);
    assert_eq!(at_cap.max_stack_depth(), 2);
    assert_eq!(at_cap.evaluate_value(), Ok(Value::Boolean(false)));

    let over_cap =
        parse_script_with_limits(&format!("true===true?false:- -true{chain}"), limits).unwrap();
    assert_eq!(over_cap.instruction_count(), 4097);
    assert_eq!(over_cap.evaluate_value(), Err(EvalError::LimitExceeded));
}
