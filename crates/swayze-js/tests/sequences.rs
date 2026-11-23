use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

fn assert_value(source: &str, expected: Value) {
    let script = parse_script(source).unwrap_or_else(|error| panic!("{source}: {error}"));
    assert_eq!(script.evaluate_value(), Ok(expected), "{source}");
}

fn assert_error_at(marked: &str, kind: ErrorKind, len: usize) {
    let start = marked.find('|').unwrap();
    let source = marked.replacen('|', "", 1);
    let error = parse_script(&source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + len
        },
        "{source}"
    );
}

#[test]
fn sequence_keeps_the_last_primitive_without_coercion() {
    use Value::{Boolean, Null, Number, Undefined};
    for (source, expected) in [
        ("1, 20+22", Number(42.0)),
        ("null, undefined, true", Boolean(true)),
        ("0,\n/* keep the tag */ null;", Null),
        ("false, undefined", Undefined),
        ("10 + (null, false ? 90 : (8, 32))", Number(42.0)),
        ("true || false, null", Null),
        ("null ?? true, false || null", Null),
    ] {
        assert_value(source, expected);
    }
    assert_eq!(
        parse_script("1, false").unwrap().evaluate(),
        Err(EvalError::NonNumericResult)
    );
    assert_eq!(
        parse_script("0, -0").unwrap().evaluate().unwrap().to_bits(),
        (-0.0f64).to_bits()
    );
    assert!(parse_script("1, 0/0").unwrap().evaluate().unwrap().is_nan());
}

#[test]
fn lazy_left_operands_join_before_the_sequence_result() {
    for source in [
        "(false && true, 42)",
        "(true ? 1 : 2, 42)",
        "(false ? 1 : 2, 42)",
    ] {
        assert_value(source, Value::Number(42.0));
    }
}

#[test]
fn comma_binds_below_the_complete_conditional_and_arms_can_group() {
    use Value::{Boolean, Null, Number, Undefined};
    for (source, expected) in [
        ("true ? 1 : 2, null", Null),
        ("true ? 1 : false ? 2 : 3, undefined", Undefined),
        ("false ? 1 : true ? 2 : 3, true", Boolean(true)),
        ("true ? (1, 42) : 3", Number(42.0)),
        ("false ? 3 : (1, 42)", Number(42.0)),
        ("(true, false) ? 1 : 42", Number(42.0)),
        ("true ? false ? (0, 1) : (0, 42) : (0, 3)", Number(42.0)),
    ] {
        assert_value(source, expected);
    }
}

#[test]
fn grouped_sequences_do_not_consume_declarator_separators_or_other_operands() {
    use Value::{Number, Undefined};
    for (source, expected) in [
        ("const a = (1, 20), b = a + 22; b", Number(42.0)),
        (
            "const a = (true ? 1 : 0, 20), b = (false, a + 22); b",
            Number(42.0),
        ),
        (
            "let a = true ? 20 : (false, 19), b = a + 22; b",
            Number(42.0),
        ),
        ("let a = (1,2), b, c = (b,true); b", Undefined),
        ("let x = 0; x = (null, 42); x", Number(42.0)),
        ("let x = 37; x += (0, true ? 2 : 100, 5); x", Number(42.0)),
        ("let x = null; x ??= (1, undefined); x", Undefined),
        (
            "let i=0; for (const a=(1,2), b=a+3; i<b; i+=b) {} i",
            Number(5.0),
        ),
        (
            "let i; for (i=(true,1); i<3; i+=(null,1)) {} i",
            Number(3.0),
        ),
    ] {
        assert_value(source, expected);
    }
}

#[test]
fn all_control_tests_use_the_final_operand_for_truthiness() {
    for (source, expected) in [
        (
            "let n=0; if (true,false) {n=1;} else if (null,true) {n=2;} n",
            2.0,
        ),
        ("let n=0; while (false,n<3) {n+=1;} n", 3.0),
        ("let n=0; do {n+=1;} while (true,n<3); n", 3.0),
        (
            "let n=0; for (let i=(7,0), end=3; false,i<end; i++) {n+=i+1;} n",
            6.0,
        ),
        ("let n=0; for (;true ? true : false, null;) {n=1;} n", 0.0),
    ] {
        assert_value(source, Value::Number(expected));
    }
}

