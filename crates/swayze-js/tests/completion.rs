use swayze_js::{
    ErrorKind, EvalError, EvalScratch, Limits, Span, Value, parse_script,
    parse_script_for_completion, parse_script_for_completion_with_limits,
};

fn execute(source: &str, scratch: &mut EvalScratch) -> Result<(), EvalError> {
    parse_script_for_completion(source)
        .unwrap_or_else(|e| panic!("{source:?}: {e:?}"))
        .execute_with_scratch(scratch)
}

#[test]
fn normal_exit_allows_complete_statements_and_preserves_legacy_expression_requirement() {
    let scratch = &mut EvalScratch::new();
    for source in ["", "\t\n\u{FEFF}", "// comment", "/* comment */"] {
        assert_eq!(execute(source, scratch), Ok(()));
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_eq!(
            error.message(),
            "empty scripts are outside the arithmetic subset"
        );
        assert_eq!(error.span().start, source.len());
        assert_eq!(error.span().end, source.len());
    }
    for source in [
        ";",
        "{}",
        "1;;",
        "{ 's'.length; }",
        "const x = 1;",
        "let x; x = 1;",
        "let x=0; x++;",
        "let x=1; ++x, x*=2;",
        "if (true) { 1; } else { false.length; }",
        "if (false) { false.length; } else if (false) { false.length; }",
        "outer: { break outer; false.length; }",
        "outer: if (true) { break outer; false.length; } else { false.length; }",
        "while (false) { false.length; }",
        "for (;;) { break; }",
        "do {} while (false);",
        "let s=''; s ||= ('a' + 'b'); s &&= (s += 'c'); s ??= false.length; { s.length; }",
        "let x=0; for (let i=0; i<3; i++) { x+=i; continue; false.length; } if (x!==3) { false.length; }",
    ] {
        assert_eq!(execute(source, scratch), Ok(()), "{source:?}");
        let error = parse_script(source).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_eq!(
            error.message(),
            "a final expression is required after statements"
        );
        assert_eq!(
            error.span(),
            Span {
                start: source.len(),
                end: source.len()
            }
        );
    }
}

#[test]
fn final_expression_runs_without_observing_its_value() {
    let scratch = &mut EvalScratch::new();
    for source in [
        "-0",
        "null;",
        "typeof missing",
        "'literal'",
        "'' + ''",
        "'a' + 'b';",
        "false ? false.length : (true && 'x')",
        "let x; (x ||= 'ok')",
        "let x; 1; x = 'ok'; x.length;",
    ] {
        assert_eq!(execute(source, scratch), Ok(()));
        assert!(
            parse_script(source)
                .unwrap()
                .evaluate_primitive_with_scratch(scratch)
                .is_ok()
        );
    }
    let legacy = parse_script("'a' + 'b';").unwrap();
    assert_eq!(legacy.evaluate(), Err(EvalError::NonNumericResult));
    assert_eq!(legacy.evaluate_value(), Err(EvalError::NonScalarResult));
    assert_eq!(
        legacy.evaluate_primitive(),
        Err(EvalError::RuntimeStringResultRequiresScratch)
    );
    assert_eq!(
        parse_script("let x=1; x+=1; x").unwrap().evaluate_value(),
        Ok(Value::Number(2.0))
    );
}

#[test]
fn completion_checks_skipped_accesses_and_does_not_forgive_incomplete_input() {
    for (source, at, len, kind) in [
        (
            "if(false){ missing; }",
            "missing",
            7,
            ErrorKind::Unsupported,
        ),
        (
            "break_here: {break break_here; missing;}",
            "missing",
            7,
            ErrorKind::Unsupported,
        ),
        (
            "for(;false;missing){}",
            "missing",
            7,
            ErrorKind::Unsupported,
        ),
        (
            "const x=0; if(false){ x+=1; }",
            "+=",
            2,
            ErrorKind::Unsupported,
        ),
        ("let x=x;", "x;", 1, ErrorKind::Unsupported),
        ("let x; let x;", "x;", 1, ErrorKind::Malformed),
        ("let undefined;", "undefined", 9, ErrorKind::Malformed),
        ("{ let x=1; } x++;", "x++", 1, ErrorKind::Unsupported),
    ] {
        let error = parse_script_for_completion(source).unwrap_err();
        let start = source.rfind(at).unwrap();
        assert_eq!(error.kind(), kind, "{source:?}: {error:?}");
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + len
            },
            "{source:?}"
        );
    }
    for source in ["if(false){ missing; }", "let x=x;"] {
        let error = parse_script(source).unwrap_err();
        assert_eq!(
            error.message(),
            "a final expression is required after statements"
        );
        assert_eq!(error.span().start, source.len());
    }
    for source in [
        "#! ignore\nlet x=1",
        "let x; x=1",
        "let x; x++\n1",
        "1\n2",
        "if(true) { 1 }",
        "if(true) {",
        "do{}while(false)",
        "label:",
        "break;",
        "1+",
        "1; /*",
    ] {
        assert_eq!(
            parse_script_for_completion(source).unwrap_err(),
            parse_script(source).unwrap_err(),
            "{source:?}"
        );
    }
}

