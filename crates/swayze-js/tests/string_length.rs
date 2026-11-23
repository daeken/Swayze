use swayze_js::{
    ErrorKind, EvalError, EvalScratch, Limit, Limits, PrimitiveValue, Span, Value, parse_script,
    parse_script_with_limits,
};

fn assert_number(source: &str, expected: f64) {
    let script = parse_script(source).unwrap_or_else(|error| panic!("{source:?}: {error}"));
    assert_eq!(script.evaluate(), Ok(expected), "{source:?}");
}

fn assert_error(source: &str, kind: ErrorKind, start: usize, width: usize) {
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source:?}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + width
        },
        "{source:?}: {error}"
    );
}

#[test]
fn lengths_count_exact_code_units_without_coercion() {
    for (source, expected) in [
        ("''.length", 0.0),
        ("'Aé😀'.length", 4.0),
        (r"'\u{1f600}\uD800\0\uDC00'.length", 5.0),
        ("'a\0b'.length", 3.0),
        ("'\u{2028}\u{2029}'.length", 2.0),
        ("'a\\\r\nb\\\u{2028}c\\\u{2029}'.length", 3.0),
        ("const length = 'abc'; length.length", 3.0),
        (
            "{ let undefined = 'abc'; if (undefined.length) {} } ''.length",
            0.0,
        ),
    ] {
        assert_number(source, expected);
    }
    let empty = parse_script("''.length").unwrap();
    assert!(empty.evaluate().unwrap().is_sign_positive());
    assert_eq!(empty.evaluate_value(), Ok(Value::Number(0.0)));
    assert_eq!(
        empty.evaluate_primitive(),
        Ok(PrimitiveValue::Scalar(Value::Number(0.0)))
    );
}

#[test]
fn suffix_binds_to_primary_not_pending_unary_binary_or_power() {
    for (source, expected) in [
        ("-(((('abc')))).length + 2", -1.0),
        ("2 ** -('ab').length", 0.25),
        ("'abc'.length ** 2 ** ('ab').length", 81.0),
        ("(-'abc'.length) ** 2", 9.0),
        ("-('abc'.length ** 2)", -9.0),
        ("1 + ('x' + 'yz').length * 2", 7.0),
        ("(false ? null : ('x', '😀')).length", 2.0),
        ("((null ?? 'ab')).length", 2.0),
        ("let s; ((s = 'ab', s += '😀')).length + s.length", 8.0),
        ("let i=0; let s='abc'; (i++, s).length + i", 4.0),
        ("let i=1; ++i ** 'ab'.length + i++ + i", 9.0),
        ("~'abc'.length", -4.0),
    ] {
        assert_number(source, expected);
    }
    assert_eq!(
        parse_script("!''.length").unwrap().evaluate_value(),
        Ok(Value::Boolean(true))
    );
    assert_eq!(
        parse_script("void ('x' + 'y').length")
            .unwrap()
            .evaluate_value(),
        Ok(Value::Undefined)
    );
    for source in ["-'ab'.length ** 2", "void ('ab').length ** 2"] {
        assert_error(source, ErrorKind::Malformed, source.find("**").unwrap(), 2);
    }
}

#[test]
fn cached_name_lookahead_and_assignment_and_update_dispatch_are_preserved() {
    assert_number(
        "let s='abc', i=0;
         s.length;
         { s.length; }
         for (s.length; (s).length && i < 2; s.length, i++) {
             if ((s).length) { continue; }
         }
         do { s.length; } while (i--, ''.length);
         while ((s).length && i) { --i; }
         s.length + i",
        3.0,
    );
    let plain = parse_script("let s='abc', i=0; s; {s;} for(s; i<2; s, i++){i+=0;} s").unwrap();
    let suffixed = parse_script(
        "let s='abc', i=0; s.length; {s.length;} for(s.length; i<2; s.length, i++){i+=0;} s.length",
    )
    .unwrap();
    assert_eq!(suffixed.instruction_count(), plain.instruction_count() + 5);
    assert_eq!(suffixed.max_stack_depth(), plain.max_stack_depth());
    assert_eq!(suffixed.evaluate(), Ok(3.0));
}

#[test]
fn receiver_errors_are_dynamic_but_syntax_and_names_are_not() {
    for source in [
        "null.length",
        "true.length",
        "undefined.length",
        "(1).length",
        "1 .length",
        "0x1 .length",
        "(-'abc').length",
        "('ab'.length).length",
        "let s='x'; s=null; s.length",
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Err(EvalError::UnsupportedPropertyReceiver),
            "{source}"
        );
    }
    assert_number(
        "false && null.length; true || undefined.length; 1 ?? true.length; false ? (1).length : 'ab'.length",
        2.0,
    );
    assert_number(
        "if (false) { null.length; } for (; false; null.length) {} 42",
        42.0,
    );
    assert_number("let x=1; x ||= null.length; x", 1.0);
    assert_number("let x=0; false && (x=x+1).length; x", 0.0);
    assert_error("false && absent.length", ErrorKind::Unsupported, 9, 6);
    assert_error(
        "false && (s.length), 0; let s='x'; 0",
        ErrorKind::Unsupported,
        10,
        1,
    );
    let reached = parse_script("null.length; 1").unwrap();
    assert_eq!(
        reached.evaluate_primitive(),
        Err(EvalError::UnsupportedPropertyReceiver)
    );
    assert_eq!(
        reached.evaluate_primitive_with_scratch(&mut EvalScratch::new()),
        Err(EvalError::UnsupportedPropertyReceiver)
    );
}

