use super::*;
use Instruction::{
    Boolean, Discard, FormDot, GetDot, Jump, JumpIfFalse, Number, PutDot, String as Literal,
};

fn dot_code(owner: u16, key: &str) -> DotCode {
    let keys = key.encode_utf16().collect::<Vec<_>>();
    DotCode {
        source_len: 64,
        sites: vec![DotOccurrence {
            owner,
            key_start: 0,
            key_end: keys.len() as u16,
            dot_start: 2,
            dot_end: 3,
        }],
        keys,
    }
}

fn branched(literal: Instruction, branch: usize) -> Vec<Instruction> {
    // Either branch has zero operands and zero packets at exit. The bad edge
    // enters another opener's RHS with an otherwise equivalent Form packet.
    vec![
        literal,
        FormDot(0),
        Boolean(false),
        JumpIfFalse(branch),
        Number(1.0),
        PutDot(0),
        Discard,
        Jump(13),
        literal,
        FormDot(0),
        Number(2.0),
        PutDot(0),
        Discard,
    ]
}

fn snapshot(realm: &Realm) -> [usize; 11] {
    [
        realm.state.globals.len(),
        realm.state.globals.capacity(),
        realm.state.object.vars.len(),
        realm.state.object.vars.capacity(),
        realm.artifacts.len(),
        realm.artifacts.capacity(),
        realm.artifact_bytes,
        realm.functions.len(),
        realm.functions.capacity(),
        realm.state.strings.used,
        realm.state.strings.chunks.capacity(),
    ]
}

#[test]
fn balanced_no_call_root_checks_opener_provenance_before_publication() {
    let mut code = CompiledScript::compile("var staged; 'abc';").unwrap();
    let literal = code.instructions[0];
    assert!(matches!(literal, Literal(_)));
    code.dot = dot_code(DOT_ROOT, "length");
    code.max_stack_depth = 1;
    code.exit = ExitStack::Empty;
    let mut realm = Realm::new();
    let pristine = snapshot(&realm);
    for branch in [10, 14] {
        code.instructions = branched(literal, branch);
        assert_eq!(
            realm.instantiate(&code).unwrap_err(),
            InstantiateError::InvalidScript
        );
        assert_eq!(snapshot(&realm), pristine);
    }
    code.instructions = branched(literal, 4);
    code.dot.sites[0].owner = 0;
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::InvalidScript
    );
    assert_eq!(snapshot(&realm), pristine);
    code.dot.sites[0].owner = DOT_ROOT;
    code.dot.sites[0].key_end += 1;
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::InvalidScript
    );
    assert_eq!(snapshot(&realm), pristine);
    code.dot.sites[0].key_end -= 1;
    assert!(code.functions.is_empty());
    assert!(
        !code
            .instructions
            .iter()
            .any(|op| matches!(op, Instruction::Call { .. }))
    );
    assert_eq!(
        realm.instantiate(&code).unwrap().execute(),
        Ok(Completion::Normal)
    );
    assert!(realm.artifacts.is_empty());
    assert_eq!(realm.state.object.vars[0].name, b"staged");
}

