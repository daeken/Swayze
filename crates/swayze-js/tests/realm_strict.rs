//! Synthetic tests of opt-in Realm Scripts, not standard harness cases.

use swayze_js::{
    Error, ErrorKind, EvalError, Limit, Limits, ParseErrorType, Span, parse_script,
    parse_script_for_completion, parse_script_for_completion_with_limits, parse_script_with_limits,
    realm::{
        CompileError, CompileLimit, CompiledScript, Completion, EngineFault, InstantiateError,
        Realm, Throw,
    },
};

fn span(source: &str, needle: &str) -> Span {
    let start = source.find(needle).unwrap();
    Span {
        start,
        end: start + needle.len(),
    }
}

fn run(realm: &mut Realm, source: &str) -> Result<Completion, EngineFault> {
    let code = CompiledScript::compile(source).unwrap();
    realm.instantiate(&code).unwrap().execute()
}

fn normal(realm: &mut Realm, source: &str) {
    assert_eq!(run(realm, source), Ok(Completion::Normal), "{source}");
}

fn type_error(realm: &mut Realm, source: &str, name: &str) {
    assert_eq!(
        run(realm, source),
        Ok(Completion::Throw(Throw::TypeError {
            span: span(source, name)
        })),
        "{source}"
    );
}

fn reference_error(realm: &mut Realm, source: &str, name: &str) {
    assert_eq!(
        run(realm, source),
        Ok(Completion::Throw(Throw::ReferenceError {
            span: span(source, name)
        })),
        "{source}"
    );
}

fn parse_error(source: &str) -> Error {
    let CompileError::Parse(error) = CompiledScript::compile(source).unwrap_err() else {
        panic!("expected a shared parse error for {source}");
    };
    error
}

fn diagnostic(error: Error) -> (ErrorKind, Span, &'static str, Option<ParseErrorType>) {
    (
        error.kind(),
        error.span(),
        error.message(),
        error.parse_error_type(),
    )
}

