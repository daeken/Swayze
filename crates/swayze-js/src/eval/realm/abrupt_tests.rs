use super::*;
use crate::parser::{BARE_CALL, CatchSlot};
use Instruction::*;

const FIRST: CatchSlot = CatchSlot { enter: 0, depth: 1 };
const THROW: Instruction = ThrowValue { start: 0, end: 1 };

fn wrap(body: &[Instruction], handler: &[Instruction]) -> Vec<Instruction> {
    let catch = (body.len() + 2) as u16;
    let after = catch + handler.len() as u16 + 2;
    let mut ops = vec![EnterTry { catch, after }];
    ops.extend_from_slice(body);
    ops.extend([LeaveTry { enter: 0 }, EnterCatch { enter: 0 }]);
    ops.extend_from_slice(handler);
    ops.push(LeaveCatch { enter: 0 });
    ops
}

fn run(ready: &mut ReadyScript<'_, '_>) -> (Result<Option<Operand>, Stop>, EvalScratch) {
    let code = ready.code;
    run_execution(code, &mut ready.execution())
}

fn run_execution(
    code: &CompiledScript,
    execution: &mut Execution<'_, '_>,
) -> (Result<Option<Operand>, Stop>, EvalScratch) {
    let stack = new_stack(code.max_stack_depth).unwrap();
    let mut scratch = EvalScratch::new();
    let result = dispatch(
        Frame::root(&code.instructions, code.max_stack_depth, code.exit),
        stack,
        &code.string_units,
        execution.callable,
        &mut scratch,
        execution,
    );
    (result, scratch)
}

fn root(ops: Vec<Instruction>, peak: usize) -> CompiledScript {
    let mut code = CompiledScript::compile("var staged;").unwrap();
    code.instructions = ops;
    code.max_stack_depth = peak;
    code.exit = ExitStack::Empty;
    code
}

fn reject_atomic(code: &CompiledScript) {
    let mut realm = Realm::new();
    let (lex, vars, artifacts, functions) = (
        realm.state.globals.capacity(),
        realm.state.object.vars.capacity(),
        realm.artifacts.capacity(),
        realm.functions.capacity(),
    );
    assert_eq!(
        realm.instantiate(code).unwrap_err(),
        InstantiateError::InvalidScript
    );
    assert!(realm.state.globals.is_empty() && realm.state.object.vars.is_empty());
    assert!(realm.artifacts.is_empty() && realm.functions.is_empty());
    assert_eq!(realm.artifact_bytes, 0);
    assert_eq!(
        (
            realm.state.globals.capacity(),
            realm.state.object.vars.capacity(),
            realm.artifacts.capacity(),
            realm.functions.capacity()
        ),
        (lex, vars, artifacts, functions)
    );
}

#[test]
fn explicit_rethrow_projects_new_span_without_a_public_value() {
    let code = root(
        wrap(
            &[Number(9.0), THROW],
            &[LoadCatch(FIRST), ThrowValue { start: 2, end: 4 }],
        ),
        1,
    );
    let mut lives = Realm::new();
    assert_eq!(
        lives.instantiate(&code).unwrap().execute(),
        Ok(Completion::Throw(Throw::Explicit {
            span: Span { start: 2, end: 4 }
        }))
    );
    assert!(matches!(
        run(&mut lives.instantiate(&code).unwrap()).0,
        Err(Stop::ValueThrow(abrupt::Thrown {
            value: Operand::Scalar(Value::Number(9.0)),
            owner: None,
            span: Span { start: 2, end: 4 }
        }))
    ));
}

#[test]
fn catch_cell_update_and_nearest_same_frame_via_the_real_dispatch() {
    let ops = vec![
        EnterTry {
            catch: 13,
            after: 17,
        }, // 0: outer
        EnterTry {
            catch: 5,
            after: 12,
        }, // 1: inner
        Number(9.0),
        THROW,
        LeaveTry { enter: 1 },
        EnterCatch { enter: 1 }, // 5: change its initialized cell and rethrow
        UpdateCatch {
            slot: CatchSlot { enter: 1, depth: 2 },
            op: Update::Increment,
            result: UpdateResult::Old,
        },
        Discard,
        LoadCatch(CatchSlot { enter: 1, depth: 2 }),
        THROW,
        JumpStatement(11), // dead statement boundary
        LeaveCatch { enter: 1 },
        LeaveTry { enter: 0 },
        EnterCatch { enter: 0 },
        LoadCatch(FIRST),
        THROW,
        LeaveCatch { enter: 0 },
    ];
    let code = root(ops, 1);
    let mut lives = Realm::new();
    assert!(matches!(
        run(&mut lives.instantiate(&code).unwrap()).0,
        Err(Stop::ValueThrow(abrupt::Thrown {
            value: Operand::Scalar(Value::Number(10.0)),
            ..
        }))
    ));
}

