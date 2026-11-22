use swayze_js::{EvalError, Limits, parse_script, parse_script_with_limits};

fn evaluate(source: &str) -> f64 {
    parse_script(source)
        .expect("valid arithmetic script")
        .evaluate()
        .expect("validated script should evaluate")
}

#[test]
fn arithmetic_respects_precedence_and_operand_order() {
    for (source, expected) in [
        ("42", 42.0_f64),
        ("2 + 3 * 4", 14.0),
        ("20 - 3 * 4", 8.0),
        ("(20 - 3) * 4", 68.0),
        ("3 - 10", -7.0),
    ] {
        assert_eq!(evaluate(source).to_bits(), expected.to_bits(), "{source}");
    }
}

#[test]
fn fractional_and_exponential_literals_evaluate_as_numbers() {
    assert_eq!(evaluate("(.5 + 1.) * 1e+1 - 1.25E-1;"), 14.875);
    assert_eq!(evaluate("-1e-325").to_bits(), (-0.0_f64).to_bits());
    assert_eq!(evaluate("-.0").to_bits(), (-0.0_f64).to_bits());
    assert_eq!(evaluate("-1e309"), f64::NEG_INFINITY);
}

#[test]
fn subtraction_associates_to_the_left_unless_grouped() {
    assert_eq!(evaluate("10 - 3 - 2"), 5.0);
    assert_eq!(evaluate("10 - (3 - 2)"), 9.0);
}

#[test]
fn division_and_remainder_share_multiplication_precedence() {
    for (source, expected) in [
        ("2 + 20 / 5 * 3 % 5 - 1", 3.0),
        ("24 / 3 / 2", 4.0),
        ("24 / (3 / 2)", 16.0),
        ("20 % 8 % 5", 4.0),
        ("20 % (8 % 5)", 2.0),
        ("-2 + +3 * -4", -14.0),
        ("- + -(2 + 3) * -2", -10.0),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn remainder_truncates_the_quotient_toward_zero() {
    assert_eq!(evaluate("-7 % 4"), -3.0);
    assert_eq!(evaluate("7 % -4"), 3.0);
    assert_eq!(evaluate("-7 % -4"), -3.0);
    assert_eq!(evaluate("(-7 / 2) % 2"), -1.5);
    // The divisor rounds up; the exact quotient is below 100 even though
    // a separate binary64 division would round the quotient to 100.
    assert_eq!(evaluate("1 % (1 / 100)"), 0.00999999999999998);
}

#[test]
fn infinities_and_nan_are_numeric_results() {
    assert_eq!(evaluate("1 / 0"), f64::INFINITY);
    assert_eq!(evaluate("-1 / 0"), f64::NEG_INFINITY);
    assert_eq!(evaluate("1 / -0"), f64::NEG_INFINITY);
    assert_eq!(evaluate("+(1 / 0)"), f64::INFINITY);
    assert_eq!(evaluate("-(1 / 0)"), f64::NEG_INFINITY);
    assert_eq!(evaluate("-7 % (1 / 0)"), -7.0);
    for source in [
        "0 / 0",
        "(1 / 0) / (1 / 0)",
        "1 % 0",
        "1 % -0",
        "(1 / 0) % 2",
        "(0 / 0) % 2",
        "2 % (0 / 0)",
        "+ -(0 / 0)",
    ] {
        assert!(evaluate(source).is_nan(), "{source}");
    }
}

#[test]
fn each_operation_rounds_as_binary64() {
    // 2^53 + 1 rounds back to 2^53, whereas changing the grouping gives 1.
    assert_eq!(
        evaluate("(9007199254740992 + 1) - 9007199254740992").to_bits(),
        0.0_f64.to_bits()
    );
    assert_eq!(evaluate("9007199254740992 + (1 - 9007199254740992)"), 1.0);
    // (2^27 + 1)(2^27 - 1) rounds to 2^54 before subtraction.
    assert_eq!(
        evaluate("134217729 * 134217727 - 134217728 * 134217728").to_bits(),
        0.0_f64.to_bits()
    );
    // A result larger than u64::MAX need not overflow a host integer.
    assert_eq!(evaluate("65536 * 65536 * 65536 * 65536"), 2.0_f64.powi(64));
    assert_eq!(evaluate("1 / 49 * 49"), 0.9999999999999999);
    assert_eq!(
        evaluate("(9007199254740992 + 1) % 2").to_bits(),
        0.0_f64.to_bits(),
    );
}

#[test]
fn arithmetic_preserves_signed_zero() {
    for source in [
        "-0",
        "+(-0)",
        "- +0",
        "0 * (0 - 1)",
        "0 / -1",
        "-1 / (1 / 0)",
        "-4 % 2",
        "-0 % 2",
    ] {
        assert_eq!(evaluate(source).to_bits(), (-0.0_f64).to_bits(), "{source}");
    }
    for source in ["0 - 0", "+0", "- -0", "-(-0)", "4 % -2"] {
        assert_eq!(evaluate(source).to_bits(), 0.0_f64.to_bits(), "{source}");
    }
    let large = "9".repeat(308);
    assert_eq!(
        evaluate(&format!("-1 / {large} / {large}")).to_bits(),
        (-0.0_f64).to_bits()
    );
}

#[test]
fn custom_parse_limits_do_not_raise_the_execution_ceiling() {
    let source = "1+".repeat(2048) + "1";
    let script = parse_script_with_limits(
        &source,
        Limits {
            max_tokens: 4097,
            ..Limits::default()
        },
    )
    .expect("valid script under the custom token limit");
    assert_eq!(script.instruction_count(), 4097);
    assert_eq!(script.evaluate(), Err(EvalError::LimitExceeded));
}

#[test]
fn unary_operators_count_toward_the_execution_ceiling() {
    let source = "- ".repeat(4095) + "0";
    let script = parse_script(&source).unwrap();
    assert_eq!(script.instruction_count(), 4096);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate().unwrap().to_bits(), (-0.0_f64).to_bits());

    let script = parse_script_with_limits(
        &format!("+ {source}"),
        Limits {
            max_tokens: 4097,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(script.instruction_count(), 4097);
    assert_eq!(script.evaluate(), Err(EvalError::LimitExceeded));
}
