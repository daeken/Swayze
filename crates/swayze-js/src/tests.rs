use crate::parser::BindingKind;
use crate::parser::Instruction::{
    self, Add, Divide, InitBinding, LoadBinding, Multiply, Negate, Number, Positive, Remainder,
    Subtract,
};
use crate::{ErrorKind, Limit, Limits, Span, parse_script, parse_script_with_limits};

fn assert_postfix(source: &str, expected: &[Instruction], depth: usize) {
    let script = parse_script(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    assert_eq!(script.instruction_count(), expected.len());
    assert_eq!(script.instructions(), expected);
    assert_eq!(script.max_stack_depth(), depth);
}

fn assert_error(source: &str, kind: ErrorKind, start: usize, end: usize) {
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source:?}: {error}");
    assert_eq!(error.span(), Span { start, end }, "{source:?}: {error}");
}

fn assert_limit(source: &str, limits: Limits, limit: Limit, start: usize, end: usize) {
    let error = parse_script_with_limits(source, limits).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{error}");
    assert_eq!(error.span(), Span { start, end }, "{error}");
}

#[test]
fn if_emits_forward_jumps_and_accounts_for_the_peak_in_either_branch() {
    use Instruction::{Boolean, Jump, JumpIfFalseStatement, StoreBinding};

    assert_postfix(
        "let x=0;if(true){x=1;}else{x=2+(3+4);}x",
        &[
            Number(0.0),
            InitBinding(BindingKind::Let),
            Boolean(true),
            JumpIfFalseStatement(7),
            Number(1.0),
            StoreBinding(0),
            Jump(13),
            Number(2.0),
            Number(3.0),
            Number(4.0),
            Add,
            Add,
            StoreBinding(0),
            LoadBinding(0),
        ],
        3,
    );
    assert_postfix(
        "if(false){}42",
        &[Boolean(false), JumpIfFalseStatement(2), Number(42.0)],
        1,
    );
}

#[test]
fn while_emits_an_exit_and_back_edge_with_balanced_stack_depth() {
    use Instruction::{Boolean, JumpIfFalseStatement, LoopBack, StoreBinding};

    assert_postfix(
        "let x=1;while(x){x=x-(2+3);}x",
        &[
            Number(1.0),
            InitBinding(BindingKind::Let),
            LoadBinding(0),
            JumpIfFalseStatement(11),
            LoadBinding(0),
            Number(2.0),
            Number(3.0),
            Add,
            Subtract,
            StoreBinding(0),
            LoopBack(2),
            LoadBinding(0),
        ],
        3,
    );
    assert_postfix(
        "while(false){}42",
        &[
            Boolean(false),
            JumpIfFalseStatement(3),
            LoopBack(0),
            Number(42.0),
        ],
        1,
    );
}

#[test]
fn while_body_and_back_edge_are_metered_even_if_skipped() {
    let source = "let x=0;while(false){x=1;}x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 16,
        max_nesting_depth: 1,
        // Same as an if with no else, plus one emission for the back edge.
        max_work: source.len() + 15,
    };
    assert!(parse_script_with_limits(source, limits).is_ok());
    assert_limit(
        source,
        Limits {
            max_tokens: 15,
            ..limits
        },
        Limit::Tokens,
        source.len() - 1,
        source.len(),
    );
    let close = source.find('}').unwrap();
    assert_limit(
        source,
        Limits {
            max_work: close + 1 + 11,
            ..limits
        },
        Limit::Work,
        close,
        close + 1,
    ); // The source through '}', two conversions, one marker, six prior emissions, and a lookup.
    assert_limit(
        source,
        Limits {
            max_nesting_depth: 0,
            ..limits
        },
        Limit::NestingDepth,
        13,
        14,
    );
}

