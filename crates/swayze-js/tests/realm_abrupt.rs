use swayze_js::{
    ErrorKind, EvalError, ParseErrorType, Span,
    realm::{
        CompileError, CompiledScript, Completion, EngineFault, InstantiateError, IntrinsicCall,
        PropertyFault, Realm, Throw,
    },
};

fn run(realm: &mut Realm, source: &str) -> Result<Completion, EngineFault> {
    let code =
        CompiledScript::compile(source).unwrap_or_else(|err| panic!("compile {err:?}: {source}"));
    realm
        .instantiate(&code)
        .unwrap_or_else(|err| panic!("instantiate {err:?}: {source}"))
        .execute()
}

fn normal(realm: &mut Realm, source: &str) {
    assert_eq!(run(realm, source), Ok(Completion::Normal), "{source}");
}

fn span(source: &str, text: &str) -> Span {
    let start = source.find(text).unwrap();
    Span {
        start,
        end: start + text.len(),
    }
}

#[derive(Clone, Copy)]
enum Automatic {
    Reference,
    Type,
}

impl Automatic {
    fn name(self) -> &'static str {
        match self {
            Self::Reference => "ReferenceError",
            Self::Type => "TypeError",
        }
    }

    fn diagnostic(self, span: Span) -> Throw {
        match self {
            Self::Reference => Throw::ReferenceError { span },
            Self::Type => Throw::TypeError { span },
        }
    }
}

#[test]
fn automatic_producers_preserve_order_and_uncaught_spans() {
    use Automatic::{Reference, Type};

    // The same producer is tested with and without a target. The state probe
    // is a separate Script so its failure cannot be caught by the tested guard.
    for (strict, op, suffix, site, kind, effect) in [
        (false, "throw absent;", "", "absent", Reference, 0),
        (false, "absent(side=9);", "", "absent", Reference, 0),
        (
            false,
            "out=typeof later;",
            "let later;",
            "later",
            Reference,
            0,
        ),
        (true, "absent=(side=1);", "", "absent", Reference, 1),
        (
            false,
            "later=(side=1);",
            "let later;",
            "later",
            Reference,
            1,
        ),
        (
            false,
            "out=zero(side=1,absent,side=9);",
            "",
            "absent",
            Reference,
            1,
        ),
        (false, "null.p=absent;", "", "absent", Reference, 0),
        (false, "fixed=(side=1);", "", "fixed", Type, 1),
        (true, "undefined=(side=1);", "", "undefined", Type, 1),
        (false, "out=null.p;", "", ".", Type, 0),
        (false, "out=null.p(side=9);", "", ".", Type, 0),
        (false, "(void 0).p=(side=1);", "", ".", Type, 1),
        (true, "f.name=(side=1);", "", ".", Type, 1),
        (
            true,
            "f.constructor.prototype=(side=1);",
            "",
            ".prototype",
            Type,
            1,
        ),
        (true, "'abc'.length=(side=1);", "", ".", Type, 1),
        (
            false,
            "out=zero(side=1,side=side*10+2);",
            "",
            "(side=1,side=side*10+2)",
            Type,
            12,
        ),
    ] {
        let prefix = if strict { "'use strict'; " } else { "" };
        for caught in [false, true] {
            let mut realm = Realm::new();
            normal(
                &mut realm,
                "var side=0,out=77,saved,hits=0,tail=0,zero=0; const fixed=1; function f(a){}",
            );
            let source = if caught {
                format!("{prefix}try {{ {op} tail=1; }} catch(e) {{ saved=e; hits++; }} {suffix}")
            } else {
                format!("{prefix}{op} tail=1; {suffix}")
            };
            let mut origin = span(&source, site);
            if site == ".prototype" {
                origin.end = origin.start + 1; // Only the failing dot, not the property key.
            }
            let expected = if caught {
                Completion::Normal
            } else {
                Completion::Throw(kind.diagnostic(origin))
            };
            assert_eq!(run(&mut realm, &source), Ok(expected), "{source}");
            let value = if caught {
                format!(
                    "hits !== 1 || typeof saved !== 'object' || saved === null || saved.name !== '{}'",
                    kind.name()
                )
            } else {
                "hits !== 0 || saved !== void 0".to_owned()
            };
            normal(
                &mut realm,
                &format!(
                    "if ({value} || side !== {effect} || tail !== 0 || out !== 77 || zero !== 0 || fixed !== 1 || undefined !== void 0 || f.name !== 'f' || 'abc'.length !== 3) bad;"
                ),
            );
        }
    }

    let mut realm = Realm::new();
    normal(&mut realm, "var side=0,hits=0,tail=0; function f(a){}");
    normal(
        &mut realm,
        "try { undefined=(side=1); f.name=(side=side*10+2); 'abc'.length=(side=side*10+3); tail=1; } catch(e) { hits++; }",
    );
    normal(
        &mut realm,
        "if (side !== 123 || hits !== 0 || tail !== 1 || undefined !== void 0 || f.name !== 'f' || 'abc'.length !== 3) bad;",
    );
}

