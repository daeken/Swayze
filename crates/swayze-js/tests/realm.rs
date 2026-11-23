use swayze_js::{
    ErrorKind, EvalError, Span, parse_script, parse_script_for_completion,
    realm::{
        CompileError, CompileLimit, CompiledScript, Completion, EngineFault, InstantiateError,
        Realm, Throw,
    },
};

fn run(realm: &mut Realm, source: &str) -> Result<Completion, EngineFault> {
    let script = CompiledScript::compile(source).unwrap();
    realm.instantiate(&script).unwrap().execute()
}

fn normal(realm: &mut Realm, source: &str) {
    assert_eq!(run(realm, source), Ok(Completion::Normal), "{source}");
}

fn span(source: &str, name: &str) -> Span {
    let start = source.rfind(name).unwrap();
    Span {
        start,
        end: start + name.len(),
    }
}

fn reference(realm: &mut Realm, source: &str, name: &str) {
    assert_eq!(
        run(realm, source),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: span(source, name)
        })),
        "{source}"
    );
}

fn immutable(realm: &mut Realm, source: &str, name: &str) {
    assert_eq!(
        run(realm, source),
        Ok(Completion::Throw(Throw::TypeError {
            span: span(source, name)
        })),
        "{source}"
    );
}

#[test]
fn detached_code_relinks_and_scalar_state_owns_its_names() {
    let consumer = CompiledScript::compile("if (typeof n === 'number') { n += +`2`; }").unwrap();
    let mut left = Realm::new();
    let mut right = Realm::new();
    assert_eq!(
        left.instantiate(&consumer).unwrap().execute(),
        Ok(Completion::Normal)
    );
    let source = String::from("let n = 40, b = false, z = null, u, neg = -0, nan = 0 / 0;");
    let producer = CompiledScript::compile(&source).unwrap();
    drop(source);
    assert_eq!(
        left.instantiate(&producer).unwrap().execute(),
        Ok(Completion::Normal)
    );
    assert_eq!(
        right.instantiate(&producer).unwrap().execute(),
        Ok(Completion::Normal)
    );
    drop(producer);
    assert_eq!(
        left.instantiate(&consumer).unwrap().execute(),
        Ok(Completion::Normal)
    );
    normal(
        &mut left,
        "if (!(n === 42 && b === false && z === null && u === undefined && 1 / neg < 0 && nan !== nan)) { bad; }",
    );
    normal(&mut right, "if (n !== 40) { bad; }");
    reference(&mut Realm::new(), "n;", "n");
}

#[test]
fn syntax_conflicts_are_phase_specific_and_atomic() {
    let source = "let twice = 1; const twice = 2;";
    assert_eq!(
        CompiledScript::compile(source).unwrap_err(),
        CompileError::SyntaxError {
            span: span(source, "twice")
        }
    );
    let mut realm = Realm::new();
    normal(&mut realm, "let old = 2;");
    for (source, conflict) in [
        ("let skipped = 3, old = 4;", "old"),
        ("let skipped = 3, undefined = 4;", "undefined"),
    ] {
        let script = CompiledScript::compile(source).unwrap();
        assert_eq!(
            realm.instantiate(&script).unwrap_err(),
            InstantiateError::SyntaxError {
                span: span(source, conflict)
            }
        );
        normal(
            &mut realm,
            "if (old !== 2 || typeof skipped !== 'undefined') { bad; }",
        );
        reference(&mut realm, "skipped;", "skipped");
    }
    normal(&mut realm, "let skipped = 5;");
}

#[test]
fn tdz_is_installed_before_the_body_and_survives_abandonment_and_throw() {
    let mut realm = Realm::new();
    let abandoned = CompiledScript::compile("let abandoned = 1;").unwrap();
    drop(realm.instantiate(&abandoned).unwrap());
    assert!(matches!(
        realm.instantiate(&abandoned),
        Err(InstantiateError::SyntaxError { .. })
    ));
    reference(&mut realm, "typeof abandoned;", "abandoned");
    reference(&mut realm, "abandoned = 7;", "abandoned");
    reference(&mut realm, "let a = 1, self = self, later = 5;", "self");
    reference(&mut realm, "later;", "later");
    normal(&mut realm, "if (a !== 1) { bad; }");
    assert_eq!(
        run(&mut realm, "forward = 2; let forward = 3;"),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: Span { start: 0, end: 7 }
        })),
    );
    reference(&mut realm, "typeof forward;", "forward");
    normal(
        &mut realm,
        "false && forward; true || abandoned; if (false) { later = foo; }",
    );
    normal(
        &mut realm,
        "if (typeof ((noSuch)) !== 'undefined') { bad; }",
    );
    reference(&mut realm, "typeof (0, noSuch);", "noSuch");
}