#[test]
fn if_tokens_and_work_are_counted_once_even_in_untaken_branches() {
    for (source, tokens, work_beyond_source) in [
        ("let x=0;if(false){x=1;}x", 16, 14),
        ("let x=0;if(false){x=1;}else{x=2;}x", 23, 20),
    ] {
        // Besides the source bytes: decimal input bytes, the '(' marker,
        // instruction emissions, and binding lookups for each store and load.
        let limits = Limits {
            max_source_bytes: source.len(),
            max_tokens: tokens,
            max_work: source.len() + work_beyond_source,
            max_nesting_depth: 1,
        };
        assert!(parse_script_with_limits(source, limits).is_ok());
        for (limit, below) in [
            (
                Limit::Tokens,
                Limits {
                    max_tokens: tokens - 1,
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
            assert_limit(source, below, limit, source.len() - 1, source.len());
        }

        let body_start = source.find('{').unwrap() + 1;
        assert_limit(
            source,
            Limits {
                max_tokens: 10,
                ..limits
            },
            Limit::Tokens,
            body_start,
            body_start + 1,
        );
        // Scanning through '{', plus the conversion of `0`, the group marker,
        // and four instructions (Number, InitBinding, Boolean, JumpIfFalse).
        assert_limit(
            source,
            Limits {
                max_work: body_start + 6,
                ..limits
            },
            Limit::Work,
            body_start,
            body_start + 1,
        );
    }
}

#[test]
fn booleans_are_whole_tokens_not_names_or_prefixes_of_names() {
    use ErrorKind::{Malformed, Unsupported};
    use Instruction::Boolean;

    assert_postfix("true", &[Boolean(true)], 1);
    assert_postfix("(/*comment*/false\u{00A0});", &[Boolean(false)], 1);
    for literal in ["true", "false"] {
        for suffix in ["x", "0", "_", "$"] {
            let name = format!("{literal}{suffix}");
            assert_error(&name, Unsupported, 0, name.len());
            assert_postfix(
                &format!("let {name}=true;{name}"),
                &[Boolean(true), InitBinding(BindingKind::Let), LoadBinding(0)],
                1,
            );
        }
        for suffix in ["é", "\u{200D}", "\\u0061"] {
            assert_error(
                &format!("{literal}{suffix}"),
                Unsupported,
                literal.len(),
                literal.len() + suffix.chars().next().unwrap().len_utf8(),
            );
        }
        for declaration in ["const", "let"] {
            let start = declaration.len() + 1;
            assert_error(
                &format!("{declaration} {literal}=1; 2"),
                Unsupported,
                start,
                start + literal.len(),
            );
        }
    }
    assert_postfix(
        "const True=false; True",
        &[
            Boolean(false),
            InitBinding(BindingKind::Const),
            LoadBinding(0),
        ],
        1,
    );
    assert_error("true=1; false", Unsupported, 4, 5);
    assert_error("1true", Unsupported, 1, 2);
    assert_error("false true", Malformed, 6, 10);
    assert_error("false\ntrue", Unsupported, 6, 10);
    assert_error("true;false", Unsupported, 5, 10);
}

#[test]
fn boolean_literals_share_token_and_emission_budgets_without_decimal_conversion() {
    let source = "true?false:0";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 5,
        max_nesting_depth: 1,
        // Source bytes, one decimal conversion byte, five emissions, two pending pushes.
        max_work: source.len() + 8,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 5);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate_value(), Ok(crate::Value::Boolean(false)));
    assert_limit(
        source,
        Limits {
            max_tokens: 4,
            ..limits
        },
        Limit::Tokens,
        11,
        12,
    );
    assert_limit(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        Limit::Work,
        11,
        12,
    );
}

#[test]
fn precedence_associativity_and_grouping() {
    assert_postfix(
        "1 + 2 * (3-4);",
        &[
            Number(1.),
            Number(2.),
            Number(3.),
            Number(4.),
            Subtract,
            Multiply,
            Add,
        ],
        4,
    );
    assert_postfix(
        "10 - 3 - 2 + 1",
        &[
            Number(10.),
            Number(3.),
            Subtract,
            Number(2.),
            Subtract,
            Number(1.),
            Add,
        ],
        2,
    );
    assert_postfix(
        "2*3*4",
        &[Number(2.), Number(3.), Multiply, Number(4.), Multiply],
        2,
    );
    assert_postfix(
        "(1+2)*3",
        &[Number(1.), Number(2.), Add, Number(3.), Multiply],
        2,
    );
    assert_postfix("((0))", &[Number(0.)], 1);
}

#[test]
fn equal_precedence_logical_operators_associate_left() {
    use Instruction::ShortCircuit;
    for (symbol, when_truthy) in [("&&", false), ("||", true)] {
        assert_postfix(
            &format!("1{symbol}2{symbol}3"),
            &[
                Number(1.),
                ShortCircuit {
                    target: 3,
                    when_truthy,
                },
                Number(2.),
                ShortCircuit {
                    target: 5,
                    when_truthy,
                },
                Number(3.),
            ],
            1,
        );
        assert_postfix(
            &format!("1{symbol}(2{symbol}3)"),
            &[
                Number(1.),
                ShortCircuit {
                    target: 5,
                    when_truthy,
                },
                Number(2.),
                ShortCircuit {
                    target: 5,
                    when_truthy,
                },
                Number(3.),
            ],
            1,
        );
    }
}

#[test]
fn logical_punctuators_use_maximal_munch_without_crossing_trivia() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, start, end) in [
        ("!===1", Malformed, 0, 3), // `!==` followed by `=`, not `!` followed by `===`
        ("!=1", Unsupported, 0, 2),
        ("1&2", Unsupported, 1, 2),
        ("1|2", Unsupported, 1, 2),
        ("1&=2", Unsupported, 1, 3),
        ("1|=2", Unsupported, 1, 3),
        ("1&&=2", Unsupported, 1, 4),
        ("1||=2", Unsupported, 1, 4),
        ("1&&&2", Unsupported, 3, 4),
        ("1||||2", Malformed, 3, 5),
        ("1&/**/&2", Unsupported, 1, 2),
        ("1|\n|2", Unsupported, 1, 2),
        ("1|| =2", Unsupported, 4, 5),
        ("1!", Unsupported, 1, 2),
        ("1||2++", Unsupported, 4, 6),
        ("1&&2?.x", Unsupported, 4, 6),
        ("1||2??3", Unsupported, 4, 6),
        ("1&&2??=3", Unsupported, 4, 7),
    ] {
        assert_error(source, kind, start, end);
    }
}