#[test]
fn property_targets_and_ineligible_dots_are_static_errors() {
    for op in [
        "=", "+=", "-=", "*=", "/=", "%=", "**=", "&=", "^=", "|=", "<<=", ">>=", ">>>=", "&&=",
        "||=", "??=", "++", "--",
    ] {
        let prefix = "let s='x'; false && (s.length ";
        let rhs = if op == "++" || op == "--" { "" } else { " 1" };
        assert_error(
            &format!("{prefix}{op}{rhs})"),
            ErrorKind::Unsupported,
            prefix.len(),
            op.len(),
        );
    }
    for source in [
        "let s='x'; ++s.length; 0",
        "let s='x'; s++.length; 0",
        "let s='x'; true || ++s.length",
        "let s='x'; true || s++.length",
        "let s='x'; for (++s.length; false;) {} 0",
        "let s='x'; for (; false; s++.length) {} 0",
        "'x'..length",
        ".length",
        "! .length",
        "if (true).length {} 0",
        "while (false).length {} 0",
        "do {} while (false).length; 0",
        "for (;;).length {} 0",
        "for (;; 'a').length {} 0",
        "{}.length 0",
        "(1).toString()",
        "1 .toString()",
        "'x'.",
        "'x'.Length",
        "'x'.lengths",
        "'x'.length0",
        "'x'.length$",
        "'x'.length_",
        "'x'.lengthé",
        "'x'.length\u{200c}",
        r"'x'.leng\u0074h",
        r"'x'.length\u0061",
        r"'x'.\u006cength",
        "'x'.len/* trivia cannot join tokens */gth",
        "'x'.'length'",
        "'x'.true",
    ] {
        assert_error(source, ErrorKind::Unsupported, source.find('.').unwrap(), 1);
    }
    for (source, start, width) in [
        ("'x'.length.length", 10, 1),
        ("'x'?.length", 3, 2),
        ("'x'?.['length']", 3, 2),
        ("'x'['length']", 3, 1),
        ("'x'.length()", 10, 1),
        ("1.length", 2, 1),
        ("1..length", 2, 1),
        ("1.5.length", 3, 1),
        ("1e2.length", 3, 1),
        ("0x1.length", 3, 1),
        (".5.length", 2, 1),
    ] {
        assert_error(source, ErrorKind::Unsupported, start, width);
    }
    assert_number("true?.5:0", 0.5);
    assert_number("false ? .5 : 'ab'.length", 2.0);
    assert_error("'x'.5", ErrorKind::Malformed, 3, 2);
}

#[test]
fn only_eligible_dots_probe_suffix_trivia_before_reporting_unsupported() {
    let at_dot = |source: &str, limits: Limits| {
        let start = source.find('.').unwrap();
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported, "{source:?}");
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + 1
            },
            "{source:?}"
        );
        assert_eq!(
            error.message(),
            "token is outside the arithmetic subset",
            "{source:?}"
        );
    };
    for source in [
        "(1).toString()",
        "(1)./* suffix trivia */ toString()",
        "let s='x'; s./* suffix trivia */ length2",
        "(1).other/* not needed to reject other",
        "++s./*",
        "s++./*",
        "let s='x'; false && ++s./*",
        "let s='x'; false && s++./*",
        "for (;; 'a')./*",
        "if (true)./*",
        "while (false)./*",
        "do {} while (false)./*",
    ] {
        at_dot(source, Limits::default());
    }
    // Only the two bytes up to and including the dot fit. The old dot
    // diagnostic still wins over later lookahead or the name emission.
    let two_bytes = Limits {
        max_work: 2,
        ..Limits::default()
    };
    at_dot("s.other", two_bytes);
    at_dot(
        "s./* done */other",
        Limits {
            max_work: 12,
            ..Limits::default()
        },
    );
    at_dot(
        "++s./*",
        Limits {
            max_work: 4,
            ..Limits::default()
        },
    );
    let work = parse_script_with_limits("s./*", two_bytes).unwrap_err();
    assert_eq!(work.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(work.span(), Span { start: 2, end: 3 });
    let comment = parse_script("(1)./*").unwrap_err();
    assert_eq!(comment.kind(), ErrorKind::Malformed);
    assert_eq!(comment.span(), Span { start: 4, end: 6 });
    assert_eq!(comment.message(), "unterminated block comment");
}

