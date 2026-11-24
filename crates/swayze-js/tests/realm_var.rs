use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, parse_script, parse_script_for_completion,
    parse_script_for_completion_with_limits, parse_script_with_limits,
    realm::{
        CompileError, CompileLimit, CompiledScript, Completion, EngineFault, InstantiateError,
        Realm, Throw,
    },
};

fn run(realm: &mut Realm, source: &str) -> Result<Completion, EngineFault> {
    let code = CompiledScript::compile(source).unwrap();
    realm.instantiate(&code).unwrap().execute()
}

fn normal(realm: &mut Realm, source: &str) {
    assert_eq!(run(realm, source), Ok(Completion::Normal), "{source}");
}

fn span(source: &str, last: &str) -> Span {
    let start = source.rfind(last).unwrap();
    Span {
        start,
        end: start + last.len(),
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

#[test]
fn real_declarations_hoist_through_control_and_only_reached_initializers_run() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "
        if (before !== undefined) bad;
        before = 4; var before;
        var self = self, a = 1, b = (a += 2), a, c = a;
        if (false) var skipped = missing; else var taken = 2;
        if (true) if (false) var near = missing; else var nearest = 5;
        if (true) var emptyThen; else var emptyElse;
        while (false) { var w = missing; }
        for (;;) { break; var unreachable = missing; }
        block: { break block; var labelled = missing; }
        do { var reached = 3; } while (false);
        'var notADeclaration;'; // var alsoNotADeclaration;
    ",
    );
    normal(
        &mut realm,
        "
        if (before !== 4 || a !== 3 || b !== 3 || c !== 3 || self !== undefined ||
            skipped !== undefined || taken !== 2 || near !== undefined || nearest !== 5 ||
            emptyThen !== undefined || emptyElse !== undefined || w !== undefined ||
            unreachable !== undefined || labelled !== undefined || reached !== 3) bad;
    ",
    );
    reference(&mut realm, "notADeclaration;", "notADeclaration");
    reference(&mut realm, "alsoNotADeclaration;", "alsoNotADeclaration");
    normal(&mut realm, "var only;");
    normal(&mut realm, "if (only !== undefined) bad;");
}

#[test]
fn classic_head_reuses_the_loop_endpoint_and_script_slots_on_reentry() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "
        'use strict';
        if (i !== undefined || j !== undefined || testOnly !== undefined) bad;
        var count = 0, outer = 0;
        for (; outer < 2; outer++) {
            loop: for (var i = (count += 1), j = i; i < 4; i++) {
                if (i < 3) continue loop;
                var seen = i; var i;
            }
        }
        for (var testOnly; false;) { var bodyOnly = bad; }
        if (count !== 2 || i !== 4 || j !== 2 || seen !== 3 || bodyOnly !== undefined) bad;
    ",
    );
    normal(
        &mut realm,
        "if (i !== 4 || j !== 2 || testOnly !== undefined) bad;",
    );
}

#[test]
fn static_conflicts_use_the_later_real_occurrence_but_follow_strict_witness_and_grammar() {
    for (source, later) in [
        ("let clash; if (false) { var clash; }", "clash"),
        (
            "if (false) var clash; else var clash; const clash = 0;",
            "clash",
        ),
        ("let clash; for (var clash = 0;;) { break; }", "clash"),
        ("const clash = 0; for (;;) { break; var clash; }", "clash"),
        (r"'use strict'; let clash; var clash = '\9';", r"\9"),
    ] {
        assert_eq!(
            CompiledScript::compile(source).unwrap_err(),
            CompileError::SyntaxError {
                span: span(source, later)
            }
        );
    }
    assert!(matches!(
        CompiledScript::compile(
            r"'use strict'; let clash; var clash = '\9'; for (var x of missing) {}"
        ),
        Err(CompileError::Parse(_))
    ));
    let mut realm = Realm::new();
    normal(&mut realm, "var dup = 1, dup = 2, dup;");
    normal(&mut realm, "'use strict'; var dup = dup + 3; var dup;");
    normal(&mut realm, "if (dup !== 5) bad;");
}

#[test]
fn gdi_is_lexical_first_and_does_not_publish_on_conflict() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "let lexical = 1; const sealed = 2; var declared = 3;",
    );
    let abandoned = CompiledScript::compile("let tdz = 4;").unwrap();
    drop(realm.instantiate(&abandoned).unwrap());
    for (source, winner) in [
        (
            "let freshLex; var freshVar, lexical; let declared;",
            "declared",
        ),
        (
            "var freshVar, lexical; let undefined, declared;",
            "undefined",
        ),
        ("var lexical, sealed;", "lexical"),
        ("var sealed, tdz;", "sealed"),
        ("var tdz;", "tdz"),
        ("let declared, lexical;", "declared"),
        ("let lexical, declared;", "lexical"),
    ] {
        let code = CompiledScript::compile(source).unwrap();
        assert_eq!(
            realm.instantiate(&code).unwrap_err(),
            InstantiateError::SyntaxError {
                span: span(source, winner)
            }
        );
    }
    reference(&mut realm, "freshLex;", "freshLex");
    reference(&mut realm, "freshVar;", "freshVar");
    normal(
        &mut realm,
        "if (lexical !== 1 || sealed !== 2 || declared !== 3) bad;",
    );
}