#[test]
fn logical_operators_charge_work_and_tokens_without_using_nesting_depth() {
    let source = "!true&&false";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 4,
        max_nesting_depth: 0,
        // Source bytes, two pending pushes, and four emissions.
        max_work: source.len() + 6,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 4);
    assert_eq!(script.max_stack_depth(), 1);
    assert_limit(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        Limit::Work,
        7,
        12,
    );
    assert_limit(
        source,
        Limits {
            max_tokens: 3,
            ..limits
        },
        Limit::Tokens,
        7,
        8,
    );
    assert_limit(
        "true&&(false||true)",
        Limits {
            max_nesting_depth: 0,
            ..Limits::default()
        },
        Limit::NestingDepth,
        6,
        7,
    );
}

#[test]
fn conditionals_compile_to_forward_jumps_and_join_at_the_same_stack_depth() {
    use Instruction::{Jump, JumpIfFalse};
    assert_postfix(
        "9+(0?2*3:4?5:6)",
        &[
            Number(9.),
            Number(0.),
            JumpIfFalse(7),
            Number(2.),
            Number(3.),
            Multiply,
            Jump(12),
            Number(4.),
            JumpIfFalse(11),
            Number(5.),
            Jump(12),
            Number(6.),
            Add,
        ],
        3,
    );
    assert_postfix(
        "1?0?2:3:4",
        &[
            Number(1.),
            JumpIfFalse(8),
            Number(0.),
            JumpIfFalse(6),
            Number(2.),
            Jump(7),
            Number(3.),
            Jump(9),
            Number(4.),
        ],
        1,
    );
}

#[test]
fn malformed_conditional_delimiters_and_longer_punctuators() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, start, end) in [
        ("?1:2", 0, 1),
        ("1?", 2, 2),
        ("1:?2", 1, 2),
        ("1?:2", 2, 3),
        ("1?2", 3, 3),
        ("1?2:", 4, 4),
        ("1?2:;", 4, 5),
        ("1?2:3:4", 5, 6),
        ("(1?2):3", 4, 5),
        ("1?(2:3)", 4, 5),
        ("1?2+ :3", 5, 6),
        ("1 + ?2:3", 4, 5),
        ("const x=1?2; x", 11, 12),
        ("let x=1; x=1?2", 14, 14),
    ] {
        assert_error(source, Malformed, start, end);
    }
    for (source, end) in [("1??2", 3), ("1??=2", 4), ("1?.x", 3), ("1?.(2)", 3)] {
        assert_error(source, Unsupported, 1, end);
    }
    assert_error("1?2:3\n4", Unsupported, 6, 7);
}

#[test]
fn conditional_syntax_jumps_and_pending_frames_are_metered() {
    let limits = Limits {
        max_tokens: 5,
        max_work: 15, // 5 scanned bytes, 3 conversions, 5 emissions, 2 pending pushes
        max_nesting_depth: 1,
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits("0?1:2", limits)
            .unwrap()
            .evaluate(),
        Ok(2.0)
    );
    assert_limit(
        "0?1:2",
        Limits {
            max_tokens: 4,
            ..limits
        },
        Limit::Tokens,
        4,
        5,
    );
    assert_limit(
        "0?1:2",
        Limits {
            max_work: 14,
            ..limits
        },
        Limit::Work,
        4,
        5,
    );

    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    assert_limit("(1?2:3)", limits, Limit::NestingDepth, 2, 3);
    assert_limit("1?2:3?4:5", limits, Limit::NestingDepth, 5, 6);
    assert_limit("1?2?3:4:5", limits, Limit::NestingDepth, 3, 4);
    assert!(parse_script_with_limits("(1)?2:3", limits).is_ok());

    let count = 10_000;
    let source = "0?1:".repeat(count) + "2";
    let script = parse_script_with_limits(
        &source,
        Limits {
            max_tokens: 4 * count + 1,
            max_nesting_depth: count,
            ..limits
        },
    )
    .unwrap();
    assert_eq!(script.instruction_count(), 4 * count + 1);
    assert_eq!(script.max_stack_depth(), 1);
}

#[test]
fn postfix_orders_prefix_and_multiplicative_operators() {
    assert_postfix(
        "- + -(1+2)/-4%+5*6",
        &[
            Number(1.),
            Number(2.),
            Add,
            Negate,
            Positive,
            Negate,
            Number(4.),
            Negate,
            Divide,
            Number(5.),
            Positive,
            Remainder,
            Number(6.),
            Multiply,
        ],
        2,
    );
    assert_postfix(
        "8/-(-2)",
        &[Number(8.), Number(2.), Negate, Negate, Divide],
        2,
    );
    assert_postfix("1+ +2", &[Number(1.), Number(2.), Positive, Add], 2);
    assert_postfix("1- -2", &[Number(1.), Number(2.), Negate, Subtract], 2);
}