#[test]
fn trivia_continues_the_read_and_a_continued_string_ends_the_prologue() {
    for line in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        assert_number(&format!("'use strict'{line}.{line}length"), 10.0);
        assert_number(
            &format!("'x'// before{line}./* after {line} */ length"),
            1.0,
        );
        assert_number(&format!("'x'/* before {line} */.// after{line}length"), 1.0);
        let source = format!("'use strict'{line}.length; 'use strict'");
        let script = parse_script(&source).unwrap();
        let units: Vec<_> = "use strict".encode_utf16().collect();
        assert_eq!(
            script.evaluate_primitive(),
            Ok(PrimitiveValue::String(&units))
        );
    }
    assert_error("'use strict'; 'x'.length", ErrorKind::Unsupported, 0, 12);
    assert_error("'x'; 'use strict'; 0", ErrorKind::Unsupported, 5, 12);
    assert_error("'x'.length\n42", ErrorKind::Unsupported, 11, 2);
    assert_error("{'x'.length}", ErrorKind::Unsupported, 11, 1);
}

#[test]
fn runtime_aliases_and_warm_storage_keep_exact_length() {
    let script =
        parse_script(r"let s='😀'+'\uD800'; const old=s; s+=s; s+='\0'; old.length*10+s.length")
            .unwrap();
    assert_eq!(script.evaluate(), Ok(37.0));
    let shorter = parse_script("let s='' + 'é'; s.length*10+(''+'').length").unwrap();
    let mut scratch = EvalScratch::new();
    for (script, expected) in [(&script, 37.0), (&shorter, 10.0), (&script, 37.0)] {
        assert_eq!(
            script.evaluate_primitive_with_scratch(&mut scratch),
            Ok(PrimitiveValue::Scalar(Value::Number(expected)))
        );
    }
    let max = "a".repeat(4094);
    // 100 reads at maximum length would exceed every String budget if each
    // read charged even one unit per code unit.
    assert_number(
        &format!("const s='{max}';let i=100;while(i){{s.length;i--;}}s.length"),
        4094.0,
    );
}

#[test]
fn exact_source_token_work_and_nesting_limits() {
    let source = "'😀'\u{2028}. /*é*/length";
    let dot = source.find('.').unwrap();
    let name = source.find("length").unwrap();
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 3,
        max_work: source.len() + 2 + 2, // scanned bytes, UTF-16 output, two emissions
        max_nesting_depth: 0,
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 2);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate(), Ok(2.0));
    for (limits, kind, span) in [
        (
            Limits {
                max_source_bytes: source.len() - 1,
                ..limits
            },
            Limit::SourceBytes,
            (source.len() - 1, 1),
        ),
        (
            Limits {
                max_tokens: 1,
                ..limits
            },
            Limit::Tokens,
            (dot, 1),
        ),
        (
            Limits {
                max_tokens: 2,
                ..limits
            },
            Limit::Tokens,
            (name, 1),
        ),
        (
            Limits {
                max_work: limits.max_work - 2,
                ..limits
            },
            Limit::Work,
            (source.len() - 1, 1),
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            (dot, 1),
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(kind));
        assert_eq!(
            error.span(),
            Span {
                start: span.0,
                end: span.0 + span.1
            }
        );
    }
    let grouped = format!("{}'x'{}.length", "(".repeat(128), ")".repeat(128));
    assert_number(&grouped, 1.0);
    let error = parse_script_with_limits(
        &grouped,
        Limits {
            max_nesting_depth: 127,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(
        error.span(),
        Span {
            start: 127,
            end: 128
        }
    );
}

#[test]
fn exact_prepared_instruction_and_dispatch_limits() {
    let prefix = "''.length;".repeat(1365);
    let limits = Limits {
        max_tokens: 6000,
        ..Limits::default()
    };
    let at_cap = parse_script_with_limits(&format!("{prefix}''"), limits).unwrap();
    assert_eq!(at_cap.instruction_count(), 4096);
    assert_eq!(at_cap.evaluate_primitive(), Ok(PrimitiveValue::String(&[])));
    let over_cap = parse_script_with_limits(&format!("{prefix}''.length"), limits).unwrap();
    assert_eq!(over_cap.instruction_count(), 4097);
    assert_eq!(over_cap.evaluate(), Err(EvalError::LimitExceeded));

    // Seven dispatches per iteration plus two for initialization and two for
    // the falsy test. Seven dispatches remain on exit, no exit dispatch.
    let prefix = "let i=2339; while(i){i=i-1;} ";
    assert_number(&format!("{prefix}+ + + + + ''.length"), 0.0);
    for (tail, expected) in [
        (
            "+ + + + + (null.length)",
            EvalError::UnsupportedPropertyReceiver,
        ),
        ("+ + + + + + ''.length", EvalError::DispatchLimitExceeded),
        // Four unary dispatches plus the Number push and a discard, then the
        // Null push. The read would be dispatch 16,385.
        ("+ + + + 0, null.length", EvalError::DispatchLimitExceeded),
    ] {
        let script = parse_script(&format!("{prefix}{tail}")).unwrap();
        assert_eq!(script.evaluate(), Err(expected), "{tail}");
    }
}