#[test]
fn automatic_objects_have_fresh_identity_and_real_shared_prototypes() {
    // Both first-use orders: create Error roots before any user function, or
    // add them to a Realm that has already installed callable roots.
    for callable_first in [false, true] {
        let mut realm = Realm::new();
        let producer = "function fault(k){'use strict'; if(k) undefined=(side+=1); else absent;}\n                        function quiet(){undefined=(side+=10);}";
        if callable_first {
            normal(&mut realm, producer);
        }
        normal(
            &mut realm,
            "var side=0,first,r1,r2,t1,t2,alias,again,rp,tp,ReferenceError=17,TypeError=23;\n                            try { absent; } catch(e) { first=e; }",
        );
        if !callable_first {
            normal(&mut realm, producer);
        }
        // The producer's CompiledScript is dropped by run. Its strict write
        // must still throw when called by sloppy code; the converse is below.
        normal(
            &mut realm,
            "try { fault(0); } catch(e) { r1=e; }\n                            try { fault(1); } catch(e) { t1=e; }\n                            try { fault(1); } catch(e) { t2=e; undefined=(side+=2); }\n                            try { throw r1; } catch(e) { alias=e; }",
        );
        normal(
            &mut realm,
            "if (side !== 4 || first === r1 || r1 !== alias || t1 === t2 || r1 === t1 ||\n                                first.name !== 'ReferenceError' || r1.name !== 'ReferenceError' ||\n                                t1.name !== 'TypeError' || t2.name !== 'TypeError' ||\n                                first.message !== '' || t1.message !== '' ||\n                                first.constructor !== r1.constructor || t1.constructor !== t2.constructor ||\n                                r1.constructor === t1.constructor ||\n                                r1.constructor.name !== 'ReferenceError' || t1.constructor.name !== 'TypeError' ||\n                                ReferenceError !== 17 || TypeError !== 23) bad;\n                            rp=r1.constructor.prototype; tp=t1.constructor.prototype;\n                            if (rp === tp || rp.constructor !== r1.constructor || tp.constructor !== t1.constructor) bad;",
        );
        normal(
            &mut realm,
            "rp.marker=42; rp.name='renamed'; r1.name='own';\n                            try { fault(0); } catch(e) { r2=e; }\n                            try { throw r2; } catch(e) { again=e; }",
        );
        normal(
            &mut realm,
            "if (r2 === first || r2 === r1 || r2 !== again || r2.constructor !== first.constructor ||\n                                first.name !== 'renamed' || r2.name !== 'renamed' || alias.name !== 'own' ||\n                                first.marker !== 42 || r2.marker !== 42 || r1.marker !== 42 ||\n                                t1.name !== 'TypeError' || t2.marker !== void 0 || tp.marker !== void 0) bad;",
        );
        normal(
            &mut realm,
            "'use strict'; quiet(); if (side !== 14 || undefined !== void 0) bad;",
        );
    }
}

#[test]
fn cross_frame_catch_preserves_outer_call_and_put_but_discards_inner_packets() {
    let mut realm = Realm::new();
    normal(&mut realm, r"function boom(){throw '\uD800'+'R';}");
    normal(
        &mut realm,
        "function mid(x){\n        try { x++; mid.slot=mid.go(side=1,boom(),side=9); }\n        catch(x) { saved=x; x+='!'; mutated=x; }\n        return x;\n    }",
    );
    normal(
        &mut realm,
        "function holder(){} function old(a,b){return a*100+b;} function replacement(){return -1;}\n        var side=0,saved,mutated,host=holder,alias=host,outerHits=0;\n        host.go=old; mid.go=old; mid.slot=3;\n        try { host.out=host.go((host.go=replacement,host=replacement,7),mid(41)); }\n        catch(e) { outerHits++; }",
    );
    normal(
        &mut realm,
        r"if (alias.out !== 742 || host !== replacement || host.out !== void 0 ||
        alias.go !== replacement || mid.slot !== 3 || side !== 1 || outerHits !== 0 ||
        saved !== '\uD800R' || saved.length !== 2 || mutated !== '\uD800R!' || mutated.length !== 3) bad;",
    );
}

