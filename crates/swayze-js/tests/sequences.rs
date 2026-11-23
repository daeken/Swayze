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
fn simple_assignments_in_later_operands_keep_the_rhs_tag_and_store_in_order() {
    for (source, expected) in [
        ("let x; 0, x = 3, x + 1", 4.0),
        ("let x=0; 0, x=++x", 1.0),
        ("let x=0; {x, x=42;} x", 42.0),
        ("let x,y,n=2; (n++, x=y=n++, x=x+y+n, x*100+y*10+n)", 1034.0),
        ("let x=8,y; x += (0,y=2); x*10+y", 102.0),
        ("let x; const a=(0,x=2), b=x+40; b", 42.0),
        ("let x=1; { let x; 0,x=2; } x", 1.0),
    ] {
        assert_value(source, Value::Number(expected));
    }
    for (rhs, expected) in [
        ("null", Value::Null),
        ("undefined", Value::Undefined),
        ("false", Value::Boolean(false)),
    ] {
        assert_value(&format!("let x,y; 0, x=y={rhs};"), expected);
        assert_value(&format!("let x,y; 0, x=y={rhs}, x"), expected);
    }
    let script = parse_script("let x,y; 0, x=y=-0").unwrap();
    assert_eq!(script.evaluate().unwrap().to_bits(), (-0.0f64).to_bits());
}

#[test]
fn grouped_assignment_sequences_finish_the_first_write_before_later_operands() {
    for (source, expected) in [
        ("let x,a=2,b=40; (x=a,b)+x", 42.0),
        ("let x,y,z,n=2; (x=n++, y=x+n, z=y+1, x*100+y*10+z)", 256.0),
        ("let x=8,y; (x/=y=true ? (x=2) : 100, x*10+y)", 42.0),
        ("let x,y=7,z=1; (x=false ? y=2 : z=3, x*100+y*10+z)", 373.0),
        ("let x=1; (x=x++, x)", 1.0),
        ("let x=8,y; x+=(x=3, y=x+1); x*10+y", 124.0),
        ("let x; const a=(x=2,x+1), b=x+a; b", 5.0),
        ("let x,y; if ((x=y=1, true)) { x+=y; } x", 2.0),
        (
            "let x=0,n=0; for ((x+=1,0); (x+=1,x)<5; (x+=1,n++)) {} n*10+x",
            26.0,
        ),
    ] {
        assert_value(source, Value::Number(expected));
    }
}

#[test]
fn a_comma_after_a_grouped_assignment_is_not_part_of_its_rhs() {
    for (expression, result, stored) in [
        ("(x = 1, x + 1)", 2.0, 1.0),
        ("(x = (1, x + 1))", 11.0, 11.0),
    ] {
        let source = format!("let x = 10; let r = {expression};");
        assert_value(&format!("{source} r"), Value::Number(result));
        assert_value(&format!("{source} x"), Value::Number(stored));
    }
}

#[test]
fn grouped_assignment_sequences_preserve_stored_bits_and_the_last_operand_tag() {
    use Value::{Boolean, Null, Undefined};
    for (source, expected) in [
        ("let x; (x=42, null)", Null),
        ("let x=1; (x+=1, undefined)", Undefined),
        ("let x=0; (x=void x, x===undefined)", Boolean(true)),
    ] {
        assert_value(source, expected);
    }
    for source in [
        "let x; (x=-0,x)",
        "let x=0; (x*=-1,x)",
        "let x; (x=null,-0)",
    ] {
        let value = parse_script(source).unwrap().evaluate().unwrap();
        assert_eq!(value.to_bits(), (-0.0f64).to_bits(), "{source}");
    }
}

#[test]
fn a_grouped_logical_assignment_skip_reaches_the_discard_before_the_next_operand() {
    assert_value(
        "let x = 1, n = 0; let r = (x ||= (n = 99), ++n); x * 100 + n * 10 + r",
        Value::Number(111.0),
    );
}

