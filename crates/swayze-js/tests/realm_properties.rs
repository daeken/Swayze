use swayze_js::{
    EvalError, Span,
    realm::{
        CompileError, CompileLimit, CompiledScript, Completion, EngineFault, IntrinsicCall,
        PropertyFault, Realm, Throw,
    },
};

fn run(realm: &mut Realm, source: &str) -> Result<Completion, EngineFault> {
    let code = CompiledScript::compile(source)
        .unwrap_or_else(|error| panic!("{error:?}: {}", &source[..source.len().min(200)]));
    realm.instantiate(&code).unwrap().execute()
}

fn normal(realm: &mut Realm, source: &str) {
    assert_eq!(run(realm, source), Ok(Completion::Normal), "{source}");
}

fn dot(source: &str) -> Span {
    let start = source.find('.').unwrap();
    Span {
        start,
        end: start + 1,
    }
}

#[test]
fn real_metadata_and_identity_survive_ready_and_script_drop() {
    let mut realm = Realm::new();
    let producer = CompiledScript::compile(
        "function add(a,b){return a+b;} var saved; var inf = function(){};",
    )
    .unwrap();
    normal(&mut realm, "var side = 0;");
    assert_eq!(
        realm.instantiate(&producer).unwrap().execute(),
        Ok(Completion::Normal)
    );
    drop(producer);
    normal(
        &mut realm,
        "saved = add; if (add.name !== 'add' || add.length !== 2 ||
        typeof add.prototype !== 'object' || add.prototype.constructor !== add ||
        !(add.prototype == add.prototype) || add.prototype == add || add.prototype == null ||
        !add.prototype || add.prototype !== (add.prototype ?? fail) || inf.name !== 'inf' ||
        (function(){}).name !== '' || typeof add.constructor !== 'function' ||
        add.constructor.name !== 'Function' || add.constructor.length !== 1 ||
        add.constructor.prototype.name !== '' || add.constructor.prototype.length !== 0 ||
        add.constructor.prototype.constructor !== add.constructor ||
        add.prototype.constructor(1,2) !== 3) fail;",
    );
    let replaces = CompiledScript::compile("function add(){return -1;}").unwrap();
    drop(realm.instantiate(&replaces).unwrap());
    drop(replaces);
    normal(
        &mut realm,
        "if (add === saved || add.prototype === saved.prototype || saved(2,3) !== 5 || add() !== -1) fail;",
    );
}

#[test]
fn origin_scoped_names_literals_templates_and_dot_keys_are_independent_across_calls() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "function take(v){return v;} function first(){return take(first.name) + 'A';}
         var left = first; var obj = first.prototype; obj.left = first.name;",
    );
    normal(
        &mut realm,
        "function first(){return 'B';} function right(v){return `${v}:${first.name}:${first()}`;}
         var second = first; obj.right = right.name; var literal = 'C';",
    );
    normal(
        &mut realm,
        "if (left() !== 'firstA' || right(obj.left) !== 'first:first:B' || obj.right !== 'right' ||
         right(second.name) !== 'first:first:B' || obj.left.length !== 5 ||
         +obj.left === +obj.left || literal !== 'C') fail;",
    );
    normal(
        &mut realm,
        "function keyLeft(){return obj.left;} function keyRight(){return obj.right;}",
    );
    normal(
        &mut realm,
        "if (keyLeft() !== 'first' || keyRight() !== 'right') fail;",
    );
}

#[test]
fn member_get_is_early_but_set_uses_the_current_receiver_rows() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "function a(v){return v+1;} function b(v){return v+2;} var alias = a; a.m = a;
         if ((a.m)((alias.m = b, 5)) !== 6 || (0,a.m)(5) !== 7) fail;
         a.slot = (alias.other = 'first', alias.slot = 'inner', 'outer');
         if (a.slot !== 'outer' || a.other !== 'first') fail;
         function rewrites(){a.slot = 10; return 20;} a.slot = rewrites();
         if (a.slot !== 20) fail;
         a.slot = (a = b, 30); if (alias.slot !== 30 || a.slot !== void 0) fail;",
    );
}

