use swayze_js::{EvalError, Value, parse_script};

#[test]
fn typed_results_and_numeric_compatibility() {
    for (source, expected) in [
        ("true", Value::Boolean(true)),
        ("(false);", Value::Boolean(false)),
        ("2+3", Value::Number(5.0)),
        ("1/0", Value::Number(f64::INFINITY)),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
        let numeric_result = match expected {
            Value::Number(value) => Ok(value),
            Value::Boolean(_) => Err(EvalError::NonNumericResult),
        };
        assert_eq!(script.evaluate(), numeric_result, "{source}");
    }
}

#[test]
fn arithmetic_converts_both_operands_and_returns_numbers() {
    for (source, expected) in [
        ("+true", 1.0),
        ("+false", 0.0),
        ("-true", -1.0),
        ("true + true", 2.0),
        ("5 + false", 5.0),
        ("true - 3", -2.0),
        ("3 - true", 2.0),
        ("true * 4", 4.0),
        ("4 * false", 0.0),
        ("true / 2", 0.5),
        ("2 / true", 2.0),
        ("true / false", f64::INFINITY),
        ("true % 2", 1.0),
        ("3 % true", 0.0),
        ("-false", -0.0),
        ("-2 * false", -0.0),
        ("false / -2", -0.0),
        ("-1 % true", -0.0),
        ("true ? -false : +false", -0.0),
        ("const z=-false; z", -0.0),
    ] {
        let script = parse_script(source).unwrap();
        let Value::Number(actual) = script.evaluate_value().unwrap() else {
            panic!("expected Number: {source}");
        };
        assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
        assert_eq!(
            script.evaluate().unwrap().to_bits(),
            expected.to_bits(),
            "{source}"
        );
    }
    for source in ["false / false", "true % false"] {
        let script = parse_script(source).unwrap();
        assert!(matches!(script.evaluate_value(), Ok(Value::Number(n)) if n.is_nan()));
        assert!(script.evaluate().unwrap().is_nan());
    }
}

#[test]
fn lexical_slots_preserve_values_and_can_change_type_between_assignments() {
    for (source, expected) in [
        ("const x=true; x", Value::Boolean(true)),
        ("let x=false; x", Value::Boolean(false)),
        ("const x=7; let y=x; y", Value::Number(7.0)),
        ("let x=1; x=false; x", Value::Boolean(false)),
        ("let x=true; x=x?0:true; x", Value::Number(0.0)),
        (
            "let x=7; const before=x; x=true; const middle=x; x=2; middle ? before+x : false",
            Value::Number(9.0),
        ),
    ] {
        let script = parse_script(source).unwrap();
        assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
        assert_eq!(script.evaluate_value(), Ok(expected), "repeat: {source}");
    }
}

#[test]
fn mixed_conditionals_preserve_the_selected_arm_and_choose_by_truthiness() {
    for (source, expected) in [
        ("true ? 2 : false", Value::Number(2.0)),
        ("false ? 2 : false", Value::Boolean(false)),
        ("0 ? 2 : true", Value::Boolean(true)),
        ("-1 ? true : 0", Value::Boolean(true)),
        ("0 / 0 ? 2 : false", Value::Boolean(false)),
        (
            "(false ? 1 : true) ? false ? 2 : true : 3",
            Value::Boolean(true),
        ),
        ("true ? 0 : false ? 2 : 3", Value::Number(0.0)),
        ("false ? false : true ? 4 : false", Value::Number(4.0)),
        ("2 + (false ? 7 : true)", Value::Number(3.0)),
        ("false + false ? true : false", Value::Boolean(false)),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate_value(),
            Ok(expected),
            "{source}"
        );
    }
}