#[test]
fn retained_unwind_discards_inner_packets_keeps_caller_packet_and_current_local() {
    let mut code = CompiledScript::compile(concat!(
        "function boom(){'\\ud800';} function mid(a){mid.s=mid.go(boom());}",
        "function holder(){} var host=holder; host.go=mid; mid.go=holder; mid.s=3; ",
        "host.s=host.go(41); host.s;",
    ))
    .unwrap();
    assert!(matches!(code.functions[0].instructions[0], String(_)));
    code.functions[0].instructions[1] = THROW;
    let body = &mut code.functions[1];
    assert!(body.instructions.iter().any(|op| matches!(op, FormDot(_))));
    assert!(
        body.instructions
            .iter()
            .any(|op| matches!(op, PrepareDotCall(_)))
    );
    // Keep a write to the ordinary local made before the throw, but restore
    // the catcher definition's live lookup after running the other body.
    let mut body_ops = vec![
        Number(42.0),
        StoreLocal {
            slot: 0,
            keep: false,
        },
    ];
    body_ops.extend_from_slice(&body.instructions);
    let own_access = body.accesses.start;
    body.instructions = wrap(
        &body_ops,
        &[
            LoadBinding(own_access),
            Discard,
            LoadCatch(FIRST),
            Discard,
            LoadLocal(0),
            Return { value: true },
        ],
    );
    let mut realm = Realm::new();
    assert!(matches!(
        run(&mut realm.instantiate(&code).unwrap()).0,
        Ok(Some(Operand::Scalar(Value::Number(42.0))))
    ));
    let observe = CompiledScript::compile("if(mid.s!==3 || host.s!==42){bad;}").unwrap();
    assert_eq!(
        realm.instantiate(&observe).unwrap().execute(),
        Ok(Completion::Normal)
    );
}

#[test]
fn retained_throw_moves_scratch_utf16_and_uncaught_owner_into_packet() {
    let mut producer = CompiledScript::compile("function boom(){'\\ud800'+'R';}").unwrap();
    let tail = producer.functions[0].instructions.last_mut().unwrap();
    assert_eq!(*tail, Discard);
    *tail = ThrowValue { start: 2, end: 6 };
    let mut realm = Realm::new();
    assert_eq!(
        realm.instantiate(&producer).unwrap().execute(),
        Ok(Completion::Normal)
    );
    drop(producer);
    let caller = CompiledScript::compile("boom();").unwrap();
    let (result, scratch) = run(&mut realm.instantiate(&caller).unwrap());
    let Err(Stop::ValueThrow(abrupt::Thrown {
        value: Operand::String(value),
        owner: Some(FunctionOrigin {
            artifact: 0,
            body: 0,
        }),
        span: Span { start: 2, end: 6 },
    })) = result
    else {
        panic!("lost retained provenance")
    };
    assert_eq!(
        value.get(scratch.transient(&[]).with_retained(&[])),
        Some(&[0xd800, 82][..])
    );
}

#[test]
fn caught_automatic_errors_pass_values_but_engine_faults_bypass_catch() {
    for source in ["missing;", "(1)();"] {
        let mut code = CompiledScript::compile(source).unwrap();
        code.instructions.push(Discard);
        code.instructions = wrap(&code.instructions, &[LoadCatch(FIRST), THROW]);
        code.exit = ExitStack::Empty;
        let mut realm = Realm::new();
        assert_eq!(
            realm.instantiate(&code).unwrap().execute(),
            Ok(Completion::Throw(Throw::Explicit {
                span: Span { start: 0, end: 1 }
            })),
        );
        let actual = run(&mut realm.instantiate(&code).unwrap()).0;
        assert!(matches!(
            actual,
            Err(Stop::ValueThrow(abrupt::Thrown {
                value: Operand::Object(_),
                ..
            }))
        ));
    }
    let mut code = CompiledScript::compile("var state=1; bad=2;").unwrap();
    code.instructions = wrap(&code.instructions, &[Number(99.0), THROW]);
    let mut realm = Realm::new();
    assert!(matches!(
        run(&mut realm.instantiate(&code).unwrap()).0,
        Err(Stop::Engine(EngineFault::ImplicitGlobal { .. }))
    ));
    let read = CompiledScript::compile("state;").unwrap();
    assert!(matches!(
        run(&mut realm.instantiate(&read).unwrap()).0,
        Ok(Some(Operand::Scalar(Value::Number(1.0))))
    ));
}

