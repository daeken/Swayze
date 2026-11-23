use crate::parser::BindingKind;
use crate::parser::Instruction::{
    self, Add, Divide, InitBinding, LoadBinding, Multiply, Negate, Number, Positive, Remainder,
    Subtract,
};
use crate::{ErrorKind, Limit, Limits, Span, parse_script, parse_script_with_limits};

const fn init_const(slot: usize) -> Instruction {
    InitBinding {
        slot,
        kind: BindingKind::Const,
    }
}

const fn init_let(slot: usize) -> Instruction {
    InitBinding {
        slot,
        kind: BindingKind::Let,
    }
}

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
    use Instruction::{Boolean, JumpIfFalseStatement, JumpStatement, StoreBinding};

    assert_postfix(
        "let x=0;if(true){x=1;}else{x=2+(3+4);}x",
        &[
            Number(0.0),
            init_let(0),
            Boolean(true),
            JumpIfFalseStatement(7),
            Number(1.0),
            StoreBinding(0),
            JumpStatement(13),
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
fn sibling_bindings_and_a_later_root_binding_have_distinct_fixed_slots() {
    use Instruction::{Boolean, JumpIfFalseStatement, JumpStatement, StoreBinding};
    assert_postfix(
        "let out=0;if(false){let x=1;out=x;}else{const x=2;out=x;}const y=out;y",
        &[
            Number(0.0),
            init_let(0),
            Boolean(false),
            JumpIfFalseStatement(9),
            Number(1.0),
            init_let(1),
            LoadBinding(1),
            StoreBinding(0),
            JumpStatement(13),
            Number(2.0),
            init_const(2),
            LoadBinding(2),
            StoreBinding(0),
            LoadBinding(0),
            init_const(3),
            LoadBinding(3),
        ],
        1,
    );
}

#[test]
fn both_conditional_rhs_edges_join_before_assignment_stores() {
    use Instruction::{Boolean, Jump, JumpIfFalse, StoreBinding, StoreBindingKeep, Undefined};
    assert_postfix(
        "let x,y; x = y = true ? 1 : 2; x",
        &[
            Undefined,
            init_let(0),
            Undefined,
            init_let(1),
            Boolean(true),
            JumpIfFalse(8),
            Number(1.0),
            Jump(9),
            Number(2.0),
            StoreBindingKeep(1),
            StoreBinding(0),
            LoadBinding(0),
        ],
        1,
    );
}

#[test]
fn arm_stores_precede_the_join_and_the_outer_store_follows_it() {
    use Instruction::{Boolean, Jump, JumpIfFalse, StoreBinding, StoreBindingKeep, Undefined};
    assert_postfix(
        "let x,y,z,w; x = true ? y=z=1 : w=2; x",
        &[
            Undefined,
            init_let(0),
            Undefined,
            init_let(1),
            Undefined,
            init_let(2),
            Undefined,
            init_let(3),
            Boolean(true),
            JumpIfFalse(14),
            Number(1.0),
            StoreBindingKeep(2),
            StoreBindingKeep(1),
            Jump(16),
            Number(2.0),
            StoreBindingKeep(3),
            StoreBinding(0),
            LoadBinding(0),
        ],
        1,
    );
}

#[test]
fn control_test_stores_follow_the_rhs_join_and_precede_comma_and_test() {
    use Instruction::{
        Boolean, Discard, Jump, JumpIfFalse, JumpIfFalseStatement, StoreBindingKeep, StrictEqual,
        Undefined,
    };
    assert_postfix(
        "let x,y,z; if (x=y=false ? z=1 : z=2, x===2) {} y",
        &[
            Undefined,
            init_let(0),
            Undefined,
            init_let(1),
            Undefined,
            init_let(2),
            Boolean(false),
            JumpIfFalse(11),
            Number(1.0),
            StoreBindingKeep(2),
            Jump(13),
            Number(2.0),
            StoreBindingKeep(2),
            StoreBindingKeep(1),
            StoreBindingKeep(0),
            Discard,
            LoadBinding(0),
            Number(2.0),
            StrictEqual,
            JumpIfFalseStatement(20),
            LoadBinding(1),
        ],
        2,
    );
}

#[test]
fn sequence_stores_finish_inside_out_before_discard_and_the_next_operand() {
    use Instruction::{
        Boolean, Discard, Jump, JumpIfFalse, JumpIfFalseStatement, StoreBindingKeep, Undefined,
    };
    assert_postfix(
        "let x,y,z; if (true, x=y=false ? z=1 : z=2, x=3) {} y",
        &[
            Undefined,
            init_let(0),
            Undefined,
            init_let(1),
            Undefined,
            init_let(2),
            Boolean(true),
            Discard,
            Boolean(false),
            JumpIfFalse(13),
            Number(1.0),
            StoreBindingKeep(2),
            Jump(15),
            Number(2.0),
            StoreBindingKeep(2),
            StoreBindingKeep(1),
            StoreBindingKeep(0),
            Discard,
            Number(3.0),
            StoreBindingKeep(0),
            JumpIfFalseStatement(21),
            LoadBinding(1),
        ],
        1,
    );
}

#[test]
fn updates_load_and_store_the_same_slot_once_with_no_result() {
    use Instruction::{Boolean, JumpIfFalseStatement, StoreBinding};
    assert_postfix(
        "let x=0;++x;if(true){let x=0;x--;--x;}x++;x",
        &[
            Number(0.0),
            init_let(0),
            LoadBinding(0),
            Number(1.0),
            Add,
            StoreBinding(0),
            Boolean(true),
            JumpIfFalseStatement(18),
            Number(0.0),
            init_let(1),
            LoadBinding(1),
            Number(1.0),
            Subtract,
            StoreBinding(1),
            LoadBinding(1),
            Number(1.0),
            Subtract,
            StoreBinding(1),
            LoadBinding(0),
            Number(1.0),
            Add,
            StoreBinding(0),
            LoadBinding(0),
        ],
        2,
    );
}

#[test]
fn expression_updates_each_emit_one_checked_instruction_in_operand_order() {
    use crate::parser::{Update, UpdateResult};
    use Instruction::{Exponentiate, UpdateBinding};

    assert_postfix(
        "let x=2; let y=x++**--x; 1 + ++x + y",
        &[
            Number(2.0),
            init_let(0),
            UpdateBinding {
                slot: 0,
                op: Update::Increment,
                result: UpdateResult::Old,
            },
            UpdateBinding {
                slot: 0,
                op: Update::Decrement,
                result: UpdateResult::New,
            },
            Exponentiate,
            init_let(1),
            Number(1.0),
            UpdateBinding {
                slot: 0,
                op: Update::Increment,
                result: UpdateResult::New,
            },
            Add,
            LoadBinding(1),
            Add,
        ],
        2,
    );
}

#[test]
fn leading_expression_updates_emit_one_operand_and_discard_only_statement_results() {
    use crate::parser::{Update, UpdateResult};
    use Instruction::{Discard, Exponentiate, UpdateBinding};

    assert_postfix(
        "let x=2; x++ + 1; ++x ** 2;",
        &[
            Number(2.0),
            init_let(0),
            UpdateBinding {
                slot: 0,
                op: Update::Increment,
                result: UpdateResult::Old,
            },
            Number(1.0),
            Add,
            Discard,
            UpdateBinding {
                slot: 0,
                op: Update::Increment,
                result: UpdateResult::New,
            },
            Number(2.0),
            Exponentiate,
        ],
        2,
    );
}

#[test]
fn while_emits_an_exit_and_back_edge_with_balanced_stack_depth() {
    use Instruction::{Boolean, JumpIfFalseStatement, LoopBack, StoreBinding};

    assert_postfix(
        "let x=1;while(x){x=x-(2+3);}x",
        &[
            Number(1.0),
            init_let(0),
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
fn do_while_emits_body_first_and_patches_each_forward_loop_jump_once() {
    use Instruction::{Boolean, JumpIfFalseStatement, JumpStatement, LoopBack};

    let source = "do{continue;break;continue;break;}while(false);true";
    assert_postfix(
        source,
        &[
            JumpStatement(4), // continue
            JumpStatement(7), // break
            JumpStatement(4),
            JumpStatement(7),
            Boolean(false), // trailing test
            JumpIfFalseStatement(7),
            LoopBack(0), // body entry
            Boolean(true),
        ],
        1,
    );
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 17,
        max_nesting_depth: 1,
        // Source bytes, eight emissions, one frame, one parenthesis marker,
        // two continue fixups and two break fixups.
        max_work: source.len() + 14,
    };
    assert_eq!(
        parse_script_with_limits(source, limits)
            .unwrap()
            .instruction_count(),
        8
    );
    let final_expr = source.find("true").unwrap();
    assert_limit(
        source,
        Limits {
            max_tokens: 16,
            ..limits
        },
        Limit::Tokens,
        final_expr,
        final_expr + 1,
    );
    let close = source.find('}').unwrap();
    for max_work in [close + 1 + 6, final_expr + 12] {
        // Enough for the first but not the second continue/break fixup, respectively.
        assert_limit(
            source,
            Limits { max_work, ..limits },
            Limit::Work,
            close,
            close + 1,
        );
    }
    assert_limit(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        Limit::Work,
        final_expr,
        source.len(),
    );
}

#[test]
fn for_emits_checked_statement_edges_between_test_update_body_and_exit() {
    use Instruction::{JumpIfFalseStatement, JumpStatement, LessThan, LoopBack, StoreBinding};

    assert_postfix(
        "let i=9;for(i=0;i<3;i=i+(1+1)){if(i){break;}continue;}i",
        &[
            Number(9.0),
            init_let(0),
            Number(0.0),
            StoreBinding(0),
            LoadBinding(0), // 4: test
            Number(3.0),
            LessThan,
            JumpIfFalseStatement(21),
            JumpStatement(16),
            LoadBinding(0), // 9: update
            Number(1.0),
            Number(1.0),
            Add,
            Add,
            StoreBinding(0),
            LoopBack(4),
            LoadBinding(0), // 16: body
            JumpIfFalseStatement(19),
            JumpStatement(21), // break
            LoopBack(9),       // continue
            LoopBack(9),       // body fallthrough
            LoadBinding(0),    // 21: exit
        ],
        3,
    );
}

#[test]
fn for_no_comma_logical_store_and_leading_update_keep_their_instructions() {
    use crate::parser::ShortCircuitCondition;
    use Instruction::{
        Boolean, JumpIfFalseStatement, JumpStatement, LoopBack, ShortCircuitStatement, StoreBinding,
    };

    assert_postfix(
        "let x=false;for(x||=true;false;x++){continue;}x",
        &[
            Boolean(false),
            init_let(0),
            LoadBinding(0),
            ShortCircuitStatement {
                target: 6,
                condition: ShortCircuitCondition::Truthy,
            },
            Boolean(true),
            StoreBinding(0),
            Boolean(false), // 6: test
            JumpIfFalseStatement(16),
            JumpStatement(14),
            LoadBinding(0), // 9: update
            Number(1.0),
            Add,
            StoreBinding(0),
            LoopBack(6),
            LoopBack(9),    // 14: continue
            LoopBack(9),    // body fallthrough
            LoadBinding(0), // 16: exit
        ],
        2,
    );
}

#[test]
fn for_assignment_sequences_store_first_and_join_with_an_empty_stack() {
    use crate::parser::ShortCircuitCondition;
    use Instruction::{
        Boolean, Discard, JumpIfFalseStatement, JumpStatement, LoopBack, Null,
        ShortCircuitStatement, StoreBinding, StoreBindingKeep, Undefined,
    };

    assert_postfix(
        "let x;for(x+=true,x=false;false;x||=null,false){continue;}x",
        &[
            Undefined,
            init_let(0),
            LoadBinding(0),
            Boolean(true),
            Add,
            StoreBinding(0), // consuming store before the next operand
            Boolean(false),
            StoreBindingKeep(0),
            Discard,        // final initializer result
            Boolean(false), // 9: test
            JumpIfFalseStatement(21),
            JumpStatement(19),
            LoadBinding(0), // 12: update
            ShortCircuitStatement {
                target: 16,
                condition: ShortCircuitCondition::Truthy,
            },
            Null,
            StoreBinding(0),
            Boolean(false), // 16: update suffix, reached on both paths
            Discard,
            LoopBack(9),
            LoopBack(12),   // 19: continue
            LoopBack(12),   // body fallthrough
            LoadBinding(0), // 21: exit
        ],
        2,
    );
}

#[test]
fn for_update_sequences_discard_only_the_suffix_values() {
    use Instruction::{
        Boolean, Discard, JumpIfFalseStatement, JumpStatement, LoopBack, Null, StoreBinding,
        StoreBindingKeep, Subtract,
    };

    assert_postfix(
        "let x=false;for(++x,x=true;false;x--,null){continue;}x",
        &[
            Boolean(false),
            init_let(0),
            LoadBinding(0),
            Number(1.0),
            Add,
            StoreBinding(0), // leading prefix update leaves no operand
            Boolean(true),
            StoreBindingKeep(0),
            Discard,
            Boolean(false), // 9: test
            JumpIfFalseStatement(21),
            JumpStatement(19),
            LoadBinding(0), // 12: update
            Number(1.0),
            Subtract,
            StoreBinding(0), // leading postfix update leaves no operand
            Null,
            Discard,
            LoopBack(9),
            LoopBack(12),   // 19: continue
            LoopBack(12),   // body fallthrough
            LoadBinding(0), // 21: exit
        ],
        2,
    );
}

#[test]
fn for_expression_clauses_join_at_discards_before_statement_edges() {
    use crate::parser::ShortCircuitCondition;
    use Instruction::{
        Boolean, Discard, Jump, JumpIfFalse, JumpIfFalseStatement, JumpStatement, LoopBack, Null,
        ShortCircuit, Void,
    };

    assert_postfix(
        "for(true?null:false,void 1;true;false&&1,(null,true)){continue;break;}42",
        &[
            Boolean(true),
            JumpIfFalse(4),
            Null,
            Jump(5),
            Boolean(false),
            Discard, // join of the completed conditional, at the comma
            Number(1.0),
            Void,
            Discard,       // final initializer result
            Boolean(true), // 9: test
            JumpIfFalseStatement(24),
            JumpStatement(21),
            Boolean(false), // 12: update
            ShortCircuit {
                target: 15,
                condition: ShortCircuitCondition::Falsy,
            },
            Number(1.0),
            Discard, // join of the short circuit, at the comma
            Null,
            Discard, // grouped comma
            Boolean(true),
            Discard, // final update result
            LoopBack(9),
            LoopBack(12),      // 21: body, continue
            JumpStatement(24), // break
            LoopBack(12),      // body fallthrough
            Number(42.0),      // 24: exit
        ],
        1,
    );
}

#[test]
fn logical_assignment_tests_one_load_and_skips_past_the_store() {
    use Instruction::{ShortCircuitStatement, StoreBinding};

    assert_postfix(
        "let x=0;let y=4;x||=y*(x+1);x",
        &[
            Number(0.0),
            init_let(0),
            Number(4.0),
            init_let(1),
            LoadBinding(0),
            ShortCircuitStatement {
                target: 12,
                condition: crate::parser::ShortCircuitCondition::Truthy,
            },
            LoadBinding(1),
            LoadBinding(0), // explicit read in the RHS
            Number(1.0),
            Add,
            Multiply,
            StoreBinding(0),
            LoadBinding(0),
        ],
        3,
    );
}

#[test]
fn nested_control_edges_target_their_own_tests_and_joins() {
    use Instruction::{Boolean, JumpIfFalseStatement, JumpStatement, LoopBack};

    assert_postfix(
        "while(false){if(true){while(false){}}else{if(1+(2+3)){}}}42",
        &[
            Boolean(false),
            JumpIfFalseStatement(15),
            Boolean(true),
            JumpIfFalseStatement(8),
            Boolean(false),
            JumpIfFalseStatement(7),
            LoopBack(4),
            JumpStatement(14),
            Number(1.0),
            Number(2.0),
            Number(3.0),
            Add,
            Add,
            JumpIfFalseStatement(14),
            LoopBack(0),
            Number(42.0),
        ],
        3,
    );
}

#[test]
fn else_if_edges_go_directly_to_the_next_test_or_the_common_join() {
    use Instruction::{Boolean, JumpIfFalseStatement, JumpStatement, StoreBinding};

    assert_postfix(
        "let x=0;if(false){}else if(false){}else if(true){}else{x=42;}x",
        &[
            Number(0.0),
            init_let(0),
            Boolean(false),
            JumpIfFalseStatement(5),
            JumpStatement(13),
            Boolean(false), // 5: second test
            JumpIfFalseStatement(8),
            JumpStatement(13),
            Boolean(true), // 8: third test
            JumpIfFalseStatement(11),
            JumpStatement(13),
            Number(42.0), // 11: final else
            StoreBinding(0),
            LoadBinding(0), // 13: common join
        ],
        1,
    );
}

#[test]
fn if_breaks_from_every_arm_share_the_direct_join() {
    use Instruction::{Boolean, Discard, JumpIfFalseStatement, JumpStatement, LoopBack};

    assert_postfix(
        "aa:bb:if(true){break aa;}
         else if(false){while(true){{break bb;}true;}false;}
         else{break aa;}true",
        &[
            Boolean(true),
            JumpIfFalseStatement(4),
            JumpStatement(16), // break aa in the then-arm
            JumpStatement(16), // ordinary end of then-arm
            Boolean(false),
            JumpIfFalseStatement(15),
            Boolean(true), // while test
            JumpIfFalseStatement(12),
            JumpStatement(16), // break bb skips the enclosing block, loop, and arm
            Boolean(true),
            Discard,
            LoopBack(6),
            Boolean(false), // still in the else-if arm, not the break's join
            Discard,
            JumpStatement(16), // ordinary end of else-if arm
            JumpStatement(16), // break aa in the else-arm
            Boolean(true),     // common join after all arms
        ],
        1,
    );
}

#[test]
fn loop_jumps_resolve_to_their_own_test_and_exit_across_if_frames() {
    use Instruction::{Boolean, JumpIfFalseStatement, JumpStatement, LoopBack};

    assert_postfix(
        "while(true){if(false){break;}while(false){if(false){break;}else{continue;}break;}if(true){continue;}break;}42",
        &[
            Boolean(true),
            JumpIfFalseStatement(19),
            Boolean(false),
            JumpIfFalseStatement(5),
            JumpStatement(19),
            Boolean(false),
            JumpIfFalseStatement(14),
            Boolean(false),
            JumpIfFalseStatement(11),
            JumpStatement(14),
            JumpStatement(12),
            LoopBack(5),
            JumpStatement(14),
            LoopBack(5),
            Boolean(true),
            JumpIfFalseStatement(17),
            LoopBack(0),
            JumpStatement(19),
            LoopBack(0),
            Number(42.0),
        ],
        1,
    );
}

#[test]
fn loop_jumps_charge_emission_and_each_deferred_break_patch() {
    let source = "while(false){break;continue;break;}true";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 13,
        max_nesting_depth: 1,
        // Source bytes, seven instructions, one expression marker, one frame,
        // and two deferred break patches.
        max_work: source.len() + 11,
    };
    assert_eq!(
        parse_script_with_limits(source, limits)
            .unwrap()
            .instruction_count(),
        7
    );
    let end = source.find("true").unwrap();
    assert_limit(
        source,
        Limits {
            max_tokens: 12,
            ..limits
        },
        Limit::Tokens,
        end,
        end + 1,
    );
    let semicolon = source.find(';').unwrap();
    assert_limit(
        source,
        Limits {
            max_work: semicolon + 1 + 4,
            ..limits
        },
        Limit::Work,
        semicolon,
        semicolon + 1,
    ); // Enough for the first `break;` token pair, but not its instruction.
    let close = source.find('}').unwrap();
    assert_limit(
        source,
        Limits {
            max_work: close + 1 + 9,
            ..limits
        },
        Limit::Work,
        close,
        close + 1,
    ); // Enough for the back edge and one patch, but not the second patch.
    assert_limit(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        Limit::Work,
        end,
        end + 4,
    );
}

#[test]
fn nested_controls_charge_one_frame_push_per_statement_and_lex_each_token_once() {
    let source = "let x=0;if(false){while(true){x=1;}}else{if(true){x=2;}}x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 35,
        max_nesting_depth: 1,
        // 15 instructions + 3 conversions + 3 expression markers + 3
        // statement frames + 3 if jump fixups + 10 lookup units (including
        // 4 parent hops). Else reuses the frame but gets its own scope.
        max_work: source.len() + 37,
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(2.0)
    );
    for (limit, below) in [
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
        assert_limit(source, below, limit, source.len() - 1, source.len());
    }

    let inner_open = source.find("{x=1").unwrap();
    // Enough work through the inner '{', but not to push the inner frame:
    // 3 for the declaration, 3 for each test, and 1 for the outer frame.
    assert_limit(
        source,
        Limits {
            max_work: inner_open + 1 + 10,
            ..limits
        },
        Limit::Work,
        inner_open,
        inner_open + 1,
    );
}

#[test]
fn while_body_and_back_edge_are_metered_even_if_skipped() {
    let source = "let x=0;while(false){x=1;}x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 16,
        max_nesting_depth: 1,
        // Includes the back-edge emission; unlike if, the exit fixup is not charged.
        max_work: source.len() + 17,
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
            max_work: close + 1 + 10,
            ..limits
        },
        Limit::Work,
        close,
        close + 1,
    ); // Source through '}', two conversions, a marker, a frame and six emissions.
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
        ("let x=0;if(false){x=1;}x", 16, 17),
        ("let x=0;if(false){x=1;}else{x=2;}x", 23, 25),
    ] {
        // Besides the source bytes: decimal input bytes, the '(' marker, the
        // statement frame, instruction emissions, if fixups, and binding lookups.
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
        // four instructions (Number, InitBinding, Boolean, JumpIfFalseStatement),
        // and the statement frame.
        assert_limit(
            source,
            Limits {
                max_work: body_start + 7,
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
                &[Boolean(true), init_let(0), LoadBinding(0)],
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
        &[Boolean(false), init_const(0), LoadBinding(0)],
        1,
    );
    assert_error("true=1; false", Unsupported, 4, 5);
    assert_error("1true", Unsupported, 1, 2);
    assert_error("false true", Malformed, 6, 10);
    assert_error("false\ntrue", Unsupported, 6, 10);
    assert_postfix(
        "true;false",
        &[Boolean(true), Instruction::Discard, Boolean(false)],
        1,
    );
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
fn equal_precedence_short_circuit_operators_associate_left() {
    use crate::parser::ShortCircuitCondition::{Falsy, NonNullish, Truthy};
    use Instruction::ShortCircuit;
    for (symbol, condition) in [("&&", Falsy), ("||", Truthy), ("??", NonNullish)] {
        assert_postfix(
            &format!("1{symbol}2{symbol}3"),
            &[
                Number(1.),
                ShortCircuit {
                    target: 3,
                    condition,
                },
                Number(2.),
                ShortCircuit {
                    target: 5,
                    condition,
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
                    condition,
                },
                Number(2.),
                ShortCircuit {
                    target: 5,
                    condition,
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
        ("!=1", Malformed, 0, 2),
        ("1&=2", Unsupported, 1, 3),
        ("1|=2", Unsupported, 1, 3),
        ("1&&=2", Unsupported, 1, 4),
        ("1||=2", Unsupported, 1, 4),
        ("1&&&2", Malformed, 3, 4),
        ("1||||2", Malformed, 3, 5),
        ("1&/**/&2", Malformed, 6, 7),
        ("1|\n|2", Malformed, 3, 4),
        ("1|| =2", Unsupported, 4, 5),
        ("1!", Unsupported, 1, 2),
        ("1||2++", Unsupported, 4, 6),
        ("1&&2?.x", Unsupported, 4, 6),
        ("1||2??3", Malformed, 4, 6),
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
fn sequence_discard_is_the_join_target_for_short_circuit_and_conditional_paths() {
    use crate::parser::ShortCircuitCondition;
    use Instruction::{Boolean, Discard, Jump, JumpIfFalse, ShortCircuit};

    assert_postfix(
        "(false && true, 42)",
        &[
            Boolean(false),
            ShortCircuit {
                condition: ShortCircuitCondition::Falsy,
                target: 3,
            },
            Boolean(true),
            Discard,
            Number(42.),
        ],
        1,
    );
    // The consequent jumps to the discard; the alternate falls through to it.
    assert_postfix(
        "(true ? 1 : 2, 42)",
        &[
            Boolean(true),
            JumpIfFalse(4),
            Number(1.),
            Jump(5),
            Number(2.),
            Discard,
            Number(42.),
        ],
        1,
    );
}

#[test]
fn expression_statement_discards_are_the_join_targets() {
    use crate::parser::ShortCircuitCondition;
    use Instruction::{Boolean, Discard, Jump, JumpIfFalse, Null, ShortCircuit};

    assert_postfix(
        "true ? 1 : 2; {false && true;} null;",
        &[
            Boolean(true),
            JumpIfFalse(4),
            Number(1.),
            Jump(5),
            Number(2.),
            Discard,
            Boolean(false),
            ShortCircuit {
                condition: ShortCircuitCondition::Falsy,
                target: 9,
            },
            Boolean(true),
            Discard,
            Null,
        ],
        1,
    );
}

#[test]
fn sequence_discards_each_completed_left_operand_before_the_next() {
    use Instruction::{Boolean, Discard, Jump, JumpIfFalse};
    assert_postfix(
        "9+(true?2:3,4,5)",
        &[
            Number(9.),
            Boolean(true),
            JumpIfFalse(5),
            Number(2.),
            Jump(6),
            Number(3.),
            Discard,
            Number(4.),
            Discard,
            Number(5.),
            Add,
        ],
        2,
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
    for (source, end) in [("1??=2", 4), ("1?.x", 3), ("1?.(2)", 3)] {
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
    assert_error("1;;", Unsupported, 3, 3);
    assert_error("1;\u{00A0}🚧", Unsupported, 4, 8);
    assert_error("1; let x", Malformed, 8, 8);
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
            init_const(0),
            LoadBinding(0),
            Number(5.),
            Multiply,
            init_const(1),
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
            init_const(0),
            LoadBinding(0),
            init_const(1),
            LoadBinding(1),
            init_const(2),
            LoadBinding(2),
            init_const(3),
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
            init_const(0),
            LoadBinding(0),
            init_const(1),
            LoadBinding(1),
        ],
        2,
    );
}

#[test]
fn declaration_lists_compile_each_item_before_the_next() {
    assert_postfix(
        "let x,y=x,z; const a=20,b=a+22; b",
        &[
            Instruction::Undefined,
            init_let(0),
            LoadBinding(0),
            init_let(1),
            Instruction::Undefined,
            init_let(2),
            Number(20.),
            init_const(3),
            LoadBinding(3),
            Number(22.),
            Add,
            init_const(4),
            LoadBinding(4),
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
        "null",
    ] {
        let source = format!("const {name}=1; 2");
        assert_error(&source, Unsupported, 6, 6 + name.len());
    }
    for (source, start, end) in [
        ("constx=1; x", 0, 6),
        ("Const=1; x", 0, 5),
        ("consté x=1; x", 5, 7),
        ("const\\u0078=1; x", 5, 6),
        ("const é=1; 1", 6, 8),
        ("const xé=1; 1", 7, 9),
        ("const x\\u0061=1; 1", 7, 8),
        ("const x=1; x\u{200D}", 12, 15),
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
        ("const x==1; x", Malformed, 7, 9),
        ("const x===1; x", Malformed, 7, 10),
        ("Const x=1; x", Malformed, 6, 7), // Syntax is checked before name resolution.
        ("const x=1", Unsupported, 9, 9),
        ("const x=1;", Unsupported, 10, 10),
        ("const x=1\nx", Unsupported, 10, 11),
        ("const x=1\nconst y=2; x", Unsupported, 10, 15),
        ("const [x]=1; x", Unsupported, 6, 7),
        ("const {x}=1; x", Unsupported, 6, 7),
        ("const x=(1;); x", Malformed, 10, 11),
        ("const x=1;;", Unsupported, 11, 11),
        ("const x=1; x; const y=1;", Unsupported, 24, 24),
        ("const x=1; x=2", Unsupported, 14, 14),
        ("const x=1; x=2; x", Unsupported, 12, 13),
        ("const x=1; x += 2; x", Unsupported, 13, 15),
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
    for (source, start, end, message) in [
        ("x", 0, 1, "unbound name"),
        (
            "const x=x; x",
            8,
            9,
            "before lexical binding initialization",
        ),
        (
            "const x=y; const y=1; x",
            8,
            9,
            "before lexical binding initialization",
        ),
        (
            "const x=1; const y=y+x; y",
            19,
            20,
            "before lexical binding initialization",
        ),
        ("const x=1; X", 11, 12, "unbound name"), // case-sensitive
    ] {
        assert_error(source, Unsupported, start, end);
        assert!(
            parse_script(source)
                .unwrap_err()
                .message()
                .contains(message)
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
            init_let(0),
            Number(5.),
            init_const(1),
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
            init_let(0),
            LoadBinding(0),
            init_let(1),
            LoadBinding(1),
            init_let(2),
            LoadBinding(2),
            init_let(3),
            LoadBinding(3),
        ],
        1,
    );
}

#[test]
fn grouped_assignments_keep_the_rhs_in_stack_order_after_expression_joins() {
    use Instruction::{Boolean, Jump, JumpIfFalse, StoreBindingKeep, Undefined};
    assert_postfix(
        "let x;let y;1+(x=(y=true?2:3))",
        &[
            Undefined,
            init_let(0),
            Undefined,
            init_let(1),
            Number(1.0),
            Boolean(true),
            JumpIfFalse(9),
            Number(2.0),
            Jump(10),
            Number(3.0),
            StoreBindingKeep(1),
            StoreBindingKeep(0),
            Add,
        ],
        2,
    );
}

#[test]
fn grouped_compound_loads_before_rhs_and_joins_at_the_binary_operation() {
    use Instruction::{Boolean, Jump, JumpIfFalse, StoreBindingKeep};
    assert_postfix(
        "let x=8;1+(x/=true?(x=2):4)",
        &[
            Number(8.0),
            init_let(0),
            Number(1.0),
            LoadBinding(0),
            Boolean(true),
            JumpIfFalse(9),
            Number(2.0),
            StoreBindingKeep(0),
            Jump(10),
            Number(4.0),
            Divide,
            StoreBindingKeep(0),
            Add,
        ],
        3,
    );
}

#[test]
fn grouped_logical_loads_once_and_skips_past_store_keep() {
    use crate::parser::ShortCircuitCondition::{Falsy, NonNullish, Truthy};
    use Instruction::{Boolean, ShortCircuit, StoreBindingKeep};

    for (operator, condition) in [("&&=", Falsy), ("||=", Truthy), ("??=", NonNullish)] {
        assert_postfix(
            &format!("let x=0;1+(x{operator}false)"),
            &[
                Number(0.0),
                init_let(0),
                Number(1.0),
                LoadBinding(0),
                ShortCircuit {
                    target: 7,
                    condition,
                },
                Boolean(false),
                StoreBindingKeep(0),
                Add,
            ],
            2,
        );
    }
}

#[test]
fn grouped_assignment_sequence_stores_after_its_rhs_join_and_before_discard() {
    use Instruction::{Boolean, Discard, Jump, JumpIfFalse, StoreBindingKeep, Undefined};
    assert_postfix(
        "let x,y; 1+(x=y=true?2:3, y=4, x+y)",
        &[
            Undefined,
            init_let(0),
            Undefined,
            init_let(1),
            Number(1.0),
            Boolean(true),
            JumpIfFalse(9),
            Number(2.0),
            Jump(10),
            Number(3.0),
            StoreBindingKeep(1),
            StoreBindingKeep(0),
            Discard,
            Number(4.0),
            StoreBindingKeep(1),
            Discard,
            LoadBinding(0),
            LoadBinding(1),
            Add,
            Add,
        ],
        3,
    );
    assert_postfix(
        "let x=8; 1+(x/=2, x)",
        &[
            Number(8.0),
            init_let(0),
            Number(1.0),
            LoadBinding(0),
            Number(2.0),
            Divide,
            StoreBindingKeep(0),
            Discard,
            LoadBinding(0),
            Add,
        ],
        3,
    );
}

#[test]
fn logical_arm_skips_target_the_consequent_jump_or_outer_compound_tail() {
    use crate::parser::ShortCircuitCondition::{Falsy, Truthy};
    use Instruction::{Boolean, Jump, JumpIfFalse, ShortCircuit, StoreBindingKeep};
    assert_postfix(
        "let x=1,y=2; const v=x+=true?y||=3:y&&=4;v",
        &[
            Number(1.0),
            init_let(0),
            Number(2.0),
            init_let(1),
            LoadBinding(0),
            Boolean(true),
            JumpIfFalse(12),
            LoadBinding(1),
            ShortCircuit {
                target: 11,
                condition: Truthy,
            },
            Number(3.0),
            StoreBindingKeep(1),
            Jump(16),
            LoadBinding(1),
            ShortCircuit {
                target: 16,
                condition: Falsy,
            },
            Number(4.0),
            StoreBindingKeep(1),
            Add,
            StoreBindingKeep(0),
            init_const(2),
            LoadBinding(2),
        ],
        2,
    );
}

#[test]
fn grouped_logical_assignment_skip_targets_the_sequence_discard() {
    use crate::parser::ShortCircuitCondition::{Falsy, NonNullish, Truthy};
    use Instruction::{Boolean, Discard, ShortCircuit, StoreBindingKeep};
    for (operator, condition) in [("&&=", Falsy), ("||=", Truthy), ("??=", NonNullish)] {
        assert_postfix(
            &format!("let x=0; 1+(x{operator}false, x)"),
            &[
                Number(0.0),
                init_let(0),
                Number(1.0),
                LoadBinding(0),
                ShortCircuit {
                    target: 7,
                    condition,
                },
                Boolean(false),
                StoreBindingKeep(0),
                Discard,
                LoadBinding(0),
                Add,
            ],
            2,
        );
    }
}

#[test]
fn initializerless_let_pushes_undefined_and_initializes_its_own_slot() {
    assert_postfix(
        "let first; let second=first; let third; second",
        &[
            Instruction::Undefined,
            init_let(0),
            LoadBinding(0),
            init_let(1),
            Instruction::Undefined,
            init_let(2),
            LoadBinding(1),
        ],
        1,
    );
}

#[test]
fn compound_assignment_loads_once_before_the_rhs_and_stores_to_the_same_slot() {
    use Instruction::{Jump, JumpIfFalse, StoreBinding};
    assert_postfix(
        "let x=8;let y=2;x/=y?y:4;x",
        &[
            Number(8.0),
            init_let(0),
            Number(2.0),
            init_let(1),
            LoadBinding(0),
            LoadBinding(1),
            JumpIfFalse(9),
            LoadBinding(1),
            Jump(10),
            Number(4.0),
            Divide,
            StoreBinding(0),
            LoadBinding(0),
        ],
        2,
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
        ("{let x=1;} x", Unsupported, 11, 12),
    ] {
        assert_error(source, kind, start, end);
    }
}

#[test]
fn assignment_statement_sequence_joins_at_the_suffix_and_discards_only_suffix_values() {
    use crate::parser::ShortCircuitCondition;
    use Instruction::{
        Boolean, Discard, ShortCircuitStatement, StoreBinding, StoreBindingKeep, Undefined, Void,
    };

    assert_postfix(
        "let x=true,y;{x||=false,y=x,void x;}y",
        &[
            Boolean(true),
            init_let(0),
            Undefined,
            init_let(1),
            LoadBinding(0),
            ShortCircuitStatement {
                target: 8,
                condition: ShortCircuitCondition::Truthy,
            },
            Boolean(false),
            StoreBinding(0),
            LoadBinding(0), // both paths enter the suffix with an empty stack
            StoreBindingKeep(1),
            Discard,
            LoadBinding(0),
            Void,
            Discard, // statement terminator consumes the last operand
            LoadBinding(1),
        ],
        1,
    );
}

#[test]
fn update_statements_use_the_same_consuming_store_with_or_without_a_suffix() {
    use crate::parser::{Update, UpdateResult};
    use Instruction::{Discard, StoreBinding, StoreBindingKeep, UpdateBinding};

    for (leading, operation) in [
        ("++x", Add),
        ("x++", Add),
        ("--x", Subtract),
        ("x--", Subtract),
    ] {
        assert_postfix(
            &format!("let x=0;{leading};x"),
            &[
                Number(0.0),
                init_let(0),
                LoadBinding(0),
                Number(1.0),
                operation,
                StoreBinding(0),
                LoadBinding(0),
            ],
            2,
        );
        assert_postfix(
            &format!("let x=0,y=0;{{{leading},y=x++,x;}}y"),
            &[
                Number(0.0),
                init_let(0),
                Number(0.0),
                init_let(1),
                LoadBinding(0),
                Number(1.0),
                operation,
                StoreBinding(0), // enter the suffix with an empty stack
                UpdateBinding {
                    slot: 0,
                    op: Update::Increment,
                    result: UpdateResult::Old,
                },
                StoreBindingKeep(1),
                Discard,
                LoadBinding(0),
                Discard,
                LoadBinding(1),
            ],
            2,
        );
    }
}

#[test]
fn no_comma_assignment_statements_keep_their_consuming_opcodes() {
    use crate::parser::ShortCircuitCondition;
    use Instruction::{Boolean, ShortCircuitStatement, StoreBinding, Undefined};

    assert_postfix(
        "let x;x=true;x+=true;x||=false;x",
        &[
            Undefined,
            init_let(0),
            Boolean(true),
            StoreBinding(0),
            LoadBinding(0),
            Boolean(true),
            Add,
            StoreBinding(0),
            LoadBinding(0),
            ShortCircuitStatement {
                target: 12,
                condition: ShortCircuitCondition::Truthy,
            },
            Boolean(false),
            StoreBinding(0),
            LoadBinding(0),
        ],
        2,
    );
}

#[test]
fn assignment_statements_require_explicit_semicolons() {
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
        ("let x=1; (x + x=2)", Unsupported, 15, 16),
        ("let x=1; ((x)||=2)", Unsupported, 13, 16),
        ("let x=1; x=>2; x", Unsupported, 10, 12),
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
    let last_name = source.rfind('x').unwrap();
    assert_limit(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        Limit::Work,
        last_name,
        last_name + 1,
    );
    assert_limit("let x=0;x=(1);x", limits, Limit::NestingDepth, 10, 11);

    // Stop within the deferred assignment target lookup: 13 scanned bytes,
    // two conversions, five emissions, and one candidate visit.
    assert_limit(
        "let x=0;x=1;x",
        Limits {
            max_work: 21,
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
    assert_error(";", Unsupported, 1, 1);
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
    for source in ["1--2", "1++2", "1-=2", "1/=2", "1%=2"] {
        assert_error(source, Unsupported, 1, 3);
    }
    assert_error("++2", Unsupported, 0, 2);
    assert_error("--2", Unsupported, 0, 2);
    assert_error("1**=2", Unsupported, 1, 4);
    assert_postfix("1/*comment*/+2", &[Number(1.), Number(2.), Add], 2);
    assert_postfix("1//2", &[Number(1.)], 1);
    assert_postfix("-/*comment*/1", &[Number(1.), Negate], 1);
    assert_error("/2/", Unsupported, 0, 1);
    assert_error("8 / /2/", Unsupported, 4, 5);
    assert_error("+ /2/", Unsupported, 2, 3);
}

#[test]
fn exponentiation_emits_right_associative_postfix_with_a_unary_exponent() {
    use Instruction::Exponentiate;
    assert_postfix(
        "2*3**4**-5",
        &[
            Number(2.0),
            Number(3.0),
            Number(4.0),
            Number(5.0),
            Negate,
            Exponentiate,
            Exponentiate,
            Multiply,
        ],
        4,
    );
    assert_postfix(
        "let x=2; x**=x**3; x",
        &[
            Number(2.0),
            init_let(0),
            LoadBinding(0),
            LoadBinding(0),
            Number(3.0),
            Exponentiate,
            Exponentiate,
            Instruction::StoreBinding(0),
            LoadBinding(0),
        ],
        3,
    );
}

#[test]
fn exponentiation_token_and_work_costs_match_existing_accounting() {
    let limits = Limits {
        max_source_bytes: 7,
        max_tokens: 5,
        max_work: 17, // Seven bytes, three conversions, two pushes, five emissions.
        max_nesting_depth: 0,
    };
    let source = "1**1**1";
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(1.0)
    );
    assert_limit(
        source,
        Limits {
            max_tokens: 3,
            ..limits
        },
        Limit::Tokens,
        4,
        5,
    );
    assert_limit(
        source,
        Limits {
            max_work: 16,
            ..limits
        },
        Limit::Work,
        1,
        3,
    );

    let source = "let x=1;x**=2;x";
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 10,
        // Two conversions, seven emissions (no pending `**=`), two lookups at two units each.
        max_work: source.len() + 13,
        max_nesting_depth: 0,
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(1.0)
    );
    assert_limit(
        source,
        Limits {
            max_tokens: 6,
            ..limits
        },
        Limit::Tokens,
        9,
        10,
    );
    // Stop at the binary emission: 14 scanned bytes, two conversions, four earlier emissions.
    assert_limit(
        source,
        Limits {
            max_work: 20,
            ..limits
        },
        Limit::Work,
        9,
        12,
    );
    assert_limit(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
        Limit::Work,
        14,
        15,
    );
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
        "0_1",
        "0_0.5",
        "0_8e2",
        "00_1",
        "07_7",
        "077.toString()",
        "1n",
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
        ("._5", 0, 1),
        ("1_0n", 3, 4),
        (".5_0n", 4, 5),
        ("1.e+2_0n", 7, 8),
        ("1e3_09n", 6, 7),
        ("1_0tail", 3, 4),
        ("1_0$tail", 3, 4),
        ("1_0\\u0061", 3, 4),
        ("\u{FEFF}.5_0é", 7, 9),
        ("1e2_0.3", 5, 6),
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
    assert_postfix(
        ".5; 1e0",
        &[Number(0.5), Instruction::Discard, Number(1.0)],
        1,
    );
    assert_postfix("1./*comment*/", &[Number(1.)], 1);
    assert_postfix(".5e-1//comment", &[Number(0.05)], 1);
}

#[test]
fn decimal_separator_positions_are_local_to_each_digit_sequence() {
    for (source, expected) in [
        ("1_000", 1000.0),
        ("1_000.", 1000.0),
        (".0_5", 0.05),
        ("0.0_5", 0.05),
        ("1_0.2_5", 10.25),
        ("0e0_7", 0.0),
        ("1e0_2", 100.0),
        ("1.e+0_2", 100.0),
        (".5_0E-0_2", 0.005),
        ("1_0.2_5e+0_2", 1025.0),
        ("1_0.2_5e-0_2", 0.1025),
    ] {
        assert_postfix(source, &[Number(expected)], 1);
    }
    assert_postfix(
        "let _1 = 1_0; _1/*🦀*/+.5_0;",
        &[Number(10.0), init_let(0), LoadBinding(0), Number(0.5), Add],
        2,
    );
    for (source, start) in [
        ("1__2", 1),
        ("1_", 1),
        ("1_.2", 1),
        ("1._2", 2),
        ("1_e2", 1),
        ("1.2_e3", 3),
        ("1e_2", 2),
        ("1.e_2", 3),
        ("1e+_2", 3),
        ("1e-_2", 3),
        ("1e2_", 3),
        ("1e3_09_", 6),
        ("1e+2__3", 4),
        ("1e2_.3", 3),
        (".5__0", 2),
        (".5_", 2),
        ("1_ 2", 1),
        ("1_/*no join*/2", 1),
        ("\u{FEFF}1_２", 4),
    ] {
        assert_error(source, ErrorKind::Malformed, start, start + 1);
    }
}

fn separate_decimal_digits(source: &str) -> String {
    let mut separated = String::new();
    let mut digits = source.chars().peekable();
    while let Some(c) = digits.next() {
        separated.push(c);
        if c.is_ascii_digit() && digits.peek().is_some_and(char::is_ascii_digit) {
            separated.push('_');
        }
    }
    separated
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
        assert_postfix(&separate_decimal_digits(input), &[Number(expected)], 1);
    }
    // The 309-digit overflow midpoint is 2^1024 - 2^970. The preceding
    // integer rounds to the largest finite Number; the midpoint overflows.
    let prefix = concat!(
        "17976931348623158079372897140530341507993413271003782693617377898044496829276475",
        "094664901797758720709633028641669288791094655554785194040263065748867150582068",
        "190890200070838367627385484581771153176447573027006985557136695962284291481986",
        "083493647529271907416844436551070434271155969950809304288017790417449779",
    );
    for (tail, expected) in [(1, f64::MAX), (2, f64::INFINITY)] {
        let input = format!("{prefix}{tail}");
        assert_postfix(&input, &[Number(expected)], 1);
        assert_postfix(&separate_decimal_digits(&input), &[Number(expected)], 1);
    }
    let above = format!("1{}", "0".repeat(309));
    assert_postfix(&above, &[Number(f64::INFINITY)], 1);
    assert_error(&format!("{above}n"), ErrorKind::Unsupported, 310, 311);
}

#[test]
fn separated_decimal_integer_overflow_still_scans_the_entire_lexeme() {
    // The shortcut uses 310 significant digits, skips all conversion work, and
    // is not subject to the decimal point/exponent byte ceiling.
    for zeros in [309, 4096] {
        let overflow = format!("1{}", "_0".repeat(zeros));
        let end = overflow.len();
        let limits = Limits {
            max_tokens: 1,
            max_source_bytes: end,
            max_work: end + 1,
            ..Limits::default()
        };
        assert_eq!(
            parse_script_with_limits(&overflow, limits)
                .unwrap()
                .evaluate(),
            Ok(f64::INFINITY)
        );
        assert_limit(
            &overflow,
            Limits {
                max_work: end - 1,
                ..limits
            },
            Limit::Work,
            end - 1,
            end,
        );
        assert_error(&format!("{overflow}_"), ErrorKind::Malformed, end, end + 1);
        assert_error(
            &format!("{overflow}n"),
            ErrorKind::Unsupported,
            end,
            end + 1,
        );
        assert_error(
            &format!("{overflow}🔑"),
            ErrorKind::Unsupported,
            end,
            end + 4,
        );
    }
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
        assert_postfix(&separate_decimal_digits(source), &[Number(expected)], 1);
    }
    let zeros = "0".repeat(400);
    assert_postfix(&format!("125{zeros}e-402"), &[Number(1.25)], 1);
    assert_postfix(&format!("125{zeros}.0e-402"), &[Number(1.25)], 1);
    let compensated = separate_decimal_digits(&format!("125{zeros}.0e-402"));
    assert_postfix(&compensated, &[Number(1.25)], 1);
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
fn separated_decimal_conversion_cap_includes_separators() {
    let defaults = Limits::default();
    for (prefix, value) in [(".", 0.0), ("1_0.", 10.0), ("1e+", 10.0), ("1e-", 0.1)] {
        let at_cap = format!("{prefix}0_{}1", "0".repeat(4093 - prefix.len()));
        assert_eq!(at_cap.len(), 4096);
        assert_postfix(&at_cap, &[Number(value)], 1);
        let above_cap = format!("{at_cap}0");
        assert_limit(&above_cap, defaults, Limit::DecimalLiteralBytes, 0, 4097);
        // Validate the lexeme before either treating it as a Number or applying the cap.
        assert_error(&format!("{above_cap}_"), ErrorKind::Malformed, 4097, 4098);
        assert_error(&format!("{above_cap}n"), ErrorKind::Unsupported, 4097, 4098);
    }
}

#[test]
fn separated_decimal_exact_work_and_token_budget() {
    let source = ".5_0E+0_1";
    let limits = Limits {
        max_tokens: 1,
        max_source_bytes: 9,
        max_work: 26, // Nine scan, nine normalization, seven conversion, one emission.
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(5.0)
    );
    for (work, start, end) in [(7, 7, 8), (8, 8, 9), (9, 0, 9), (18, 0, 9), (25, 0, 9)] {
        assert_limit(
            source,
            Limits {
                max_work: work,
                ..limits
            },
            Limit::Work,
            start,
            end,
        );
    }
    assert_limit(
        source,
        Limits {
            max_tokens: 0,
            ..limits
        },
        Limit::Tokens,
        0,
        1,
    );
    assert_limit(
        source,
        Limits {
            max_source_bytes: 8,
            ..limits
        },
        Limit::SourceBytes,
        8,
        9,
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
fn void_replaces_one_operand_in_postfix_order() {
    use Instruction::{Not, Void};
    assert_postfix(
        "1+!void -2",
        &[Number(1.), Number(2.), Negate, Void, Not, Add],
        2,
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
