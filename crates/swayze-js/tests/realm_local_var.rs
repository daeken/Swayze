use swayze_js::{
    ErrorKind, EvalError, Limit, Span, parse_script_for_completion,
    realm::{CompileError, CompileLimit, CompiledScript, Completion, EngineFault, Realm, Throw},
};

fn run(realm: &mut Realm, source: &str) -> Result<Completion, EngineFault> {
    let code =
        CompiledScript::compile(source).unwrap_or_else(|error| panic!("{error:?}: {source}"));
    realm.instantiate(&code).unwrap().execute()
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

#[test]
fn whole_body_hoisting_covers_every_access_and_does_not_publish_names() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r#"
        const late = 90, dead = 91, counter = 92;
        var callback = 93;
        function id(x) { return x; }
        function f() {
            if (typeof late !== 'undefined' || dead !== void 0) bad;
            counter ??= 0;
            if (counter++ !== 0 || ++counter !== 2 || (counter += 3) !== 5) bad;
            counter ||= bad;
            if ((counter &&= 6) !== 6) bad;
            late = 3;
            callback = id;
            if (false) var dead = bad;
            if (true) var late, fresh; else var late = bad;
            { var combined = callback(late + counter); }
            if (fresh !== void 0 || typeof f !== 'undefined') bad;
            return combined;
            var f, counter, callback, afterReturn;
        }
        if (f() !== 9 || late !== 90 || dead !== 91 || counter !== 92 || callback !== 93) bad;
    "#,
    );
    normal(
        &mut realm,
        "let fresh, combined, afterReturn; if (typeof f !== 'function') bad;",
    );
}

#[test]
fn formals_and_var_share_slots_and_noop_reentry_is_not_a_write() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r#"
        var log = 0;
        function iterate(p, missing) {
            'use strict';
            if (tail !== void 0 || missing !== void 0) bad;
            var p;
            var undefined = 5;
            for (var i = 0, p = p + 1, l; i < 4; i++) {
                var p, i, l;
                if (i === 1) continue;
                if (i === 3) break;
                l ??= 10;
                p += i + l++;
            }
            var first = (log = log * 10 + 1), second = (log = log * 10 + 2, first + p);
            return p + second + undefined;
            var tail;
        }
        function self(self) { var self; return self; }
        function fresh(flag) { if (flag) var a = 7; return a; }
        function discardExtra(first) {
            var local;
            if (local !== void 0) bad;
            return first;
        }
        if (iterate(2) !== 58 || log !== 12 || self(3) !== 3) bad;
        if (fresh(true) !== 7 || fresh(false) !== void 0) bad;
        if (discardExtra(6, log += 1, log += 2) !== 6 || log !== 15) bad;
    "#,
    );
}

#[test]
fn self_initializer_reads_the_fresh_slot_or_supplied_formal() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r#"
        const x = 91;
        function shadow(seed) {
            if (seed) { x = seed; }
            var x = x;
            return x;
        }
        function alias(x) { var x = x; return x; }
        if (shadow(7) !== 7 || shadow() !== void 0 || x !== 91) bad;
        if (alias(8) !== 8 || alias() !== void 0) bad;
    "#,
    );
}

#[test]
fn strictness_and_free_names_stay_definition_local_and_live() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r#"
        var effect = 0;
        function sloppy() { var marker; 'use strict'; undefined = (effect = 1); return typeof late; }
        function ownStrict() { 'use strict'; var x; undefined = (effect = 2); }
    "#,
    );
    normal(
        &mut realm,
        r#"
        'use strict';
        function inherited() { var x; undefined = (effect = 3); }
        if (sloppy() !== 'undefined' || effect !== 1) bad;
    "#,
    );
    normal(&mut realm, "let late = 9; if (sloppy() !== 'number') bad;");
    for name in ["ownStrict", "inherited"] {
        assert!(matches!(
            run(&mut realm, &format!("{name}();")),
            Ok(Completion::Throw(Throw::TypeError { .. }))
        ));
    }
    normal(&mut realm, "if (effect !== 3 || undefined !== void 0) bad;");
    let source = r#"'use strict'; function f(){return; var x = '\9';}"#;
    assert_eq!(
        CompiledScript::compile(source).unwrap_err(),
        CompileError::SyntaxError {
            span: span(source, r"\9")
        }
    );
    CompiledScript::compile(r#"function f(){var x; 'use strict'; '\8';}"#).unwrap();
}