#[test]
fn entire_input_is_required() {
    use ErrorKind::{Malformed, Unsupported};
    assert_error("1; 2", Unsupported, 3, 4);
    assert_error("1;;", Unsupported, 2, 3);
    assert_error("1;\u{00A0}🚧", Unsupported, 4, 8);
    assert_error("1; let x", Unsupported, 3, 6);
    assert_postfix("1; // comment", &[Number(1.)], 1);
    assert_error("1;\0", Unsupported, 2, 3);
    assert_error("1 23", Malformed, 2, 4);
    assert_error("1\n2", Unsupported, 2, 3);
    assert_error("1\n(2)", Unsupported, 2, 3); // call, not ASI
    assert_error("1(2)", Unsupported, 1, 2);
    assert_error("(1)2", Malformed, 3, 4);
    assert_postfix("1\n-2\n;\t  ", &[Number(1.), Number(2.), Subtract], 2);
    assert_postfix("8\n/\n2", &[Number(8.), Number(2.), Divide], 2);
    assert_error("1\n++2", Unsupported, 2, 4);
    assert_error("1\n--2", Unsupported, 2, 4);
    assert_error("1;\n/2/", Unsupported, 3, 4);
}

#[test]
fn consts_compile_to_ordered_slots_and_share_the_operand_stack() {
    assert_postfix(
        "const x = 2; const y = x * 5; y + 1;",
        &[
            Number(2.),
            InitBinding(BindingKind::Const),
            LoadBinding(0),
            Number(5.),
            Multiply,
            InitBinding(BindingKind::Const),
            LoadBinding(1),
            Number(1.),
            Add,
        ],
        2,
    );
    assert_postfix(
        "const constx=1; const const0=constx; const const_=const0; const const$=const_; const$",
        &[
            Number(1.),
            InitBinding(BindingKind::Const),
            LoadBinding(0),
            InitBinding(BindingKind::Const),
            LoadBinding(1),
            InitBinding(BindingKind::Const),
            LoadBinding(2),
            InitBinding(BindingKind::Const),
            LoadBinding(3),
        ],
        1,
    );
    assert_postfix(
        "const\u{00A0}$a0 = 2\n+ 1; const _B=$a0; _B",
        &[
            Number(2.),
            Number(1.),
            Add,
            InitBinding(BindingKind::Const),
            LoadBinding(0),
            InitBinding(BindingKind::Const),
            LoadBinding(1),
        ],
        2,
    );
}

#[test]
fn consts_decline_reserved_and_non_subset_names_without_splitting() {
    use ErrorKind::Unsupported;
    for name in [
        "const",
        "let",
        "var",
        "for",
        "return",
        "class",
        "enum",
        "await",
        "yield",
        "implements",
        "static",
        "using",
        "eval",
        "arguments",
        "NaN",
        "undefined",
    ] {
        let source = format!("const {name}=1; 2");
        assert_error(&source, Unsupported, 6, 6 + name.len());
    }
    for (source, start, end) in [
        ("constx=1; x", 0, 6),
        ("Const x=1; x", 0, 5),
        ("consté x=1; x", 5, 7),
        ("const\\u0078=1; x", 5, 6),
        ("const é=1; 1", 6, 8),
        ("const xé=1; 1", 7, 9),
        ("const x\\u0061=1; 1", 7, 8),
        ("const x=1; x\u{200D}", 12, 15),
        ("const x==1; x", 7, 9),
        ("const x=>1; x", 7, 9),
    ] {
        assert_error(source, Unsupported, start, end);
    }
}

#[test]
fn const_grammar_requires_initializers_delimiters_and_one_final_expression() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, start, end) in [
        ("const", Malformed, 5, 5),
        ("const x; x", Malformed, 7, 8),
        ("const x=; x", Malformed, 8, 9),
        ("const x===1; x", Malformed, 7, 10),
        ("const x=1", Unsupported, 9, 9),
        ("const x=1;", Unsupported, 10, 10),
        ("const x=1\nx", Unsupported, 10, 11),
        ("const x=1\nconst y=2; x", Unsupported, 10, 15),
        ("const x=1,y=2; x", Unsupported, 9, 10),
        ("const [x]=1; x", Unsupported, 6, 7),
        ("const {x}=1; x", Unsupported, 6, 7),
        ("const x=(1;); x", Malformed, 10, 11),
        ("const x=1;; x", Unsupported, 10, 11),
        ("const x=1; x; x", Unsupported, 14, 15),
        ("const x=1; x; const y=1; y", Unsupported, 14, 19),
        ("const x=1; x=2", Unsupported, 12, 13),
        ("const x=1; x += 2", Unsupported, 13, 15),
        ("const x=1; x(2)", Unsupported, 12, 13),
        ("const x=1; x.y", Unsupported, 12, 13),
        ("(const x=1; x)", Unsupported, 1, 6),
    ] {
        assert_error(source, kind, start, end);
    }
}

#[test]
fn duplicate_declarations_are_early_errors_and_unavailable_reads_are_unsupported() {
    use ErrorKind::{Malformed, Unsupported};
    let duplicate = "const x=1; const y=x; const x=2; y";
    assert_error(duplicate, Malformed, 28, 29);
    assert_eq!(
        parse_script(duplicate).unwrap_err().message(),
        "duplicate binding declaration"
    );
    for (source, start, end) in [
        ("x", 0, 1),
        ("const x=x; x", 8, 9),
        ("const x=y; const y=1; x", 8, 9),
        ("const x=1; const y=y+x; y", 19, 20),
        ("const x=1; X", 11, 12), // case-sensitive
    ] {
        assert_error(source, Unsupported, start, end);
        assert!(
            parse_script(source)
                .unwrap_err()
                .message()
                .contains("earlier declaration")
        );
    }
}

