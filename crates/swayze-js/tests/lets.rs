use swayze_js::{
    Error, ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

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

#[test]
fn omitted_initializer_produces_a_tagged_value_and_allows_updates() {
    use Value::{Boolean, Null, Number, Undefined};
    for (source, expected) in [
        ("let x; x", Undefined),
        (
            "let/**/x\n/* trivia */; const copy=x; copy===undefined",
            Boolean(true),
        ),
        ("let x; x=null; x", Null),
        ("let x; x=false; x??=42; x", Boolean(false)),
        ("let x; x??=42; x", Number(42.0)),
        (
            "let result; {let undefined; undefined??=42; result=undefined;} result",
            Number(42.0),
        ),
        ("let x; x&&=42; x", Undefined),
        ("let x; ++x; x!==x", Boolean(true)),
        ("let x; x--; x!==x", Boolean(true)),
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate_value(),
            Ok(expected),
            "{source}"
        );
    }
    assert_eq!(
        parse_script("let x; x").unwrap().evaluate(),
        Err(EvalError::NonNumericResult)
    );
}

#[test]
fn skipped_blocks_do_not_touch_outer_bindings_or_look_up_the_name_undefined() {
    let source = "const x=9; let result;
                  if(false){let x; x=1; result=x;}
                  else {let undefined=42; let x; result=x;}
                  while(false){let result; result=100;}
                  result===undefined ? x : 0";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(9.0));
}

#[test]
fn every_reentry_initializes_again_even_after_an_entry_skips_the_declaration() {
    let script = parse_script(
        "let seen=0; let i=0; const x=9;
         for(;i<4;i++){
             if(i===1){continue;}
             let x;
             if(x===undefined){seen=seen*10+i+1;}
             x=true;
         }
         seen*10+x",
    )
    .unwrap();
    for _ in 0..2 {
        assert_eq!(script.evaluate(), Ok(1349.0));
    }

    // The backward edge targets the synthesized push at the start of the body.
    let source = "let i=0; do {let x; if(x===undefined){i++;} x=99;} while(i<3); i";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(3.0));
}

// `|` marks the start of the expected diagnostic.
fn error_at(marked: &str, len: usize, kind: ErrorKind) -> Error {
    let (before, after) = marked.split_once('|').unwrap();
    let error = parse_script(&format!("{before}{after}")).unwrap_err();
    assert_eq!(error.kind(), kind, "{marked}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start: before.len(),
            end: before.len() + len
        },
        "{marked}: {error}"
    );
    error
}

#[test]
fn declaration_rules_apply_before_initialization_even_in_inactive_code() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, len) in [
        ("const copy=|x; let x; copy", 1),
        ("const x=1; if(false){let copy=|x; let x;} x", 1),
        ("let x=1; while(false){{|x=2;} let x;} x", 1),
        ("const x=1; {++|x; let x;} x", 1),
        ("if(false){let copy=|undefined; let undefined;} 0", 9),
        ("for(;;){continue; |x??=1; let x;} 0", 1),
    ] {
        assert!(
            error_at(source, len, Unsupported)
                .message()
                .contains("before lexical binding initialization")
        );
    }
    for source in ["{let x;} |x", "if(false){let x;}else{|x=1;} 0"] {
        assert!(
            error_at(source, 1, Unsupported)
                .message()
                .contains("unbound name")
        );
    }
    let root = error_at("let |undefined; 0", 9, Malformed);
    assert!(
        root.message()
            .contains("cannot be declared in the Script scope")
    );
    for source in [
        "let x; let |x; 0",
        "const x=1; let |x; 0",
        "if(false){let x; const |x=1;} 0",
    ] {
        assert_eq!(
            error_at(source, 1, Malformed).message(),
            "duplicate binding declaration"
        );
    }
}

#[test]
fn declarations_still_require_explicit_semicolons_and_name_targets() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, len, kind) in [
        ("const x|;", 1, Malformed),
        ("let x\n|x", 1, Malformed),
        ("{let x/*\n*/|} 0", 1, Malformed),
        ("let x|", 0, Malformed),
        ("let x;|", 0, Unsupported),
        ("let |{x}; x", 1, Unsupported),
        ("|var x; x", 3, Unsupported),
        ("for(let x|){} 0", 1, Malformed),
        ("for(;false;|let x){} 0", 3, Unsupported),
    ] {
        error_at(source, len, kind);
    }
}

#[test]
fn exact_parse_budgets_charge_the_implicit_push_but_no_token_or_name_access() {
    let source = "let x /*🦀*/ ; x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 4,
        max_nesting_depth: 0,
        // Scanned bytes, three emissions, and one candidate + one byte for the read.
        max_work: source.len() + 5,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 3);
    assert_eq!(script.evaluate_value(), Ok(Value::Undefined));
    for (limit, limits) in [
        (
            Limit::SourceBytes,
            Limits {
                max_source_bytes: limits.max_source_bytes - 1,
                ..limits
            },
        ),
        (
            Limit::Tokens,
            Limits {
                max_tokens: limits.max_tokens - 1,
                ..limits
            },
        ),
        (
            Limit::Work,
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        assert_eq!(
            error.span(),
            Span {
                start: source.len() - 1,
                end: source.len()
            }
        );
    }
    let semicolon = source.find(';').unwrap();
    // Just enough for the semicolon token, then enough for that and the push.
    for max_work in [semicolon + 1, semicolon + 2] {
        let error = parse_script_with_limits(source, Limits { max_work, ..limits }).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
        assert_eq!(
            error.span(),
            Span {
                start: semicolon,
                end: semicolon + 1
            }
        );
    }
}

#[test]
fn implicit_initialization_counts_toward_prepared_and_dispatch_ceilings() {
    let skipped = "{let x;}".repeat(2046);
    let limits = Limits {
        max_tokens: 11_000,
        ..Limits::default()
    };
    // Two guard instructions, two per skipped declaration, and two for +false.
    for (expr, instructions, result) in [
        ("+false", 4096, Ok(0.0)),
        ("+ +false", 4097, Err(EvalError::LimitExceeded)),
    ] {
        let script =
            parse_script_with_limits(&format!("if(false){{{skipped}}}{expr}"), limits).unwrap();
        assert_eq!(script.instruction_count(), instructions);
        assert_eq!(script.evaluate(), result);
    }

    // 2 for the counter's initializer; 1489 * (2 for the test, 4 for the
    // declarations, 4 for --n, 1 back edge); 2 for the final test; 1 for n.
    let source = "let n=1489; while(n){let x; let y; --n;}";
    let exact = parse_script(&format!("{source}n")).unwrap();
    let over = parse_script(&format!("{source}+n")).unwrap();
    for _ in 0..2 {
        assert_eq!(exact.evaluate(), Ok(0.0));
        assert_eq!(over.evaluate(), Err(EvalError::DispatchLimitExceeded));
    }
}
