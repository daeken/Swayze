use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

fn assert_numbers(cases: &[(&str, f64)]) {
    for &(source, expected) in cases {
        let script = parse_script(source).unwrap_or_else(|err| panic!("{source}: {err}"));
        assert_eq!(
            script.evaluate().map(f64::to_bits),
            Ok(expected.to_bits()),
            "{source}"
        );
    }
}

#[test]
fn reduces_the_truncated_binary64_value_modulo_two_to_the_32nd() {
    assert_numbers(&[
        ("6.9 & 3.1", 2.0),
        ("-3.9 ^ 6.9", -5.0),
        ("~4294967297.875", -2.0),
        ("4294967297.875 | 0", 1.0),
        ("-4294967297.875 | 0", -1.0),
        ("-4294967295.75 | 0", 1.0),
        ("2147483647.9 | 0", 2_147_483_647.0),
        ("2147483648 | 0", -2_147_483_648.0),
        ("-2147483649 | 0", 2_147_483_647.0),
        ("4294967295 & -1", -1.0),
        ("4294967296 | 0", 0.0),
        ("9007199254740993 | 0", 0.0), // Rounded to 2^53 before conversion.
        ("9007199254740994 >>> 0", 2.0),
        ("-9223372036854777856 | 0", -2048.0), // -(2^63 + 2048), outside i64.
        ("1.7976931348623157e308 | 0", 0.0),
        ("-5e-324 | 0", 0.0),
        ("-.9999 >>> 0", 0.0),
    ]);
}

#[test]
fn maps_nonfinite_values_and_both_signed_zeros_to_integer_zero() {
    assert_numbers(&[
        ("~0", -1.0),
        ("~-0", -1.0),
        ("-0 | -0", 0.0),
        ("-0 << 31", 0.0),
        ("-0 >> 31", 0.0),
        ("-0 >>> 0", 0.0),
        ("(0 / 0) | 7", 7.0),
        ("(1 / 0) & -1", 0.0),
        ("-1e9999 >>> 3", 0.0),
        ("5 << (0 / 0)", 5.0),
        ("-7 >> (1 / 0)", -7.0),
        ("-7 >>> -1e9999", 4_294_967_289.0),
    ]);
}

#[test]
fn coerces_all_four_primitive_types_before_bitwise_operations() {
    assert_numbers(&[
        ("~true", -2.0),
        ("~false", -1.0),
        ("~null", -1.0),
        ("~undefined", -1.0),
        ("true & 3.5", 1.0),
        ("-2 | true", -1.0),
        ("true ^ undefined", 1.0),
        ("false | null", 0.0),
        ("null << true", 0.0),
        ("true << true", 2.0),
        ("-4 >> undefined", -4.0),
        ("-4 >>> null", 4_294_967_292.0),
        ("let n = true; n = n << n; n | undefined", 2.0),
    ]);
}

#[test]
fn masks_shift_counts_and_distinguishes_sign_extension_from_zero_fill() {
    assert_numbers(&[
        ("3 << 31", -2_147_483_648.0),
        ("1 << 32", 1.0),
        ("1 << -1.9", -2_147_483_648.0),
        ("1 << 4294967329.75", 2.0),
        ("8 >> -4294967294.9", 2.0),
        ("2147483648 >> 0", -2_147_483_648.0),
        ("2147483648 >>> 0", 2_147_483_648.0),
        ("-2 >> 1", -1.0),
        ("-2 >>> 1", 2_147_483_647.0),
        ("-1 >>> 32", 4_294_967_295.0),
        ("-1 >>> -1", 1.0),
        ("-2147483648 >> 31", -1.0),
        ("-2147483648 >>> 31", 1.0),
    ]);
}