#[test]
fn mode_is_owned_by_detached_code_not_the_realm() {
    let mut left = Realm::new();
    let mut right = Realm::new();
    let source = String::from(r#""use strict"; undefined = 47;"#);
    let strict = CompiledScript::compile(&source).unwrap();
    let expected = Ok(Completion::Throw(Throw::TypeError {
        span: span(&source, "undefined"),
    }));
    drop(source);
    let sloppy = CompiledScript::compile("undefined = 47;").unwrap();
    for realm in [&mut left, &mut right] {
        assert_eq!(
            realm.instantiate(&sloppy).unwrap().execute(),
            Ok(Completion::Normal)
        );
        assert_eq!(realm.instantiate(&strict).unwrap().execute(), expected);
        assert_eq!(
            realm.instantiate(&sloppy).unwrap().execute(),
            Ok(Completion::Normal)
        );
        assert_eq!(realm.instantiate(&strict).unwrap().execute(), expected);
    }
}

#[test]
fn exact_only_in_the_root_directive_prologue() {
    let mut realm = Realm::new();
    normal(&mut realm, "'use strict';");
    normal(&mut realm, "\"use strict\" /* comment */\n;");
    for head in [
        "'use strict';",
        "'local'; \"use strict\"; 'use strict';",
        "#! boot\r\n// hi\u{2028} 'local'; /* ok */ \"use strict\"\n;",
        r"'use\x20strict'; 'use strict';",
    ] {
        let source = format!("{head} undefined = 1;");
        type_error(&mut realm, &source, "undefined");
    }
    // A bare quoted directive can remain in the prologue without activating.
    for head in [
        r"'use\x20strict';",
        r#""\u0075se strict";"#,
        "'use\\\n strict';",
        "'Use Strict';",
    ] {
        normal(&mut realm, &format!("{head} undefined = 1;"));
    }
    // Each nondirective must also prevent a subsequent exact spelling from activating.
    for head in [
        "('use strict');",
        "'use strict'.length;",
        "'use strict' /* line\n */ .length;",
        "'use strict'\n + 'more';",
        "'use strict'\n || 'more';",
        "'use strict'\n === '';",
        "'use strict'\n ? 1 : 2;",
        "'use strict'\n , 1;",
        "`use strict`;",
        "`${'use strict'}`;",
        ";",
        "0;",
        "{ 'use strict'; }",
        "label: { 'use strict'; }",
        "if (true) 'use strict'; else 'use strict';",
        "while (false) { 'use strict'; }",
    ] {
        normal(&mut realm, &format!("{head} 'use strict'; undefined = 1;"));
    }
    for source in [
        "'use strict'",
        "'use strict'\nundefined = 1;",
        "'\\8'; 'use strict'",
    ] {
        let at = source.find("undefined").map_or(
            Span {
                start: source.len(),
                end: source.len(),
            },
            |_| span(source, "undefined"),
        );
        assert_eq!(
            diagnostic(parse_error(source)),
            (
                ErrorKind::Unsupported,
                at,
                "expression statements require explicit semicolons",
                None
            )
        );
    }
    // Same-line decrement followed by '>' is not an HTML closing comment.
    normal(
        &mut realm,
        "'use strict'; let n = 0o2; if (!(n-->0 && n === 1)) missing;",
    );
}

#[test]
fn strict_escape_witness_is_the_first_quoted_escape_in_source_order() {
    for quote in ['\'', '"'] {
        for digit in ['8', '9'] {
            let escape = format!("\\{digit}");
            let literal = format!("{quote}😀{escape}suffix{quote};");
            for source in [
                format!("'local'; {literal} 'another'; 'use strict'; '\\8';"),
                format!("'use strict'; {literal} '\\9';"),
            ] {
                assert_eq!(
                    CompiledScript::compile(&source).unwrap_err(),
                    CompileError::SyntaxError {
                        span: span(&source, &escape)
                    },
                    "{source}"
                );
            }
            normal(&mut Realm::new(), &literal);
        }
    }
    for (source, escape) in [
        (r#"'use strict'; if (false) "dead\9";"#, r"\9"),
        (r#"'use strict'; false && '\8';"#, r"\8"),
        (r#"'use strict'; true ? 4 : '\9';"#, r"\9"),
        (
            r#"'use strict'; false && `head${`inner${"😀\8"}tail`}rest`;"#,
            r"\8",
        ),
    ] {
        assert_eq!(
            CompiledScript::compile(source).unwrap_err(),
            CompileError::SyntaxError {
                span: span(source, escape)
            },
            "{source}"
        );
    }
    let mut realm = Realm::new();
    normal(
        &mut realm,
        r#"'use strict'; // \8 \9
        if ('89' !== '\x38\u0039' || '\\8'.length !== 2 || '\0'.length !== 1 || `89\\9${'89'}`.length !== 6) missing;
        /* \9 */ '7';"#,
    );
    normal(
        &mut realm,
        r#"'start\9'; (`nondirective`); 'use strict'; if ("\8\9" !== '89') missing; undefined = 7;"#,
    );
}

#[test]
fn strict_escape_validation_preserves_shared_errors_but_precedes_realm_lowering() {
    let good = r"'x8'; 'use strict'; ";
    let witnessed = r"'\8'; 'use strict'; ";
    for (tail, needle, kind, message) in [
        (
            r"'\xg';",
            r"\xg",
            ErrorKind::Malformed,
            "expected a hexadecimal digit in string escape",
        ),
        (
            r"`raw\9`;",
            r"\9",
            ErrorKind::Malformed,
            "invalid decimal or octal template escape",
        ),
        (
            r"'\7';",
            r"\7",
            ErrorKind::Unsupported,
            "Annex B decimal and octal string escapes are outside the subset",
        ),
        (
            r"'\08';",
            r"\08",
            ErrorKind::Unsupported,
            "Annex B decimal and octal string escapes are outside the subset",
        ),
        (
            "09;",
            "9",
            ErrorKind::Unsupported,
            "leading-zero numeric syntax is outside the subset",
        ),
        (
            "eval = 3;",
            "eval",
            ErrorKind::Unsupported,
            "reserved or excluded name is outside the subset",
        ),
        (
            "#! after\n",
            "#",
            ErrorKind::Unsupported,
            "token is outside the arithmetic subset",
        ),
        (
            r"'tag'`\9`;",
            "`",
            ErrorKind::Unsupported,
            "token is outside the arithmetic subset",
        ),
    ] {
        let source = format!("{witnessed}{tail}");
        let span = span(&source, needle);
        let error = parse_error(&source);
        assert_eq!(diagnostic(error), (kind, span, message, None), "{source}");
        assert_eq!(parse_error(&format!("{good}{tail}")), error);
    }
    assert_eq!(
        diagnostic(parse_error(" #! early\n'use strict';")),
        (
            ErrorKind::Unsupported,
            Span { start: 1, end: 2 },
            "token is outside the arithmetic subset",
            Some(ParseErrorType::SyntaxError)
        )
    );
    let long_literal = format!("{witnessed}'{}';", "q".repeat(4095));
    let edge = witnessed.len() + 4096;
    assert_eq!(
        diagnostic(parse_error(&long_literal)),
        (
            ErrorKind::LimitExceeded(Limit::StringLiteralBytes),
            Span {
                start: edge,
                end: edge + 1
            },
            "quoted string literal byte ceiling exceeded (including quotes)",
            None
        )
    );
    let duplicate = "let x; let x;";
    let at = good.len() + duplicate.rfind('x').unwrap();
    assert_eq!(
        CompiledScript::compile(&format!("{good}{duplicate}")).unwrap_err(),
        CompileError::SyntaxError {
            span: Span {
                start: at,
                end: at + 1
            }
        }
    );
    let name = "q".repeat(257);
    assert_eq!(
        CompiledScript::compile(&format!("{good}{name};")).unwrap_err(),
        CompileError::LimitExceeded {
            limit: CompileLimit::NameBytes,
            span: Span {
                start: good.len(),
                end: good.len() + name.len()
            }
        }
    );
    let updates = "x++;".repeat(1025);
    let end = good.len() + updates.len();
    assert_eq!(
        CompiledScript::compile(&format!("{good}{updates}")).unwrap_err(),
        CompileError::LimitExceeded {
            limit: CompileLimit::Instructions,
            span: Span { start: end, end }
        }
    );
    for tail in [duplicate, &format!("{name};"), &updates] {
        let source = format!("{witnessed}{tail}");
        assert_eq!(
            CompiledScript::compile(&source).unwrap_err(),
            CompileError::SyntaxError {
                span: span(&source, r"\8")
            }
        );
    }
}

#[test]
fn legacy_guard_span_phase_and_work_threshold_do_not_move() {
    // The first statement costs 8 credits including its discard. The
    // second costs 24 through its semicolon, before looking into the body.
    let source = r#"'\9';"use strict";'\xg';"#;
    let legacy = (
        ErrorKind::Unsupported,
        span(source, "\"use strict\""),
        "Use Strict Directives are outside the subset",
        None,
    );
    assert_eq!(diagnostic(parse_script(source).unwrap_err()), legacy);
    assert_eq!(
        diagnostic(parse_script_for_completion(source).unwrap_err()),
        legacy
    );
    for (work, expected) in [
        (
            31,
            (
                ErrorKind::LimitExceeded(Limit::Work),
                Span { start: 17, end: 18 },
                "parse work ceiling exceeded",
                None,
            ),
        ),
        (32, legacy),
    ] {
        let limits = Limits {
            max_work: work,
            ..Limits::default()
        };
        assert_eq!(
            diagnostic(parse_script_with_limits(source, limits).unwrap_err()),
            expected
        );
        assert_eq!(
            diagnostic(parse_script_for_completion_with_limits(source, limits).unwrap_err()),
            expected
        );
    }
    let source = "'use strict' /* eof */";
    assert_eq!(
        diagnostic(parse_script(source).unwrap_err()),
        (legacy.0, span(source, "'use strict'"), legacy.2, None)
    );
    assert_eq!(
        parse_script_for_completion(source).unwrap_err(),
        parse_script(source).unwrap_err()
    );
}

#[test]
fn put_errors_follow_rhs_preserve_prior_effects_and_stop_later_effects() {
    let mut realm = Realm::new();
    normal(&mut realm, "let effect = 0;");
    type_error(
        &mut realm,
        "'use strict'; effect = 1; undefined = (effect += 2); effect = 0;",
        "undefined",
    );
    normal(&mut realm, "if (effect !== 3) bad;");
    reference_error(
        &mut realm,
        "'use strict'; absent = (effect = 4); effect = 0;",
        "absent",
    );
    normal(
        &mut realm,
        "if (effect !== 4 || typeof absent !== 'undefined') bad;",
    );
    let source = "absent = (effect = 5);";
    assert_eq!(
        run(&mut realm, source),
        Err(EngineFault::ImplicitGlobal {
            span: span(source, "absent")
        })
    );
    normal(
        &mut realm,
        "if (effect !== 5 || typeof absent !== 'undefined') bad;",
    );
    for source in [
        "'use strict'; undefined = badRhs;",
        "'use strict'; absent = badRhs;",
    ] {
        reference_error(&mut realm, source, "badRhs");
    }
    reference_error(
        &mut realm,
        "'use strict'; absent += (effect = 42);",
        "absent",
    );
    reference_error(
        &mut realm,
        "'use strict'; absent ??= (effect = 42);",
        "absent",
    );
    reference_error(&mut realm, "'use strict'; ++absent;", "absent");
    normal(&mut realm, "if (effect !== 5) bad;");
    type_error(
        &mut realm,
        "'use strict'; undefined += `text${(effect = 6)}`;",
        "undefined",
    );
    type_error(
        &mut realm,
        "'use strict'; undefined = 'literal';",
        "undefined",
    );
    normal(&mut realm, "undefined = 'literal'; if (effect !== 6) bad;");
    for source in [
        "'use strict'; undefined *= (1).length;",
        "'use strict'; absent = (1).length;",
    ] {
        assert_eq!(
            run(&mut realm, source),
            Err(EngineFault::Evaluation(
                EvalError::UnsupportedPropertyReceiver
            ))
        );
    }
}

#[test]
fn strict_stores_use_the_same_reached_paths_for_updates_and_short_circuiting() {
    let mut realm = Realm::new();
    normal(&mut realm, "let n = 0;");
    normal(
        &mut realm,
        "'use strict'; undefined &&= absent; false && (undefined ??= absent); if (true) n++; else absent = 3; for (; false; ++undefined) {} true ? n : (undefined = 2);",
    );
    for body in [
        "undefined ??= (n += 1);",
        "if (undefined ||= (n += 1)) bad;",
        "++undefined;",
        "undefined--;",
        "if (++undefined) bad;",
        "if (undefined++) bad;",
        "for (++undefined; false;) {}",
        "for (; n < 4; undefined++) { n += 1; }",
        "false ? 0 : (undefined = 1);",
    ] {
        type_error(&mut realm, &format!("'use strict'; {body}"), "undefined");
    }
    normal(&mut realm, "if (n !== 4) bad;");
    normal(&mut realm, "'use strict'; if (n++ !== 4 || ++n !== 6) bad;");
    assert_eq!(
        run(
            &mut realm,
            "'use strict'; n = 8; while (true) {} undefined = 1;"
        ),
        Err(EngineFault::Evaluation(EvalError::DispatchLimitExceeded))
    );
    normal(&mut realm, "if (n !== 8) bad;");
}

#[test]
fn strict_keeps_read_tdz_const_and_preflight_outcomes() {
    let mut realm = Realm::new();
    normal(&mut realm, "'use strict'; const fixed = 31;");
    let code = CompiledScript::compile("'use strict'; let dropped = 19;").unwrap();
    drop(realm.instantiate(&code).unwrap());
    let source = "'use strict'; let skipped = 7, fixed = 3;";
    let code = CompiledScript::compile(source).unwrap();
    assert_eq!(
        realm.instantiate(&code).unwrap_err(),
        InstantiateError::SyntaxError {
            span: span(source, "fixed")
        }
    );
    normal(
        &mut realm,
        "'use strict'; if (typeof skipped !== 'undefined' || fixed !== 31) bad;",
    );
    reference_error(&mut realm, "'use strict'; skipped;", "skipped");
    reference_error(&mut realm, "'use strict'; typeof dropped;", "dropped");
    reference_error(&mut realm, "'use strict'; dropped = 5;", "dropped");
    reference_error(&mut realm, "'use strict'; typeof (0, absent);", "absent");
    type_error(&mut realm, "'use strict'; fixed = 'text';", "fixed");
    normal(&mut realm, "'use strict'; fixed ||= absent;");
}