#[test]
fn abandonment_abrupt_rhs_and_relinking_keep_the_installed_cells() {
    let source = "if (typeof v !== 'number') missing; v += 2;";
    let consumer = CompiledScript::compile(source).unwrap();
    let mut left = Realm::new();
    let mut right = Realm::new();
    let miss = Ok(Completion::Throw(Throw::ReferenceError {
        span: span(source, "missing"),
    }));
    assert_eq!(left.instantiate(&consumer).unwrap().execute(), miss);
    normal(&mut left, "var v = 10, effect = 0;");
    let abandoned = CompiledScript::compile("var v = bad, ha = bad; let tdz = bad;").unwrap();
    drop(left.instantiate(&abandoned).unwrap());
    assert_eq!(
        left.instantiate(&consumer).unwrap().execute(),
        Ok(Completion::Normal)
    );
    assert_eq!(right.instantiate(&consumer).unwrap().execute(), miss);
    reference(&mut left, "typeof tdz;", "tdz");
    reference(
        &mut left,
        "var x = (effect = 2), v = missing, z = 3;",
        "missing",
    );
    normal(
        &mut left,
        "if (ha !== undefined || x !== 2 || effect !== 2 || v !== 12 || z !== undefined) bad;",
    );
    assert_eq!(
        run(&mut left, "var v = 13, never = 3; for (;;) {}"),
        Err(EngineFault::Evaluation(EvalError::DispatchLimitExceeded))
    );
    normal(&mut left, "if (v !== 13 || never !== 3) bad;");
}

#[test]
fn seed_writes_use_the_scripts_mode_after_rhs_evaluation() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        "var lead; 'use strict'; var effect = 0, undefined = (effect = 1);",
    );
    normal(
        &mut realm,
        "'use strict'; var undefined; if (false) var undefined = bad;",
    );
    reference(
        &mut realm,
        "'use strict'; var undefined = missing;",
        "missing",
    );
    let strict = "'use strict'; var undefined = (effect = 2), hoisted = bad;";
    assert_eq!(
        run(&mut realm, strict),
        Ok(Completion::Throw(Throw::TypeError {
            span: span(strict, "undefined")
        }))
    );
    normal(
        &mut realm,
        "if (effect !== 2 || undefined !== void 0 || hoisted !== void 0) bad;",
    );
}

#[test]
fn persistent_vars_and_lexicals_share_checked_string_storage_and_limits() {
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r"var text = '\uD800', joined = `${text}\0`; let alias = joined;",
    );
    normal(
        &mut realm,
        r"text = joined + '\uDC00'; var saved = text; joined = 'new';",
    );
    normal(
        &mut realm,
        r"if (saved !== '\uD800\0\uDC00' || alias !== '\uD800\0' || joined !== 'new') bad;",
    );

    let mut realm = Realm::new();
    let long = "a".repeat(4094);
    let fill = format!(
        "var slot, i = 0; while (i < 4) {{ var slot = '{long}'; i++; }} let tight = '12345678';"
    );
    let sloppy = format!("{fill} var undefined = 'no copy'; slot = '!';");
    assert_eq!(
        run(&mut realm, &sloppy),
        Err(EngineFault::StringCopyLimitExceeded {
            span: span(&sloppy, "slot")
        })
    );
    let strict = format!(
        "'use strict'; {} var undefined = (i = 5, 'no copy');",
        fill.replace("let tight", "var again")
    );
    assert_eq!(
        run(&mut realm, &strict),
        Ok(Completion::Throw(Throw::TypeError {
            span: span(&strict, "undefined")
        }))
    );
    let overflow = "var slot = '!';";
    assert_eq!(
        run(&mut realm, overflow),
        Err(EngineFault::StringRetentionLimitExceeded {
            span: span(overflow, "slot")
        })
    );
    normal(
        &mut realm,
        "var undefined = 'still ignored', aliased = slot; if (i !== 5 || aliased.length !== 4094 || tight !== '12345678') bad;",
    );

    let mut realm = Realm::new();
    // When both ceilings would reject a reached var initializer, the copy
    // meter wins; the failed PutValue does not replace the previously set var.
    normal(&mut realm, &fill);
    let both = format!("{} var slot = '!';", fill.replace("let tight", "var again"));
    assert_eq!(
        run(&mut realm, &both),
        Err(EngineFault::StringCopyLimitExceeded {
            span: span(&both, "slot")
        })
    );
    normal(
        &mut realm,
        "if (slot.length !== 4094 || tight !== again) bad;",
    );
}