#[test]
fn matches_precedence_and_associativity_with_other_expression_operators() {
    assert_numbers(&[
        ("~2 * 3", -9.0),
        ("~-1", 0.0),
        ("~!0", -2.0),
        ("1 << 1 + 2 * 3", 128.0),
        ("1 + 1 << 3 + 1", 32.0),
        ("64 >> 2 << 1 >>> 1", 16.0),
        ("2 & 1 === 0", 0.0),
        ("2 === 2 & 3", 1.0),
        ("1 ^ 3 & 2", 3.0),
        ("1 | 3 ^ 1", 3.0),
        ("1 | 0 && 2", 2.0),
        ("1 || 0 | 2", 1.0),
        ("0 && 9 | 2 || 3", 3.0),
        ("null ?? 1 | 2 ?? 4", 3.0),
        ("null ?? (false | 1 && 7)", 7.0),
        ("(null ?? 1 | 2) && 6", 6.0),
        ("true || 0 | 1 ? null ?? 2 ^ 1 : 0", 3.0),
        ("let n = 0; if (true & 3) { n = 1 << 2; } n", 4.0),
    ]);
    for (source, expected) in [
        ("!~0", Value::Boolean(false)),
        ("1 << 2 < 3", Value::Boolean(false)),
        ("3 > 1 << 2", Value::Boolean(false)),
        ("1 === 2 < 3", Value::Boolean(false)),
        ("false | false ? true : null", Value::Null),
        ("false ?? 1 | 2", Value::Boolean(false)),
        ("true || undefined & null", Value::Boolean(true)),
        ("false && undefined ^ null", Value::Boolean(false)),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate_value(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn bitwise_operators_do_not_hide_mixing_of_nullish_and_logical_operators() {
    for (source, second_op) in [
        ("null ?? false ^ 6 && 7", "&&"),
        ("null ?? 1 & 2 | 3 || 7", "||"),
        ("false || 1 | 2 ?? 7", "??"),
        ("true && false & 1 ?? 7", "??"),
    ] {
        let error = parse_script(source).unwrap_err();
        let start = source.rfind(second_op).unwrap();
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

#[test]
fn recognizes_whole_compounds_and_does_not_join_across_trivia() {
    for op in ["&=", "^=", "|=", "<<=", ">>=", ">>>=", "&&=", "||="] {
        let prefix = "let x=1;(0,x\u{00a0}";
        let source = format!("{prefix}{op}2)");
        let error = parse_script(&source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported, "{source}");
        assert_eq!(
            error.span(),
            Span {
                start: prefix.len(),
                end: prefix.len() + op.len()
            },
            "{source}"
        );
    }
    for (source, kind, start, end) in [
        ("1<<==2", ErrorKind::Unsupported, 1, 4),
        ("1>>>>=2", ErrorKind::Malformed, 4, 6), // `>>>` then `>=`.
        ("1<<<2", ErrorKind::Malformed, 3, 4),
        ("1>=>>2", ErrorKind::Malformed, 3, 5),
        ("1>>> =2", ErrorKind::Unsupported, 5, 6),
        ("1^^2", ErrorKind::Malformed, 2, 3),
        ("1>/**/>>2", ErrorKind::Malformed, 6, 8),
        ("1<<\n<2", ErrorKind::Malformed, 4, 5),
        ("1&/**/=2", ErrorKind::Unsupported, 6, 7),
        ("1~=2", ErrorKind::Unsupported, 1, 2), // No `~=` punctuator.
        ("~", ErrorKind::Malformed, 1, 1),
        ("1 <<", ErrorKind::Malformed, 4, 4),
        ("false ? 1>>> : 2", ErrorKind::Malformed, 13, 14),
    ] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), kind, "{source}");
        assert_eq!(error.span(), Span { start, end }, "{source}");
    }
    assert_numbers(&[("~~3", 3.0), ("1/* before */>>/* after */1", 0.0)]);
}

#[test]
fn operators_charge_bytes_one_token_one_push_and_one_instruction() {
    for op in ["&", "^", "|", "<<", ">>", ">>>", "~"] {
        let unary = op == "~";
        let lhs = if unary { "" } else { "true" };
        let source = format!("{lhs}{op}false");
        let limits = Limits {
            max_source_bytes: source.len(),
            max_tokens: if unary { 2 } else { 3 },
            max_work: source.len() + if unary { 3 } else { 4 },
            max_nesting_depth: 0,
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), limits.max_tokens);
        assert_eq!(script.max_stack_depth(), if unary { 1 } else { 2 });
        for (limits, limit, start, end) in [
            (
                Limits {
                    max_tokens: limits.max_tokens - 1,
                    ..limits
                },
                Limit::Tokens,
                lhs.len() + op.len(),
                lhs.len() + op.len() + 1,
            ),
            (
                Limits {
                    max_work: limits.max_work - 1,
                    ..limits
                },
                Limit::Work,
                lhs.len(),
                lhs.len() + op.len(),
            ),
        ] {
            let error = parse_script_with_limits(&source, limits).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{source}");
            assert_eq!(error.span(), Span { start, end }, "{source}");
        }
    }

    // The fourth byte of `>>>=` costs work before rejection in an expression.
    for (max_work, kind, start) in [
        (7, ErrorKind::Unsupported, 1),
        (6, ErrorKind::LimitExceeded(Limit::Work), 4),
    ] {
        let limits = Limits {
            max_tokens: 2,
            max_work,
            ..Limits::default()
        };
        let error = parse_script_with_limits("1>>>=2", limits).unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.span(), Span { start, end: 5 });
    }
}

#[test]
fn long_prefix_and_infix_chains_are_iterative_and_keep_the_prepared_cap() {
    let limits = Limits {
        max_tokens: 4097,
        max_nesting_depth: 0,
        ..Limits::default()
    };
    let prefixes = "~".repeat(4095);
    let at_cap = parse_script_with_limits(&format!("{prefixes}false"), limits).unwrap();
    assert_eq!(at_cap.instruction_count(), 4096);
    assert_eq!(at_cap.max_stack_depth(), 1);
    assert_eq!(at_cap.evaluate(), Ok(-1.0));
    let over_cap = parse_script_with_limits(&format!("~{prefixes}false"), limits).unwrap();
    assert_eq!(over_cap.evaluate(), Err(EvalError::LimitExceeded));

    // The RHS is skipped but its two instructions per `^ false` still count.
    let chain = "^false".repeat(2046);
    let at_cap = parse_script_with_limits(&format!("true||~false{chain}"), limits).unwrap();
    assert_eq!(at_cap.instruction_count(), 4096);
    assert_eq!(at_cap.max_stack_depth(), 2);
    assert_eq!(at_cap.evaluate_value(), Ok(Value::Boolean(true)));
    let over_cap = parse_script_with_limits(&format!("true||~~false{chain}"), limits).unwrap();
    assert_eq!(over_cap.instruction_count(), 4097);
    assert_eq!(over_cap.evaluate_value(), Err(EvalError::LimitExceeded));
}

#[test]
fn bitwise_operations_each_cost_one_dispatch_in_a_loop() {
    let prefix = "let n=1819; while(n) { n=n-(true & true); }";
    // Two initializer dispatches, nine per iteration, two for the final
    // test, then one load and eight unary ops: exactly 16,384 dispatches.
    let at_cap = parse_script(&format!("{prefix}~~~~~~~~n")).unwrap();
    assert_eq!(at_cap.evaluate(), Ok(0.0));
    let over_cap = parse_script(&format!("{prefix}~~~~~~~~~n")).unwrap();
    assert_eq!(over_cap.evaluate(), Err(EvalError::DispatchLimitExceeded));
}
