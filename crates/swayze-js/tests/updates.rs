use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

const FORMS: [(&str, f64); 4] = [("++x", 1.0), ("x++", 1.0), ("--x", -1.0), ("x--", -1.0)];

fn assert_error(marked: &str, kind: ErrorKind, message: &str) {
    let (prefix, rest) = marked.split_once('@').unwrap();
    let (token, suffix) = rest.split_once('@').unwrap();
    let source = format!("{prefix}{token}{suffix}");
    let error = parse_script(&source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source:?}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start: prefix.len(),
            end: prefix.len() + token.len(),
        },
        "{source:?}: {error}"
    );
    assert!(error.message().contains(message), "{source:?}: {error}");
}

#[test]
fn all_four_forms_work_in_statements_and_both_for_clauses() {
    for (form, step) in FORMS {
        let source = format!("let x=42;{form};x");
        let script = parse_script(&source).unwrap();
        assert_eq!(script.instruction_count(), 7);
        assert_eq!(script.max_stack_depth(), 2);
        assert_eq!(script.evaluate(), Ok(42.0 + step), "{source}");
        assert_eq!(script.evaluate(), Ok(42.0 + step), "{source}");

        let source = format!("let x=42;if(false){{{form};}}else{{{form};}}{form};x");
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(42.0 + 2.0 * step)
        );
        // The initializer runs once, continue runs the update, and break skips it.
        let source =
            format!("let x=42;let n=0;for({form};;{form}){{if(n){{break;}}n++;continue;}}x");
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(42.0 + 2.0 * step)
        );
        let source = format!("let x=42;for({form};false;{form}){{{form};}}x");
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(42.0 + step));
    }
}

#[test]
fn nearest_bindings_are_updated_and_body_bindings_are_fresh() {
    let source = "let x=0;let trace=0;
        for(++x;x<4;x++){let x=7;++x;trace=trace*10+x;continue;}
        trace*10+x";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(8884.0));
    assert_eq!(
        parse_script("let x=1; if(true){let undefined=true; undefined++; x+=undefined;}x")
            .unwrap()
            .evaluate(),
        Ok(3.0)
    );
}

#[test]
fn numeric_updates_use_number_coercion_and_binary64_rounding() {
    for (source, expected) in [
        ("let x=true; ++x; x", 2.0),
        ("let x=false; x--; x", -1.0),
        ("let x=null; x++; x", 1.0),
        ("let x=-0; ++x; x", 1.0),
        ("let x=-0; --x; x", -1.0),
        ("let x=1; x--; x", 0.0),
        ("let x=-1; x++; x", 0.0),
        ("let x=1e309; --x; x", f64::INFINITY),
        ("let x=-1e309; ++x; x", f64::NEG_INFINITY),
        ("let x=9007199254740991; ++x; x", 9007199254740992.0),
        ("let x=9007199254740992; x++; x", 9007199254740992.0),
        ("let x=9007199254740992; x--; x", 9007199254740991.0),
        ("let x=9007199254740994; ++x; x", 9007199254740996.0),
        ("let x=9007199254740994; --x; x", 9007199254740992.0),
        ("let x=-9007199254740992; --x; x", -9007199254740992.0),
    ] {
        let Value::Number(actual) = parse_script(source).unwrap().evaluate_value().unwrap() else {
            panic!("expected a Number for {source}");
        };
        assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
    }
    for (form, _) in FORMS {
        for value in ["undefined", "0/0"] {
            let source = format!("let x={value};{form};x");
            assert!(parse_script(&source).unwrap().evaluate().unwrap().is_nan());
        }
    }
}

#[test]
fn resolves_only_initialized_let_targets_after_syntax_in_source_order() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, message) in [
        ("++@absent@;later", "unbound name"),
        ("@undefined@--;0", "unbound name"),
        ("while(false){@local@++;}0", "unbound name"),
        ("for(;false;--@x@){let x=0;}0", "unbound name"),
        ("const x=0;x@++@;0", "assignment to const"),
        ("const x=0;@--@x;0", "assignment to const"),
        ("const x=0;for(x@--@;false;){}0", "assignment to const"),
        (
            "let x=0;if(false){const x=0;@++@x;}0",
            "assignment to const",
        ),
        ("let x=0;if(false){++@x@;const x=0;}0", "initialization"),
        (
            "let x=0;if(false){for(;false;@x@++){}let x=1;}0",
            "initialization",
        ),
        ("for(;;){break;@x@--;let x=1;}0", "initialization"),
        ("if(false){let x=0;x++;}--@x@;0", "unbound name"),
    ] {
        assert_error(source, Unsupported, message);
    }
    assert_error("missing++;let x=@;@0", Malformed, "operand");
    assert_error(
        "const x=0;++x;0;@extra@",
        Unsupported,
        "additional statements",
    );
    assert_error("const x=0;x++;@@", Unsupported, "final expression");
}

