use super::*;
use crate::Span;
use crate::eval::{
    DotSite, EvalScratch, Operand, Properties, StringOperand,
    realm::{
        Bindings, CompiledScript, Completion, EngineFault, EvalError, Execution, Realm, Stop, Throw,
    },
};

const SITE: Span = Span { start: 0, end: 1 };

fn fresh(exec: &mut Execution<'_, '_>, kind: ErrorKind) -> ObjectId {
    match exec.materialize_error(kind, SITE, |_| Ok(())) {
        Ok(Operand::Object(id)) => id,
        _ => panic!("fresh Error"),
    }
}

fn size(heap: &Heap) -> (usize, usize) {
    (heap.objects.len(), heap.rows.len())
}

fn objects(exec: &Execution<'_, '_>) -> (Option<ErrorRoots>, Vec<Object>, Vec<Row>, [usize; 4]) {
    (
        exec.heap.error_roots,
        exec.heap.objects.clone(),
        exec.heap.rows.clone(),
        exec.heap.test_snapshot(),
    )
}

fn execution_state(
    exec: &Execution<'_, '_>,
) -> ([usize; 7], Vec<u16>, Vec<(FunctionOrigin, ObjectId)>) {
    (
        [
            exec.lookup.0,
            exec.property_work.0,
            exec.heap_requests,
            exec.copied,
            exec.functions.capacity(),
            exec.callable.artifacts.len(),
            exec.realm.object.vars.len(),
        ],
        exec.realm.strings.units().to_vec(),
        exec.functions
            .iter()
            .map(|f| (f.origin, f.object))
            .collect(),
    )
}

fn dot(exec: &Execution<'_, '_>, index: u16) -> DotSite {
    let (span, mode) = exec.dot_metadata(None, index).ok().unwrap();
    DotSite {
        owner: None,
        index,
        span,
        mode,
    }
}

fn expect_string(value: Result<Operand, Stop>, expected: &str) {
    let Ok(Operand::String(string)) = value else {
        panic!("expected String")
    };
    let scratch = EvalScratch::new();
    assert_eq!(
        string.get(scratch.pools(&[])),
        Some(expected.encode_utf16().collect::<Vec<_>>().as_slice())
    );
}