#[test]
fn every_body_checks_actual_owner_and_all_edges_before_gdi_commit() {
    let first = CompiledScript::compile("function keep(){ return 7; } var saved = keep;").unwrap();
    let mut realm = Realm::new();
    assert_eq!(
        realm.instantiate(&first).unwrap().execute(),
        Ok(Completion::Normal)
    );
    let old = snapshot(&realm);
    let old_keep = realm.state.object.vars[0].value;
    let mut code = CompiledScript::compile(
        "function keep(){ 'x'; } function other(){ 'y'; } var newvar; let newlex;",
    )
    .unwrap();
    let literal = code.functions[1].instructions[0];
    code.functions[1].instructions = branched(literal, 10);
    code.functions[1].max_stack_depth = 1;
    code.dot = dot_code(1, "length");
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::InvalidScript
    );
    assert_eq!(snapshot(&realm), old);
    code.functions[1].instructions = branched(literal, 4);
    for owner in [0, DOT_ROOT] {
        code.dot.sites[0].owner = owner;
        assert_eq!(
            realm.instantiate(&code).unwrap_err(),
            InstantiateError::InvalidScript
        );
        assert_eq!(snapshot(&realm), old);
        assert_eq!(realm.state.object.vars[0].value, old_keep);
    }
    code.dot.sites[0].owner = 1;
    // Search the exact budget boundary of this compiled program at the same
    // Realm shape; the last failed staging must publish no replacement/rows.
    let (mut lo, mut hi) = (0, PREFLIGHT_WORK);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        let mut probe = Realm::new();
        assert_eq!(
            probe.instantiate(&first).unwrap().execute(),
            Ok(Completion::Normal)
        );
        match probe.instantiate_with_work(&code, mid) {
            Ok(_) => hi = mid,
            Err(InstantiateError::WorkLimitExceeded) => lo = mid,
            Err(error) => panic!("{error:?}"),
        }
    }
    assert_eq!(
        realm.instantiate_with_work(&code, lo).unwrap_err(),
        InstantiateError::WorkLimitExceeded
    );
    assert_eq!(snapshot(&realm), old);
    assert_eq!(realm.state.object.vars[0].value, old_keep);
    drop(realm.instantiate_with_work(&code, hi).unwrap());
    let observer = CompiledScript::compile("other(); if (saved() !== 7) fail;").unwrap();
    assert_eq!(
        realm.instantiate(&observer).unwrap().execute(),
        Ok(Completion::Normal)
    );
}

#[test]
fn primitive_get_checks_owner_even_when_unreached_and_no_artifact_exists() {
    let mut realm = Realm::new();
    let mut code = CompiledScript::compile("'💡';").unwrap();
    let literal = code.instructions[0];
    code.instructions = vec![Jump(4), literal, GetDot(0), Discard];
    code.exit = ExitStack::Empty;
    code.max_stack_depth = 1;
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::InvalidScript
    );
    code.dot = dot_code(DOT_ROOT, "length");
    assert_eq!(
        realm.instantiate(&code).unwrap().execute(),
        Ok(Completion::Normal)
    );
    code.dot.sites[0].dot_end += 1;
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::InvalidScript
    );
}

#[test]
fn actual_lookup_debits_visited_rows_and_units_through_first_mismatch() {
    let mut realm = Realm::new();
    let mut code = CompiledScript::compile("function f(){}").unwrap();
    code.dot = dot_code(DOT_ROOT, "abc");
    let mut ready = realm.instantiate(&code).unwrap();
    let mut execution = ready.execution();
    let receiver = Operand::Function(FunctionId::User(0));
    let source = EvalScratch::new();
    let source = source.transient(&[]);
    let (span, mode) = execution
        .dot_metadata(None, 0)
        .unwrap_or_else(|_| panic!("metadata"));
    let site = DotSite {
        owner: None,
        index: 0,
        span,
        mode,
    };
    execution
        .put_dot(site, receiver, Operand::Scalar(Value::Number(8.0)), source)
        .unwrap_or_else(|_| panic!("put"));
    assert_eq!(execution.realm.strings.units(), [97, 98, 99]);
    execution.property_work = Work(4);
    assert!(matches!(
        execution.get_dot(site, receiver, source),
        Err(Stop::Engine(EngineFault::PropertyWorkLimitExceeded))
    ));
    execution.property_work = Work(5);
    assert!(matches!(
        execution.get_dot(site, receiver, source),
        Ok(Operand::Scalar(Value::Number(8.0)))
    ));
    assert_eq!(execution.property_work.0, 0);
}

#[test]
fn literal_only_prefix_and_origin_selected_name_are_independent() {
    let code = CompiledScript::compile("function first(){'A';} function second(){'BC';}").unwrap();
    let mut realm = Realm::new();
    drop(realm.instantiate(&code).unwrap());
    let artifact = &realm.artifacts[0];
    assert_eq!(artifact.literal_units(), [65, 66, 67]);
    let scratch = EvalScratch::new();
    let mut source = scratch.transient(&[]);
    source.artifacts = &realm.artifacts;
    assert_eq!(
        source.code_name(FunctionOrigin {
            artifact: 0,
            body: 1
        }),
        Some(&[115, 101, 99, 111, 110, 100][..])
    );
    assert_eq!(
        source.code_name(FunctionOrigin {
            artifact: 1,
            body: 1
        }),
        None
    );
    assert_eq!(
        source.code_name(FunctionOrigin {
            artifact: 0,
            body: 2
        }),
        None
    );
    let name_tail_probe = CompiledScript::compile("'0123'; 'x';").unwrap();
    let Literal(range) = name_tail_probe.instructions[2] else {
        panic!("literal")
    };
    assert_eq!(source.artifact_literal(0, range), None);
}