#[test]
fn explicit_values_preserve_identity_and_nested_catch_shadowing() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r"function callable(){return 19;}
        var e=90, seen, after, once=0, saved, old, result, post, changed, neg, nan, flag, nil;
        try { seen=e; throw (once+=1, callable); }
        catch(e) {
            old=e;
            try { throw /* no line terminator */ ('\uDC00'
                + 'Q'); }
            catch(e) { saved=e; }
            result=e();
            e=8;
            post=e++;
            changed=e;
        }
        after=e;
        try { throw -0; } catch(e) { neg=e; }
        try { throw 0/0; } catch(e) { nan=e; }
        try { throw false; } catch(e) { flag=e; }
        try { throw null; } catch(e) { nil=e; }",
    );
    normal(
        &mut realm,
        r"if (once !== 1 || seen !== 90 || after !== 90 || e !== 90 || old !== callable ||
        result !== 19 || post !== 8 || changed !== 9 || nan === nan || 1/neg !== -1/0 ||
        flag !== false || nil !== null ||
        saved !== '\uDC00Q' || saved.length !== 2) bad;",
    );
    // A Realm-owned string can itself travel back through an explicit packet.
    normal(
        &mut realm,
        "try { throw saved; } catch(e) { old=e; } if (old !== saved) bad;",
    );
}

#[test]
fn outward_break_continue_and_return_disarm_only_exited_regions() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "var i,trace=0,after=0,outerValue,side=0,hits=0,tail=0;
        try {
            outer: for(i=0;i<4;i++){
                step: {
                    try {
                        if(i===0) continue outer;
                        if(i===2) break step;
                        throw i;
                    } catch(e) {
                        trace=trace*10+e;
                        if(i===1) continue outer;
                        break step;
                    }
                    after=99;
                }
                after=after*10+i;
            }
            while(true){break;}
            throw 8;
        } catch(e) { outerValue=e; }
        function keep(x){try { return x; } catch(e) { hits+=100; } tail=1;}
        function raiser(){throw (side+=1,void 0);}
        function recover(){
            try { return raiser(); }
            catch(x) { hits++; if (typeof x === 'undefined') return 41; }
            tail=2;
        }
        function fallthrough(){try { side+=2; } catch(e) { hits+=100; }}
        var kept=keep(13), recovered=recover(), fell=fallthrough();",
    );
    normal(&mut realm, "if (i !== 4 || trace !== 13 || after !== 23 || outerValue !== 8 ||
        kept !== 13 || recovered !== 41 || fell !== void 0 || hits !== 1 || tail !== 0 || side !== 3) bad;");
}

#[test]
fn catch_binding_does_not_link_or_hoist_but_nonconflicting_var_does() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "var seen,skipped=0,side=0;
        try { side=1; }
        catch(hidden) { skipped++; var hoisted=99; }
        try { try { hidden; } catch(other) { seen=other.name; } throw 2; }
        catch(hidden) { 'use strict'; undefined=(side=hidden); hidden+=3; skipped=hidden; }",
    );
    // The direct read itself must succeed for hoisted, and fail for the catch name.
    normal(
        &mut realm,
        "if (hoisted !== void 0 || seen !== 'ReferenceError' || side !== 2 || skipped !== 5) bad;",
    );
    let source = "hidden;";
    assert_eq!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: span(source, "hidden")
        }))
    );
    normal(&mut realm, "let hidden=7; if (hidden !== 7) bad;");
}

#[test]
fn outcome_uses_issuing_site_not_the_call_or_original_rethrow_site() {
    let mut realm = Realm::new();
    let producer = "function raise(){ /* retained explicit producer */ throw 'private'; }
                    function miss(){ /* retained automatic producer */ null.x; }";
    normal(&mut realm, producer);
    assert_eq!(
        run(&mut realm, "raise();"),
        Ok(Completion::Throw(Throw::Explicit {
            span: span(producer, "throw")
        }))
    );
    assert_eq!(
        run(&mut realm, "miss();"),
        Ok(Completion::Throw(Throw::TypeError {
            span: span(producer, ".")
        }))
    );
    // The inner call's throw wins over the explicit statement awaiting its result.
    assert_eq!(
        run(&mut realm, "throw raise();"),
        Ok(Completion::Throw(Throw::Explicit {
            span: span(producer, "throw")
        }))
    );
    let source = "try { absent; } catch(e) { e.name='changed'; throw e; }";
    assert_eq!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::Explicit {
            span: span(source, "throw")
        }))
    );
    let source = "try { throw 9; } catch(e) {} absent;";
    assert_eq!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: span(source, "absent")
        }))
    );
}