fn evaluate(realm: &mut Realm, source: &str) -> Result<Completion, EngineFault> {
    let code = CompiledScript::compile(source).unwrap();
    realm.instantiate(&code).unwrap().execute()
}

// The parser producer lives on a different lane. Build the control edges by
// hand, keeping every lookup/property site from the real compiler.
fn capture_guard(code: &mut CompiledScript) {
    assert_eq!(code.exit, ExitStack::Empty);
    let handler = code.instructions.split_off(code.instructions.len() - 2);
    let [LoadBinding(_), store @ StoreBinding(_)] = handler[..] else {
        panic!("expected a trailing capture = placeholder statement: {handler:?}")
    };
    code.instructions = wrap(&code.instructions, &[LoadCatch(FIRST), store]);
}

#[test]
fn automatic_producers_keep_exact_diagnostics_and_order_and_deliver_real_errors() {
    use abrupt::Automatic::{ReferenceError as Reference, TypeError as Type};
    for (body, token, kind, effect) in [
        ("missing;", "missing", Reference, 0),
        (
            "typeof pending; const pending = 1;",
            "pending",
            Reference,
            0,
        ),
        (
            "'use strict'; undefined = (side = 1);",
            "undefined",
            Type,
            1,
        ),
        (
            "'use strict'; missing = (side = 2);",
            "missing",
            Reference,
            2,
        ),
        (
            "pending = (side = 3); const pending = 1;",
            "pending",
            Reference,
            3,
        ),
        ("fixed = (side = 4);", "fixed", Type, 4),
        ("'use strict'; 'x'.length = (side = 5);", ".", Type, 5),
        (
            "'use strict'; f.constructor.prototype = (side = 6);",
            ".prototype",
            Type,
            6,
        ),
        ("null.x;", ".", Type, 0),
        ("null.x(side = 7);", ".", Type, 0),
        ("null.x = (side = 8);", ".", Type, 8),
        ("f.prototype(side = 9);", "(side = 9)", Type, 9),
        // RHS lookup precedes both the readonly and nullish Put failures.
        ("null.x = missing;", "missing", Reference, 0),
        (
            "'use strict'; undefined = missing;",
            "missing",
            Reference,
            0,
        ),
    ] {
        let source = format!("{body} capture = placeholder;");
        let mut code = CompiledScript::compile(&source).unwrap();
        let start = source.find(token).unwrap();
        let len = if token.starts_with('.') {
            1
        } else {
            token.len()
        };
        let span = Span {
            start,
            end: start + len,
        };
        let name = match kind {
            Reference => "ReferenceError",
            Type => "TypeError",
        };
        for caught in [false, true] {
            let mut realm = Realm::new();
            let diagnostic = match kind {
                Reference => Throw::ReferenceError { span },
                Type => Throw::TypeError { span },
            };
            assert_eq!(
                evaluate(
                    &mut realm,
                    "var side = 0, capture; const fixed = 1; function f(){}"
                ),
                Ok(Completion::Normal),
            );
            if caught {
                capture_guard(&mut code);
            }
            assert_eq!(
                realm.instantiate(&code).unwrap().execute(),
                Ok(if caught {
                    Completion::Normal
                } else {
                    Completion::Throw(diagnostic)
                }),
                "{body}",
            );
            let observed = if caught {
                format!(
                    "if (typeof capture !== 'object' || capture.name !== '{name}' || capture.message !== '' || capture.constructor.name !== '{name}' || capture.constructor.prototype.constructor !== capture.constructor || side !== {effect} || f.name !== 'f' || fixed !== 1) bad;"
                )
            } else {
                format!(
                    "if (capture !== undefined || side !== {effect} || f.name !== 'f' || fixed !== 1) bad;"
                )
            };
            assert_eq!(
                evaluate(&mut realm, &observed),
                Ok(Completion::Normal),
                "{body}"
            );
        }
    }

    let mut realm = Realm::new();
    assert_eq!(
        evaluate(&mut realm, "var capture, state;"),
        Ok(Completion::Normal)
    );
    let mut code =
        CompiledScript::compile("state = typeof missing; capture = placeholder;").unwrap();
    capture_guard(&mut code);
    assert_eq!(
        realm.instantiate(&code).unwrap().execute(),
        Ok(Completion::Normal)
    );
    assert_eq!(
        evaluate(
            &mut realm,
            "if (state !== 'undefined' || capture !== undefined) bad;"
        ),
        Ok(Completion::Normal)
    );
}