#[test]
fn native_roots_are_real_descriptors_and_fresh_errors_use_the_existing_value_domain() {
    let code = CompiledScript::compile(
        "a.name; a.message; a.constructor; a.toString; a.__proto__; a.missing; a.prototype;",
    )
    .unwrap();
    let mut realm = Realm::new();
    let mut ready = realm.instantiate(&code).unwrap();
    let mut exec = ready.execution();
    for kind in ErrorKind::ALL {
        assert_eq!(exec.heap.callable_object(kind.callable(), &[]), None);
    }
    let scratch = EvalScratch::new();
    let source = scratch.transient(&[]);
    for (kind, name, proto, ctor, proto_parent, ctor_parent) in [
        (ErrorKind::Error, "Error", 4, 5, 0, 1),
        (ErrorKind::ReferenceError, "ReferenceError", 6, 7, 4, 5),
        (ErrorKind::TypeError, "TypeError", 8, 9, 4, 5),
    ] {
        let id = fresh(&mut exec, kind);
        let other = fresh(&mut exec, kind);
        let proto = ObjectId(proto);
        let ctor = ObjectId(ctor);
        assert_ne!(id, other);
        assert_eq!(exec.heap.objects[usize::from(id.0)], Object::error(proto));
        assert_eq!(
            exec.heap.objects[usize::from(other.0)],
            Object::error(proto)
        );
        assert_eq!(
            exec.heap.objects[usize::from(proto.0)].shape,
            Shape::Complete
        );
        assert_eq!(
            exec.heap.objects[usize::from(proto.0)].prototype,
            Some(ObjectId(proto_parent))
        );
        assert_eq!(
            exec.heap.objects[usize::from(ctor.0)].prototype,
            Some(ObjectId(ctor_parent))
        );
        assert_eq!(exec.callable_object(kind.callable()).ok(), Some(ctor));
        assert!(!exec.heap.is_noncallable(ctor));
        assert_eq!(exec.heap.origin(kind.callable(), &[]), None);
        assert!(matches!(
            exec.classify_callable(kind.callable(), SITE),
            Err(Stop::Engine(EngineFault::Evaluation(
                EvalError::InvalidScript
            )))
        ));

        let receiver = exec.load_value(StoredValue::Object(id)).ok().unwrap();
        let get = |exec: &mut Execution<'_, '_>, index, value| {
            exec.get_dot(dot(exec, index), value, source)
        };
        expect_string(get(&mut exec, 0, receiver), name);
        expect_string(get(&mut exec, 1, receiver), "");
        assert!(
            matches!(get(&mut exec, 2, receiver), Ok(Operand::Function(actual)) if actual == kind.callable())
        );
        assert!(
            matches!(get(&mut exec, 6, Operand::Function(kind.callable())), Ok(Operand::Object(actual)) if actual == proto)
        );
        assert!(matches!(
            get(&mut exec, 3, receiver),
            Err(Stop::Engine(EngineFault::UnsupportedProperty {
                cause: super::super::PropertyFault::Builtin,
                ..
            }))
        ));
        assert!(matches!(
            get(&mut exec, 4, receiver),
            Err(Stop::Engine(EngineFault::UnsupportedProperty {
                cause: super::super::PropertyFault::AnnexB,
                ..
            }))
        ));
        assert!(matches!(
            get(&mut exec, 5, receiver),
            Ok(Operand::Scalar(Value::Undefined))
        ));
        for (object, key, expected) in [
            (ctor, "name", Descriptor::Data(kind.name(), META)),
            (
                ctor,
                "length",
                Descriptor::Data(StoredValue::Scalar(Value::Number(1.0)), META),
            ),
            (
                ctor,
                "prototype",
                Descriptor::Data(StoredValue::Object(proto), FROZEN),
            ),
            (proto, "name", Descriptor::Data(kind.name(), METHOD)),
            (
                proto,
                "message",
                Descriptor::Data(StoredValue::Empty, METHOD),
            ),
            (
                proto,
                "constructor",
                Descriptor::Data(StoredValue::Function(kind.callable()), METHOD),
            ),
        ] {
            let key: Vec<_> = key.encode_utf16().collect();
            assert!(
                matches!(exec.heap.lookup(object, &key, &[], &mut Work(1024)), Ok(Lookup::Found { own: true, descriptor, .. }) if descriptor == expected)
            );
        }
    }
    assert_eq!(size(exec.heap), (16, 46));
    assert!(exec.realm.strings.units().is_empty());
    let receiver = Operand::Object(ObjectId(10));
    let shadow = Operand::String(StringOperand::StaticText(StaticText::ObjectName));
    exec.put_dot(dot(&exec, 1), receiver, shadow, source)
        .ok()
        .unwrap();
    expect_string(exec.get_dot(dot(&exec, 1), receiver, source), "Object");
    expect_string(
        exec.get_dot(dot(&exec, 1), Operand::Object(ObjectId(11)), source),
        "",
    );
    assert_eq!(exec.heap.objects[10].shape, Shape::ErrorData);
    assert_eq!(size(exec.heap), (16, 47));
    assert_eq!(
        exec.realm.strings.units(),
        &[109, 101, 115, 115, 97, 103, 101]
    );
}

#[test]
fn error_pack_location_and_user_backlinks_survive_both_allocation_orders() {
    let code = CompiledScript::compile("function f(){} f.x='persist';").unwrap();
    let expr = CompiledScript::compile("(function(){});").unwrap();
    let empty = CompiledScript::compile("1;").unwrap();
    for errors_first in [true, false] {
        let mut realm = Realm::new();
        if errors_first {
            let mut ready = realm.instantiate(&empty).unwrap();
            assert_eq!(
                fresh(&mut ready.execution(), ErrorKind::ReferenceError),
                ObjectId(10)
            );
        }
        assert_eq!(
            realm.instantiate(&code).unwrap().execute(),
            Ok(Completion::Normal)
        );
        let before = realm.functions[0];
        let mut ready = realm.instantiate(&expr).unwrap();
        let mut exec = ready.execution();
        if !errors_first {
            assert_eq!(size(exec.heap), (6, 32));
            assert_eq!(fresh(&mut exec, ErrorKind::ReferenceError), ObjectId(12));
        }
        let native = exec
            .callable_object(ErrorKind::ReferenceError.callable())
            .ok()
            .unwrap();
        assert_eq!(native, ObjectId(if errors_first { 7 } else { 9 }));
        // Interleave reached users and fresh Errors while both object and
        // descriptor logical tiers grow. Neither kind may be renumbered.
        for i in 1..=20 {
            assert_eq!(exec.create_function(0).ok(), Some(FunctionId::User(i)));
            fresh(&mut exec, ErrorKind::TypeError);
        }
        assert_eq!(
            exec.callable_object(ErrorKind::ReferenceError.callable())
                .ok(),
            Some(native)
        );
        assert_eq!(
            exec.callable_object(FunctionId::User(0)).ok(),
            Some(before.object)
        );
        assert_eq!(
            exec.function_origin(FunctionId::User(0)).ok(),
            Some(before.origin)
        );
        assert_eq!(size(exec.heap), (73, 131));
        drop(ready);
        let observer = CompiledScript::compile("if (f.x !== 'persist') missing;").unwrap();
        assert_eq!(
            realm.instantiate(&observer).unwrap().execute(),
            Ok(Completion::Normal)
        );
    }
}