#[test]
fn descriptor_barriers_and_intrinsic_calls_have_distinct_reached_faults() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "function f(x){} var side = 0; var proto = f.constructor.prototype;",
    );
    let builtin = "f.call(side = 1);";
    assert_eq!(
        run(&mut realm, builtin),
        Err(EngineFault::UnsupportedProperty {
            span: dot(builtin),
            cause: PropertyFault::Builtin
        })
    );
    normal(
        &mut realm,
        "if (side !== 0) fail; f.call = f; if (f.call !== f || typeof f.call !== 'function') fail;",
    );
    let constructor = "f.constructor(side = 2);";
    let start = constructor.find('(').unwrap();
    assert_eq!(
        run(&mut realm, constructor),
        Err(EngineFault::UnsupportedIntrinsicCall {
            intrinsic: IntrinsicCall::Function,
            span: Span {
                start,
                end: constructor.len() - 1
            },
        })
    );
    normal(
        &mut realm,
        "if (side !== 2 || proto(side = 3) !== void 0 || side !== 3) fail;",
    );
    let noncallable = "f.prototype(side = 4);";
    assert_eq!(
        run(&mut realm, noncallable),
        Ok(Completion::Throw(Throw::TypeError {
            span: Span {
                start: noncallable.find('(').unwrap(),
                end: noncallable.len() - 1
            },
        }))
    );
    normal(
        &mut realm,
        "if (side !== 4) fail; proto.shared = f; if (f.shared(1) !== void 0) fail;",
    );
    for (source, cause) in [
        ("f.caller;", PropertyFault::RestrictedAccessor),
        ("f.__proto__ = (side = 5);", PropertyFault::AnnexB),
    ] {
        assert_eq!(
            run(&mut realm, source),
            Err(EngineFault::UnsupportedProperty {
                span: dot(source),
                cause
            })
        );
    }
    normal(&mut realm, "if (side !== 5) fail;");
}

#[test]
fn put_checks_nullish_readonly_and_reference_mode_after_rhs() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "function f(a){} var side = 0; f.length = (side = 1, 'ignored'); if (side !== 1 || f.length !== 1) fail;",
    );
    normal(
        &mut realm,
        "if (('💡'.length = (side = 2, 9)) !== 9 || '💡'.length !== 2 || side !== 2) fail;",
    );
    for (source, effect) in [
        ("void 0; null.p = (side = 3);", 3),
        ("'use strict'; f.name = (side = 4);", 4),
        ("'use strict'; 'abc'.length = (side = 5);", 5),
        ("(void 0).p = (side = 6);", 6),
    ] {
        assert_eq!(
            run(&mut realm, source),
            Ok(Completion::Throw(Throw::TypeError { span: dot(source) }))
        );
        normal(&mut realm, &format!("if (side !== {effect}) fail;"));
    }
    let source = "null.p = pending; let pending;";
    let start = source.find("pending").unwrap();
    assert_eq!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: Span {
                start,
                end: start + 7
            }
        }))
    );
    assert_eq!(
        run(&mut realm, "(1).length;"),
        Err(EngineFault::Evaluation(
            EvalError::UnsupportedPropertyReceiver
        ))
    );
    normal(
        &mut realm,
        "function strict(){'use strict'; f.name = (side = 7);} function sloppy(){f.name = (side = 8);} ",
    );
    assert!(matches!(
        run(&mut realm, "strict();"),
        Ok(Completion::Throw(Throw::TypeError { .. }))
    ));
    normal(
        &mut realm,
        "'use strict'; if (side !== 7) fail; sloppy(); if (side !== 8 || f.name !== 'f') fail;",
    );
}

#[test]
fn object_to_primitive_is_unsupported_and_does_not_change_function_faults() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "function f(){} var o = f.prototype; var x = o; if (x !== o || x != o || o == f || o == void 0 || !o) fail;",
    );
    for source in [
        "1 + o;", "o == 1;", "o < 1;", "o * 1;", "`x${o}`;", "+o;", "x++;", "x += 1;",
    ] {
        assert_eq!(
            run(&mut realm, source),
            Err(EngineFault::UnsupportedObjectConversion),
            "{source}"
        );
    }
    assert_eq!(
        run(&mut realm, "1 + f;"),
        Err(EngineFault::Evaluation(
            EvalError::UnsupportedCallableConversion
        ))
    );
    normal(&mut realm, "if (x !== o) fail;");
}

#[test]
fn shared_utf16_copy_boundary_is_atomic_for_key_and_value() {
    fn setup() -> Realm {
        let mut realm = Realm::new();
        let piece = "a".repeat(4094);
        normal(
            &mut realm,
            &format!("function f(){{}} function piece(){{return '{piece}';}} var side = 0, sink;"),
        );
        realm
    }
    let repeated = "sink = piece();".repeat(4);
    let mut realm = setup();
    normal(
        &mut realm,
        &format!("{repeated}f.new = (side = 1, 'abcde');"),
    );
    normal(&mut realm, "if (side !== 1 || f.new !== 'abcde') fail;");
    let mut realm = setup();
    let over = format!("{repeated}f.new = (side = 2, 'abcdef');");
    assert_eq!(
        run(&mut realm, &over),
        Err(EngineFault::StringCopyLimitExceeded { span: dot(&over) })
    );
    normal(&mut realm, "if (side !== 2 || f.new !== void 0) fail;");
    // 16,376 from the unsuccessful execution plus 16,384 now succeed;
    // exactly eight units remain, independently of the per-execution credit.
    normal(&mut realm, &format!("{repeated}f.new = 'abcde';"));
    normal(&mut realm, "f.other = 'abc';");
    let extra = "f.new = 'x';";
    assert_eq!(
        run(&mut realm, extra),
        Err(EngineFault::StringRetentionLimitExceeded { span: dot(extra) })
    );
    normal(
        &mut realm,
        "if (f.new !== 'abcde' || f.other !== 'abc') fail;",
    );
}

