use swayze_js::{
    ErrorKind, EvalError, Limit, ParseErrorType, Span, parse_script_for_completion,
    realm::{
        CompileError, CompileLimit, CompiledScript, Completion, EngineFault, InstantiateError,
        Realm, Throw,
    },
};

fn run(realm: &mut Realm, source: &str) -> Result<Completion, EngineFault> {
    let code = CompiledScript::compile(source).unwrap_or_else(|err| panic!("{err:?}: {source}"));
    realm.instantiate(&code).unwrap().execute()
}

fn normal(realm: &mut Realm, source: &str) {
    assert_eq!(run(realm, source), Ok(Completion::Normal), "{source}");
}

fn fault(error: EvalError) -> Result<Completion, EngineFault> {
    Err(EngineFault::Evaluation(error))
}

fn span(source: &str, text: &str) -> Span {
    let start = source.rfind(text).unwrap();
    Span {
        start,
        end: start + text.len(),
    }
}

#[test]
fn gdi_last_wins_installs_even_when_abandoned_and_replaces_without_alias_mutation() {
    let mut realm = Realm::new();
    let install = "if (f() !== 2) bad; function f(){return 1;} var f; function f(){return 2;}";
    normal(&mut realm, install);
    normal(&mut realm, "var old = f; var f;");
    {
        let code = CompiledScript::compile(install).unwrap();
        let ready = realm.instantiate(&code).unwrap();
        drop(ready);
    }
    normal(
        &mut realm,
        "if (old === f || old() !== 2 || f() !== 2) bad;",
    );
    normal(&mut realm, "var f = 9; if (f !== 9 || old() !== 2) bad;");
    normal(&mut realm, "var undefined; var other = 99;");
    let source = "let fresh; function old(){} function undefined(){} var ghost;";
    let code = CompiledScript::compile(source).unwrap();
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::TypeError {
            span: span(source, "undefined")
        }
    );
    normal(
        &mut realm,
        "let fresh = 3; let ghost = 4; if (old() !== 2) bad;",
    );
    let source = "function undefined(){} let other;";
    let code = CompiledScript::compile(source).unwrap();
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::SyntaxError {
            span: span(source, "other")
        }
    );
    let source = "function undefined(){} function fresh(){}";
    let code = CompiledScript::compile(source).unwrap();
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::SyntaxError {
            span: span(source, "fresh")
        }
    );
    let source = "function clash(){} let clash;";
    assert_eq!(
        CompiledScript::compile(source).unwrap_err(),
        CompileError::SyntaxError {
            span: span(source, "clash")
        }
    );
}

#[test]
fn callee_getvalue_precedes_arguments_and_noncallability_follows_them() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "function f(a){return 10 + a;} function g(a){return 20 + a;} var n = 0;",
    );
    normal(
        &mut realm,
        "if (f((f = g, n += 1), n += 2) !== 11 || n !== 3 || f(2) !== 22) bad;",
    );
    let source = "typeof (unbound)(n = 4);";
    assert_eq!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: span(source, "unbound")
        }))
    );
    normal(&mut realm, "if (n !== 3) bad;");
    let source = "tdz(n = 5); let tdz;";
    assert_eq!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: Span { start: 0, end: 3 }
        }))
    );
    normal(&mut realm, "if (n !== 3) bad;");
    let source = "n(n = 4, n += 5);";
    assert_eq!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::TypeError {
            span: span(source, "(n = 4, n += 5)")
        }))
    );
    normal(&mut realm, "if (n !== 9) bad;");
    let source = "n(n = 10, absent, n = 11);";
    assert_eq!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: span(source, "absent")
        }))
    );
    normal(&mut realm, "if (n !== 10) bad;");
    let source = "typeof f(absent);";
    assert_eq!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: span(source, "absent")
        }))
    );
}

