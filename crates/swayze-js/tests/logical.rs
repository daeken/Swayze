use swayze_js::{ErrorKind, EvalError, Limits, Value, parse_script, parse_script_with_limits};

fn assert_value(source: &str, expected: Value) {
    let script = parse_script(source).unwrap_or_else(|error| panic!("{source}: {error}"));
    let actual = script.evaluate_value().unwrap();
    match (actual, expected) {
        (Value::Number(actual), Value::Number(expected)) if expected.is_nan() => {
            assert!(actual.is_nan(), "{source}: {actual:?}");
        }
        (Value::Number(actual), Value::Number(expected)) => {
            assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
        }
        _ => assert_eq!(actual, expected, "{source}"),
    }
}

#[test]
fn truthiness_matches_the_conditional_and_selected_values_keep_their_tags_and_bits() {
    use Value::{Boolean, Number};

    for (operand, value, truthy) in [
        ("false", Boolean(false), false),
        ("true", Boolean(true), true),
        ("0", Number(0.0), false),
        ("-0", Number(-0.0), false),
        ("0/0", Number(f64::NAN), false),
        ("1/0", Number(f64::INFINITY), true),
        ("1/-0", Number(f64::NEG_INFINITY), true),
        ("5e-324", Number(f64::from_bits(1)), true),
        ("-7", Number(-7.0), true),
    ] {
        assert_value(&format!("{operand} ? true : false"), Boolean(truthy));
        assert_value(&format!("!({operand})"), Boolean(!truthy));
        assert_value(
            &format!("{operand} && 29"),
            if truthy { Number(29.0) } else { value },
        );
        assert_value(
            &format!("{operand} || 29"),
            if truthy { value } else { Number(29.0) },
        );
        assert_value(&format!("true && {operand}"), value);
        assert_value(&format!("false || {operand}"), value);
    }
    for source in ["!0", "0 || false", "true && false", "true || 29"] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Err(EvalError::NonNumericResult)
        );
    }
}

#[test]
fn precedence_grouping_conditionals_and_bindings() {
    use Value::{Boolean, Number};

    for (source, expected) in [
        ("!!true", Boolean(true)),
        ("!2 * 0", Number(0.0)),
        ("!1===true", Boolean(false)),
        ("+!-0", Number(1.0)),
        ("0 && 1 + 2", Number(0.0)),
        ("7 || 1 < 2", Number(7.0)),
        ("0 && 1 === 2", Number(0.0)),
        ("2 || 0 && 9", Number(2.0)),
        ("0 && 1 || 2", Number(2.0)),
        ("(2 || 0) && 9", Number(9.0)),
        ("1 || 0 ? 3 : 4", Number(3.0)),
        ("0 && 1 ? 3 : 4", Number(4.0)),
        ("1 ? 0 || !0 && 7 : 9", Number(7.0)),
        ("0 ? 9 : 1 && (0 ? 5 : 2 || 9) + 1", Number(3.0)),
        (
            "0 /* lhs */\n|| !/* prefix */false // next\n&& 8",
            Number(8.0),
        ),
        (
            "let x = false || -0; const fallback = x || !x; x = fallback && (x ? 9 : x); x;",
            Number(-0.0),
        ),
    ] {
        assert_value(source, expected);
    }
    let script = parse_script("10 + (false || 2 * (true && 3 + 4 * 5))").unwrap();
    assert_eq!(script.max_stack_depth(), 5);
    assert_eq!(script.evaluate(), Ok(56.0));
}

#[test]
fn skipped_rhs_is_still_validated() {
    for (source, kind) in [
        ("true || missing", ErrorKind::Unsupported),
        ("false && missing", ErrorKind::Unsupported),
        ("const x = true || x; x", ErrorKind::Unsupported),
        ("const x = 1; false && (x = 2)", ErrorKind::Unsupported),
        ("true || (1 ? 2 : missing)", ErrorKind::Unsupported),
        ("false && (1 +)", ErrorKind::Malformed),
        ("true ||", ErrorKind::Malformed),
        ("!", ErrorKind::Malformed),
    ] {
        assert_eq!(parse_script(source).unwrap_err().kind(), kind, "{source}");
    }
}

#[test]
fn long_flat_chains_and_skipped_operands_respect_evaluation_limits() {
    let body = "false||".repeat(2047);
    let chain = parse_script(&format!("{body}true")).unwrap();
    assert_eq!(chain.instruction_count(), 4095);
    assert_eq!(chain.max_stack_depth(), 1);
    assert_eq!(chain.evaluate_value(), Ok(Value::Boolean(true)));

    let negations = "!".repeat(4093);
    let at_cap = parse_script(&format!("true||{negations}false")).unwrap();
    assert_eq!(at_cap.instruction_count(), 4096);
    assert_eq!(at_cap.max_stack_depth(), 1);
    assert_eq!(at_cap.evaluate_value(), Ok(Value::Boolean(true)));

    let over_cap = parse_script_with_limits(
        &format!("true||{negations}!false"),
        Limits {
            max_tokens: 4097,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(over_cap.instruction_count(), 4097);
    assert_eq!(over_cap.evaluate_value(), Err(EvalError::LimitExceeded));
}