#[test]
fn locals_unwind_after_rhs_throw_and_strings_promote_only_on_global_publication() {
    let mut realm = Realm::new();
    let text = "a".repeat(2_048);
    normal(
        &mut realm,
        &format!(
            r#"
        var sink, effect = 0;
        function payload() {{ return '{text}'; }}
        function transfer(x, publish) {{ var tmp = x; if (publish) sink = tmp; return tmp; }}
        function fail(flag) {{
            if (typeof a !== 'undefined') bad;
            if (flag) var a = (effect = 1, absent), b = (effect = 2);
            return a;
        }}
    "#
        ),
    );
    assert!(matches!(
        run(&mut realm, "fail(true);"),
        Ok(Completion::Throw(Throw::ReferenceError { .. }))
    ));
    normal(
        &mut realm,
        "if (effect !== 1 || fail(false) !== void 0) bad;",
    );
    normal(
        &mut realm,
        "for (var i=0; i<20; i++) { if (transfer(payload()).length !== 2048) bad; }",
    );
    // Previous local copies did not touch the lifetime arena. Exactly sixteen
    // writes of this size fit and are split to respect the per-execute cap.
    for _ in 0..2 {
        normal(
            &mut realm,
            "for (i=0; i<8; i++) { transfer(payload(), true); }",
        );
    }
    assert!(matches!(
        run(&mut realm, "transfer(payload(), true);"),
        Err(EngineFault::StringRetentionLimitExceeded { .. })
    ));
    normal(
        &mut realm,
        "if (transfer(payload()).length !== 2048 || sink.length !== 2048) bad;",
    );
}

fn names(prefix: char, count: usize) -> String {
    (0..count)
        .map(|i| format!("{prefix}{i:03}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn per_body_count_names_and_duplicate_work_are_bounded() {
    let mut realm = Realm::new();
    let formals = names('p', 64);
    let locals = names('a', 64);
    let body = format!("var {formals}, {locals}, a063; return a063 = p000;");
    normal(
        &mut realm,
        &format!(
            "function wide({formals}){{{body}}} function other(){{var {};}} if (wide(7) !== 7) bad;",
            names('b', 128)
        ),
    );
    let source = format!("function wide({formals}){{{body} var overflow;}}");
    assert_eq!(
        CompiledScript::compile(&source).unwrap_err(),
        CompileError::LimitExceeded {
            limit: CompileLimit::LocalSlots,
            span: span(&source, "overflow")
        }
    );
    let long = "n".repeat(256);
    normal(
        &mut realm,
        &format!("function f(){{var {long} = 1; return {long};}} if (f() !== 1) bad;"),
    );
    let source = format!("function f(){{ return; var {long}X; }}");
    assert_eq!(
        CompiledScript::compile(&source).unwrap_err(),
        CompileError::LimitExceeded {
            limit: CompileLimit::NameBytes,
            span: span(&source, &format!("{long}X"))
        }
    );
    let duplicates = |n| {
        format!(
            "function many(){{return; var {};}}",
            vec![&long; n]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    CompiledScript::compile(&duplicates(253)).unwrap();
    let CompileError::Parse(error) = CompiledScript::compile(&duplicates(254)).unwrap_err() else {
        panic!("expected parser work limit")
    };
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
}

#[test]
fn noop_occurrences_still_consume_the_aggregate_token_budget() {
    let exact = format!("function f(){{var x{};}};", ",x".repeat(2043));
    CompiledScript::compile(&exact).unwrap();
    let CompileError::Parse(error) = CompiledScript::compile(&(exact + ";")).unwrap_err() else {
        panic!("expected parser token limit")
    };
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Tokens));
}

#[test]
fn aggregate_live_slots_include_var_tails_and_suspended_caller_operands() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        &format!(
            r#"
        var effect = 0;
        function sink() {{ return 8; }}
        function wide(n) {{
            var {}, last;
            if (last !== void 0) bad;
            last = n;
            if (n) return wide((effect += 1, n - 1)) + last;
            return 0;
        }}
    "#,
            names('a', 125)
        ),
    );
    // 32 frames x 127 slots + active operand peak 3 + 29 preserved root
    // operands = 4096. Recursive callers preserve their locals, not a
    // second copy of their argument/operand peaks.
    let call = |n| {
        format!(
            "if (sink({},wide(31)) !== 8 || effect !== 31) bad;",
            vec!["1"; n].join(",")
        )
    };
    normal(&mut realm, &call(28));
    normal(&mut realm, "effect = 0;");
    assert_eq!(
        run(&mut realm, &call(29)),
        Err(EngineFault::Evaluation(EvalError::LiveSlotLimitExceeded))
    );
    normal(&mut realm, "if (effect !== 31 || wide(2) !== 3) bad;");
}