#[test]
fn consts_charge_parsing_and_resolution_to_existing_budgets() {
    let source = "const x=0;x";
    let limits = Limits {
        max_tokens: 6,
        max_work: 17,
        ..Limits::default()
    };
    assert!(parse_script_with_limits(source, limits).is_ok());
    assert_limit(
        source,
        Limits {
            max_tokens: 5,
            ..limits
        },
        Limit::Tokens,
        10,
        11,
    );
    assert_limit(
        source,
        Limits {
            max_work: 15,
            ..limits
        },
        Limit::Work,
        10,
        11,
    );
    assert_limit(
        "const x=(0);x",
        Limits {
            max_nesting_depth: 0,
            ..Limits::default()
        },
        Limit::NestingDepth,
        8,
        9,
    );
}

#[test]
fn lets_and_assignments_compile_to_ordered_slot_operations() {
    use Instruction::StoreBinding;
    assert_postfix(
        "let x = 2; const step = 5; x = x * step; x + 1;",
        &[
            Number(2.),
            InitBinding(BindingKind::Let),
            Number(5.),
            InitBinding(BindingKind::Const),
            LoadBinding(0),
            LoadBinding(1),
            Multiply,
            StoreBinding(0),
            LoadBinding(0),
            Number(1.),
            Add,
        ],
        2,
    );
    assert_postfix(
        "let/**/let0=1; let let_=let0; let let$=let_; let Let=let$; Let",
        &[
            Number(1.),
            InitBinding(BindingKind::Let),
            LoadBinding(0),
            InitBinding(BindingKind::Let),
            LoadBinding(1),
            InitBinding(BindingKind::Let),
            LoadBinding(2),
            InitBinding(BindingKind::Let),
            LoadBinding(3),
        ],
        1,
    );
}

#[test]
fn mutable_binding_gaps_and_duplicate_declarations() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, start, end) in [
        ("let let=1; 2", Unsupported, 4, 7),
        ("let const=1; 2", Unsupported, 4, 9),
        ("let eval=1; 2", Unsupported, 4, 8),
        ("leté=1; 2", Unsupported, 3, 5),
        ("let\\u0078=1; 2", Unsupported, 3, 4),
        ("let x; x", Malformed, 5, 6),
        ("let x=; x", Malformed, 6, 7),
        ("let x=x; x", Unsupported, 6, 7),
        ("let x=y; let y=1; x", Unsupported, 6, 7),
        ("let x=1; x=y; let y=0; x", Unsupported, 11, 12),
        ("let x=1; x=y; x", Unsupported, 11, 12),
        ("x=1; let x=2; x", Unsupported, 0, 1),
        ("x=1; 2", Unsupported, 0, 1),
        ("const x=1; x=2; x", Unsupported, 12, 13),
        ("let x=1; let x=2; x", Malformed, 13, 14),
        ("let x=1; const x=2; x", Malformed, 15, 16),
        ("const x=1; let x=2; x", Malformed, 15, 16),
        ("let x=1; x=0; let x=2; x", Malformed, 18, 19),
        ("{let x=1;} x", Unsupported, 0, 1),
    ] {
        assert_error(source, kind, start, end);
    }
}

#[test]
fn only_explicit_simple_assignments_before_the_final_expression() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, start, end) in [
        ("let x=1; x=2", Unsupported, 12, 12),
        ("let x=1; x=2;", Unsupported, 13, 13),
        ("let x=1; x=; x", Malformed, 11, 12),
        ("let x=1; x=", Malformed, 11, 11),
        ("let x=1; x=2\nx", Unsupported, 13, 14),
        ("let x=1; x=2/*\u{2028}*/x", Unsupported, 19, 20),
        ("let x=1\nx=2; x", Unsupported, 8, 9),
        ("let x=1; (x)=2; x", Unsupported, 12, 13),
        ("let x=1; (x=2)", Unsupported, 11, 12),
        ("let x=1; x + (x=2)", Unsupported, 15, 16),
        ("let x=1; let y=(x=2); y", Unsupported, 17, 18),
        ("let x=1; x=x=2; x", Unsupported, 12, 13),
        ("let x=1; x+=2; x", Unsupported, 10, 12),
        ("let x=1; x**=2; x", Unsupported, 10, 13),
        ("let x=1; x==2; x", Unsupported, 10, 12),
        ("let x=1; x=>2; x", Unsupported, 10, 12),
        ("let x=1; x; x=2; x", Unsupported, 12, 13),
        ("let x=1; x=2; x; 🚧", Unsupported, 17, 21),
        ("let x=1; x/*🚧", Malformed, 10, 16),
        ("let x=1; x/*🚧*/2", Malformed, 18, 19),
        ("let x=1; x/*\u{2028}*/2", Unsupported, 17, 18),
    ] {
        assert_error(source, kind, start, end);
    }
}