#[test]
fn compile_and_instantiation_failures_do_not_enter_catch() {
    let mut realm = Realm::new();
    normal(&mut realm, "let old=2; var side=0;");
    for (source, token) in [
        ("try { side=1; } catch(42) { side=9; }", "42"),
        ("try { throw; } catch(e) { side=9; }", ";"),
        ("try { throw /*\n*/ 4; } catch(e) { side=9; }", "4"),
    ] {
        let mut expected = span(source, token);
        if token == "4" {
            expected.end = expected.start; // A restricted line break points at the next token boundary.
        }
        let CompileError::Parse(error) = CompiledScript::compile(source).unwrap_err() else {
            panic!("expected a parser diagnostic: {source}");
        };
        assert_eq!(
            (error.kind(), error.parse_error_type(), error.span()),
            (
                ErrorKind::Malformed,
                Some(ParseErrorType::SyntaxError),
                expected
            ),
            "{source}"
        );
    }
    let source = "'use strict'; try { side=1; } catch(eval) { side=9; }";
    assert_eq!(
        CompiledScript::compile(source).unwrap_err(),
        CompileError::SyntaxError {
            span: span(source, "eval")
        }
    );

    let source = "try { side=1; } catch(e) { side=9; } let fresh=3,old=4;";
    let code = CompiledScript::compile(source).unwrap();
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::SyntaxError {
            span: span(source, "old")
        }
    );
    normal(&mut realm, "let fresh=5; if (side !== 0 || old !== 2) bad;");
}

#[test]
fn engine_stops_bypass_catches_and_leave_the_same_realm_reusable() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "var side=0,before=0,hits=0,tail=0,ref,ty; function f(){}
        try { absent; } catch(e) { ref=e; }
        try { null.p; } catch(e) { ty=e; }",
    );
    for (op, site, side, expected) in [
        ("newName=(side=1);", "newName", 1, None),
        ("f.call(side=9);", ".", 0, None),
        (
            "f.constructor(side=1);",
            "(side=1)",
            1,
            Some(IntrinsicCall::Function),
        ),
        (
            "ref.constructor(side=1);",
            "(side=1)",
            1,
            Some(IntrinsicCall::ReferenceError),
        ),
        (
            "ty.constructor(side=1);",
            "(side=1)",
            1,
            Some(IntrinsicCall::TypeError),
        ),
        ("while(true){}", "while", 0, None),
    ] {
        let source = format!(
            "side=0; before=0; try {{ try {{ before=1; {op} tail=1; }} catch(e) {{ hits+=1; }} }} catch(e) {{ hits+=10; }}"
        );
        let span = span(&source, site);
        let fault = match (expected, site) {
            (Some(intrinsic), _) => EngineFault::UnsupportedIntrinsicCall { span, intrinsic },
            (None, ".") => EngineFault::UnsupportedProperty {
                span,
                cause: PropertyFault::Builtin,
            },
            (None, "while") => EngineFault::Evaluation(EvalError::DispatchLimitExceeded),
            (None, _) => EngineFault::ImplicitGlobal { span },
        };
        assert_eq!(run(&mut realm, &source), Err(fault), "{source}");
        normal(
            &mut realm,
            &format!(
                "if (before !== 1 || side !== {side} || hits !== 0 || tail !== 0 || typeof newName !== 'undefined') bad;"
            ),
        );
        normal(
            &mut realm,
            "try { throw 7; } catch(e) { side=e; } if (side !== 7) bad;",
        );
    }
}

#[test]
fn untargeted_errors_do_not_consume_heap_and_caught_errors_stop_at_capacity() {
    // No functions or retaining artifacts: the exact heap has 1280 slots,
    // of which the base and Error packs use 4 + 6. Each caught error uses one.
    const ERROR_SLOTS: usize = 1280 - 4 - 6;
    let mut realm = Realm::new();
    normal(&mut realm, "var n=0,hits=0,tail=0;");
    for source in [
        "absent;",
        "try {} catch(e) { hits++; } absent;",
        "try { throw 1; } catch(e) { absent; hits++; }",
    ] {
        let code = CompiledScript::compile(source).unwrap();
        for _ in 0..16 {
            assert_eq!(
                realm.instantiate(&code).unwrap().execute(),
                Ok(Completion::Throw(Throw::ReferenceError {
                    span: span(source, "absent")
                })),
                "{source}"
            );
        }
    }
    let source = "try { n++; absent; tail=1; } catch(e) { hits++; }";
    let code = CompiledScript::compile(source).unwrap();
    for i in 0..ERROR_SLOTS {
        assert_eq!(
            realm.instantiate(&code).unwrap().execute(),
            Ok(Completion::Normal),
            "caught error {}",
            i + 1
        );
    }
    assert_eq!(
        realm.instantiate(&code).unwrap().execute(),
        Err(EngineFault::PropertyLimitExceeded {
            span: span(source, "absent")
        })
    );
    normal(
        &mut realm,
        "if (n !== 1271 || hits !== 1270 || tail !== 0) bad;",
    );
    assert_eq!(
        run(&mut realm, "absent;"),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: Span { start: 0, end: 6 }
        }))
    );
    normal(
        &mut realm,
        "try { throw 7; } catch(e) { tail=e; } if (tail !== 7 || n !== 1271 || hits !== 1270) bad;",
    );
}