#[test]
fn grouped_logical_assignment_sequences_run_later_operands_on_both_paths() {
    use Value::{Boolean, Null, Number, Undefined};
    for (op, initial, rhs, stored, taken) in [
        ("||=", "true", "null", Boolean(true), false),
        ("||=", "false", "null", Null, true),
        ("&&=", "null", "true", Null, false),
        ("&&=", "true", "undefined", Undefined, true),
        ("??=", "false", "true", Boolean(false), false),
        ("??=", "undefined", "false", Boolean(false), true),
    ] {
        let body = format!(
            "let x={initial}, n=0, seen;\n             const result=(x{op}((n+=1),{rhs}), n=n+10, seen=x, null);"
        );
        for (tail, expected) in [
            ("x", stored),
            ("seen", stored),
            ("n", Number(if taken { 11.0 } else { 10.0 })),
            ("result", Null),
        ] {
            assert_value(&format!("{body} {tail}"), expected);
        }
    }
}

#[test]
fn grouped_assignment_sequence_checks_all_targets_even_when_skipped() {
    for (source, len) in [
        ("const x=1; (x/*☃*/|+=missing,0)", 2),
        ("const x=true; (x|||=missing,0)", 3),
        ("let x=0; { false && (|x+=1,0); let x; } x", 1),
        ("false && (|missing=1,0)", 7),
        ("false && (|undefined??=1,0)", 9),
        ("let x=false; (x &&= |missing,0)", 7),
        ("let x; const y=1; false && (x=2, y|=3,0)", 1),
    ] {
        assert_error_at(source, ErrorKind::Unsupported, len);
    }
}

#[test]
fn existing_sequence_contexts_admit_simple_chains_after_a_comma() {
    for (source, expected) in [
        ("let x=1; if (true, x=0) { x=99; } x", 0.0),
        ("let x,y; if (x=0, y=1) { x=41; } x+y", 42.0),
        (
            "let n=3,total=0; while (true,n=n-1) { total+=n; } total",
            3.0,
        ),
        (
            "let n=3,total=0; do { total+=n; } while (true,n=n-1); total",
            6.0,
        ),
        (
            "let n=3,total=0; for (;false,n=n-1;) { total+=n; } total",
            3.0,
        ),
        (
            "let i,total=0; for (i,i=3;i; i,i=i-1) { total+=i; } total",
            6.0,
        ),
    ] {
        assert_value(source, Value::Number(expected));
    }
}

#[test]
fn skipped_paths_skip_both_rhs_and_stores_but_still_check_bindings() {
    assert_value(
        "let x=1,y=2,n=4;
         false && (0,x=y=n++);
         true ? 0 : (0,x=y=++n);
         if (true) {} else if (0,x=y=n++) {}
         x*100+y*10+n",
        Value::Number(124.0),
    );
    for (source, len) in [
        ("const x=1; false && (0,x/*☃*/|=missing)", 1),
        ("let x=1; { false && (0,|x=2); let x; } x", 1),
        ("let x; true ? 0 : (0,x=|missing)", 7),
        ("if (true) {} else if (0,|undefined=1) {} 42", 9),
        ("do {let x; break;} while (0,|x=1); 0", 1),
        ("for (;false;0, |missing=1) {} 0", 7),
    ] {
        assert_error_at(source, ErrorKind::Unsupported, len);
    }
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
        ("{1,2|} 0", 1, Unsupported),
        ("1, 2; 3,|", 0, Malformed),
        ("let x; x=(1,2)\n|x", 1, Unsupported),
        ("let x; 0, x|+=2", 2, Unsupported),
        ("let x; if (0,x|??=2) {} x", 3, Unsupported),
        ("let x; 0, -x|=2", 1, Unsupported),
        ("let x; 0, 1+x|=2", 1, Unsupported),
        ("let x; 0, (x)|=2", 1, Unsupported),
        ("let x,y; (x=1, y|+=2)", 2, Unsupported),
        ("let x; ((x)|=1,2)", 1, Unsupported),
        ("let x; (1+x|=1,2)", 1, Unsupported),
        ("let x; (x=|,b)", 1, Malformed),
        ("let x; (x+=1,|)", 1, Malformed),
        ("let x; (x=1,|,2)", 1, Malformed),
        ("let x; (x&&=1,|", 0, Malformed),
        ("let x; (x=1,2|", 0, Malformed),
        ("let x; (x=1,2|;", 1, Malformed),
        ("let x; (x=1,2|:3)", 1, Malformed),
        ("let x; (x=1,2)|)", 1, Malformed),
        ("let x; 0, x=|, 1", 1, Malformed),
        ("let x; 0, x=|", 0, Malformed),
        ("let x; {0, x=1|} x", 1, Unsupported),
    ] {
        assert_error_at(source, kind, len);
    }
}