#[test]
fn pending_and_entered_receivers_reduce_the_exact_live_frontier() {
    let mut realm = Realm::new();
    let args = |n| vec!["1"; n].join(",");
    let formals = (0..63)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    normal(
        &mut realm,
        &format!(
            "function sink(){{return 1;}} function wide(n,{formals}){{if (!n) return 0; return sink({},wide(n-1));}} sink.m = sink; wide.m = wide;",
            args(62)
        ),
    );
    for template in [0, 1, 2] {
        // The old bare path reserves 4096 at 28 leading args. Each of these
        // introduces exactly one live packet or raw receiver at that frontier.
        let source = |n| match template {
            0 => format!("sink.m({},wide(31));", args(n)),
            1 => format!("sink({},wide.m(31));", args(n)),
            _ => format!("sink.m = sink({},wide(31));", args(n)),
        };
        normal(&mut realm, &source(27));
        normal(&mut realm, "sink.m = sink;");
        assert_eq!(
            run(&mut realm, &source(28)),
            Err(EngineFault::Evaluation(EvalError::LiveSlotLimitExceeded))
        );
    }
}

#[test]
fn member_call_argument_limit_does_not_change_after_lowering() {
    let mut realm = Realm::new();
    normal(&mut realm, "function f(){} f.m = f;");
    let args = |n| vec!["1"; n].join(",");
    normal(&mut realm, &format!("f.m({});", args(64)));
    assert!(matches!(
        CompiledScript::compile(&format!("f.m({});", args(65))),
        Err(CompileError::LimitExceeded {
            limit: CompileLimit::Arguments,
            ..
        })
    ));
}

#[test]
fn property_work_is_shared_across_retained_calls_and_dispatch_is_unchanged() {
    let mut realm = Realm::new();
    let key = "x".repeat(256);
    normal(
        &mut realm,
        &format!(
            "function f(){{}} function hit(){{return f.{key};}} function countdown(n){{while(n){{n -= 1;}} return n;}} f.{key} = 1;"
        ),
    );
    // 508 * (1 visited node + 1 row + 256 key comparisons) = 131064.
    normal(&mut realm, "for(var i = 0; i < 508; i++) { hit(); }");
    assert_eq!(
        run(&mut realm, "for(i = 0; i < 509; i++) { hit(); }"),
        Err(EngineFault::PropertyWorkLimitExceeded)
    );
    normal(&mut realm, "if (i !== 508) fail;");
    normal(&mut realm, "void f.z;countdown(2339);");
    assert_eq!(
        run(&mut realm, "void void f.z;countdown(2339);"),
        Err(EngineFault::Evaluation(EvalError::DispatchLimitExceeded))
    );
}

#[test]
fn own_and_realm_row_frontiers_allow_replacement_but_not_insertion() {
    use std::fmt::Write;
    use swayze_js::realm::InstantiateError;
    let mut realm = Realm::new();
    let mut declarations = String::new();
    for index in 0..64 {
        write!(declarations, "function f{index}(){{}}").unwrap();
    }
    normal(&mut realm, &declarations);
    // 27 + 4*64 initial rows; 62*61 + 31 insertions consume the remainder.
    for owner in 0..63 {
        let mut source = String::new();
        for key in 0..if owner < 62 { 61 } else { 31 } {
            write!(source, "f{owner}.p{key} = 1;").unwrap();
        }
        normal(&mut realm, &source);
        if owner == 0 {
            let overflow = "f0.overflow = 2;";
            assert_eq!(
                run(&mut realm, overflow),
                Err(EngineFault::PropertyLimitExceeded {
                    span: dot(overflow)
                })
            );
        }
    }
    normal(&mut realm, "f62.p30 = 7; if (f62.p30 !== 7) fail;");
    let overflow = "f62.overflow = 2;";
    assert_eq!(
        run(&mut realm, overflow),
        Err(EngineFault::PropertyLimitExceeded {
            span: dot(overflow)
        })
    );
    assert_eq!(
        run(&mut realm, "(function(){});"),
        Err(EngineFault::FunctionShapeLimitExceeded)
    );
    let rejected = CompiledScript::compile("var poisoned; function f0(){} let rejected;").unwrap();
    assert_eq!(
        realm.instantiate(&rejected).unwrap_err(),
        InstantiateError::PropertyLimitExceeded
    );
    normal(
        &mut realm,
        "if (f0.p0 !== 1 || typeof poisoned !== 'undefined') fail; let rejected = 7;",
    );
}
