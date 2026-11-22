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
fn subtraction_associates_to_the_left_unless_grouped() {
    assert_eq!(evaluate("10 - 3 - 2"), 5.0);
    assert_eq!(evaluate("10 - (3 - 2)"), 9.0);
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
}

#[test]
fn arithmetic_preserves_signed_zero() {
    assert_eq!(evaluate("0 * (0 - 1)").to_bits(), (-0.0_f64).to_bits());
    assert_eq!(evaluate("0 - 0").to_bits(), 0.0_f64.to_bits());
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
