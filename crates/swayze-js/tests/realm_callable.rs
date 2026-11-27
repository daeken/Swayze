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
fn legacy_diagnostic_enums_remain_exhaustive_for_downstream_users() {
    fn diagnostic(kind: ErrorKind) -> &'static str {
        match kind {
            ErrorKind::Malformed => "malformed",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::LimitExceeded(limit) => match limit {
                Limit::SourceBytes => "source",
                Limit::Tokens => "tokens",
                Limit::NestingDepth => "nesting",
                Limit::Work => "work",
                Limit::DecimalLiteralBytes => "decimal",
                Limit::StringLiteralBytes => "literal bytes",
                Limit::StringLiteralUnits => "literal units",
                Limit::Allocation => "allocation",
            },
        }
    }

    let error = parse_script_for_completion("function f(){}").unwrap_err();
    assert_eq!(diagnostic(error.kind()), "unsupported");
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
        "var expr = function named(){};",
        "if (false) function f(){}",
        "{function f(){}}",
        "function *f(){}",
        "function f(){var x}",
        "function f(){for (var i;;) var x;}",
        "function f(){if (false) {let x;}}",
        "function f(){this;}",
        "function f(){new f;}",
        "function f(){arguments;}",
        "function f(){f.length();}",
        "function f(){((f.length))();}",
        "function f(){f.x;}",
        "function f(){break outside;}",
        "function f(){return; var dead = function(){};}",
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
    for (source, limit, at) in [
        (
            format!("function bad({}){{}}", formals(65)),
            CompileLimit::Parameters,
            "p64",
        ),
        (
            format!("check({});", args(65)),
            CompileLimit::Arguments,
            ",",
        ),
    ] {
        assert_eq!(
            CompiledScript::compile(&source).unwrap_err(),
            CompileError::LimitExceeded {
                limit,
                span: span(&source, at)
            }
        );
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
    let too_long = format!("{name}b");
    let source = format!("function f({too_long}){{}}");
    assert_eq!(
        CompiledScript::compile(&source).unwrap_err(),
        CompileError::LimitExceeded {
            limit: CompileLimit::NameBytes,
            span: span(&source, &too_long)
        }
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

#[test]
fn anonymous_primary_resumes_script_state_across_expression_and_statement_owners() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r#"
        (function () { return 1; })();
        let f = ((function (x) { var y = x + 1; return y; })), g = function () {};
        function apply(a,b,c) { return a(b()) + c; }
        var x = 0, out = 0;
        if (apply(function (z) { return z + 2; }, function () { return 3; }, 4) !== 9) bad;
        if (`a${1 + (function () { return 2; })()}${`b${(function () { return 'c'; })()}`}d` !== 'a3bcd') bad;
        if (f(9) !== 10 || g() !== undefined) bad;
        outer: for (var f2 = function () { return 2; }, i = 0;
                    i < (function () { return 3; })();
                    i += (function () { return 1; })()) {
            if (i === 0) do {
                out += (function () { return 20; })(); continue;
            } while ((function () { return false; })());
            else if (i === 1) continue outer;
            else stop: { f2 = function () { return 4; }, out += 2; break stop; bad; }
        }
        x_loop: while ((function () { return x < 2; })()) { x++; }
        if (x !== 2 || out !== 22 || i !== 3 || f2() !== 4) bad;
        if (false ? function () {} : (0, function () { return true; })()) ; else bad;
    "#,
    );
}

#[test]
fn anonymous_identity_is_reached_fresh_and_escapes_its_producer_without_self_binding() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r#"
        let switcher = function () { return switcher; };
        var alias = switcher, late = function () { return typeof fresh; }, f, old, i = 0;
        while (i < 2) { old = f; f = function () { return `body${i}`; }; i++; }
        if (f === old || f() !== old() || late() !== 'undefined') bad;
    "#,
    );
    normal(
        &mut realm,
        r#"
        let fresh = 5;
        switcher = function () { return 7; };
        if (alias() !== switcher || alias()() !== 7 || old() !== 'body2' || late() !== 'number') bad;
    "#,
    );
    let probe = "if (late() !== 'number') bad; let never;";
    normal(&mut realm, probe);
    normal(
        &mut realm,
        "var tdz = function () { return typeof notYet; };",
    );
    let source = "tdz(); let notYet;";
    assert!(matches!(
        run(&mut realm, source),
        Ok(Completion::Throw(Throw::ReferenceError { .. }))
    ));
}