#[test]
fn automatic_catches_are_fresh_and_reached_error_constructors_are_engine_faults() {
    let mut realm = Realm::new();
    assert_eq!(
        evaluate(&mut realm, "var capture, earlier, type, base, side = 0;"),
        Ok(Completion::Normal)
    );
    let mut missing = CompiledScript::compile("missing; capture = placeholder;").unwrap();
    capture_guard(&mut missing);
    assert_eq!(
        realm.instantiate(&missing).unwrap().execute(),
        Ok(Completion::Normal)
    );
    assert_eq!(
        evaluate(&mut realm, "earlier = capture;"),
        Ok(Completion::Normal)
    );
    assert_eq!(
        realm.instantiate(&missing).unwrap().execute(),
        Ok(Completion::Normal)
    );
    let mut bad_call = CompiledScript::compile("(1)(); type = placeholder;").unwrap();
    capture_guard(&mut bad_call);
    assert_eq!(
        realm.instantiate(&bad_call).unwrap().execute(),
        Ok(Completion::Normal)
    );
    assert_eq!(
        evaluate(
            &mut realm,
            "if (capture === earlier || capture.constructor !== earlier.constructor || capture.constructor.prototype !== earlier.constructor.prototype || capture.constructor === type.constructor || capture.constructor.prototype === type.constructor.prototype) bad;"
        ),
        Ok(Completion::Normal)
    );

    // There is deliberately no guest Error binding. Seed a real base Error
    // through the same private materializer solely to exercise its call tag.
    {
        let seed = CompiledScript::compile("base = placeholder;").unwrap();
        let mut ready = realm.instantiate(&seed).unwrap();
        let mut exec = ready.execution();
        let Ok(value) =
            exec.materialize_error(ErrorKind::Error, Span { start: 0, end: 1 }, |_| Ok(()))
        else {
            panic!("expected Error object")
        };
        let StoreBinding(site) = seed.instructions[1] else {
            panic!("expected base assignment")
        };
        let (target, span, _) = exec.site(site).ok().unwrap();
        assert!(
            exec.put(target, span, value, EvalScratch::new().transient(&[]))
                .is_ok()
        );
    }

    for (receiver, intrinsic, effect) in [
        ("capture", IntrinsicCall::ReferenceError, 1),
        ("type", IntrinsicCall::TypeError, 2),
        ("base", IntrinsicCall::Error, 3),
    ] {
        let source = format!("{receiver}.constructor(side = {effect}); capture = placeholder;");
        let mut code = CompiledScript::compile(&source).unwrap();
        capture_guard(&mut code);
        let start = source.find('(').unwrap();
        let end = source.find(')').unwrap() + 1;
        assert_eq!(
            realm.instantiate(&code).unwrap().execute(),
            Err(EngineFault::UnsupportedIntrinsicCall {
                span: Span { start, end },
                intrinsic
            })
        );
        assert_eq!(
            evaluate(
                &mut realm,
                &format!("if (side !== {effect} || capture.name !== 'ReferenceError') bad;")
            ),
            Ok(Completion::Normal)
        );
    }
}

#[test]
fn retained_automatic_throw_uses_issuing_span_then_restores_catcher_mode_and_packets() {
    let mut realm = Realm::new();
    let producer = format!(
        "function boom(){{'use strict'; {} absent;}}",
        " ".repeat(512)
    );
    assert_eq!(evaluate(&mut realm, &producer), Ok(Completion::Normal));
    let start = producer.find("absent").unwrap();
    assert_eq!(
        evaluate(&mut realm, "boom();"),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: Span {
                start,
                end: start + 6
            }
        }))
    );

    let mut caller = CompiledScript::compile(concat!(
        "function mid(a){mid.open=mid.go(boom()); mid.name = 1; capture = placeholder;}",
        "function holder(){} var capture; var host=holder; host.go=mid; mid.go=holder; mid.open=3; ",
        "host.result=host.go(42);",
    )).unwrap();
    let body = &mut caller.functions[0];
    let mut handler = body.instructions.split_off(body.instructions.len() - 7);
    let [
        LoadBinding(_),
        FormDot(_),
        Number(1.0),
        PutDot(_),
        Discard,
        LoadBinding(_),
        StoreBinding(_),
    ] = handler[..]
    else {
        panic!("expected sloppy readonly store and capture: {handler:?}")
    };
    handler[5] = LoadCatch(FIRST);
    handler.extend([LoadLocal(0), Return { value: true }]);
    body.instructions = wrap(&body.instructions, &handler);
    assert_eq!(
        realm.instantiate(&caller).unwrap().execute(),
        Ok(Completion::Normal)
    );
    assert_eq!(
        evaluate(
            &mut realm,
            "if (host.result !== 42 || mid.open !== 3 || mid.name !== 'mid' || capture.name !== 'ReferenceError') bad;"
        ),
        Ok(Completion::Normal)
    );
}