#[test]
fn first_error_exact_debit_and_detached_reserve_failures_are_atomic() {
    let empty = CompiledScript::compile("1;").unwrap();
    let one = CompiledScript::compile("function f(){}").unwrap();
    for after_user in [false, true] {
        let mut realm = Realm::new();
        if after_user {
            drop(realm.instantiate(&one).unwrap());
        }
        let mut ready = realm.instantiate(&empty).unwrap();
        let mut exec = ready.execution();
        let work = if after_user { 89 } else { 114 };
        let bytes = 16 * size_of::<Object>() + 64 * size_of::<Row>();
        let before = objects(&exec);
        for (avail_work, avail_bytes, fail_at) in [
            (work - 1, bytes, None),
            (work, bytes - 1, None),
            (work, bytes, Some(0)),
            (work, bytes, Some(1)),
        ] {
            exec.property_work = Work(avail_work);
            exec.heap_requests = avail_bytes;
            let before_execution = execution_state(&exec);
            let mut calls = Vec::new();
            let result = exec.materialize_error(ErrorKind::Error, SITE, |attempt| {
                calls.push(attempt);
                if Some(attempt) == fail_at {
                    Err(InstantiateError::AllocationFailed)
                } else {
                    Ok(())
                }
            });
            if avail_work < work {
                assert!(matches!(
                    result,
                    Err(Stop::Engine(EngineFault::PropertyWorkLimitExceeded))
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(Stop::Engine(EngineFault::Evaluation(
                        EvalError::AllocationFailed
                    )))
                ));
            }
            assert_eq!(
                calls,
                fail_at
                    .map(|last| (0..=last).collect::<Vec<_>>())
                    .unwrap_or_default()
            );
            assert_eq!(objects(&exec), before);
            assert_eq!(execution_state(&exec), before_execution);
        }
        let plan = ErrorCreation::plan(exec.heap, exec.functions.len(), ErrorKind::Error).unwrap();
        drop(plan.reserve_with(|_| Ok(())).unwrap());
        assert_eq!(objects(&exec), before);
        let id = fresh(&mut exec, ErrorKind::Error);
        assert_eq!(id, ObjectId(if after_user { 12 } else { 10 }));
        assert_eq!(
            size(exec.heap),
            if after_user { (13, 50) } else { (11, 46) }
        );
        assert_eq!((exec.property_work.0, exec.heap_requests), (0, 0));
    }
}

#[test]
fn reached_creator_and_error_materializer_debit_the_same_execution_ledger() {
    let code = CompiledScript::compile("(function(){});").unwrap();
    for fits in [true, false] {
        let mut realm = Realm::new();
        let mut ready = realm.instantiate(&code).unwrap();
        let mut exec = ready.execution();
        // First Error is 114; the reached function fits both existing tiers
        // and charges 7 pack checks plus 2 * (2 objects + 4 rows).
        exec.property_work = Work(114 + 19 - usize::from(!fits));
        exec.heap_requests = 16 * size_of::<Object>() + 64 * size_of::<Row>();
        assert_eq!(fresh(&mut exec, ErrorKind::Error), ObjectId(10));
        assert_eq!(exec.heap_requests, 0);
        let before = objects(&exec);
        let before_execution = execution_state(&exec);
        if fits {
            assert_eq!(exec.create_function(0).ok(), Some(FunctionId::User(0)));
            assert_eq!(exec.property_work.0, 0);
            assert_eq!(size(exec.heap), (13, 50));
        } else {
            assert!(matches!(
                exec.create_function(0),
                Err(Stop::Engine(EngineFault::PropertyWorkLimitExceeded))
            ));
            assert_eq!(objects(&exec), before);
            assert_eq!(execution_state(&exec), before_execution);
        }
    }
}

