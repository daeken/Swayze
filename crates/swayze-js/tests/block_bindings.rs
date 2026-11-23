use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, parse_script, parse_script_with_limits,
};

#[test]
fn descendants_read_and_write_earlier_lexical_bindings() {
    let script = parse_script(
        "let out=1;
         if (true) {
             out=out+1;
             const step=out;
             let local=5;
             if (step===2) { local=local+step; out=local; }
         }
         out",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(7.0));
}

#[test]
fn disjoint_branches_and_later_blocks_can_use_the_same_name() {
    for (test, expected) in [("true", 25.0), ("false", 75.0)] {
        let source = format!(
            "let out=0;
             if ({test}) {{ let x=2; x=x*10; out=x; }}
             else {{ const x=7; out=x*10; }}
             if (true) {{ const x=5; out=out+x; }}
             while (false) {{ let x=0; out=x; }}
             const answer=out;
             answer"
        );
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(expected));
    }
}

#[test]
fn loop_reinitializes_let_and_const_after_continue_and_fallthrough() {
    let script = parse_script(
        "let n=5; let total=0;
         while (n) {
             n=n-1;
             if (n===3) { continue; }
             const base=n+1;
             let local=base;
             if (n===4) { local=100; continue; }
             if (n===0) { total=total+local+base; break; }
             total=total+local+base;
         }
         total",
    )
    .unwrap();
    // The first continue leaves an initialized slot behind, and the next
    // iteration skips it. Subsequent entries must initialize it again.
    assert_eq!(script.evaluate(), Ok(12.0));
    assert_eq!(script.evaluate(), Ok(12.0));
}

#[test]
fn inner_break_preserves_mutations_to_the_enclosing_local() {
    let script = parse_script(
        "let outer=2; let total=0;
         while (outer) {
             let local=outer;
             while (true) { const bump=10; local=local+bump; break; }
             total=total+local;
             outer=outer-1;
         }
         total",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(23.0));
}

// Mark the expected diagnostic span at the `|` in each input.
fn error_at(marked: &str, len: usize, kind: ErrorKind) -> swayze_js::Error {
    let (before, after) = marked.split_once('|').unwrap();
    let error = parse_script(&format!("{before}{after}")).unwrap_err();
    assert_eq!(error.kind(), kind, "{marked}: {error}");
    let start = before.len();
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + len
        },
        "{marked}: {error}"
    );
    error
}

#[test]
fn inaccessible_names_are_rejected_even_in_inactive_or_unreachable_code() {
    for source in [
        "if(true){let x=1;}|x",
        "let out=0;if(true){const x=1;}else{out=|x;}out",
        "if(true){while(false){let x=1;}|x=2;}0",
        "while(|x){const x=1;}0",
        "while(true){let x=1;break;}|x=2;0",
        "let out=0;while(true){if(true){let x=1;continue;}out=|x;}out",
        "if(false){const x=|x;}0",
        "if(false){let a=|x;let x=1;}0",
        "if(false){|x=1;let x=2;}0",
    ] {
        error_at(source, 1, ErrorKind::Unsupported);
    }
    error_at("if(true){const x=1;x|=2;}0", 1, ErrorKind::Unsupported);
}

#[test]
fn shadowing_works_in_both_declaration_orders() {
    for (source, expected) in [
        (
            "const x=4; let out=0;
             if(x) {
                 let x=2; x=x+1;
                 if(x===3) { const x=7; out=x; }
                 out=out+x;
             }
             out*10+x",
            104.0,
        ),
        (
            "let out=0; if(true){const x=7;out=x;} let x=2; x=x+1; out*10+x",
            73.0,
        ),
    ] {
        assert_eq!(parse_script(source).unwrap().evaluate(), Ok(expected));
    }
}

#[test]
fn later_enclosing_binding_does_not_hide_an_earlier_descendant_local() {
    let source = "let x=8; let out=0;
                  if(true) {
                      if(true) { let x=3; while(x<5) { x=x+1; } out=x; }
                      let x=6; out=out*10+x;
                  }
                  out*10+x";
    assert_eq!(parse_script(source).unwrap().evaluate(), Ok(568.0));
}

#[test]
fn test_and_other_arm_are_outside_a_shadowing_scope() {
    for (outer, expected) in [(9.0, 9.0), (0.0, 1.0)] {
        let source = format!("let x={outer}; let out=0; if(x){{out=x;}}else{{let x=1;out=x;}}out");
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(expected));
    }
}