#[test]
fn automatic_error_creation_is_only_for_a_real_target_and_failure_skips_the_handler() {
    let mut realm = Realm::new();
    assert_eq!(
        evaluate(&mut realm, "var side, capture;"),
        Ok(Completion::Normal)
    );
    let source = "side = 7; missing; capture = placeholder;";
    let plain = CompiledScript::compile(source).unwrap();
    let start = source.find("missing").unwrap();
    let span = Span {
        start,
        end: start + 7,
    };
    let mut guarded = CompiledScript::compile(source).unwrap();
    capture_guard(&mut guarded);
    let mut active = CompiledScript::compile(source).unwrap();
    active.instructions.truncate(active.instructions.len() - 2);
    active.instructions = wrap(&[Undefined, THROW], &active.instructions);

    for (code, limits, expected) in [
        (&plain, (true, true, true), None),
        (&active, (true, true, true), None),
        (
            &guarded,
            (true, false, false),
            Some(EngineFault::PropertyWorkLimitExceeded),
        ),
        (
            &guarded,
            (false, true, false),
            Some(EngineFault::Evaluation(EvalError::AllocationFailed)),
        ),
        (
            &guarded,
            (false, false, true),
            Some(EngineFault::Evaluation(EvalError::AllocationFailed)),
        ),
    ] {
        assert_eq!(evaluate(&mut realm, "side = 0;"), Ok(Completion::Normal));
        {
            let mut ready = realm.instantiate(code).unwrap();
            let mut exec = ready.execution();
            if limits.0 {
                exec.property_work = Work(0);
            }
            if limits.1 {
                exec.heap_requests = 0;
            }
            let before = (
                exec.heap.test_snapshot(),
                exec.property_work.0,
                exec.heap_requests,
            );
            FAIL_CAUGHT_ERROR_RESERVE.with(|fail| fail.set(limits.2));
            let (stop, _) = run_execution(code, &mut exec);
            let did_not_reach_reserve = FAIL_CAUGHT_ERROR_RESERVE.with(|fail| fail.replace(false));
            assert_eq!(did_not_reach_reserve, limits.2 && expected.is_none());
            match expected {
                Some(expected) => {
                    assert!(matches!(stop, Err(Stop::Engine(actual)) if actual == expected))
                }
                None => assert!(
                    matches!(stop, Err(Stop::Automatic(abrupt::Automatic::ReferenceError, actual)) if actual == span)
                ),
            }
            assert_eq!(
                (
                    exec.heap.test_snapshot(),
                    exec.property_work.0,
                    exec.heap_requests
                ),
                before
            );
        }
        assert_eq!(
            evaluate(&mut realm, "if (side !== 7 || capture !== undefined) bad;"),
            Ok(Completion::Normal)
        );
    }
    assert_eq!(
        realm.instantiate(&guarded).unwrap().execute(),
        Ok(Completion::Normal)
    );
    assert_eq!(
        evaluate(&mut realm, "if (capture.name !== 'ReferenceError') bad;"),
        Ok(Completion::Normal)
    );
}

#[test]
fn dead_loop_seeding_and_exceptional_only_entry() {
    let mut code = root(
        wrap(
            &[
                Undefined,
                THROW,
                Boolean(false),
                JumpIfFalseStatement(8),
                Undefined,
                Discard,
                LoopBack(3),
            ],
            &[],
        ),
        1,
    );
    assert!(matches!(
        run(&mut Realm::new().instantiate(&code).unwrap()).0,
        Ok(None)
    ));
    code.instructions[7] = LoopBack(4); // incoming height is one at the test consumer
    reject_atomic(&code);
    code.instructions[7] = LoopBack(3);
    code.instructions[4] = JumpIfFalseStatement(9); // dead edge to exceptional entry
    reject_atomic(&code);
    code.instructions[4] = JumpIfFalseStatement(8);
    code.instructions[2] = Discard; // fallthrough into the dead island would also be valid
    code.instructions[7] = JumpStatement(9);
    reject_atomic(&code);
}