#[test]
fn skipped_operands_and_statements_still_resolve_all_sequence_operands() {
    use ErrorKind::Unsupported;
    for (source, len) in [
        ("false && (|missing, 42)", 7),
        ("true ? 42 : (0, |missing)", 7),
        ("if (true) {} else if (|missing, false) {} 42", 7),
        ("for (;false;){let x=(|missing,0);} 42", 7),
        ("do {break;} while (true, |missing); 42", 7),
        ("let x=true; x ??= (|missing, 0); x", 7),
        ("let outer=1; if(false){let a=(|outer,42), outer=2;} 42", 5),
        ("let x=(|missing,0); x", 7),
        ("|missing,42", 7),
    ] {
        assert_error_at(source, Unsupported, len);
    }
}

#[test]
fn rejects_incomplete_sequences_and_leaves_other_grammar_restrictions_in_place() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, len, kind) in [
        ("|,1", 1, Malformed),
        ("1,|", 0, Malformed),
        ("1,|;", 1, Malformed),
        ("(1,|)", 1, Malformed),
        ("if (1,|){} 0", 1, Malformed),
        ("true ? 42 : (1,|)", 1, Malformed),
        ("(1,|,2)", 1, Malformed),
        ("true ? 1|, 2 : 3", 1, Malformed),
        ("true ? false ? 1 : 2|, 3 : 4", 1, Malformed),
        ("true ? (1, 2) : |,3", 1, Malformed),
        ("for (;true,|;){} 0", 1, Malformed),
        ("const x=1, |2; x", 1, Malformed),
        ("let x; x=true ? 1 : 2|, 3; x", 1, Unsupported),
        ("let x; x ??= 1|, 2; x", 1, Unsupported),
        ("let x; for(x=1|,2;;){} x", 1, Unsupported),
        ("let x; for(;false;x=1|,2){} x", 1, Unsupported),
        ("for (|(1,2);false;){} 0", 1, Unsupported),
        ("for (;false;|(1,2)){} 0", 1, Unsupported),
        ("{1,2|} 0", 1, Unsupported),
        ("1, 2; 3,|", 0, Malformed),
        ("let x; x=(1,2)\n|x", 1, Unsupported),
        ("let x; (1,x|=2)", 1, Unsupported),
    ] {
        assert_error_at(source, kind, len);
    }
}

#[test]
fn ungrouped_sequences_in_a_consequent_or_assignment_rhs_error_at_the_comma() {
    assert_error_at("(true ? 1|, 2 : 3)", ErrorKind::Malformed, 1);
    assert_error_at("let x; x = 1|, 2; x", ErrorKind::Unsupported, 1);
}

#[test]
fn each_sequence_comma_costs_one_token_one_byte_and_one_emission() {
    let source = "true,null,false";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 5,
        max_nesting_depth: 0,
        max_work: source.len() + 5,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(false)));
    assert_eq!(script.instruction_count(), 5);
    assert_eq!(script.max_stack_depth(), 1);
    for (limits, kind, start, end) in [
        (
            Limits {
                max_tokens: 4,
                ..limits
            },
            Limit::Tokens,
            10,
            11,
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            10,
            15,
        ),
        // Four bytes and the initial Boolean emission, then one comma byte.
        (
            Limits {
                max_work: 6,
                ..limits
            },
            Limit::Work,
            4,
            5,
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(kind));
        assert_eq!(error.span(), Span { start, end });
    }
}

#[test]
fn a_flat_sequence_can_reach_the_prepared_instruction_cap_without_nesting() {
    let chain = format!("!false{}", ",null".repeat(2047));
    let limits = Limits {
        max_tokens: 4097,
        max_nesting_depth: 0,
        ..Limits::default()
    };
    let at_cap = parse_script_with_limits(&chain, limits).unwrap();
    assert_eq!(at_cap.instruction_count(), 4096);
    assert_eq!(at_cap.max_stack_depth(), 1);
    assert_eq!(at_cap.evaluate_value(), Ok(Value::Null));
    let over_cap = parse_script_with_limits(&format!("!{chain}"), limits).unwrap();
    assert_eq!(over_cap.instruction_count(), 4097);
    assert_eq!(over_cap.evaluate_value(), Err(EvalError::LimitExceeded));
}

#[test]
fn only_reached_discard_instructions_use_dispatch_fuel() {
    let run = |lhs, final_prefix_count| {
        let prefix = "+ ".repeat(final_prefix_count);
        let source = format!("let n=1637; while ({lhs} || (null,null), n) {{n-=1;}} {prefix}n");
        parse_script(&source).unwrap().evaluate()
    };
    // Initialization (2), 1637 iterations (5 test, 4 decrement, 1 back edge
    // each), the final test (5) and final result (7) use exactly 16,384 units.
    assert_eq!(run("true", 6), Ok(0.0));
    assert_eq!(run("true", 7), Err(EvalError::DispatchLimitExceeded));
    assert_eq!(run("false", 6), Err(EvalError::DispatchLimitExceeded));
}
