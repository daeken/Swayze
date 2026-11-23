use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

fn assert_number(source: &str, expected: f64) {
    let script = parse_script(source).unwrap_or_else(|e| panic!("{source}: {e}"));
    let Value::Number(actual) = script.evaluate_value().unwrap() else {
        panic!("expected Number: {source}");
    };
    if expected.is_nan() {
        assert!(actual.is_nan(), "{source}: got {actual:?}, expected NaN");
    } else {
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "{source}: got {actual:?}, expected {expected:?}"
        );
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
            end: prefix.len() + token.len()
        },
        "{source}: {error}"
    );
    assert!(error.message().contains(message), "{source}: {error}");
}

#[test]
fn ordinary_powers_coerce_primitives_including_unary_rhs() {
    for (source, expected) in [
        ("2**3**2", 512.0),
        ("(2**3)**2", 64.0),
        ("2*3**2 + 32/2**3 + 9%2**3", 23.0),
        ("(-2)**3", -8.0),
        ("2**-2", 0.25),
        ("(-2)**-3", -0.125),
        ("2**(-2)**2", 16.0),
        ("4**4**-.5 * -2", -4.0),
        ("4**.5", 2.0),
        ("true**true", 1.0),
        ("false**true", 0.0),
        ("2**!true", 1.0),
        ("2**~0", 0.5),
        ("2**void 1", f64::NAN),
        ("(void 1)**null", 1.0),
        ("null**-true", f64::INFINITY),
        ("(-4)**0.5", f64::NAN),
        ("(-4)**-.5", f64::NAN),
    ] {
        assert_number(source, expected);
    }
}

#[test]
fn nan_and_zero_exponents_are_checked_before_the_base() {
    for base in ["0", "(-0)", "1", "(-1)", "1e309", "(-1e309)", "2", "(0/0)"] {
        for exponent in ["undefined", "(0/0)"] {
            assert_number(&format!("{base}**{exponent}"), f64::NAN);
        }
        for exponent in ["0", "-0"] {
            assert_number(&format!("{base}**{exponent}"), 1.0);
        }
    }
    for exponent in ["2", "-2", "1e309", "-1e309"] {
        assert_number(&format!("undefined**{exponent}"), f64::NAN);
    }
}

#[test]
fn infinite_exponents_depend_only_on_base_magnitude() {
    for (base, positive, negative) in [
        ("0", 0.0, f64::INFINITY),
        ("(-0)", 0.0, f64::INFINITY),
        (".5", 0.0, f64::INFINITY),
        ("(-.5)", 0.0, f64::INFINITY),
        ("1", f64::NAN, f64::NAN),
        ("(-1)", f64::NAN, f64::NAN),
        ("2", f64::INFINITY, 0.0),
        ("(-2)", f64::INFINITY, 0.0),
        ("1e309", f64::INFINITY, 0.0),
        ("(-1e309)", f64::INFINITY, 0.0),
    ] {
        assert_number(&format!("{base}**1e309"), positive);
        assert_number(&format!("{base}**-1e309"), negative);
    }
}

#[test]
fn zero_and_infinite_base_signs_use_the_parity_of_the_represented_exponent() {
    for (exponent, odd) in [
        ("3", true),
        ("-3", true),
        ("2", false),
        ("-2", false),
        (".5", false),
        ("-.5", false),
        ("9007199254740991", true),
        ("-9007199254740991", true),
        ("9007199254740992", false),
        ("-9007199254740992", false),
        ("9007199254740993", false),
        ("-9007199254740993", false), // Rounded when parsed.
    ] {
        for (base, infinite, negative) in [
            ("0", false, false),
            ("(-0)", false, true),
            ("1e309", true, false),
            ("(-1e309)", true, true),
        ] {
            let magnitude = if infinite != exponent.starts_with('-') {
                f64::INFINITY
            } else {
                0.0
            };
            let expected = if negative && odd {
                -magnitude
            } else {
                magnitude
            };
            assert_number(&format!("{base}**{exponent}"), expected);
        }
    }
    assert_number("(-1)**9007199254740991", -1.0);
    assert_number("(-1)**9007199254740993", 1.0);
    // Sign is applied to the magnitude even if the finite power over/underflows.
    assert_number("(-2)**2049", f64::NEG_INFINITY);
    assert_number("(-2)**-2049", -0.0);
}

#[test]
fn grouping_controls_unary_bases_without_leaking_across_conditional_or_sequence_boundaries() {
    for (source, expected) in [
        ("-(2**2)", -4.0),
        ("(+(2))**3", 8.0),
        ("(~1)**2", 4.0),
        ("(!0)**2", 1.0),
        ("2**-(2**2)", 0.0625),
        ("true ? 2**-2 : 0, (-2)**2", 4.0),
        ("(0, -2)**2", 4.0),
        ("2**(false ? 3 : 1, -2)", 0.25),
        ("+(false ? 5 : 2**2)", 4.0),
        ("0**0 ? 2**+2 : 3", 4.0),
        ("let x=-2, y=x**2; y", 4.0),
    ] {
        assert_number(source, expected);
    }
    for source in [
        "-2 @**@ 2",
        "+2@**@2",
        "!2@**@2",
        "~2@**@2",
        "void 2 @**@ 2",
        "void (2)@**@2",
        "-(2**2)@**@2",
        "2**-2@**@2",
        "2*(-2 + +2@**@2)",
        "true ? 1 : -2@**@2",
        "false && void 2 @**@ 2",
        "(1, -2@**@2)",
        "const x=void 2@**@2; x",
        "let x=2; x**=void 1@**@2; x",
    ] {
        assert_diagnostic(source, ErrorKind::Malformed, "unary base");
    }
}

