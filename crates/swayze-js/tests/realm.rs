use swayze_js::{
    ErrorKind, EvalError, ParseErrorType, Span, parse_script, parse_script_for_completion,
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
fn lossless_strings_outlive_detached_code_and_scratch() {
    let check = CompiledScript::compile(
        r"if (literal !== '\uD800\0a' || literal.length !== 3 || !literal ||
            !(literal < '\uD800\u0001') || joined !== '\uD800\0a\uDC00' ||
            pair !== '𐀀' || pair.length !== 2 || composed === 'é' ||
            made !== '\0\uD80042\uDC00' || typename !== 'boolean' || typeof made !== 'string' ||
            empty || empty.length !== 0 || empty !== '' || (empty ?? bad) !== '' ||
            (empty || joined) !== joined || (joined && empty) !== '') { bad; }",
    )
    .unwrap();
    let mut realm = Realm::new();
    {
        let source = String::from(
            r"const literal = '\uD800\0a', joined = literal + '\uDC00';
              const pair = '\uD800' + '\uDC00', composed = `e${'\u0301'}`;
              let made = `\0${'\uD800' + 42}\uDC00`, typename = typeof true, empty = '' + '';",
        );
        let producer = CompiledScript::compile(&source).unwrap();
        drop(source);
        assert_eq!(
            realm.instantiate(&producer).unwrap().execute(),
            Ok(Completion::Normal)
        );
    }
    normal(&mut realm, "'unrelated' + 'scratch'; `ignore ${88}`;");
    assert_eq!(
        realm.instantiate(&check).unwrap().execute(),
        Ok(Completion::Normal)
    );
    normal(
        &mut Realm::new(),
        "if (typeof literal !== 'undefined') { bad; }",
    );
}

#[test]
fn realm_string_operators_share_coercion_and_update_order() {
    let mut realm = Realm::new();
    normal(&mut realm, "let x = ' 41 ', y = '-0'; const fixed = '5';");
    normal(
        &mut realm,
        "let saved = x; if (x != 41 || x === 41 || !(x < 42) || !(x < '9') ||
         +x !== 41 || x - 1 !== 40 || 2 * x !== 82 || (x & 7) !== 1 ||
         1 / y++ !== -1 / 0 || y !== 1 || x++ !== 41 || x !== 42 ||
         --x !== 41 || saved !== ' 41 ' || '>' + saved !== '> 41 ' ||
         saved + false !== ' 41 false' || 3 + saved !== '3 41 ' ||
         saved + ('4' + '2') !== ' 41 42' || ('4' + '2') + saved !== '42 41 ') { bad; }",
    );
    immutable(&mut realm, "fixed++;", "fixed");
    normal(&mut realm, "if (fixed !== '5') { bad; }");
}

#[test]
fn saved_realm_and_transient_values_survive_nested_puts_and_growth() {
    let mut realm = Realm::new();
    normal(&mut realm, "let x = 'old'; const alias = x;");
    // The right-hand assignment grows the Realm while the old LHS is on
    // the operand stack. The assignment's result still comes from scratch.
    let long = "z".repeat(300);
    normal(&mut realm, &format!("x += (x = '{long}' + '!');"));
    normal(
        &mut realm,
        &format!("if (x !== 'old{long}!' || alias !== 'old') {{ bad; }}"),
    );
    normal(
        &mut realm,
        "x = 'A'; let rendered = `${x}:${`${x}${(x = 'B' + 'C')}${x}`}:${x}`;
         let both = ('L' + x) + (x = 'R' + 'S');
         if (rendered !== 'A:ABCBC:BC' || both !== 'LBCRS' || x !== 'RS') { bad; }",
    );
    normal(
        &mut realm,
        "if (rendered !== 'A:ABCBC:BC' || both !== 'LBCRS' || alias !== 'old') { bad; }",
    );
    assert_eq!(
        run(&mut realm, "x = 'written'; while (true) {}"),
        Err(EngineFault::Evaluation(EvalError::DispatchLimitExceeded))
    );
    normal(&mut realm, "if (x !== 'written') { bad; }");
}

#[test]
fn per_execute_copy_limit_is_inclusive_and_failed_init_leaves_tdz() {
    let mut realm = Realm::new();
    normal(&mut realm, "let x, i, effect = 0;");
    let text = "a".repeat(2048);
    // Eight copies total exactly 16,384; overwrite each time. All are
    // scratch Strings, so the older append budget is not the limiting one.
    let exact = format!("i = 0; while (i < 8) {{ x = '{text}' + ''; i++; }}");
    let source = format!("{exact} let failed = (effect = 1, 'z'), next = 9;");
    assert_eq!(
        run(&mut realm, &source),
        Err(EngineFault::StringCopyLimitExceeded {
            span: span(&source, "failed")
        })
    );
    reference(&mut realm, "failed;", "failed");
    reference(&mut realm, "next;", "next");
    normal(
        &mut realm,
        "if (x.length !== 2048 || i !== 8 || effect !== 1) { bad; } x = 'fresh execution';",
    );
    normal(&mut Realm::new(), "let x = 'fresh Realm';");
}