#[test]
fn later_object_only_growth_failure_does_not_change_pack_rows_or_ledger() {
    let code = CompiledScript::compile("1;").unwrap();
    let mut realm = Realm::new();
    let mut ready = realm.instantiate(&code).unwrap();
    let mut exec = ready.execution();
    for _ in 0..6 {
        fresh(&mut exec, ErrorKind::ReferenceError);
    }
    assert_eq!(size(exec.heap), (16, 46));
    let before = objects(&exec);
    exec.property_work = Work(16 + 2 + 7);
    exec.heap_requests = 32 * size_of::<Object>();
    let state = execution_state(&exec);
    let mut calls = Vec::new();
    assert!(matches!(
        exec.materialize_error(ErrorKind::TypeError, SITE, |phase| {
            calls.push(phase);
            Err(InstantiateError::AllocationFailed)
        }),
        Err(Stop::Engine(EngineFault::Evaluation(
            EvalError::AllocationFailed
        )))
    ));
    assert_eq!(calls, [0]);
    assert_eq!(objects(&exec), before);
    assert_eq!(execution_state(&exec), state);
    assert_eq!(fresh(&mut exec, ErrorKind::TypeError), ObjectId(16));
    assert_eq!((exec.property_work.0, exec.heap_requests), (0, 0));
    assert_eq!(exec.heap.rows, before.2);
    assert_eq!(exec.heap.error_roots, before.0);
}

fn add_users(exec: &mut Execution<'_, '_>, count: usize) -> Result<FunctionId, InstantiateError> {
    // Same template, multiple fresh identities: no need for hundreds of
    // parser/Ready fixtures to isolate the retained heap frontiers.
    let origins = vec![
        FunctionOrigin {
            artifact: 0,
            body: 0
        };
        count
    ];
    Ok(Creation::plan(
        exec.heap,
        exec.functions,
        0,
        &exec.callable.artifacts[0],
        &origins,
    )?
    .reserve()?
    .publish())
}

#[test]
fn object_and_user_limits_are_independent_on_a_mixed_heap() {
    let code = CompiledScript::compile("(function(){});").unwrap();
    for user_cap in [true, false] {
        let mut realm = Realm::new();
        let mut ready = realm.instantiate(&code).unwrap();
        let mut exec = ready.execution();
        fresh(&mut exec, ErrorKind::Error);
        assert_eq!(add_users(&mut exec, 256), Ok(FunctionId::User(0)));
        assert_eq!(
            add_users(&mut exec, if user_cap { 256 } else { 254 }),
            Ok(FunctionId::User(256))
        );
        assert_eq!(
            size(exec.heap),
            if user_cap { (1035, 2094) } else { (1031, 2086) }
        );
        if user_cap {
            assert_eq!(
                add_users(&mut exec, 1),
                Err(InstantiateError::FunctionLimitExceeded)
            );
            for id in 1035..1280 {
                assert_eq!(fresh(&mut exec, ErrorKind::Error), ObjectId(id));
            }
        } else {
            for id in 1031..1278 {
                assert_eq!(fresh(&mut exec, ErrorKind::Error), ObjectId(id));
            }
            assert_eq!(add_users(&mut exec, 1), Ok(FunctionId::User(510)));
        }
        assert_eq!(exec.heap.objects.len(), OBJECT_LIMIT);
        let before = objects(&exec);
        let state = execution_state(&exec);
        assert!(matches!(
            exec.materialize_error(ErrorKind::Error, SITE, |_| panic!("limit before reserve")),
            Err(Stop::Engine(EngineFault::PropertyLimitExceeded {
                span: SITE
            }))
        ));
        assert_eq!(objects(&exec), before);
        assert_eq!(execution_state(&exec), state);
    }

    let mut realm = Realm::new();
    let mut ready = realm.instantiate(&code).unwrap();
    let mut exec = ready.execution();
    for _ in 0..(OBJECT_LIMIT - 1 - 10) {
        fresh(&mut exec, ErrorKind::TypeError);
    }
    assert_eq!(exec.heap.objects.len(), OBJECT_LIMIT - 1);
    let before = objects(&exec);
    assert_eq!(
        add_users(&mut exec, 1),
        Err(InstantiateError::PropertyLimitExceeded)
    );
    assert_eq!(objects(&exec), before);
    assert!(exec.functions.is_empty());
}