#[test]
fn late_gdi_reserve_failures_keep_all_tables_detached() {
    let mut realm = Realm::new();
    let code = CompiledScript::compile("function f(){} var v; let l;").unwrap();
    let state = snapshot(&realm);
    let heap = realm.heap.test_snapshot();
    for fail_at in 0..3 {
        let mut visited = 0;
        assert_eq!(
            realm
                .instantiate_with_reserve(&code, PREFLIGHT_WORK, |index| {
                    assert_eq!(index, visited);
                    visited += 1;
                    if index == fail_at {
                        Err(InstantiateError::AllocationFailed)
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err(),
            InstantiateError::AllocationFailed
        );
        assert_eq!(visited, fail_at + 1);
        assert_eq!(snapshot(&realm), state);
        assert_eq!(realm.heap.test_snapshot(), heap);
    }
    drop(realm.instantiate(&code).unwrap());
    assert_eq!(realm.heap.test_snapshot()[0], 6);
}

#[test]
fn put_reserve_failures_leave_heap_arena_and_meters_untouched() {
    let mut realm = Realm::new();
    let mut code = CompiledScript::compile("function f(){}").unwrap();
    code.dot = dot_code(DOT_ROOT, "new");
    code.dot.keys.extend("old".encode_utf16());
    code.dot.sites.push(DotOccurrence {
        key_start: 3,
        key_end: 6,
        ..code.dot.sites[0]
    });
    let mut ready = realm.instantiate(&code).unwrap();
    let mut exec = ready.execution();
    // Install row 32 before attempting a second distinct field.
    let receiver = Operand::Function(FunctionId::User(0));
    let scratch = EvalScratch::new();
    let source = scratch.transient(&[]);
    let (span, mode) = exec
        .dot_metadata(None, 0)
        .unwrap_or_else(|_| panic!("metadata"));
    let site = DotSite {
        owner: None,
        index: 0,
        span,
        mode,
    };
    let rhs = Operand::Scalar(Value::Number(42.0));
    let snapshot = |exec: &Execution<'_, '_>| {
        (
            exec.heap.test_snapshot(),
            exec.realm.strings.used,
            exec.realm.strings.chunks.capacity(),
            exec.copied,
            exec.property_work.0,
            exec.heap_requests,
        )
    };
    exec.put_dot(DotSite { index: 1, ..site }, receiver, rhs, source)
        .unwrap_or_else(|_| panic!("first"));
    // Keep row 32 and fill the current string chunk, so both reserves are
    // actually needed for row 33 and its new copied field key.
    exec.realm
        .strings
        .append(&[0; CHUNK_UNITS - 3], &mut exec.copied, site.span)
        .unwrap_or_else(|_| panic!("fill"));
    let initial = snapshot(&exec);
    for fail_arena in [false, true] {
        let result = exec.put_dot_with(
            site,
            receiver,
            rhs,
            source,
            |edit| {
                assert!(edit.request_bytes() > 0);
                if fail_arena {
                    edit.reserve()
                } else {
                    Err(InstantiateError::AllocationFailed)
                }
            },
            |plan| plan.reserve_for_test(|_, _| Vec::<u16>::new().try_reserve_exact(usize::MAX)),
        );
        assert!(matches!(
            result,
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::AllocationFailed
            )))
        ));
        assert_eq!(snapshot(&exec), initial);
    }
    exec.put_dot(site, receiver, rhs, source)
        .unwrap_or_else(|_| panic!("put"));
    assert_eq!(&exec.realm.strings.units()[CHUNK_UNITS..], [110, 101, 119]);
}
