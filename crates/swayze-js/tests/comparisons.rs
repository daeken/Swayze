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
fn loose_comparisons_cover_the_primitive_cross_product() {
    // Expected equality classes: NaN has no class, not even with itself.
    let cases = [
        ("null", Some(0)),
        ("undefined", Some(0)),
        ("false", Some(1)),
        ("0", Some(1)),
        ("true", Some(2)),
        ("1", Some(2)),
        ("2", Some(3)),
        ("0/0", None),
    ];
    for (lhs, lhs_class) in cases {
        for (rhs, rhs_class) in cases {
            let equal = lhs_class.is_some() && lhs_class == rhs_class;
            assert_boolean(&format!("{lhs}=={rhs}"), equal);
            assert_boolean(&format!("{lhs}!={rhs}"), !equal);
        }
    }
}

#[test]
fn loose_comparisons_use_binary64_equality_not_truthiness_or_integer_conversion() {
    for (lhs, rhs, equal) in [
        ("+0", "-0", true),
        ("-0", "false", true),
        ("-0", "null", false),
        ("undefined", "0/0", false),
        ("0/0", "true", false),
        ("1/0", "1e309", true),
        ("-1/0", "-1e309", true),
        ("-1/0", "1/0", false),
        ("1/0", "true", false),
        ("5e-324", "false", false), // Smallest positive subnormal.
        ("1.0000000000000002", "true", false), // Next Number above one.
        ("9007199254740992", "9007199254740993", true), // Both round to 2^53.
    ] {
        for (lhs, rhs) in [(lhs, rhs), (rhs, lhs)] {
            assert_boolean(&format!("{lhs}=={rhs}"), equal);
            assert_boolean(&format!("{lhs}!={rhs}"), !equal);
        }
    }
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
fn loose_equality_shares_precedence_and_left_associativity_with_strict_equality() {
    for (source, expected) in [
        ("2 == 2 == 1", true),
        ("2 == (2 == 1)", false),
        ("2 != 2 != false", false),
        ("true == 1 === true", true),
        ("true === 1 != false", false),
        ("1 != 2 !== 1", true),
        ("2 !== 1 == 1", true),
        ("true == 2 < 1", false),
        ("1 + 1 << 1 != 1 + 3", false),
        ("1/* before */==/**/!false", true),
        ("1!=!false", false),
        ("true ? false : 1 == 2", false),
    ] {
        assert_boolean(source, expected);
    }
}

#[test]
fn loose_equality_integrates_with_bindings_and_lazy_control() {
    use Value::{Boolean, Number};
    for (source, expected) in [
        ("2 & 1 == true", Number(0.0)),
        ("0 && true != 0", Number(0.0)),
        ("true || 0 != false", Boolean(true)),
        ("false ?? 1 == 0", Boolean(false)),
        ("null ?? (false != 1 && 7)", Number(7.0)),
        ("(null ?? true == 1) || 7", Boolean(true)),
        ("null == undefined && false != 1 ? 8 : 9", Number(8.0)),
        ("let x=0; x=1==true; x!=false", Boolean(true)),
        (
            "let n=true; let steps=0; while(n!=0){if(n==1){steps=steps+1;} n=n-1;} steps==true",
            Boolean(true),
        ),
        (
            "let x=true; if(x){const undefined=0; x=undefined==null;} x",
            Boolean(false),
        ),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
    }
}

#[test]
fn loose_equality_does_not_hide_forbidden_nullish_mixing() {
    for logical in ["&&", "||"] {
        for (source, conflict) in [
            (format!("null ?? 1 == true {logical} 7"), logical),
            (format!("true ? 1 : false {logical} 1 != true ?? 7"), "??"),
        ] {
            let start = source.rfind(conflict).unwrap();
            let error = parse_script(&source).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Malformed, "{source}");
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
}

#[test]
fn rejects_unsupported_punctuators_as_whole_tokens() {
    for (op, width) in [("=>", 2), ("!", 1)] {
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
        ("==1", ErrorKind::Malformed, 0, 2),
        ("1!=", ErrorKind::Malformed, 3, 3),
        ("1==>2", ErrorKind::Malformed, 3, 4), // `==` then `>`, not `=` then `=>`.
        ("1!===>2", ErrorKind::Unsupported, 4, 6), // `!==` then `=>`.
        ("1==/**/=2", ErrorKind::Unsupported, 7, 8),
        ("1!=/**/=2", ErrorKind::Unsupported, 7, 8),
        ("1=/**/=2", ErrorKind::Unsupported, 1, 2),
        ("1!/**/==2", ErrorKind::Unsupported, 1, 2),
        ("1====2", ErrorKind::Unsupported, 4, 5),
        ("1>==2", ErrorKind::Unsupported, 3, 4),
        ("true ? false : 1 >=", ErrorKind::Malformed, 19, 19),
        ("true ? false : 1 ==", ErrorKind::Malformed, 19, 19),
        ("let x==1; x", ErrorKind::Malformed, 5, 7),
        ("if(0){x==;}0", ErrorKind::Malformed, 9, 10),
        ("let x=1; x==((x)=0)", ErrorKind::Unsupported, 16, 17),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), kind, "{source}");
        assert_eq!(error.span(), Span { start, end }, "{source}");
    }
}

#[test]
fn comparison_tokens_charge_each_byte_and_each_push_and_emission_once() {
    for op in ["==", "!=", "===", "!==", "<", "<=", ">", ">="] {
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

    let source = "let x=0;x!=true";
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
    assert_eq!(error.span(), Span { start: 8, end: 9 }); // Deferred name comparison.

    // Even a rejected punctuator is one token and charges all its bytes.
    for (work, kind, start) in [
        (5, ErrorKind::Unsupported, 1),
        (4, ErrorKind::LimitExceeded(Limit::Work), 2),
    ] {
        let limits = Limits {
            max_tokens: 2,
            max_work: work,
            ..Limits::default()
        };
        let error = parse_script_with_limits("1=>2", limits).unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.span(), Span { start, end: 3 });
    }
}

#[test]
fn long_comparison_chains_are_iterative_and_count_even_in_a_skipped_arm() {
    let limits = Limits {
        max_tokens: 4097,
        max_nesting_depth: 1,
        ..Limits::default()
    };
    for rhs in ["<true", "==true", "!=true"] {
        let chain = rhs.repeat(2044);
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
}

#[test]
fn right_nested_loose_equality_reaches_the_operand_stack_ceiling() {
    let levels = 2047;
    let source = format!("{}!false{}", "true==(".repeat(levels), ")".repeat(levels));
    let script = parse_script_with_limits(
        &source,
        Limits {
            max_tokens: 4 * levels + 2,
            max_nesting_depth: levels,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(script.instruction_count(), 4096);
    assert_eq!(script.max_stack_depth(), 2048);
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
}

#[test]
fn loose_inequality_costs_one_dispatch_in_each_loop_test() {
    let prefix = "let n=1819; while(n!=false){n=n-1;}";
    // Two initializer dispatches, nine per iteration, four for the final
    // test, then one load and six unary ops: exactly 16,384 dispatches.
    let at_cap = parse_script(&format!("{prefix}!!!!!!n")).unwrap();
    assert_eq!(at_cap.evaluate_value(), Ok(Value::Boolean(false)));
    let over_cap = parse_script(&format!("{prefix}!!!!!!!n")).unwrap();
    assert_eq!(
        over_cap.evaluate_value(),
        Err(EvalError::DispatchLimitExceeded)
    );
}