#[test]
fn ungrouped_sequences_in_a_consequent_or_assignment_rhs_error_at_the_comma() {
    use ErrorKind::{Malformed, Unsupported};
    assert_error_at("(true ? 1|, 2 : 3)", Malformed, 1);
    assert_error_at("let x; (true ? 1|, x=2 : 3)", Malformed, 1);
    assert_error_at("let x,y; (x=true ? y=1|, 2 : 3)", Malformed, 1);
    assert_error_at("let x,y; (x=0,y=true ? 1|,2 : 3)", Malformed, 1);
    for source in [
        "let x; x=1|, 2; x",
        "let x; x=1|, x=2; x",
        "let x; for (x=1|,x=2;;) {} x",
        "let x; for (;false;x=1|,x=2) {} x",
    ] {
        assert_error_at(source, Unsupported, 1);
    }
    assert_error_at("let x=0, |x=1; x", Malformed, 1);
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
fn sequence_assignment_links_have_the_same_cost_as_other_simple_prefixes() {
    let plain = "let x;null,true,false";
    let source = "let x;null,x=x=true,x=false";
    let plain_limits = Limits {
        max_source_bytes: plain.len(),
        max_tokens: 8,
        max_nesting_depth: 0,
        // Two init instructions, three literal emissions, two discards.
        max_work: plain.len() + 7,
    };
    let script = parse_script_with_limits(plain, plain_limits).unwrap();
    assert_eq!(script.instruction_count(), 7);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(false)));

    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: plain_limits.max_tokens + 3 * 2,
        max_nesting_depth: 2,
        // Each link adds a name marker, a pending push, a store, and
        // a two-unit lookup. The comma itself adds nothing beyond Discard.
        max_work: source.len() + 7 + 3 * 5,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 10);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(false)));

    let first_comma = source.find(',').unwrap();
    let first_equals = source.find('=').unwrap();
    let second_equals = first_equals + 2;
    let second_comma = source.rfind(',').unwrap();
    let last_name = second_comma + 1;
    for (limits, limit, start) in [
        (
            Limits {
                max_nesting_depth: 1,
                ..limits
            },
            Limit::NestingDepth,
            second_equals,
        ),
        // Source bytes through the first comma, three emissions before Discard.
        (
            Limits {
                max_work: first_comma + 1 + 3,
                ..limits
            },
            Limit::Work,
            first_comma,
        ),
        // Through the assignment token, four emissions and one name charge, before the push.
        (
            Limits {
                max_work: first_equals + 1 + 5,
                ..limits
            },
            Limit::Work,
            first_equals,
        ),
        // At the second comma: five emissions, two names, two pushes, and two stores.
        (
            Limits {
                max_work: second_comma + 1 + 11,
                ..limits
            },
            Limit::Work,
            second_comma,
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            last_name,
        ),
        (
            Limits {
                max_tokens: limits.max_tokens - 1,
                ..limits
            },
            Limit::Tokens,
            source.len() - "false".len(),
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{error}");
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + 1
            },
            "{error}"
        );
    }
}