#[test]
fn errors_only_happen_on_reached_operations_and_keep_prior_effects() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "let effect = 0; const fixed = 1; const empty = 0;",
    );
    normal(
        &mut realm,
        "if (false) { fixed = missing; missing = 3; } fixed ||= missing; empty &&= missing; fixed ??= missing;",
    );
    reference(&mut realm, "fixed = missing;", "missing");
    immutable(&mut realm, "fixed = (effect = 3);", "fixed");
    normal(&mut realm, "if (effect !== 3) { bad; }");
    immutable(&mut realm, "fixed += (effect = 4);", "fixed");
    normal(&mut realm, "if (effect !== 4) { bad; }");
    immutable(&mut realm, "if (++fixed) { bad; }", "fixed");
    immutable(&mut realm, "fixed++;", "fixed");
    immutable(&mut realm, "empty ||= 1;", "empty");
    immutable(&mut realm, "fixed = 'does not escape';", "fixed");
    reference(&mut realm, "missing += (effect = 9);", "missing");
    normal(&mut realm, "if (effect !== 4 || fixed !== 1) { bad; }");
    let source = "missing = (effect = 5);";
    assert_eq!(
        run(&mut realm, source),
        Err(EngineFault::ImplicitGlobal {
            span: span(source, "missing")
        })
    );
    normal(
        &mut realm,
        "if (effect !== 5 || typeof missing !== 'undefined') { bad; }",
    );
    normal(&mut realm, "if (effect++ !== 5 || ++effect !== 7) { bad; }");
}

#[test]
fn readonly_undefined_ignores_writes_not_rhs_and_preserves_update_values() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "let effect = 0; undefined = (effect = 1); undefined = `a${2}`; undefined += 'text';",
    );
    reference(&mut realm, "undefined = missing;", "missing");
    assert_eq!(
        run(&mut realm, "undefined *= (1).length;"),
        Err(EngineFault::Evaluation(
            EvalError::UnsupportedPropertyReceiver
        ))
    );
    normal(
        &mut realm,
        "if (undefined !== void 0 || effect !== 1 || (undefined = 8) !== 8) { bad; }",
    );
    normal(
        &mut realm,
        "let pre = ++undefined, post = undefined++; if (undefined !== void 0 || pre === pre || post === post) { bad; }",
    );
    normal(
        &mut realm,
        "undefined &&= missing; undefined ??= (effect = 2); if (undefined !== void 0 || effect !== 2) { bad; }",
    );
}

#[test]
fn local_strings_do_not_escape_and_engine_stops_preserve_earlier_state() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r"let x = +('4' + '1'), y = '\uD800'.length; if (x !== 41 || y !== 1) { bad; }",
    );
    normal(&mut realm, "'a' + 'b'; false && (x = 'unreached');");
    let source = "y = 2; x = `a${y}`;";
    assert_eq!(
        run(&mut realm, source),
        Err(EngineFault::PersistentString {
            span: span(source, "x")
        })
    );
    normal(&mut realm, "if (x !== 41 || y !== 2) { bad; }");
    let source = "let text = typeof x, next = 9;";
    assert_eq!(
        run(&mut realm, source),
        Err(EngineFault::PersistentString {
            span: span(source, "text")
        })
    );
    reference(&mut realm, "text;", "text");
    reference(&mut realm, "next;", "next");
    assert_eq!(
        run(&mut realm, "x = 42; while (true) {}"),
        Err(EngineFault::Evaluation(EvalError::DispatchLimitExceeded))
    );
    normal(&mut realm, "if (x !== 42) { bad; }");
}