#[test]
fn outward_edges_match_inherited_arm_and_opener_is_in_parent_arm() {
    let mut code = root(
        vec![
            EnterTry {
                catch: 8,
                after: 10,
            },
            EnterTry { catch: 5, after: 7 },
            ExitRegions {
                target: 7,
                pop: 1,
                backward: false,
            },
            JumpStatement(4),
            LeaveTry { enter: 1 },
            EnterCatch { enter: 1 },
            LeaveCatch { enter: 1 },
            LeaveTry { enter: 0 },
            EnterCatch { enter: 0 },
            LeaveCatch { enter: 0 },
        ],
        0,
    );
    assert!(matches!(
        run(&mut Realm::new().instantiate(&code).unwrap()).0,
        Ok(None)
    ));
    code.instructions[2] = ExitRegions {
        target: 7,
        pop: 2,
        backward: false,
    };
    reject_atomic(&code);
    code.instructions[2] = ExitRegions {
        target: 5,
        pop: 1,
        backward: false,
    };
    reject_atomic(&code);
    code.instructions[2] = ExitRegions {
        target: 0,
        pop: 2,
        backward: true,
    };
    code.instructions.push(LoopBack(0)); // dead same-arm backedge to opener
    assert!(matches!(
        run(&mut Realm::new().instantiate(&code).unwrap()).0,
        Err(Stop::Engine(EngineFault::Evaluation(
            EvalError::DispatchLimitExceeded
        )))
    ));
}

#[test]
fn catch_sentinel_never_resolves_as_a_bare_name_in_any_owner_or_dead_arm() {
    let mut code =
        CompiledScript::compile("function first(){source;} function second(){other;} var staged;")
            .unwrap();
    let index = code.functions[0].accesses.start;
    code.sites[index].role = Role::Catch;
    code.sites[index].start = 0;
    code.sites[index].end = 0;
    code.functions[0].instructions.clear();
    code.functions[0].max_stack_depth = 0;
    code.instructions = vec![LoadBinding(index), Discard];
    code.max_stack_depth = 1;
    reject_atomic(&code); // unguarded root -> retained sentinel
    code.instructions.clear();
    code.max_stack_depth = 0;
    code.functions[1].instructions = vec![LoadBinding(index), Discard];
    reject_atomic(&code); // unguarded sibling -> retained sentinel
    code.functions[1].instructions = wrap(&[Undefined, THROW, LoadBinding(index), Discard], &[]);
    reject_atomic(&code); // guarded dead bare access
    code.functions[1].instructions.clear();
    code.functions[1].max_stack_depth = 0;
    assert!(Realm::new().instantiate(&code).is_ok());

    let mut code = CompiledScript::compile("source;").unwrap();
    code.sites[0] = Site {
        span: Span { start: 0, end: 6 },
        start: 0,
        end: 0,
        role: Role::Catch,
    };
    reject_atomic(&code); // body-free unguarded root
}

#[test]
fn malformed_cell_packet_floor_and_call_arity() {
    let mut code = root(
        wrap(
            &[Undefined, THROW],
            &[LoadCatch(CatchSlot { enter: 0, depth: 2 }), Discard],
        ),
        1,
    );
    reject_atomic(&code);
    code.instructions = wrap(&[LoadCatch(FIRST), Discard], &[]);
    reject_atomic(&code); // protected arm cannot see a catch cell
    code.instructions = wrap(
        &[
            Undefined,
            THROW,
            Undefined,
            Call {
                start: 0,
                end: 1,
                argc: 1,
                member: BARE_CALL,
            },
            Discard,
        ],
        &[],
    );
    reject_atomic(&code); // insufficient dead call operands
    let mut code = CompiledScript::compile("'foo'.length;").unwrap();
    let get = code.instructions[1];
    let GetDot(site) = get else {
        panic!("dot fixture")
    };
    code.instructions = wrap(
        &[
            Undefined,
            THROW,
            Undefined,
            PrepareDotCall(site),
            Discard,
            Undefined,
            Call {
                start: 0,
                end: 1,
                argc: 1,
                member: site,
            },
            Discard,
        ],
        &[],
    );
    code.max_stack_depth = 2;
    code.exit = ExitStack::Empty;
    reject_atomic(&code); // dead Discard would consume the prepared callee
}

#[test]
fn guarded_reserve_is_lazy_fallible_and_fuel_only() {
    let code = root(wrap(&[Number(4.0), THROW], &[]), 1);
    let empty = CompiledScript::compile("var unrelated;").unwrap();
    let mut realm = Realm::new();
    super::abrupt::FAIL_RESERVE.with(|flag| flag.set(true));
    assert!(matches!(
        run(&mut realm.instantiate(&empty).unwrap()).0,
        Ok(None)
    ));
    assert!(matches!(
        run(&mut realm.instantiate(&code).unwrap()).0,
        Err(Stop::Engine(EngineFault::Evaluation(
            EvalError::AllocationFailed
        )))
    ));
    let mut ready = realm.instantiate(&code).unwrap();
    let mut execution = ready.execution();
    execution.property_work = Work(0);
    assert!(matches!(
        dispatch(
            Frame::root(&code.instructions, 1, code.exit),
            new_stack(1).unwrap(),
            &code.string_units,
            execution.callable,
            &mut EvalScratch::new(),
            &mut execution
        ),
        Ok(None)
    ));
}