#[test]
fn assignment_lookahead_and_resolution_respect_inclusive_budgets() {
    let source = "let x=0;x/*α*/=x+1;x/*\u{2028}*/+x; //β";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 15,
        // Two decimal conversions, two pending operators, nine emissions,
        // and four name resolutions at two units each.
        max_work: source.len() + 21,
        max_nesting_depth: 0,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.evaluate(), Ok(2.0));
    let semicolon = source.rfind(';').unwrap();
    assert_limit(
        source,
        Limits {
            max_tokens: 14,
            ..limits
        },
        Limit::Tokens,
        semicolon,
        semicolon + 1,
    );
    assert_limit(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        Limit::Work,
        source.len() - 'β'.len_utf8(),
        source.len(),
    );
    assert_limit("let x=0;x=(1);x", limits, Limit::NestingDepth, 10, 11);

    // Stop within the assignment target lookup: eight scanned bytes,
    // one conversion, two emitted ops, and two scanned lookahead bytes.
    assert_limit(
        "let x=0;x=1;x",
        Limits {
            max_work: 14,
            ..limits
        },
        Limit::Work,
        8,
        9,
    );
}

#[test]
fn malformed_subset_and_unsupported_constructs() {
    use ErrorKind::{Malformed, Unsupported};
    assert_error("1+", Malformed, 2, 2);
    assert_error("1+;", Malformed, 2, 3);
    assert_error("1+*2", Malformed, 2, 3);
    assert_error("(1", Malformed, 2, 2);
    assert_error("1)", Malformed, 1, 2);
    assert_error("()", Malformed, 1, 2);
    assert_error("((1)", Malformed, 4, 4);
    assert_error("(1;)", Malformed, 2, 3);
    assert_error("(1\n2)", Malformed, 3, 4);
    assert_error("", Unsupported, 0, 0);
    assert_error(";", Unsupported, 0, 1);
    assert_error("-", Malformed, 1, 1);
    assert_error("(+)", Malformed, 2, 3);
    assert_error("1/", Malformed, 2, 2);
    assert_error("1%", Malformed, 2, 2);
    assert_error("1%%2", Malformed, 2, 3);
    assert_error("() => 1", Unsupported, 3, 5);
    assert_error("(()/*comment*/=>1)", Unsupported, 14, 16);
}

#[test]
fn operator_lexical_boundaries_are_conservative() {
    use ErrorKind::Unsupported;
    for source in ["1--2", "1++2", "1**2", "1-=2", "1/=2", "1%=2"] {
        assert_error(source, Unsupported, 1, 3);
    }
    assert_error("++2", Unsupported, 0, 2);
    assert_error("--2", Unsupported, 0, 2);
    assert_error("1**=2", Unsupported, 1, 4);
    assert_error("-1**2", Unsupported, 2, 4);
    assert_error("(-1)**2", Unsupported, 4, 6);
    assert_error("1\n**2", Unsupported, 2, 4);
    assert_postfix("1/*comment*/+2", &[Number(1.), Number(2.), Add], 2);
    assert_postfix("1//2", &[Number(1.)], 1);
    assert_postfix("-/*comment*/1", &[Number(1.), Negate], 1);
    assert_error("/2/", Unsupported, 0, 1);
    assert_error("8 / /2/", Unsupported, 4, 5);
    assert_error("+ /2/", Unsupported, 2, 3);
}

#[test]
fn whitespace_and_utf8_spans() {
    use ErrorKind::{Malformed, Unsupported};
    let white = "\u{FEFF}\t\u{000B}\u{000C} \u{00A0}\u{1680}\u{2000}\u{200A}\u{202F}\u{205F}\u{3000}\n\r\u{2028}\u{2029}";
    assert_postfix(
        &format!("{white}1{white}+{white}2{white};{white}"),
        &[Number(1.), Number(2.), Add],
        2,
    );
    assert_error(white, Unsupported, white.len(), white.len());
    assert_error("1\u{2028}2", Unsupported, 4, 5);
    assert_error("1+\u{0085}2", Unsupported, 2, 4); // NEL is not a JS line terminator
    assert_error("1+\u{180E}2", Unsupported, 2, 5); // not ECMAScript WhiteSpace
    assert_error("1+\u{200B}2", Unsupported, 2, 5);
    assert_error("1+\u{00A0}", Malformed, 4, 4);
    assert_error("1+３", Unsupported, 2, 5); // ASCII decimal digits only
    let error = parse_script("1+🔑").unwrap_err();
    assert!(error.to_string().contains("bytes 2..6"));
    assert!(!format!("{error:?} {error}").contains('🔑'));
}

#[test]
fn non_slice_numeric_lexemes_are_not_split() {
    use ErrorKind::Unsupported;
    for input in [
        "00",
        "08",
        "01.5",
        "08e2",
        "077.toString()",
        "0x10",
        "0o10",
        "0b10",
        "1n",
        "1_000",
        "1foo",
        "1$foo",
        "1\\u0061",
    ] {
        assert_error(input, Unsupported, 1, 2);
    }
    for (source, start, end) in [
        (".", 0, 1),
        ("...5", 0, 1),
        (".５", 0, 1),
        ("(1).toString()", 3, 4),
        ("1 .toString()", 2, 3),
        ("1..toString()", 2, 3),
        ("1.toString()", 2, 3),
        ("1.5.toString()", 3, 4),
        ("1e3.toString()", 3, 4),
        ("1...2", 2, 3),
        ("1.2.3", 3, 4),
        ("1else", 2, 3),
        ("1.evil", 3, 4),
        ("1e2tail", 3, 4),
        ("1e2e3", 3, 4),
        ("1é", 1, 3),
        (".5é", 2, 4),
        ("1e2\\u0061", 3, 4),
        ("10n", 2, 3),
        (".5n", 2, 3),
        ("1.e2n", 4, 5),
        ("1._5", 2, 3),
        ("1.5_0", 3, 4),
        ("1e_2", 2, 3),
        ("1e+_2", 3, 4),
        ("1e2_0", 3, 4),
    ] {
        assert_error(source, Unsupported, start, end);
    }
}