#[test]
fn mutable_ephemeral_formals_and_control_are_frame_relative() {
    let mut realm = Realm::new();
    normal(&mut realm, "
        var p = 99, hits = 0;
        function pick(p, undefined){
            p ||= 2;
            outer: for (; p < 8; p++) {
                if (p === 4) continue outer;
                if (p === 5) break outer;
                hits += 1;
            }
            if (undefined) return p++, ++p;
            return p = p + 1;
            hits = 99;
        }
        function fall(x) { hits += x; 765; }
        function bare(){ return /* trivia\n */ ; hits = 99; }
        function id(x){return x;}
        if (100 + pick(0) !== 106 || pick(4, true) !== 7 || p !== 99 || hits !== 2) bad;
        if (fall(8) !== undefined || bare() !== undefined || id() !== undefined || hits !== 10) bad;
        if (id(pick)(5, true) !== 7 || id((0, 10)) !== 10 || (false && bad()) || id(true ? 12 : bad()) !== 12) bad;
    ");
}

#[test]
fn opaque_identity_has_only_supported_value_operations() {
    let mut realm = Realm::new();
    normal(&mut realm, "
        function one(){} function two(){} function pass(x){return x;}
        var alias = pass(one);
        if (!one || (one ?? missing) !== alias || (one || missing) !== alias || typeof one !== 'function' ||
            one !== alias || !(two != one) || !(one == alias) || one == undefined || null == one ||
            one === 'function' || two === one || typeof one() !== 'undefined') bad;
    ");
    for source in [
        "+one;",
        "'x' + one;",
        "one < 1;",
        "one == true;",
        "`${one}`;",
    ] {
        assert_eq!(
            run(&mut realm, source),
            fault(EvalError::UnsupportedCallableConversion),
            "{source}"
        );
    }
    assert_eq!(
        run(&mut realm, "one.length;"),
        fault(EvalError::UnsupportedPropertyReceiver)
    );
}

#[test]
fn live_free_names_shadow_rebind_and_recur_via_current_realm() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "function read(){return typeof later;} function readOther(){return typeof late;} function recurse(n){if (!n) return 0; return 1 + recurse(n - 1);} var old = recurse;",
    );
    normal(
        &mut realm,
        "if (read() !== 'undefined' || old(5) !== 5) bad;",
    );
    normal(
        &mut realm,
        "let later = 3; function recurse(n){return n + 10;} if (read() !== 'number' || old(5) !== 15) bad;",
    );
    let code = CompiledScript::compile("let late;").unwrap();
    drop(realm.instantiate(&code).unwrap());
    assert_eq!(
        run(&mut realm, "readOther();"),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: Span { start: 73, end: 77 }
        }))
    );
}

#[test]
fn definition_mode_and_quoted_witness_are_body_local() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r"function loose(){ '\8'; undefined = 1; } function own(){ 'directive'; 'use strict'; undefined = 1; }",
    );
    normal(&mut realm, "'use strict'; loose();");
    assert!(matches!(
        run(&mut realm, "own();"),
        Ok(Completion::Throw(Throw::TypeError { .. }))
    ));
    normal(
        &mut realm,
        "'use strict'; function inherited(){ undefined = 1; }",
    );
    assert!(matches!(
        run(&mut realm, "inherited();"),
        Ok(Completion::Throw(Throw::TypeError { .. }))
    ));
    normal(
        &mut realm,
        r"function noDirective(){ 'use strict'.length; '\9'; } function second(){'use strict';} '\8'; noDirective();",
    );
    for source in [
        r"'use strict'; function f(){ if (false) '\9'; }",
        r"function f(){'\9';'use strict';}",
        r"'use strict'; function f(){} '\9';",
    ] {
        assert_eq!(
            CompiledScript::compile(source).unwrap_err(),
            CompileError::SyntaxError {
                span: span(source, r"\9")
            }
        );
    }
}

#[test]
fn literals_and_scratch_cross_artifacts_without_aliasing_equal_offsets() {
    let mut realm = Realm::new();
    {
        let code = CompiledScript::compile(r"function a(x){if (x !== 'B') bad; return '\uD800A';}")
            .unwrap();
        drop(realm.instantiate(&code).unwrap());
    }
    normal(
        &mut realm,
        r"function b(){ return 'B'; } function relay(x){return `L${a(x)}R`; } var saved;",
    );
    normal(
        &mut realm,
        r"if ('C' + b() !== 'CB' || relay(b()) !== 'L\uD800AR') bad; saved = relay(b());",
    );
    normal(
        &mut realm,
        r"if (saved !== 'L\uD800AR' || saved.length !== 4) bad;",
    );
    normal(
        &mut realm,
        r"function write(){saved = '\uD801Z'; return saved;} if (write() !== '\uD801Z') bad;",
    );
    normal(&mut realm, r"if (saved !== '\uD801Z') bad;");
}