#[test]
fn exact_call_depth_and_aggregate_regions_under_recursive_dispatch() {
    let mut code =
        CompiledScript::compile("function again(n){if(n)again(n-1);} again(31);").unwrap();
    let body = &mut code.functions[0];
    let saved = std::mem::take(&mut body.instructions);
    // Every activation opens four guards before recursing (128 at the live
    // frame ceiling). The innermost protected arm runs the old body.
    for i in 0..4 {
        let catch = (4 + saved.len() + (3 - i) * 3 + 1) as u16;
        body.instructions.push(EnterTry {
            catch,
            after: catch + 2,
        });
    }
    for op in saved {
        body.instructions.push(match op {
            JumpIfFalseStatement(target) => JumpIfFalseStatement(target + 4),
            other => other,
        });
    }
    for enter in (0..4).rev() {
        body.instructions.extend([
            LeaveTry { enter },
            EnterCatch { enter },
            LeaveCatch { enter },
        ]);
    }
    let mut realm = Realm::new();
    assert!(matches!(
        run(&mut realm.instantiate(&code).unwrap()).0,
        Ok(Some(Operand::Scalar(Value::Undefined)))
    ));
    let input = code
        .instructions
        .iter_mut()
        .find(|op| matches!(op, Number(31.0)))
        .unwrap();
    *input = Number(32.0);
    assert!(matches!(
        run(&mut realm.instantiate(&code).unwrap()).0,
        Err(Stop::Engine(EngineFault::Evaluation(
            EvalError::CallDepthLimitExceeded
        )))
    ));
    let script = root(wrap(&[], &[]), 0);
    assert!(matches!(
        run(&mut realm.instantiate(&script).unwrap()).0,
        Ok(None)
    ));
}

#[test]
fn validates_dead_lowered_template_ternary_short_circuit_and_for_update() {
    let mut code = CompiledScript::compile(concat!(
        "function doomed(a){return 1; for(;a;a--) {",
        "if(a) `x${a ? (a && 1) : (a || 2)}y`; continue; }} doomed(2);",
    ))
    .unwrap();
    let f = &mut code.functions[0];
    // Relocate the existing lowerer's edges by one to put it inside a guard.
    for op in &mut f.instructions {
        match op {
            JumpIfFalse(target)
            | JumpIfFalseStatement(target)
            | Jump(target)
            | JumpStatement(target)
            | LoopBack(target)
            | ShortCircuit { target, .. }
            | ShortCircuitStatement { target, .. } => *target += 1,
            _ => {}
        }
    }
    f.instructions = wrap(&f.instructions, &[]);
    assert!(matches!(
        run(&mut Realm::new().instantiate(&code).unwrap()).0,
        Ok(Some(Operand::Scalar(Value::Number(1.0))))
    ));
}

#[test]
fn crossed_marker_and_stale_catch_cell_cannot_publish() {
    let mut code = root(wrap(&[Number(4.0), THROW], &[]), 1);
    code.instructions.push(LoadCatch(FIRST));
    code.instructions.push(Discard);
    reject_atomic(&code);
    code.instructions = wrap(&[Number(4.0), THROW], &[]);
    code.instructions[3] = LeaveTry { enter: 1 };
    reject_atomic(&code);
    code.instructions = wrap(
        &[
            Number(4.0),
            ThrowValue {
                start: 0,
                end: u32::MAX,
            },
        ],
        &[],
    );
    reject_atomic(&code);
}

#[test]
fn body_free_guarded_root_rejects_even_a_balanced_dead_creator_before_gdi() {
    let mut code = root(
        wrap(&[JumpStatement(4), CreateFunction(0), Discard], &[]),
        1,
    );
    assert!(code.functions.is_empty());
    assert_eq!(code.declarations, 1);
    reject_atomic(&code);

    // Preserve legacy admission for unguarded private streams. A dead creator
    // is skipped; a reached creator is still checked by the dispatcher.
    code.instructions = vec![JumpStatement(3), CreateFunction(0), Discard];
    let mut realm = Realm::new();
    assert_eq!(
        realm.instantiate(&code).unwrap().execute(),
        Ok(Completion::Normal)
    );
    code.instructions.remove(0);
    assert_eq!(
        realm.instantiate(&code).unwrap().execute(),
        Err(EngineFault::Evaluation(EvalError::InvalidScript))
    );
}