#[test]
fn first_pack_checks_shared_row_limit_but_later_errors_are_rowless() {
    let code = CompiledScript::compile("(function(){});").unwrap();
    for fits in [true, false] {
        let mut realm = Realm::new();
        let mut ready = realm.instantiate(&code).unwrap();
        let mut exec = ready.execution();
        add_users(&mut exec, 100).unwrap();
        let target = ROW_LIMIT - ERROR_ROWS + usize::from(!fits);
        let retained: Vec<u16> = (b'a'..=b'z').map(u16::from).collect();
        assert!(matches!(
            exec.realm.strings.append(&retained, &mut exec.copied, SITE),
            Ok(StoredValue::Realm(RealmRange { start: 0, end: 26 }))
        ));
        // Install bounded, unique dynamic rows with the regular detached row
        // edit, not a fake len or a row vector containing holes.
        for n in 0..target - exec.heap.rows.len() {
            let owner = ObjectId(4 + (n / retained.len()) as u16);
            let start = (n % retained.len()) as u16;
            let lookup = exec
                .heap
                .lookup(
                    owner,
                    &retained[usize::from(start)..][..1],
                    exec.realm.strings.units(),
                    &mut Work(4096),
                )
                .unwrap();
            assert!(matches!(lookup, Lookup::Missing));
            let edit = exec.heap.edit(owner, lookup).unwrap();
            edit.reserve().unwrap().publish(
                Some(RealmRange {
                    start,
                    end: start + 1,
                }),
                StoredValue::Scalar(Value::Boolean(true)),
            );
        }
        assert_eq!(exec.heap.rows.len(), target);
        if fits {
            let first = fresh(&mut exec, ErrorKind::ReferenceError);
            assert_eq!(size(exec.heap), (211, ROW_LIMIT));
            assert_eq!(
                fresh(&mut exec, ErrorKind::TypeError),
                ObjectId(first.0 + 1)
            );
            assert_eq!(size(exec.heap), (212, ROW_LIMIT));
        } else {
            let before = objects(&exec);
            let state = execution_state(&exec);
            assert!(matches!(
                exec.materialize_error(ErrorKind::Error, SITE, |_| panic!("limit before reserve")),
                Err(Stop::Engine(EngineFault::PropertyLimitExceeded {
                    span: SITE
                }))
            ));
            assert_eq!(objects(&exec), before);
            assert_eq!(execution_state(&exec), state);
        }
    }
}

#[test]
fn diagnostic_only_faults_do_not_allocate_and_forged_pack_associations_are_rejected() {
    let mut realm = Realm::new();
    let cases = [("missing;", true), ("(0)();", false), ("null.x;", false)];
    for (source, reference) in cases {
        let code = CompiledScript::compile(source).unwrap();
        let result = realm.instantiate(&code).unwrap().execute();
        if reference {
            assert!(matches!(
                result,
                Ok(Completion::Throw(Throw::ReferenceError { .. }))
            ));
        } else {
            assert!(matches!(
                result,
                Ok(Completion::Throw(Throw::TypeError { .. }))
            ));
        }
        assert_eq!(size(&realm.heap), (0, 0));
        assert_eq!(realm.heap.error_roots, None);
    }
    let code = CompiledScript::compile("1;").unwrap();
    let mut ready = realm.instantiate(&code).unwrap();
    let mut exec = ready.execution();
    fresh(&mut exec, ErrorKind::Error);
    let pack = exec.heap.error_roots.unwrap();
    let ctor = usize::from(pack.constructor(ErrorKind::TypeError).0);
    exec.heap.objects[ctor].callable = Some(ErrorKind::Error.callable());
    assert_eq!(
        exec.heap
            .callable_object(ErrorKind::TypeError.callable(), &[]),
        None
    );
    assert!(matches!(
        exec.materialize_error(ErrorKind::Error, SITE, |_| panic!("invalid before reserve")),
        Err(Stop::Engine(EngineFault::Evaluation(
            EvalError::InvalidScript
        )))
    ));
}

#[test]
#[cfg(target_arch = "aarch64")]
fn native_errors_use_the_existing_compact_layouts() {
    assert_eq!(
        (
            size_of::<Object>(),
            size_of::<Row>(),
            size_of::<FunctionRecord>(),
            size_of::<Heap>()
        ),
        (14, 40, 6, 56)
    );
    assert_eq!((size_of::<Operand>(), size_of::<StoredValue>()), (16, 16));
}