#[test]
fn shared_call_setup_charges_all_slots_after_argument_effects() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        &format!(
            r#"
            var effect = 0, entered = 0;
            function wide() {{ var {}; }}
            function tail() {{ var {}, loc = 1; loc++; }}
            function shortTail() {{ var {}; }}
            function empty() {{}}
            function abortOnEntry() {{ (0)(); }}
            function one() {{ entered = 1; }}
            "#,
            names('a', 128),
            names('b', 6),
            names('b', 6)
        ),
    );
    // 1016 * (1 + 128) + (1 + 7) = 131072 credits. The local accesses
    // in tail don't charge that meter.
    let exact = "for (var i=0; i<1016; i++) { wide(); } tail(effect = 1);";
    normal(&mut realm, exact);
    // Unlike a free name write, the body's TypeError would not itself
    // require lookup credits; the setup failure must happen before entry.
    assert!(matches!(
        run(&mut realm, "abortOnEntry();"),
        Ok(Completion::Throw(Throw::TypeError { .. }))
    ));
    let over = format!("{exact} abortOnEntry(effect = 2);");
    assert_eq!(
        run(&mut realm, &over),
        Err(EngineFault::Evaluation(
            EvalError::GlobalLookupLimitExceeded
        ))
    );
    normal(
        &mut realm,
        "if (effect !== 2) bad; one(); if (entered !== 1) bad;",
    );

    // The remaining credit pays for one no-local call. The empty body
    // succeeds; the body with a reached free put needs more credits.
    normal(&mut realm, "entered = 0;");
    let almost = "for (i=0; i<1016; i++) { wide(); } shortTail();";
    normal(&mut realm, &format!("{almost} empty();"));
    assert_eq!(
        run(&mut realm, &format!("{almost} one();")),
        Err(EngineFault::Evaluation(
            EvalError::GlobalLookupLimitExceeded
        ))
    );
    normal(&mut realm, "if (entered !== 0) bad;");
}

#[test]
fn local_var_does_not_widen_grammar_or_legacy() {
    for source in [
        "function f(){ if (false) var x }",
        "function f(){ var a,; }",
        "function f(){ var [x] = 1; }",
        "function f(){ label: var a; }",
        "function f(){ for (var x in y) {} }",
        "function f(){ for (var x of y) {} }",
        "function f(){ for (let x = 0;;) {} }",
        "function f(){ while (false) var a; }",
        "function f(){ var arguments; }",
        "function f(){ var eval; }",
        "function f(){ return; var x = function(){}; }",
    ] {
        assert!(
            matches!(CompiledScript::compile(source), Err(CompileError::Parse(_))),
            "{source}"
        );
    }
    let error = parse_script_for_completion("var x;").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert_eq!(
        error.message(),
        "reserved or excluded name is outside the subset"
    );
}