#[test]
fn respects_ecmascript_line_terminators_without_asi() {
    use ErrorKind::Unsupported;
    for newline in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        for trivia in [
            newline.to_owned(),
            format!("/*💡{newline}*/"),
            format!("//💡{newline}"),
        ] {
            for op in ["++", "--"] {
                for marked in [
                    format!("let x=0;x{trivia}@{op}@;x"),
                    format!("let x=0;if(true){{x{trivia}@{op}@;}}x"),
                    format!("let x=0;for(x{trivia}@{op}@;false;){{}}x"),
                    format!("let x=0;for(;false;x{trivia}@{op}@){{}}x"),
                    format!("let x=0;x{trivia}@{op}@x;x"),
                ] {
                    assert_error(&marked, Unsupported, "line terminator");
                }
            }
            let source = format!("let x=0;++{trivia}x;x++{trivia};x");
            assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(2.0));
        }
    }
    let source = "let $x_1=0;if(false){}$x_1/*💡*/\u{00a0}++;--/**/$x_1;$x_1";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(0.0));
    assert_eq!(
        parse_script("let x=0;for(x/**/++;x<3;++/**/x){}x")
            .unwrap()
            .evaluate(),
        Ok(3.0)
    );
    assert_error("let x=0;++x/*💡*/@@", Unsupported, "explicit semicolons");
    assert_error(
        "let x=0;x++/*\u{2028}*/@x@",
        Unsupported,
        "explicit semicolons",
    );
    assert_error(
        "let x=0;if(true){x--@}@0",
        Unsupported,
        "explicit semicolons",
    );
    assert_error(
        "let x=0;for(x++@)@{}x",
        ErrorKind::Malformed,
        "between for clauses",
    );
    assert_error(
        "let x=0;for(;false;--x@;@){}x",
        ErrorKind::Malformed,
        "after the for update",
    );
}

#[test]
fn rejects_expression_values_and_other_targets_with_maximal_munch() {
    use ErrorKind::Unsupported;
    for (source, message) in [
        ("let x=0;let y=@++@x;y", "expressions"),
        ("let x=0;x+=x@++@;x", "expressions"),
        ("let x=0;(x@--@)", "expressions"),
        ("let x=0;true?@++@x:x", "expressions"),
        ("let x=0;if(x@++@){}0", "expressions"),
        ("let x=0;while(@--@x){}0", "expressions"),
        ("let x=0;for(;x@++@;){}0", "expressions"),
        ("let x=0;for(++x;false;x@++@ + 1){}x", "expressions"),
        ("let x=0;x@++@+1;x", "expressions"),
        ("let x=0;x@--@-1;x", "expressions"),
        ("let x=0;x@++@=1;x", "expressions"),
        ("let x=0;x@--@=1;x", "expressions"),
        ("let x=0;@++@+x;x", "identifier"),
        ("let x=0;@--@(x);x", "identifier"),
        ("@++@true;0", "identifier"),
        ("@--@1;0", "identifier"),
        ("1@++@;0", "expressions"),
        ("let x=0;(x)@--@;x", "expressions"),
        ("let x=0;++x@.@p;x", "token"),
        ("let x=0;x@[@0]--;x", "token"),
        ("let x=0;x @++@+x", "expressions"),
        ("let x=0;x @--@-x", "expressions"),
        ("let x=0;x + @++@x", "expressions"),
        ("let x=0;x - @--@x", "expressions"),
        ("let x=0;@--@>x", "identifier"),
        ("1;@++@x;0", "additional statements"),
    ] {
        assert_error(source, Unsupported, message);
    }
    assert_eq!(
        parse_script("let x=0;x+=3;x-=1; x +/**/+x -/**/-x")
            .unwrap()
            .evaluate(),
        Ok(6.0)
    );
}

#[test]
fn token_and_work_budgets_include_trivia_and_only_one_target_lookup() {
    let source = "let x=false;x/*💡*/++;x";
    let last = Span {
        start: source.len() - 1,
        end: source.len(),
    };
    let emoji = source.find('💡').unwrap();
    // Source bytes, 7 emitted instructions, and one candidate + one byte per
    // lookup (one update target and one final read). No extra trivia scan.
    let limits = Limits {
        max_tokens: 9,
        max_work: source.len() + 11,
        max_nesting_depth: 0,
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(1.0)
    );
    for (limits, ceiling, span) in [
        (
            Limits {
                max_tokens: 8,
                ..limits
            },
            Limit::Tokens,
            last,
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            last,
        ),
        // Two declaration instructions have been emitted when the lexer gets here.
        (
            Limits {
                max_work: emoji + 2 + 3,
                ..limits
            },
            Limit::Work,
            Span {
                start: emoji,
                end: emoji + 4,
            },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(ceiling));
        assert_eq!(error.span(), span);
    }
}

#[test]
fn skipped_updates_count_toward_exact_prepared_instruction_ceiling() {
    // Two instructions for the declaration, two for the test, four per
    // skipped update, plus one final load and the unary operators.
    for (prefixes, expected) in [(3, Ok(0.0)), (4, Err(EvalError::LimitExceeded))] {
        let source = format!(
            "let x=false;if(false){{{}}}{}x",
            "++x;".repeat(1022),
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.instruction_count(), 4093 + prefixes);
        assert_eq!(script.max_stack_depth(), 2);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn for_updates_count_toward_exact_dispatch_ceiling() {
    // 6 setup dispatches, 11 per continuing iteration (including 4 for the
    // update), 4 for the final test, then 1 final load and the unary operators.
    for (prefixes, expected) in [
        (4, Ok(1489.0)),
        (5, Ok(1489.0)),
        (6, Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!(
            "let i=0;for(i++;i<1489;++i){{continue;}}{}i",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.instruction_count(), 19 + prefixes);
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
    let script = parse_script("let i=9007199254740992;for(;;i++){continue;}i").unwrap();
    assert_eq!(script.evaluate(), Err(EvalError::DispatchLimitExceeded));
}