#[test]
fn decimal_literal_forms_and_token_boundaries() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, expected) in [
        (".5", 0.5),
        ("1.", 1.0),
        ("1.25", 1.25),
        ("0.05", 0.05),
        ("1e3", 1000.0),
        ("1E-3", 0.001),
        ("1e+003", 1000.0),
        ("1.e3", 1000.0),
        (".5e1", 5.0),
        ("1.25e+1", 12.5),
    ] {
        assert_postfix(source, &[Number(expected)], 1);
    }
    assert_postfix("1e-2- .5", &[Number(0.01), Number(0.5), Subtract], 2);
    assert_postfix(".5\u{2028}+1.", &[Number(0.5), Number(1.0), Add], 2);
    for (source, offset) in [
        ("1e", 2),
        ("1e-", 3),
        ("1e+;", 3),
        ("1.e 2", 3),
        (".5e\u{00A0}2", 3),
    ] {
        assert_error(source, Malformed, offset, offset);
    }
    assert_error("1 .5", Malformed, 2, 4);
    assert_error("1.\n.5", Unsupported, 3, 5);
    assert_error(".5; 1e0", Unsupported, 4, 7);
    assert_postfix("1./*comment*/", &[Number(1.)], 1);
    assert_postfix(".5e-1//comment", &[Number(0.05)], 1);
}

#[test]
fn decimal_precision_and_overflow() {
    for (input, expected) in [
        ("9007199254740991", 9_007_199_254_740_991.), // 2^53 - 1
        ("9007199254740992", 9_007_199_254_740_992.),
        ("9007199254740993", 9_007_199_254_740_992.), // midpoint rounds down
        ("9007199254740995", 9_007_199_254_740_996.), // midpoint rounds up
        ("18446744073709551616", 18_446_744_073_709_551_616.), // 2^64
    ] {
        assert_postfix(input, &[Number(expected)], 1);
    }
    // The 309-digit overflow midpoint is 2^1024 - 2^970. The preceding
    // integer rounds to the largest finite Number; the midpoint overflows.
    let prefix = concat!(
        "17976931348623158079372897140530341507993413271003782693617377898044496829276475",
        "094664901797758720709633028641669288791094655554785194040263065748867150582068",
        "190890200070838367627385484581771153176447573027006985557136695962284291481986",
        "083493647529271907416844436551070434271155969950809304288017790417449779",
    );
    assert_postfix(&format!("{prefix}1"), &[Number(f64::MAX)], 1);
    assert_postfix(&format!("{prefix}2"), &[Number(f64::INFINITY)], 1);
    let above = format!("1{}", "0".repeat(309));
    assert_postfix(&above, &[Number(f64::INFINITY)], 1);
    assert_error(&format!("{above}n"), ErrorKind::Unsupported, 310, 311);
}

#[test]
fn fractional_and_exponential_literals_round_once() {
    for (source, expected) in [
        (
            "1.00000000000000011102230246251565404236316680908203125",
            1.0,
        ),
        (
            "1.00000000000000011102230246251565404236316680908203126",
            f64::from_bits(1.0_f64.to_bits() + 1),
        ),
        ("9007199254740993e0", 9_007_199_254_740_992.0),
        ("1.7976931348623157e308", f64::MAX),
        ("1e309", f64::INFINITY),
        ("2.4703282292062327e-324", 0.0),
        ("2.4703282292062328e-324", f64::from_bits(1)),
    ] {
        assert_postfix(source, &[Number(expected)], 1);
    }
    let zeros = "0".repeat(400);
    assert_postfix(&format!("125{zeros}e-402"), &[Number(1.25)], 1);
    assert_postfix(&format!("125{zeros}.0e-402"), &[Number(1.25)], 1);
    assert_postfix(&format!("0.{zeros}125e401"), &[Number(1.25)], 1);
    let huge_exponent = "9".repeat(400);
    assert_postfix(&format!("1e{huge_exponent}"), &[Number(f64::INFINITY)], 1);
    assert_postfix(&format!("1e-{huge_exponent}"), &[Number(0.0)], 1);
    assert_postfix(&format!("0e{huge_exponent}"), &[Number(0.0)], 1);
}

#[test]
fn decimal_conversion_cap_and_work_budget() {
    let defaults = Limits::default();
    for (prefix, tail, value) in [(".", "5", 0.0), ("1e+", "1", 10.0)] {
        let at_cap = format!("{prefix}{}{tail}", "0".repeat(4095 - prefix.len()));
        let above_cap = format!("{at_cap}0");
        assert_postfix(&at_cap, &[Number(value)], 1);
        assert_limit(&above_cap, defaults, Limit::DecimalLiteralBytes, 0, 4097);
    }
    let limits = Limits {
        max_tokens: 1,
        max_work: 13,
        ..defaults
    };
    assert!(parse_script_with_limits(".5E+01", limits).is_ok());
    assert_limit(
        ".5E+01",
        Limits {
            max_work: 5,
            ..limits
        },
        Limit::Work,
        5,
        6,
    );
    assert_limit(
        ".5E+01",
        Limits {
            max_work: 11,
            ..limits
        },
        Limit::Work,
        0,
        6,
    );
}

