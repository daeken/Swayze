use swayze_js::{
    Error, ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn items_initialize_left_to_right_in_their_lexical_scope() {
    for (source, expected) in [
        (
            "let x, y = 42; x === undefined ? y : 0",
            Value::Number(42.0),
        ),
        ("const a = 20, b = a + 22; b", Value::Number(42.0)),
        ("let a, b = a, c; b === c ? c : 1", Value::Undefined),
        (
            "let x=100, result; {const x=20, y=(x ? (false ? 0 : x) : 2)+22; result=y;} result+x",
            Value::Number(142.0),
        ),
        (
            "let result; {let undefined=42, x, y=x; result=y===undefined ? 0 : undefined;} result",
            Value::Number(42.0),
        ),
        (
            "let n=2, count=0; while(n){let x, y=x??21; x=0; count+=y; --n;} count",
            Value::Number(42.0),
        ),
        (
            "const a = 20\n/* before comma */, // before item\n b = false ? 0 : true ? (a+22) : 0; b",
            Value::Number(42.0),
        ),
    ] {
        let script = parse_script(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
        for _ in 0..2 {
            assert_eq!(script.evaluate_value(), Ok(expected), "{source:?}");
        }
    }
}

fn error_at(marked: &str, len: usize, kind: ErrorKind) -> Error {
    let start = marked.find('|').unwrap();
    let source = marked.replacen('|', "", 1);
    let error = parse_script(&source).unwrap_err();
    assert_eq!(error.kind(), kind, "{marked:?}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + len
        },
        "{marked:?}: {error}"
    );
    error
}

#[test]
fn later_items_shadow_throughout_the_scope_and_self_reads_are_in_the_tdz() {
    for (source, len) in [
        ("const a=|b, b=42; a", 1),
        ("let a=|b, b; a", 1),
        ("let a, b=|b; a", 1),
        ("const b=42; {let a=|b, b=1;} b", 1),
        ("let b=42; {const a=20, b=a+|b;} b", 1),
        ("let b=42; {if(false){|b=1;} let a, b;} b", 1),
        ("{let a=true ? 1 : |undefined, undefined;} 0", 9),
    ] {
        assert!(
            error_at(source, len, ErrorKind::Unsupported)
                .message()
                .contains("before lexical binding initialization")
        );
    }
}

#[test]
fn duplicate_and_root_undefined_checks_apply_to_every_item() {
    for source in [
        "let a, |a; 0",
        "const a=1, |a=2; a",
        "const a=1; let b, |a; a",
        "if(false){let a,b; const c=0, |b=0;} 0",
    ] {
        assert_eq!(
            error_at(source, 1, ErrorKind::Malformed).message(),
            "duplicate binding declaration"
        );
    }
    for source in ["let a, |undefined; 0", "const a=1, |undefined=2; a"] {
        assert_eq!(
            error_at(source, 9, ErrorKind::Malformed).message(),
            "undefined cannot be declared in the Script scope"
        );
    }
}

#[test]
fn commas_only_separate_complete_single_name_declarators() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, len, kind) in [
        ("let |,x; x", 1, Malformed),
        ("let x,|,y; y", 1, Malformed),
        ("let x,|; x", 1, Malformed),
        ("const x=1, y=2,|; y", 1, Malformed),
        ("let x,|", 0, Malformed),
        ("let x, y=|,z; z", 1, Malformed),
        ("const x|,y=2; y", 1, Malformed),
        ("const x=1,y|; x", 1, Malformed),
        ("let x |y; x", 1, Malformed),
        ("let x, y\n|x", 1, Malformed),
        ("const x=1, y=2\n|y", 1, Unsupported),
        ("const x=1, y=2|", 0, Unsupported),
        ("{let x=1, y=2|} 0", 1, Unsupported),
        ("let x, y|+=2; y", 2, Unsupported),
        ("let x, |{y}=x; y", 1, Unsupported),
        ("const x=1, |[y]=x; y", 1, Unsupported),
        ("|var x=1,y=2; y", 3, Unsupported),
        ("for(;false;|const x=1,y=2){} 0", 5, Unsupported),
        ("let x,y; for(x=1|,y=2;;){} 0", 1, Unsupported),
        ("let x,y; x=1|,y=2; y", 1, Unsupported),
        ("let x=true ? 1|,y=2 : 0; y", 1, Malformed),
    ] {
        error_at(source, len, kind);
    }
}

#[test]
fn exact_parse_budgets_charge_the_comma_once_and_both_implicit_emissions() {
    let source = "let x/*🦀*/,y=x; y";
    let comma = source.find(',').unwrap();
    let last = source.len() - 1;
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 8,
        max_nesting_depth: 0,
        // Bytes + 5 emissions + 2 for y's duplicate check + 4 to resolve x + 2 to resolve y.
        max_work: source.len() + 13,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 5);
    assert_eq!(script.evaluate_value(), Ok(Value::Undefined));
    for (at, limit, limits) in [
        (
            last,
            Limit::SourceBytes,
            Limits {
                max_source_bytes: last,
                ..limits
            },
        ),
        (
            last,
            Limit::Tokens,
            Limits {
                max_tokens: limits.max_tokens - 1,
                ..limits
            },
        ),
        (
            last,
            Limit::Work,
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
        ),
        (
            comma,
            Limit::Tokens,
            Limits {
                max_tokens: 2,
                ..limits
            },
        ),
        (
            comma,
            Limit::Work,
            Limits {
                max_work: comma + 1,
                ..limits
            },
        ),
        (
            comma,
            Limit::Work,
            Limits {
                max_work: comma + 2,
                ..limits
            },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
        assert_eq!(
            error.span(),
            Span {
                start: at,
                end: at + 1
            }
        );
    }
}

#[test]
fn one_flat_list_can_fill_the_prepared_instruction_and_binding_budgets() {
    use std::fmt::Write;

    // 2,047 distinct slots (two instructions each); no nesting used by list length.
    let mut source = String::from("let x0000=42");
    for i in 1..2047 {
        write!(source, ",x{i:04}").unwrap();
    }
    let limits = Limits {
        max_tokens: 5_000,
        max_work: 13_000_000,
        max_nesting_depth: 0,
        ..Limits::default()
    };
    for (suffix, count, expected) in [
        ("; +x0000", 4096, Ok(Value::Number(42.0))),
        (",x2047; x0000", 4097, Err(EvalError::LimitExceeded)),
    ] {
        let script = parse_script_with_limits(&format!("{source}{suffix}"), limits).unwrap();
        assert_eq!(script.instruction_count(), count);
        assert_eq!(script.evaluate_value(), expected);
    }
}

#[test]
fn each_reached_initializer_and_store_costs_one_dispatch() {
    // 2 for the counter's initializer; 1489 * (2 for the test, 4 for the
    // declaration list, 4 for --n, 1 back edge); 2 for the final test; 1 for n.
    let source = "let n=1489; while(n){let x,y=x; --n;}";
    let exact = parse_script(&format!("{source}n")).unwrap();
    let over = parse_script(&format!("{source}+n")).unwrap();
    for _ in 0..2 {
        assert_eq!(exact.evaluate(), Ok(0.0));
        assert_eq!(over.evaluate(), Err(EvalError::DispatchLimitExceeded));
    }
}
