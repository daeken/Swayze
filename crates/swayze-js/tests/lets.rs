use swayze_js::{EvalError, Limits, parse_script, parse_script_with_limits};

#[test]
fn evaluates_assignments_in_source_order_with_fresh_slots() {
    let example = parse_script("let x = 2; const step = 5; x = x * step; x + 1;").unwrap();
    let script = {
        let source = String::from(
            "let counter=2; const step=5; counter=counter*step; \
             const snapshot=counter; let offset=snapshot+1; \
             counter /* comment */\n=counter+offset; offset=offset-step; \
             counter+offset+snapshot;",
        );
        parse_script(&source).unwrap()
    };
    let other = parse_script("let counter=1; counter=counter+1; counter // end").unwrap();
    let debug = format!("{script:?}");
    for name in ["counter", "step", "snapshot", "offset"] {
        assert!(!debug.contains(name));
    }
    for _ in 0..3 {
        assert_eq!(example.evaluate(), Ok(11.0));
        assert_eq!(script.evaluate(), Ok(37.0));
        assert_eq!(other.evaluate(), Ok(2.0));
    }
}

#[test]
fn reassignment_stores_number_values_without_coercion() {
    fn run(source: &str) -> f64 {
        parse_script(source).unwrap().evaluate().unwrap()
    }
    assert_eq!(run("let x=1; x=-0; x").to_bits(), (-0.0_f64).to_bits());
    assert_eq!(run("let x=-0; x=1/x; x"), f64::NEG_INFINITY);
    assert!(run("let x=1; x=0/0; x+x").is_nan());
    assert_eq!(
        run("let x=0; x=9007199254740992+1; x-9007199254740992"),
        0.0
    );
}

#[test]
fn assignments_count_toward_the_inclusive_execution_ceiling() {
    let mut source = String::from("let x=0;");
    source.push_str(&"x=1;".repeat(2046));
    // Two instructions for initialization, two per assignment, and two for +x.
    source.push_str("+x");
    let limits = Limits {
        max_tokens: 8192,
        ..Limits::default()
    };
    let script = parse_script_with_limits(&source, limits).unwrap();
    assert_eq!(script.instruction_count(), 4096);
    assert_eq!(script.evaluate(), Ok(1.0));

    source.insert_str(source.len() - 2, "+ ");
    let script = parse_script_with_limits(&source, limits).unwrap();
    assert_eq!(script.instruction_count(), 4097);
    assert_eq!(script.evaluate(), Err(EvalError::LimitExceeded));
}