#[test]
fn anonymous_calls_keep_getvalue_arguments_and_putvalue_order() {
    let mut realm = Realm::new();
    normal(&mut realm, "var hits = 0; const fixed = 5;");
    assert!(matches!(
        run(&mut realm, "fixed = function () {};"),
        Ok(Completion::Throw(Throw::TypeError { .. }))
    ));
    assert!(matches!(
        run(&mut realm, "fixed((function () { return (hits = 1); })());"),
        Ok(Completion::Throw(Throw::TypeError { .. }))
    ));
    normal(
        &mut realm,
        r#"
        if (hits !== 1 || fixed !== 5) bad;
        hits ||= function () { bad; };
        var f;
        f ??= (function (a,b) { return a + b; });
        if ((f ||= function () { bad; })(2,3) !== 5) bad;
        var f2 = (f &&= (function () { return hits; }));
        if (f2 !== f || f2() !== 1) bad;
    "#,
    );
}

#[test]
fn anonymous_body_eligibility_and_decimal_witnesses_are_lexically_owned() {
    for source in [
        "function () {};",
        "var f = function named() {};",
        "{ function f(){} }",
        "label: function f(){}",
        "if (false) function f(){}",
        "for (let x=1;;) { (function () {return x;}); }",
        "var f = function () { if (false) { (function () {}); } };",
        "function f(){ return; (function () {}); }",
        "(function(t,t){});",
        "(function () { return this; })();",
        "(function () {}).name;",
        "new (function () {});",
    ] {
        assert!(
            matches!(CompiledScript::compile(source), Err(CompileError::Parse(_))),
            "{source}"
        );
    }
    let legacy = "(function () {});";
    let err = parse_script_for_completion(legacy).unwrap_err();
    assert_eq!(err.span(), span(legacy, "function"));
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r#"var a = function(){ return '\8'; }, b = function(){ 'use strict'; return 7; }; if (a() !== '8' || b() !== 7 || '\9' !== '9') bad;"#,
    );
    for (source, escape) in [
        (r#"'\8'; var f = function(){ '\9'; 'use strict'; };"#, "\\9"),
        (
            r#"'use strict'; false && (function () { if (false) '\8'; });"#,
            "\\8",
        ),
        (
            r#"var x = `${(function () {'use strict'; return '\9';})()}${'\8'}`;"#,
            "\\9",
        ),
        (
            r#"'use strict'; var x = `${(function () {return 1;})()}${'\8'}`;"#,
            "\\8",
        ),
    ] {
        assert_eq!(
            CompiledScript::compile(source).unwrap_err(),
            CompileError::SyntaxError {
                span: span(source, escape)
            }
        );
    }
    normal(
        &mut realm,
        "'use strict'; var w = function () { var undefined = 2; return ++undefined; }, s = function () { undefined = (effect = 3); }; var effect = 0;",
    );
    assert!(matches!(
        run(&mut realm, "if (w() !== 3) bad; s();"),
        Ok(Completion::Throw(Throw::TypeError { .. }))
    ));
    normal(
        &mut realm,
        "if (effect !== 3) bad; var sloppy = function () { undefined = (effect = 4); };",
    );
    normal(&mut realm, "'use strict'; sloppy(); if (effect !== 4) bad;");
}