#[test]
fn all_bare_binding_ops_reject_a_dense_catch_sentinel_even_when_dead() {
    let mut code = CompiledScript::compile("var staged; source;").unwrap();
    let slot = code.declarations;
    code.sites[slot] = Site {
        span: code.sites[slot].span,
        start: 0,
        end: 0,
        role: Role::Catch,
    };
    code.exit = ExitStack::Empty;
    for (operand, op, discard) in [
        (false, LoadBinding(slot), true),
        (true, StoreBinding(slot), false),
        (true, StoreBindingKeep(slot), true),
        (
            false,
            UpdateBinding {
                slot,
                op: Update::Increment,
                result: UpdateResult::New,
            },
            true,
        ),
        (
            true,
            InitBinding {
                slot,
                kind: BindingKind::Let,
            },
            false,
        ),
    ] {
        let target = 2 + usize::from(operand) + usize::from(discard);
        code.instructions = vec![JumpStatement(target)];
        code.instructions.extend(operand.then_some(Undefined));
        code.instructions.push(op);
        code.instructions.extend(discard.then_some(Discard));
        reject_atomic(&code);
    }
}

#[test]
fn active_catch_cannot_branch_back_to_its_exceptional_entry() {
    let mut code = root(
        wrap(
            &[Undefined, THROW],
            &[Boolean(false), JumpIfFalseStatement(8), LoopBack(5)],
        ),
        1,
    );
    assert!(matches!(
        run(&mut Realm::new().instantiate(&code).unwrap()).0,
        Ok(None)
    ));
    // Height, packet, and active-arm signature are all unchanged. Only the
    // exceptional-entry rule rejects this dead edge from the same catch arm.
    code.instructions[7] = LoopBack(4);
    reject_atomic(&code);
}

#[test]
fn exact_guarded_preflight_debit_counts_dot_auth_and_extra_edges() {
    let mut code = CompiledScript::compile("'foo'.length;").unwrap();
    let get = code.instructions[1];
    assert!(matches!(get, GetDot(_)));
    code.instructions = wrap(
        &[
            Boolean(false),
            JumpIfFalseStatement(6),
            Undefined,
            get,
            Discard,
        ],
        &[],
    );
    code.exit = ExitStack::Empty;
    let n = code.instructions.len();
    // Scratch (N+1), both visits (2N), N ordinary edges plus the
    // conditional's other edge and one synthetic exceptional edge, one dot.
    let exact = 4 * n + 4;
    let validate = |work: &mut Work| {
        code.dot.validate_guarded(
            DOT_ROOT,
            &code.instructions,
            code.max_stack_depth,
            code.exit,
            None,
            work,
        )
    };
    assert_eq!(
        validate(&mut Work(exact - 1)),
        Err(InstantiateError::WorkLimitExceeded)
    );
    let mut work = Work(exact);
    assert_eq!(validate(&mut work), Ok(()));
    assert_eq!(work.0, 0);
    assert!(matches!(
        run(&mut Realm::new().instantiate(&code).unwrap()).0,
        Ok(None)
    ));
}

#[test]
fn retained_literal_and_identity_are_not_rebound_to_the_catchers_pool() {
    let mut realm = Realm::new();
    let mut a = CompiledScript::compile(
        "function fromA(){'\\ud800A';} function fromObject(){fromA.prototype;}",
    )
    .unwrap();
    for f in &mut a.functions {
        *f.instructions.last_mut().unwrap() = THROW;
    }
    assert_eq!(
        realm.instantiate(&a).unwrap().execute(),
        Ok(Completion::Normal)
    );
    drop(a);

    let mut b = CompiledScript::compile(
        "function catcher(f){'b'; f();} var text; var object; text=catcher(fromA); object=catcher(fromObject);",
    ).unwrap();
    let catcher = &mut b.functions[0];
    catcher.instructions = wrap(
        &catcher.instructions,
        &[LoadCatch(FIRST), Return { value: true }],
    );
    assert!(matches!(
        run(&mut realm.instantiate(&b).unwrap()).0,
        Ok(None)
    ));
    drop(b);
    let probe =
        CompiledScript::compile("if(text!=='\\ud800A' || object!==fromA.prototype){bad;}").unwrap();
    assert_eq!(
        realm.instantiate(&probe).unwrap().execute(),
        Ok(Completion::Normal)
    );
}