#[test]
fn policies_do_not_reclassify_legacy_diagnostics_or_accept_deferred_syntax() {
    for (source, diagnostic, site) in [
        (
            "if (false) { missing; } 0",
            "unbound name is outside the subset",
            "missing",
        ),
        (
            "let outer = 1; { outer; let outer = 2; } 0",
            "access before lexical binding initialization is outside the subset",
            "outer",
        ),
        (
            "const a = 1; if (false) { a = 2; } 0",
            "assignment to const is outside the subset",
            "=",
        ),
    ] {
        let at = if source.starts_with("let outer") {
            source.find("{ outer").unwrap() + 2
        } else {
            span(source, site).start
        };
        let expected = Span {
            start: at,
            end: at + site.len(),
        };
        let first = parse_script(source).unwrap_err();
        assert_eq!(
            (first.kind(), first.span(), first.message()),
            (ErrorKind::Unsupported, expected, diagnostic)
        );
        assert_eq!(parse_script_for_completion(source).unwrap_err(), first);
    }
    for source in [
        "var x = 1;",
        "'use strict';",
        "{ let x = 1; }",
        "for (let x = 1; x;) {}",
        "if (false) { const x = 1; }",
        "globalThis.undefined;",
        "1",
    ] {
        let CompileError::Parse(error) = CompiledScript::compile(source).unwrap_err() else {
            panic!("{source}");
        };
        assert_eq!(error.kind(), ErrorKind::Unsupported, "{source}");
    }
    assert_eq!(
        parse_script("{ let x = 1; } 2").unwrap().evaluate(),
        Ok(2.0)
    );
}

#[test]
fn compile_limits_are_inclusive() {
    let long = "a".repeat(256);
    assert!(CompiledScript::compile(&format!("{long};")).is_ok());
    assert!(matches!(
        CompiledScript::compile(&format!("{long}a;")),
        Err(CompileError::LimitExceeded {
            limit: CompileLimit::NameBytes,
            ..
        })
    ));
    let exact = format!("{long};").repeat(64);
    assert!(CompiledScript::compile(&exact).is_ok());
    assert!(matches!(
        CompiledScript::compile(&(exact + "a;")),
        Err(CompileError::LimitExceeded {
            limit: CompileLimit::PooledNameBytes,
            ..
        })
    ));
    assert!(CompiledScript::compile(&"a++;".repeat(1024)).is_ok());
    assert!(matches!(
        CompiledScript::compile(&"a++;".repeat(1025)),
        Err(CompileError::LimitExceeded {
            limit: CompileLimit::Instructions,
            ..
        })
    ));
    let mut source = (0..256).map(|n| format!("let a{n};")).collect::<String>();
    assert!(CompiledScript::compile(&source).is_ok());
    source.push_str("let b;");
    assert!(matches!(
        CompiledScript::compile(&source),
        Err(CompileError::LimitExceeded {
            limit: CompileLimit::Declarations,
            ..
        })
    ));
}

#[test]
fn full_realm_and_expensive_linking_have_separate_backpressure() {
    let mut realm = Realm::new();
    for prefix in ["a", "bbb", "ccccc", "ddddddd"] {
        let source = (0..128)
            .map(|n| format!("let {prefix}{n};"))
            .collect::<String>();
        normal(&mut realm, &source);
    }
    let conflict = CompiledScript::compile("let fresh, a1;").unwrap();
    assert!(matches!(
        realm.instantiate(&conflict),
        Err(InstantiateError::SyntaxError { .. })
    ));
    let full = CompiledScript::compile("let fresh;").unwrap();
    assert_eq!(
        realm.instantiate(&full).unwrap_err(),
        InstantiateError::BindingLimitExceeded
    );
    let expensive = CompiledScript::compile(&"missing;".repeat(300)).unwrap();
    assert_eq!(
        realm.instantiate(&expensive).unwrap_err(),
        InstantiateError::WorkLimitExceeded
    );
    reference(&mut realm, "fresh;", "fresh");
    normal(
        &mut realm,
        "if (a0 !== undefined || ddddddd127 !== undefined) { bad; }",
    );
}