#[test]
fn longest_match_is_local_to_contiguous_punctuators() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, expected) in [
        ("2/*💡*/**// line\u{2028}-2", 0.25),
        ("2\n**2", 4.0),
        ("2**/*💡*/+2", 4.0),
    ] {
        assert_number(source, expected);
    }
    for (source, kind, message) in [
        ("@**@2", Malformed, "operand"),
        ("2**@@", Malformed, "operand"),
        ("2**@*@2", Malformed, "operand"),
        ("2**@**@2", Malformed, "operand"),
        ("2**@*=@2", Unsupported, "assignment expressions"),
        ("2@**=@=2", Unsupported, "assignment expressions"),
        ("2 * @*@ 2", Malformed, "operand"),
        ("2*/*💡*/@*@2", Malformed, "operand"),
        ("2/**/**@*@2", Malformed, "operand"),
        (
            "let x=2;x**/*💡*/@=@2;x",
            Unsupported,
            "assignment expressions",
        ),
        (
            "let x=2;x**@++@x",
            Unsupported,
            "increment and decrement expressions",
        ),
    ] {
        assert_diagnostic(source, kind, message);
    }
}

#[test]
fn compound_power_uses_the_existing_standalone_store_rules() {
    for (source, expected) in [
        ("let x=2;x/*💡\n*/**=x**2;x", 16.0),
        ("let x=-2;x**=(null, true ? -3 : 2);x", -0.125),
        ("let x=1;x**=1e309;x", f64::NAN),
        ("let x;x**=false;x", 1.0),
        (
            "let x=2;for(x**=2;x<16;x**=2){let x=3;x**=2;continue;}x",
            16.0,
        ),
        ("let x=2;if(false){x**=undefined;}x", 2.0),
    ] {
        assert_number(source, expected);
    }
    for (source, message) in [
        ("let x=2;(x)@**=@3;x", "assignment expressions"),
        ("let x=2;0,x@**=@3;x", "assignment expressions"),
        ("let x=2;const y=x@**=@3;y", "assignment expressions"),
        ("let x=2;false ? x@**=@3 : 1", "assignment expressions"),
        ("let x=2;for(;x@**=@2;){}x", "assignment expressions"),
        ("let x=2;x**=x@**=@2;x", "assignment expressions"),
        ("let x=2;x**=1@,@2;x", "sequence"),
        ("let x=2;x**=2\n@x@", "explicit semicolons"),
        (
            "const x=2;for(;false;x@**=@missing){}x",
            "assignment to const",
        ),
        (
            "let x=2;if(false){@x@**=2;let x=1;}x",
            "before lexical binding initialization",
        ),
        ("@undefined@**=2;1", "unbound name"),
        ("let x=2;if(false){x**=@missing@;}x", "unbound name"),
        ("true ? 1 : 2**@missing@", "unbound name"),
    ] {
        assert_diagnostic(source, ErrorKind::Unsupported, message);
    }
}

#[test]
fn flat_right_associative_chains_are_iterative_and_skipped_paths_still_count() {
    let chain = "1**".repeat(2046);
    for (extra, expected) in [
        (0, Ok(Value::Boolean(false))),
        (1, Err(EvalError::LimitExceeded)),
    ] {
        let prefix = "+ ".repeat(1 + extra);
        let source = format!("false && {chain}{prefix}1");
        if extra != 0 {
            assert_eq!(
                parse_script(&source).unwrap_err().kind(),
                ErrorKind::LimitExceeded(Limit::Tokens)
            );
        }
        let script = parse_script_with_limits(
            &source,
            Limits {
                max_tokens: 4096 + extra,
                max_nesting_depth: 0,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(script.instruction_count(), 4096 + extra);
        assert_eq!(script.evaluate_value(), expected);
    }
}

#[test]
fn each_reached_power_dispatches_once_and_skipped_powers_cost_no_fuel() {
    // Four literal/initialization dispatches, 13 dispatches per completed iteration
    // (test, `**=` store, increment, back edge), four final test dispatches, one final load.
    let program = "let x=1;let i=0;while(i<1259){x**=1;i++;}";
    for (test, expected) in [
        (true, Ok(1.0)),
        (false, Err(EvalError::DispatchLimitExceeded)),
    ] {
        // The short circuit costs 3. The conditional-and-discard costs 5 for
        // the Number arm (including the jump over the other arm), 6 for the power.
        let source = format!("{program}false && 1**1; {test} ? 1 : 1**1; x");
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
}