#[test]
fn reached_errors_are_not_hidden_by_discard_or_statement_exit() {
    let scratch = &mut EvalScratch::new();
    for source in [
        "false.length",
        "true ? false.length : 'ok';",
        "'ok' && false.length",
        "{ false.length; }",
        "let x = false.length;",
        "let x; x=false.length;",
        "let x; x ||= false.length;",
        "while (false.length) {}",
        "for (let i=0; i<1; false.length) {}",
        "do {} while (false.length);",
        "let x=0; while(x<2) { x++; } if(x===2){ false.length; }",
    ] {
        assert_eq!(
            execute(source, scratch),
            Err(EvalError::UnsupportedPropertyReceiver),
            "{source:?}"
        );
    }
    assert_eq!(
        execute("while(true){}", scratch),
        Err(EvalError::DispatchLimitExceeded)
    );
    let body = "a".repeat(4092);
    let source = format!("let s='{body}'; for(let i=0;i<8;i++){{ s+'xxxx'; }}");
    // A result of 4,096 exceeds the 4,094-unit per-result ceiling.
    assert_eq!(
        execute(&source, scratch),
        Err(EvalError::StringConcatenationLengthLimitExceeded)
    );
    let prefix = format!("let s='{body}'; for(let i=0;i<8;i++){{ s+'xx'; }}");
    for suffix in ["s+'xx';", "if(true){ s+'xx'; }"] {
        assert_eq!(
            execute(&format!("{prefix}{suffix}"), scratch),
            Err(EvalError::StringConcatenationBudgetExceeded)
        );
    }
    let source = format!("{prefix}'0123456789abcdef'+'';");
    let script = parse_script_for_completion(&source).unwrap();
    // 8 × 4,094 + 16 = 32,768; reuse of scratch gets a fresh budget.
    for _ in 0..2 {
        assert_eq!(script.execute_with_scratch(scratch), Ok(()));
    }
}

#[test]
fn normal_exit_costs_no_extra_prepared_instruction_or_dispatch() {
    let scratch = &mut EvalScratch::new();
    let limits = Limits {
        max_tokens: 12_000,
        ..Limits::default()
    };
    // Two instructions per uninitialized declaration, with no final-expression reserve.
    let declarations = (0..2048)
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("let {declarations};");
    // Binding duplicate checks are metered independently and quadratic.
    let limits = Limits {
        max_work: 8_000_000,
        ..limits
    };
    assert_eq!(
        parse_script_for_completion_with_limits(&source, limits)
            .unwrap()
            .execute_with_scratch(scratch),
        Ok(())
    );
    let source = format!("let {declarations},a2048;");
    assert_eq!(
        parse_script_for_completion_with_limits(&source, limits)
            .unwrap()
            .execute_with_scratch(scratch),
        Err(EvalError::LimitExceeded)
    );
    // 2 initializer dispatches, 9 per true iteration (test: 3 + branch: 1
    // + update: 4 + back: 1), and 4 for the final falsy test: 16,377.
    let prefix = "let x=1819; while(x>0){x--; }";
    assert_eq!(
        execute(&format!("{prefix}{{ !!!!!!false; }}"), scratch),
        Err(EvalError::DispatchLimitExceeded)
    );
    assert_eq!(
        execute(&format!("{prefix}{{ !!!!!false; }}"), scratch),
        Ok(())
    );
    assert_eq!(execute(&format!("{prefix}!!!!!!false;"), scratch), Ok(()));
    assert_eq!(
        execute(&format!("{prefix}!!!!!!!false;"), scratch),
        Err(EvalError::DispatchLimitExceeded)
    );
}