#[test]
fn shared_dynamic_capacity_counts_distinct_new_var_names_not_seed_or_reuse() {
    let mut realm = Realm::new();
    for (keyword, prefix, count) in [
        ("let", "a", 128),
        ("let", "bbb", 128),
        ("var", "ccccc", 128),
        ("var", "ddddddd", 127),
    ] {
        normal(
            &mut realm,
            &(0..count)
                .map(|n| format!("{keyword} {prefix}{n};"))
                .collect::<String>(),
        );
    }
    let mixed = CompiledScript::compile("var other, undefined; let last;").unwrap();
    assert_eq!(
        realm.instantiate(&mixed).unwrap_err(),
        InstantiateError::BindingLimitExceeded
    );
    reference(&mut realm, "other;", "other");
    reference(&mut realm, "last;", "last");
    normal(&mut realm, "var other = 42;");
    normal(
        &mut realm,
        "var other, other, undefined; if (other !== 42) bad;",
    );
    for source in ["var more;", "let more;"] {
        let code = CompiledScript::compile(source).unwrap();
        assert_eq!(
            realm.instantiate(&code).unwrap_err(),
            InstantiateError::BindingLimitExceeded
        );
    }
    let conflict = "var missing, a1; let ccccc1;";
    assert_eq!(
        realm
            .instantiate(&CompiledScript::compile(conflict).unwrap())
            .unwrap_err(),
        InstantiateError::SyntaxError {
            span: span(conflict, "ccccc1")
        }
    );
}

#[test]
fn occurrence_and_name_caps_include_unexecuted_no_init_duplicates() {
    let exact = format!("let first; if (false) {{ var {}x; }}", "x,".repeat(254));
    assert!(CompiledScript::compile(&exact).is_ok());
    let over = exact + "var overflow;";
    assert_eq!(
        CompiledScript::compile(&over).unwrap_err(),
        CompileError::LimitExceeded {
            limit: CompileLimit::Declarations,
            span: span(&over, "overflow")
        }
    );
    let long = "a".repeat(256);
    let extended = format!("var {long}a;");
    assert_eq!(
        CompiledScript::compile(&extended).unwrap_err(),
        CompileError::LimitExceeded {
            limit: CompileLimit::NameBytes,
            span: Span { start: 4, end: 261 }
        }
    );
    let exact = format!("var {}{long};", format!("{long},").repeat(63));
    assert!(CompiledScript::compile(&exact).is_ok());
    let over = exact + "var b;";
    assert!(matches!(
        CompiledScript::compile(&over),
        Err(CompileError::LimitExceeded {
            limit: CompileLimit::PooledNameBytes,
            ..
        })
    ));
}

#[test]
fn deferred_var_grammar_stays_outside_realm() {
    for source in [
        "while (false) var x;",
        "do var x; while (false);",
        "for (;;) var x;",
        "label: var x;",
        "var {x} = y;",
        "var [x] = y;",
        "var x = 1\nvar y;",
        "var x",
        "var x y;",
        "var x += 2;",
        "for (var x in y) {}",
        "for (var x of y) {}",
        "var globalThis;",
        "var eval;",
        "var arguments;",
        "var Infinity;",
        "var NaN;",
        "varé;",
        r"var\u0061;",
        "if (false) let x;",
        "for (let x;;) {}",
    ] {
        assert!(
            matches!(CompiledScript::compile(source), Err(CompileError::Parse(_))),
            "{source}"
        );
    }
    normal(
        &mut Realm::new(),
        "var variable, var_name, var2, var$, $var, _var;",
    );
}

#[test]
fn legacy_var_keeps_the_exact_lexer_stop_on_all_paths_and_before_late_errors() {
    for source in [
        "var x = ?",
        "if (false) { var x; } 0",
        "if (true) 0; else var x;",
        "for (var x;;) {} 0",
        "while (true) { break; var x; } 0",
        "let var;",
        "1 + var",
        "label: var x;",
        "var§",
    ] {
        let (expected, message) = if source == "var§" {
            (
                span(source, "§"),
                "Unicode and escaped identifiers are outside the subset",
            )
        } else {
            (
                span(source, "var"),
                "reserved or excluded name is outside the subset",
            )
        };
        let error = parse_script(source).unwrap_err();
        assert_eq!(
            (
                error.kind(),
                error.span(),
                error.message(),
                error.parse_error_type()
            ),
            (ErrorKind::Unsupported, expected, message, None),
            "{source}"
        );
        assert_eq!(parse_script_for_completion(source).unwrap_err(), error);
    }
    for (max_tokens, max_work, kind, end, start, message) in [
        (
            0,
            0,
            ErrorKind::LimitExceeded(Limit::Tokens),
            1,
            0,
            "token ceiling exceeded",
        ),
        (
            1,
            2,
            ErrorKind::LimitExceeded(Limit::Work),
            3,
            2,
            "parse work ceiling exceeded",
        ),
        (
            1,
            3,
            ErrorKind::Unsupported,
            3,
            0,
            "reserved or excluded name is outside the subset",
        ),
    ] {
        let limits = Limits {
            max_tokens,
            max_work,
            ..Limits::default()
        };
        let error = parse_script_with_limits("var!", limits).unwrap_err();
        assert_eq!(
            (
                error.kind(),
                error.span(),
                error.message(),
                error.parse_error_type()
            ),
            (kind, Span { start, end }, message, None)
        );
        assert_eq!(
            parse_script_for_completion_with_limits("var!", limits).unwrap_err(),
            error
        );
    }
}