#[test]
fn failed_compound_put_preserves_rhs_assignment_and_old_alias() {
    let mut realm = Realm::new();
    normal(&mut realm, "let x = 'a', fill, i = 0; const alias = x;");
    let text = "z".repeat(2048);
    // 7 * 2048 + 2047 + 1 = 16384. The RHS PutValue succeeds, then
    // the compound put of old 'a' + current 'b' fails its copy check.
    let source = format!(
        "while (i < 7) {{ fill = '{text}'; i++; }} fill = '{}'; x += (x = 'b');",
        &text[..2047]
    );
    let at = source.find("x +=").unwrap();
    assert_eq!(
        run(&mut realm, &source),
        Err(EngineFault::StringCopyLimitExceeded {
            span: Span {
                start: at,
                end: at + 1
            }
        })
    );
    normal(
        &mut realm,
        "if (x !== 'b' || alias !== 'a' || fill.length !== 2047 || i !== 7) { bad; }",
    );
}

#[test]
fn lifetime_retention_limit_is_inclusive_and_not_reset_by_overwrite_or_failure() {
    let mut realm = Realm::new();
    normal(&mut realm, "let x, i, effect; const fixed = 7;");
    let text = "a".repeat(2048);
    let exact = format!("i = 0; while (i < 8) {{ x = '{text}'; i++; }}");
    normal(&mut realm, &exact);
    normal(&mut realm, &exact); // 32,768 committed in two runs.
    normal(
        &mut realm,
        "let alias = x, name = typeof x, empty = '' + ''; x = 0;",
    );
    for effect in [1, 2] {
        let source = format!("x = (effect = {effect}, 'z');");
        assert_eq!(
            run(&mut realm, &source),
            Err(EngineFault::StringRetentionLimitExceeded {
                span: span(&source, "x")
            })
        );
        normal(
            &mut realm,
            &format!("if (x !== 0 || effect !== {effect}) {{ bad; }}"),
        );
    }
    normal(
        &mut realm,
        "x = alias; if (x.length !== 2048 || x !== alias || name !== 'string' || empty !== '') { bad; }",
    );
    normal(
        &mut realm,
        "undefined = 'ignored' + 'temporary'; fixed ||= 'skipped'; false && (x = 'skipped');",
    );
    let strict_readonly = "'use strict'; undefined = (effect = 3, 'not copied');";
    immutable(&mut realm, strict_readonly, "undefined");
    let strict_missing = "'use strict'; absent = (effect = 4, alias);";
    reference(&mut realm, strict_missing, "absent");
    normal(
        &mut realm,
        "'use strict'; let strict_alias = alias; if (strict_alias.length !== 2048 || effect !== 4 || undefined !== void 0) { bad; }",
    );
    immutable(&mut realm, "fixed = 'discarded';", "fixed");
    assert_eq!(
        run(&mut realm, "later = 'discarded'; let later;"),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: Span { start: 0, end: 5 },
        }))
    );
    normal(&mut Realm::new(), "let x = 'still possible';");
}

#[test]
fn assignment_keep_charges_each_copy_and_copy_wins_if_both_caps_are_full() {
    let mut realm = Realm::new();
    normal(&mut realm, "let x, y, i;");
    let text = "c".repeat(2048);
    let exact = format!("i = 0; while (i < 4) {{ x = y = '{text}'; i++; }}");
    normal(&mut realm, &exact);
    let source = format!("{exact} x = 'z';");
    assert_eq!(
        run(&mut realm, &source),
        Err(EngineFault::StringCopyLimitExceeded {
            span: span(&source, "x")
        })
    );
    normal(
        &mut realm,
        "if (x.length !== 2048 || x !== y || i !== 4) { bad; }",
    );
    let source = "x = 'z';";
    assert_eq!(
        run(&mut realm, source),
        Err(EngineFault::StringRetentionLimitExceeded {
            span: span(source, "x")
        })
    );
}

#[test]
fn existing_compare_convert_and_append_budgets_still_stop_realm_reads() {
    let mut realm = Realm::new();
    let text = " ".repeat(4093);
    normal(&mut realm, &format!("const long = '{text}1'; let i;"));
    normal(&mut realm, "i = 0; while (i < 16) { long === long; i++; }");
    assert_eq!(
        run(&mut realm, "i = 0; while (i < 17) { long === long; i++; }"),
        Err(EngineFault::Evaluation(
            EvalError::StringComparisonLimitExceeded
        ))
    );
    assert_eq!(
        run(&mut realm, "i = 0; while (i < 100) { +long; i++; }"),
        Err(EngineFault::Evaluation(
            EvalError::StringNumericConversionLimitExceeded
        ))
    );
    // Length checking still happens during +, not during the rejected store.
    assert_eq!(
        run(&mut realm, "long = long + '!';"),
        Err(EngineFault::Evaluation(
            EvalError::StringConcatenationLengthLimitExceeded
        ))
    );
    assert_eq!(
        run(&mut realm, "i = 0; while (i < 9) { long + ''; i++; }"),
        Err(EngineFault::Evaluation(
            EvalError::StringConcatenationBudgetExceeded
        ))
    );
    normal(
        &mut realm,
        "if (i !== 8 || long.length !== 4094 || +long !== 1) { bad; }",
    );
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
        "'use strict'",
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
fn realm_keeps_shared_statement_and_lexer_guards() {
    let mut realm = Realm::new();
    normal(&mut realm, "let n = 0;");
    normal(&mut realm, "if (true) if (false) n = missing; else n = 2;");
    normal(&mut realm, "if (n !== 2) missing;");
    assert!(matches!(
        CompiledScript::compile("if (false) let x = 1;"),
        Err(CompileError::Parse(_))
    ));
    let Err(CompileError::Parse(error)) = CompiledScript::compile(" #!\nlet x = 1;") else {
        panic!("misplaced hashbang must be a parse diagnostic");
    };
    assert_eq!(error.parse_error_type(), Some(ParseErrorType::SyntaxError));
    assert_eq!(error.span(), Span { start: 1, end: 2 });
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