#[test]
fn individual_limits_are_inclusive() {
    let defaults = Limits::default();
    let source_limit = Limits {
        max_source_bytes: 4,
        ..defaults
    };
    assert!(parse_script_with_limits("\u{FEFF}1", source_limit).is_ok());
    assert_limit("1 + 🔑", source_limit, Limit::SourceBytes, 4, 8);
    assert_limit(
        "1\u{3000}+ 2",
        Limits {
            max_source_bytes: 2,
            ..defaults
        },
        Limit::SourceBytes,
        1,
        4,
    );

    let token_limit = Limits {
        max_tokens: 6,
        ..defaults
    };
    assert!(parse_script_with_limits(" ( 1+2 ) ;", token_limit).is_ok());
    assert_limit(
        "(1+2);",
        Limits {
            max_tokens: 5,
            ..defaults
        },
        Limit::Tokens,
        5,
        6,
    );
    assert_limit(
        "🔑",
        Limits {
            max_tokens: 0,
            ..defaults
        },
        Limit::Tokens,
        0,
        4,
    );

    let depth_limit = Limits {
        max_nesting_depth: 1,
        ..defaults
    };
    assert!(parse_script_with_limits("(1)+(2)", depth_limit).is_ok());
    assert_limit("( (1))", depth_limit, Limit::NestingDepth, 2, 3);
    assert!(
        parse_script_with_limits(
            "1",
            Limits {
                max_nesting_depth: 0,
                ..defaults
            }
        )
        .is_ok()
    );

    assert!(
        parse_script_with_limits(
            "1+2",
            Limits {
                max_work: 9,
                ..defaults
            }
        )
        .is_ok()
    );
    assert_limit(
        "1+2",
        Limits {
            max_work: 8,
            ..defaults
        },
        Limit::Work,
        1,
        2,
    ); // operator emission
    assert_limit(
        "12",
        Limits {
            max_work: 3,
            ..defaults
        },
        Limit::Work,
        0,
        2,
    ); // literal conversion
    assert_limit(
        "1\u{FEFF}",
        Limits {
            max_work: 5,
            ..defaults
        },
        Limit::Work,
        1,
        4,
    ); // trailing trivia
}

#[test]
fn mixed_operators_charge_token_and_work_budgets() {
    let source = "-(1)/+2%3";
    let limits = Limits {
        max_tokens: 9,
        max_work: 24,
        max_nesting_depth: 1,
        ..Limits::default()
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 7);
    assert_eq!(script.max_stack_depth(), 2);
    assert_limit(
        source,
        Limits {
            max_tokens: 8,
            ..limits
        },
        Limit::Tokens,
        8,
        9,
    );
    assert_limit(
        source,
        Limits {
            max_work: 23,
            ..limits
        },
        Limit::Work,
        7,
        8,
    );
}

#[test]
fn long_prefix_chains_are_iterative_without_using_nesting_budget() {
    let count = 10_000;
    let source = "- ".repeat(count) + "0";
    let limits = Limits {
        max_nesting_depth: 0,
        ..Limits::default()
    };
    assert_limit(
        &source,
        limits,
        Limit::Tokens,
        2 * limits.max_tokens,
        2 * limits.max_tokens + 1,
    );
    let script = parse_script_with_limits(
        &source,
        Limits {
            max_tokens: count + 1,
            ..limits
        },
    )
    .unwrap();
    assert_eq!(script.instruction_count(), count + 1);
    assert_eq!(script.max_stack_depth(), 1);
}

#[test]
fn long_literals_chains_and_nesting_are_bounded() {
    let defaults = Limits::default();
    let max_source = "9".repeat(defaults.max_source_bytes);
    assert_postfix(&max_source, &[Number(f64::INFINITY)], 1);
    assert_limit(
        &format!("{max_source};"),
        defaults,
        Limit::SourceBytes,
        defaults.max_source_bytes,
        defaults.max_source_bytes + 1,
    );

    let nesting = 10_000;
    let parentheses = format!("{}1{}", "(".repeat(nesting), ")".repeat(nesting));
    assert_limit(
        &parentheses,
        defaults,
        Limit::NestingDepth,
        defaults.max_nesting_depth,
        defaults.max_nesting_depth + 1,
    );
    let limits = Limits {
        max_nesting_depth: nesting,
        max_tokens: 2 * nesting + 1,
        ..defaults
    };
    let script = parse_script_with_limits(&parentheses, limits).unwrap();
    assert_eq!(script.instructions(), [Number(1.)]);

    let chain = format!("{}1", "1+".repeat(nesting));
    assert_limit(
        &chain,
        defaults,
        Limit::Tokens,
        defaults.max_tokens,
        defaults.max_tokens + 1,
    );
    let script = parse_script_with_limits(&chain, limits).unwrap();
    assert_eq!(script.instruction_count(), 2 * nesting + 1);
    assert_eq!(script.max_stack_depth(), 2);
}