#[test]
fn grammar_reports_exclusions_and_preserves_legacy_refusal() {
    let error = parse_script_for_completion("function f(){}").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert_eq!(
        error.message(),
        "reserved or excluded name is outside the subset"
    );
    let error = parse_script_for_completion("(1)();").unwrap_err();
    assert_eq!(
        error.message(),
        "call expressions are outside the arithmetic subset"
    );
    let CompileError::Parse(error) = CompiledScript::compile("missing(); return 1;").unwrap_err()
    else {
        panic!()
    };
    assert_eq!(error.parse_error_type(), Some(ParseErrorType::SyntaxError));
    for source in [
        "function f(){return\n1;}",
        "function f(){return 1}",
        "function f(){1}",
        "function f(x,x){}",
        "function f(x=1){}",
        "function f(...x){}",
        "function f(){function g(){}}",
        "var expr = function(){};",
        "if (false) function f(){}",
        "{function f(){}}",
        "function *f(){}",
        "function f(){var x;}",
        "function f(){for (var i;;){}}",
        "function f(){if (false) {let x;}}",
        "function f(){this;}",
        "function f(){new f;}",
        "function f(){arguments;}",
        "function f(){f.length();}",
        "function f(){((f.length))();}",
        "function f(){f.x;}",
        "function f(){break outside;}",
        "function f(){return; var dead;}",
    ] {
        assert!(
            matches!(CompiledScript::compile(source), Err(CompileError::Parse(_))),
            "{source}"
        );
    }
}

#[test]
fn argument_formal_depth_slot_and_dispatch_caps() {
    let mut realm = Realm::new();
    let formals = |n| {
        (0..n)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let args = |n| vec!["1"; n].join(",");
    normal(
        &mut realm,
        &format!(
            "function check({}){{return p63;}} if (check({}) !== 1) bad;",
            formals(64),
            args(64)
        ),
    );
    for (source, limit) in [
        (
            format!("function bad({}){{}}", formals(65)),
            Limit::Parameters,
        ),
        (format!("check({});", args(65)), Limit::Arguments),
    ] {
        let CompileError::Parse(error) = CompiledScript::compile(&source).unwrap_err() else {
            panic!()
        };
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit));
    }
    normal(
        &mut realm,
        "function depth(n){if (!n) return 8; return depth(n - 1);} if (depth(31) !== 8) bad;",
    );
    assert_eq!(
        run(&mut realm, "depth(32);"),
        fault(EvalError::CallDepthLimitExceeded)
    );
    normal(
        &mut realm,
        &format!(
            "function sink(){{return 1;}} function wide(n,{}){{if (!n) return 0; return sink({},wide(n - 1));}}",
            formals(63),
            args(62)
        ),
    );
    normal(&mut realm, &format!("sink({},wide(31));", args(28))); // 29 + 31 * 127 + 64 + 66 = 4096 reserved slots
    assert_eq!(
        run(&mut realm, &format!("sink({},wide(31));", args(29))),
        fault(EvalError::LiveSlotLimitExceeded)
    );
    normal(
        &mut realm,
        "function countdown(n){while(n){n -= 1;} return n;} 0;0;countdown(2339);",
    );
    assert_eq!(
        run(&mut realm, "0;0;0;countdown(2339);"),
        fault(EvalError::DispatchLimitExceeded)
    );
    normal(&mut realm, "if (depth(1) !== 8) bad;");
}

#[test]
fn imports_reach_exact_artifact_and_identity_counts_without_mutating_on_overflow() {
    let mut realm = Realm::new();
    let code = CompiledScript::compile("function f(){return 1;}").unwrap();
    for _ in 0..16 {
        drop(realm.instantiate(&code).unwrap());
    }
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::ArtifactLimitExceeded
    );
    normal(&mut realm, "if (f() !== 1) bad;");

    let mut realm = Realm::new();
    let source = (0..128)
        .map(|i| format!("function f{i:03}(){{}} "))
        .collect::<String>();
    let code = CompiledScript::compile(&source).unwrap();
    drop(realm.instantiate(&code).unwrap());
    normal(&mut realm, "var old = f000;");
    for _ in 0..3 {
        drop(realm.instantiate(&code).unwrap());
    }
    let extra = CompiledScript::compile("function f000(){return 1;} let ghost;").unwrap();
    assert_eq!(
        realm.instantiate(&extra).unwrap_err(),
        InstantiateError::FunctionLimitExceeded
    );
    normal(
        &mut realm,
        "let ghost; if (old === f000 || f000() !== undefined) bad;",
    );
}