#[test]
fn loop_test_keeps_outer_binding_and_reentry_initializes_the_shadow() {
    let script = parse_script(
        "const x=1; let left=4; let out=0;
         while (x && left) {
             left=left-1;
             if (left===2) { continue; }
             let x=left;
             x=x+10;
             out=out+x;
         }
         out*10+x",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(341.0));
    assert_eq!(script.evaluate(), Ok(341.0));
}

#[test]
fn pre_initialization_accesses_are_rejected_even_with_a_known_outer_binding() {
    for source in [
        "let x=1;if(false){const x=|x;}x",
        "let x=1;if(false){let x=false && |x;}x",
        "let x=1;if(false){let a=|x;let x=2;}x",
        "let x=1;if(false){|x=3;const x=2;}x",
        "const x=1;if(false){|x=3;let x=2;}x",
        "let x=1;if(false){if(|x){let x=3;}let x=2;}x",
        "let x=1;if(false){while(|x){let x=3;}let x=2;}x",
        "let x=1;if(false){if(false){}else{const a=|x;}let x=2;}x",
        "let x=1;if(false){while(true){break;|x=3;}let x=2;}x",
        "let out=0;if(true){out=|x;}let x=1;out",
        "if(true){|x=3;}let x=2;x",
    ] {
        let error = error_at(source, 1, ErrorKind::Unsupported);
        assert!(
            error
                .message()
                .contains("before lexical binding initialization"),
            "{error}"
        );
    }
}

#[test]
fn const_write_checks_the_resolved_binding() {
    // A mutable inner binding may shadow a const (covered above), but an
    // initialized inner const cannot be written through an outer let.
    let error = error_at(
        "let x=1;if(false){const x=2;x|=3;}x",
        1,
        ErrorKind::Unsupported,
    );
    assert!(error.message().contains("assignment to const"), "{error}");
}

#[test]
fn shadow_access_resolution_has_an_exact_work_budget() {
    let source = "let x=1;if(x){let x=2;if(true){x=x+1;}}x";
    // Source bytes, 13 emissions, 3 conversion bytes, 2 frames, 2 groups,
    // 1 operator, 8 units for four matching candidates and 2 parent hops.
    let limits = Limits {
        max_work: source.len() + 31,
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(1.0)
    );
    let target = source.find("x=x").unwrap();
    for (max_work, start) in [
        (source.len() + 23, target), // no work left for the first parent hop
        (source.len() + 24, target), // hop paid, no work left to visit a candidate
        (source.len() + 25, target), // candidate visit paid, not the comparison
        (limits.max_work - 1, source.len() - 1), // final root name comparison
    ] {
        let error = parse_script_with_limits(source, Limits { max_work, ..limits }).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + 1
            }
        );
    }
}

#[test]
fn local_declarations_require_one_initializer_and_a_semicolon() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind) in [
        ("if(false){let x|;}0", Malformed),
        ("if(false){const x=|;}0", Malformed),
        ("while(false){let x=1|}0", Unsupported),
        ("if(false){let x=1|,y=2;}0", Unsupported),
        ("if(false){const x=1;let |x=2;}0", Malformed),
        (
            "let x=0;if(false){let x=1;while(false){let x=2;}const |x=3;}x",
            Malformed,
        ),
        ("while(true){let x=1;break;const |x=2;}0", Malformed),
    ] {
        error_at(source, 1, kind);
    }
}

#[test]
fn skipped_block_declarations_count_against_parse_budgets() {
    let source = "if(false){let x=0;}else{const x=1;}2";
    let limits = Limits {
        max_tokens: 20,
        // Source bytes, frame, pending test group, 8 emissions and 3 decimal
        // conversion bytes. Disjoint scopes don't scan each other's declarations.
        max_work: source.len() + 13,
        ..Limits::default()
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 8);
    assert_eq!(script.evaluate(), Ok(2.0));
    let span = Span {
        start: source.len() - 1,
        end: source.len(),
    };
    for (limit, limits) in [
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
        assert_eq!(error.span(), span);
    }
}

#[test]
fn skipped_initializers_count_toward_the_prepared_instruction_ceiling() {
    for (pluses, expected) in [(4091, Ok(42.0)), (4092, Err(EvalError::LimitExceeded))] {
        let source = format!("if(false){{const x={}0;}}42", "+ ".repeat(pluses));
        let limits = Limits {
            max_tokens: 4200,
            ..Limits::default()
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), pluses + 5);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn reinitialization_uses_dispatch_fuel_at_the_inclusive_limit() {
    // 9 per iteration, two top-level init instructions, two for the last
    // (falsy) test, and one load plus `pluses` for the final expression.
    for (pluses, expected) in [(8, Ok(0.0)), (9, Err(EvalError::DispatchLimitExceeded))] {
        let source = format!(
            "let n=1819;while(n){{const step=1;n=n-step;continue;}}{}n",
            "+ ".repeat(pluses),
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
    }
}
