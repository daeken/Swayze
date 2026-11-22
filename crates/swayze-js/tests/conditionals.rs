use swayze_js::{ErrorKind, EvalError, Limits, Value, parse_script, parse_script_with_limits};

#[test]
fn numeric_toboolean_and_selected_number() {
    for (source, expected) in [
        ("0 ? 11 : 22", 22.0),
        ("-0 ? 11 : 22", 22.0),
        ("0 / 0 ? 11 : 22", 22.0),
        ("-(1e309 - 1e309) ? 11 : 22", 22.0),
        ("1 / 0 ? 11 : 22", 11.0),
        ("1 / -0 ? 11 : 22", 11.0),
        ("-1 ? 11 : 22", 11.0),
        ("5e-324 ? 11 : 22", 11.0),
        ("1 ? -0 : 0", -0.0),
        ("0 ? 0/0 : -0", -0.0),
        ("0 ? 2 : 1/0", f64::INFINITY),
    ] {
        let actual = parse_script(source).unwrap().evaluate().unwrap();
        assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
    }
    assert!(
        parse_script("1 ? 0/0 : 1")
            .unwrap()
            .evaluate()
            .unwrap()
            .is_nan()
    );
}

#[test]
fn precedence_right_associativity_and_grouping() {
    for (source, expected) in [
        ("1 + 2 * 3 ? 4 + 5 * 6 : 7", 34.0),
        ("1 - 1 ? 4 : 2 + 3 * 4", 14.0),
        ("2 * (0 ? 4 : 5) + 1", 11.0),
        ("-(0 ? 4 : 5) % 3", -2.0),
        ("1 ? 2 : 3 ? 4 : 5", 2.0),
        ("(1 ? 2 : 3) ? 4 : 5", 4.0),
        ("1 ? 0 ? 2 : 3 : 4", 3.0),
        ("0 ? 0 ? 2 : 3 : 4", 4.0),
        ("0 ? 1 : 0 ? 2 : 3 ? 4 : 5", 4.0),
        ("0 ? (1 ? 2 : 3) : (0 ? 4 : 5)", 5.0),
        ("1?.5:.25", 0.5),
        ("0 /* before */\n? 1 // true arm\n: 2;", 2.0),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn conditional_initializers_assignments_and_final_expression() {
    let script = parse_script(
        "let x = 0 ? 50 : 2 + 3;
         const step = x - 5 ? 50 : 3;
         x = x ? x * step : 0;
         const pick = (x - 15 ? 0 : step) + x;
         pick ? 2 * (x ? pick : 1) : 0;",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(36.0));
    assert_eq!(script.evaluate(), Ok(36.0));
}

#[test]
fn both_arms_are_parsed_but_remain_expression_only() {
    for source in [
        "1 ? 2 : missing",
        "0 ? missing : 2",
        "const x = 0 ? x : 1; x",
        "let x = 1; 0 ? (x = 2) : x",
        "let x = 1; x = 1 ? 2 : x = 3; x",
        "true ? 2 : null",
    ] {
        assert_eq!(
            parse_script(source).unwrap_err().kind(),
            ErrorKind::Unsupported,
            "{source}"
        );
    }
}

#[test]
fn skipped_instructions_count_toward_evaluation_ceiling() {
    let body = "- ".repeat(4091);
    let limits = Limits {
        max_tokens: 4097,
        ..Limits::default()
    };
    let at_cap = parse_script_with_limits(&format!("true?false:{body}0"), limits).unwrap();
    assert_eq!(at_cap.instruction_count(), 4096);
    assert_eq!(at_cap.evaluate_value(), Ok(Value::Boolean(false)));
    assert_eq!(at_cap.evaluate(), Err(EvalError::NonNumericResult));

    let over_cap = parse_script_with_limits(&format!("true?false:{body}-0"), limits).unwrap();
    assert_eq!(over_cap.instruction_count(), 4097);
    assert_eq!(over_cap.evaluate_value(), Err(EvalError::LimitExceeded));
    assert_eq!(over_cap.evaluate(), Err(EvalError::LimitExceeded));
}