#[test]
fn body_and_formal_name_ceiling() {
    let mut realm = Realm::new();
    let unit = "function f(){}";
    normal(&mut realm, &unit.repeat(256));
    assert!(matches!(
        CompiledScript::compile(&unit.repeat(257)),
        Err(CompileError::LimitExceeded {
            limit: CompileLimit::Declarations,
            ..
        })
    ));
    let name = "a".repeat(256);
    normal(
        &mut realm,
        &format!("function f({name}){{return {name};}} if (f(3) !== 3) bad;"),
    );
    let CompileError::Parse(error) =
        CompiledScript::compile(&format!("function f({name}b){{}}")).unwrap_err()
    else {
        panic!()
    };
    assert_eq!(
        error.kind(),
        ErrorKind::LimitExceeded(Limit::FormalNameBytes)
    );
}

#[test]
fn reached_retained_lookup_budget_has_an_execute_wide_boundary() {
    let mut realm = Realm::new();
    for start in (0..252).step_by(63) {
        let declarations = (start..start + 63)
            .map(|i| format!("v{i:03}"))
            .collect::<Vec<_>>()
            .join(",");
        normal(&mut realm, &format!("let {declarations};"));
    }
    normal(
        &mut realm,
        "function f(n){ while (n) { typeof missing; n -= 1; } }",
    );
    // 252 lexical candidates, one seed, one var and one accessed site = 255
    // per iteration. One call with one formal adds two credits.
    normal(&mut realm, "f(514);");
    assert_eq!(
        run(&mut realm, "f(515);"),
        fault(EvalError::GlobalLookupLimitExceeded)
    );
    // A fresh execute gets a fresh meter, not one budget per function call.
    normal(&mut realm, "f(256); f(257);");
    assert_eq!(
        run(&mut realm, "f(257); f(257);"),
        fault(EvalError::GlobalLookupLimitExceeded)
    );
}

#[test]
fn frame_moves_do_not_promote_strings_and_shared_operation_caps_do_not_reset() {
    let mut realm = Realm::new();
    let text = "a".repeat(2_048);
    normal(
        &mut realm,
        &format!(
            "function payload(){{return '{text}';}} function box(x){{return '' + x;}} function same(x,y){{return x === y;}} function num(x){{return +x;}} function writeSaved(x){{saved = x;}} var saved;"
        ),
    );
    normal(
        &mut realm,
        "var i; for (i=0;i<32;i++) { if (!same(payload(),payload())) bad; }",
    );
    assert_eq!(
        run(
            &mut realm,
            "for (i=0;i<33;i++) { same(payload(),payload()); }"
        ),
        fault(EvalError::StringComparisonLimitExceeded)
    );
    // Argument/return/formal copies did not touch the retention meter. Eight
    // reached global writes copy exactly 16,384; the ninth can't publish.
    normal(&mut realm, "for (i=0;i<8;i++) { writeSaved(payload()); }");
    assert!(matches!(
        run(&mut realm, "for (i=0;i<9;i++) { writeSaved(payload()); }"),
        Err(EngineFault::StringCopyLimitExceeded { .. })
    ));
    assert!(matches!(
        run(&mut realm, "writeSaved(payload());"),
        Err(EngineFault::StringRetentionLimitExceeded { .. })
    ));
    normal(&mut realm, "if (saved.length !== 2048) bad;");
    normal(&mut realm, "for (i=0;i<16;i++) { box(payload()); }");
    assert_eq!(
        run(&mut realm, "for (i=0;i<17;i++) { box(payload()); }"),
        fault(EvalError::StringConcatenationBudgetExceeded)
    );
    normal(&mut realm, "for (i=0;i<31;i++) { num(payload()); }");
    assert_eq!(
        run(&mut realm, "for (i=0;i<32;i++) { num(payload()); }"),
        fault(EvalError::StringNumericConversionLimitExceeded)
    );

    // The 31 conversions consume 31 * (2,049 + 6,400) of 262,144
    // credits. ToPrimitive on the callable must fail before the String
    // needs another 2,049 credits to begin numeric conversion.
    let warmup = "for (i=0;i<31;i++) { num(payload()); }";
    for expression in ["payload() < num;", "(i += 1, payload()) < (i *= 2, num);"] {
        assert_eq!(
            run(&mut realm, &format!("{warmup} {expression}")),
            fault(EvalError::UnsupportedCallableConversion)
        );
    }
    normal(&mut realm, "if (i !== 64) bad;");
    // Subtraction instead performs lhs ToNumeric before rhs ToNumeric.
    assert_eq!(
        run(&mut realm, &format!("{warmup} payload() - num;")),
        fault(EvalError::StringNumericConversionLimitExceeded)
    );
}
