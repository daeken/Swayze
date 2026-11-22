use std::fmt::Write;

use swayze_js::{ErrorKind, EvalError, Limit, Limits, parse_script, parse_script_with_limits};

#[test]
fn evaluates_ordered_consts_with_no_source_or_cross_script_state() {
    let script = {
        let source = String::from("const first=2; const second=first*5; second+1;");
        parse_script(&source).unwrap()
    }; // The prepared script does not borrow the source.
    let debug = format!("{script:?}");
    assert!(!debug.contains("first") && !debug.contains("second"));
    let other = parse_script("const first=3; const second=first-1; first+second").unwrap();
    for _ in 0..3 {
        assert_eq!(script.evaluate(), Ok(11.0));
        assert_eq!(other.evaluate(), Ok(5.0));
    }
    assert_eq!(
        parse_script("first").unwrap_err().kind(),
        ErrorKind::Unsupported
    );
}

#[test]
fn consts_store_number_values_without_coercion() {
    fn run(source: &str) -> f64 {
        parse_script(source).unwrap().evaluate().unwrap()
    }
    assert_eq!(
        run("const z=-0; const copy=+z; copy").to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(run("const z=1 / -0; z"), f64::NEG_INFINITY);
    assert!(run("const z=0 / 0; z+z").is_nan());
    assert_eq!(run("const z=9007199254740992+1; z-9007199254740992"), 0.0);
}

#[test]
fn custom_limits_allow_more_declarations_but_bound_resolution_work() {
    let mut source = String::new();
    for i in 0..300 {
        write!(source, "const a{i:06}={i};").unwrap();
    }
    source.push_str("a000299-a000001");
    assert_eq!(
        parse_script(&source).unwrap_err().kind(),
        ErrorKind::LimitExceeded(Limit::Work)
    );
    let script = parse_script_with_limits(
        &source,
        Limits {
            max_work: 512 * 1024,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(script.instruction_count(), 603);
    assert_eq!(script.evaluate(), Ok(298.0));
}

#[test]
fn const_initialization_and_reads_count_toward_the_execution_budget() {
    let source = format!("const x=1; {}x", "+ ".repeat(4093));
    let limits = Limits {
        max_tokens: 4100,
        ..Limits::default()
    };
    let script = parse_script_with_limits(&source, limits).unwrap();
    assert_eq!(script.instruction_count(), 4096);
    assert_eq!(script.evaluate(), Ok(1.0));

    let script = parse_script_with_limits(&format!("{source}+"), limits).unwrap_err();
    assert_eq!(script.kind(), ErrorKind::Malformed);
    let script =
        parse_script_with_limits(&format!("const x=1; + {}x", "+ ".repeat(4093)), limits).unwrap();
    assert_eq!(script.instruction_count(), 4097);
    assert_eq!(script.evaluate(), Err(EvalError::LimitExceeded));
}