#[test]
fn opening_the_sequence_keeps_the_group_and_only_adds_normal_comma_costs() {
    for (operator, instructions, stack, before_binary_or_store) in
        [("=", 8, 1, 5), ("+=", 10, 2, 6), ("||=", 10, 1, 7)]
    {
        let source = format!("let x;(x{operator}true,null,false)");
        let limits = Limits {
            max_source_bytes: source.len(),
            max_tokens: 12,
            max_nesting_depth: 1,
            // All instruction emissions, a group marker and its assignment
            // attachment, and a two-unit lookup for the single write site.
            max_work: source.len() + instructions + 4,
        };
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), instructions, "{source}");
        assert_eq!(script.max_stack_depth(), stack, "{source}");
        assert_eq!(
            script.evaluate_value(),
            Ok(Value::Boolean(false)),
            "{source}"
        );

        let comma = source.find(',').unwrap();
        let at_assignment_end = comma + 1 + before_binary_or_store;
        let store = at_assignment_end + usize::from(operator == "+=");
        let span = |start, len| Span {
            start,
            end: start + len,
        };
        let mut cases = vec![
            (
                Limits {
                    max_work: store,
                    ..limits
                },
                Limit::Work,
                span(comma, 1),
            ),
            (
                Limits {
                    max_work: store + 1,
                    ..limits
                },
                Limit::Work,
                span(comma, 1),
            ),
            (
                Limits {
                    max_work: limits.max_work - 1,
                    ..limits
                },
                Limit::Work,
                span(7, 1),
            ),
            (
                Limits {
                    max_tokens: 11,
                    ..limits
                },
                Limit::Tokens,
                span(source.len() - 1, 1),
            ),
        ];
        if operator == "+=" {
            cases.push((
                Limits {
                    max_work: at_assignment_end,
                    ..limits
                },
                Limit::Work,
                span(8, 2),
            ));
        }
        for (limits, limit, expected) in cases {
            let error = parse_script_with_limits(&source, limits).unwrap_err();
            assert_eq!(
                error.kind(),
                ErrorKind::LimitExceeded(limit),
                "{source}: {error}"
            );
            assert_eq!(error.span(), expected, "{source}: {error}");
        }
    }
}

#[test]
fn the_outer_group_counts_against_the_nesting_cap_after_the_first_comma() {
    for (source, opener) in [("let x;(x=1,(null))", "(null"), ("let x;(x=1,x=2)", "=2")] {
        let limits = Limits {
            max_nesting_depth: 2,
            ..Limits::default()
        };
        let script = parse_script_with_limits(source, limits).unwrap();
        assert_eq!(script.max_stack_depth(), 1, "{source}");
        let error = parse_script_with_limits(
            source,
            Limits {
                max_nesting_depth: 1,
                ..limits
            },
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::LimitExceeded(Limit::NestingDepth),
            "{source}"
        );
        let start = source.find(opener).unwrap();
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + 1
            },
            "{source}"
        );
    }
}

#[test]
fn a_skipped_grouped_assignment_still_counts_toward_the_prepared_ceiling() {
    let tail = ",null".repeat(2045);
    let limits = Limits {
        max_tokens: 4100,
        max_nesting_depth: 1,
        ..Limits::default()
    };
    for (prefix, instructions, result) in [
        ("", 4096, Ok(Value::Null)),
        ("!", 4097, Err(EvalError::LimitExceeded)),
    ] {
        let source = format!("let x;(x&&={prefix}true{tail})");
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), instructions);
        assert_eq!(script.max_stack_depth(), 1);
        assert_eq!(script.evaluate_value(), result);
    }
}

#[test]
fn grouped_logical_assignment_sequences_obey_the_dispatch_ceiling_on_both_paths() {
    // Four initialization dispatches, then on each iteration four for the test,
    // two for the load and branch, two extra on take for the RHS and store,
    // one for each Discard (comma and semicolon), one update and one back edge.
    // The final failed test costs four. Pad the final expression to 16,384.
    for (initial, iterations, prefix_count) in [(true, 1637, 5), (false, 1364, 7)] {
        let evaluate = |prefix_count| {
            let prefix = "+ ".repeat(prefix_count);
            let source =
                format!("let x={initial},n=0; while(n<{iterations}){{(x||=false,n++);}} {prefix}n");
            parse_script(&source).unwrap().evaluate()
        };
        assert_eq!(evaluate(prefix_count), Ok(f64::from(iterations)));
        assert_eq!(
            evaluate(prefix_count + 1),
            Err(EvalError::DispatchLimitExceeded)
        );
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