#[test]
fn expression_literals_and_names_are_owned_by_each_realm() {
    let mut one = Realm::new();
    let mut two = Realm::new();
    {
        let code = CompiledScript::compile("var f = function (x) { var result = x; return `left:${result}:${dynamic}:right`; }; var alias = f;").unwrap();
        for realm in [&mut one, &mut two] {
            assert_eq!(
                realm.instantiate(&code).unwrap().execute(),
                Ok(Completion::Normal)
            );
        }
    }
    normal(&mut one, "let dynamic = 'one';");
    normal(&mut two, "var dynamic = 'two';");
    let root = CompiledScript::compile(
        "var text = 'caller:' + alias('arg'); false && function () { return 'not the source'; };",
    )
    .unwrap();
    for (realm, expected) in [(&mut one, "one"), (&mut two, "two")] {
        assert_eq!(
            realm.instantiate(&root).unwrap().execute(),
            Ok(Completion::Normal)
        );
        normal(
            realm,
            &format!("if (text !== 'caller:left:arg:{expected}:right' || alias !== f) bad;"),
        );
    }
}

#[test]
fn excluded_generator_headers_keep_the_function_keyword_diagnostic() {
    for source in [
        "var f = function* () {};",
        "(function* () {});",
        "if (false) { var f = function * () {}; }",
        "false && (function* () {});",
    ] {
        assert!(
            matches!(CompiledScript::compile(source), Err(CompileError::Parse(err))
            if err.kind() == ErrorKind::Unsupported && err.span() == span(source, "function")),
            "{source}"
        );
    }
    let source = "var f = function { };";
    assert!(
        matches!(CompiledScript::compile(source), Err(CompileError::Parse(err))
        if err.kind() == ErrorKind::Malformed && err.span() == span(source, "{"))
    );
    let source = "function* f() {}";
    assert!(
        matches!(CompiledScript::compile(source), Err(CompileError::Parse(err))
        if err.kind() == ErrorKind::Unsupported && err.span() == span(source, "*"))
    );
}

#[test]
fn anonymous_primaries_resume_the_primitive_length_suffix_only() {
    for rhs in ["function () {}.length", "(function () {}).length"] {
        normal(&mut Realm::new(), &format!("false && {rhs};"));
        assert_eq!(
            run(&mut Realm::new(), &format!("var x = {rhs};")),
            fault(EvalError::UnsupportedPropertyReceiver)
        );
    }
    for rhs in ["function () {}.name", "(function () {}).name"] {
        assert!(
            matches!(CompiledScript::compile(&format!("false && {rhs};")), Err(CompileError::Parse(err)) if err.kind() == ErrorKind::Unsupported)
        );
    }
}

#[test]
fn parked_script_and_active_function_share_statement_frame_ceiling() {
    let source = |outer: usize, inner: usize, leaf: &str| {
        format!(
            "{}false && (function () {{ {}{leaf}{} }});{}",
            "{".repeat(outer),
            "{".repeat(inner),
            "}".repeat(inner),
            "}".repeat(outer),
        )
    };
    for (outer, inner) in [(0, 128), (1, 127), (64, 64), (127, 1), (128, 0)] {
        // Expression delimiter depth is independent of the active statement
        // count. The leaf need not allocate a statement frame even at cap.
        normal(&mut Realm::new(), &source(outer, inner, "return ((1));"));
        let over = source(outer, inner, "overflow: {}");
        assert!(
            matches!(CompiledScript::compile(&over), Err(CompileError::Parse(err))
            if err.kind() == ErrorKind::LimitExceeded(Limit::NestingDepth)
                && err.span() == span(&over, "overflow")
                && err.message() == "statement nesting ceiling exceeded"),
            "outer={outer}, inner={inner}"
        );
    }
}

#[test]
fn anonymous_primary_and_body_delimiters_share_the_parked_nesting_limit() {
    let source = |count| {
        format!(
            "{}function () {{ return (1); }}{};",
            "(".repeat(count),
            ")".repeat(count)
        )
    };
    normal(&mut Realm::new(), &source(126));
    assert!(
        matches!(CompiledScript::compile(&source(127)), Err(CompileError::Parse(err)) if err.kind() == ErrorKind::LimitExceeded(Limit::NestingDepth))
    );
}
