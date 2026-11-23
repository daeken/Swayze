use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, Span, Value, parse_script, parse_script_with_limits,
};

#[test]
fn initializes_once_and_orders_test_body_and_update() {
    let script = parse_script(
        "let i=99; let trace=0;
         for (i=(null ?? 1); i<4 && trace<20; i=i+(trace ? 1 : 10)) {
             trace=trace*10+i;
         }
         trace*10+i",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(1234.0));
    assert_eq!(script.evaluate(), Ok(1234.0));

    for test in ["false", "0", "-0", "0/0", "null", "undefined"] {
        let source = format!("let i=99;for(i=3;{test};i=4){{i=5;}}i");
        assert_eq!(parse_script(&source).unwrap().evaluate(), Ok(3.0));
    }
}

#[test]
fn each_clause_can_be_omitted() {
    for statement in [
        "for (;i<3;i=i+1) {}",
        "for (i=1;;i=i+1) {if(i===3){break;}}",
        "for (i=1;i<3;) {i=i+1;}",
        "for (;;i=i+1) {if(i===3){break;}continue;}",
        "for (;i<3;) {i=i+1;continue;}",
        "for (i=3;;) {break;}",
        "for (/* init */;/* test */;/* update */) {i=3;break;}",
    ] {
        let source = format!("let i=1;{statement}i");
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(3.0),
            "{source}"
        );
    }
}

#[test]
fn loop_jumps_select_the_innermost_for_or_while_across_if_blocks() {
    let script = parse_script(
        "let i=9; let j=0; let w=0; let trace=0;
         while (true) {
             w=w+1;
             for (i=1;i<4;i=i+1) {
                 j=0;
                 while (true) {j=j+1;if(j===1){continue;}break;}
                 if (i===2) {continue;}
                 trace=trace*10+i+j;
                 if (i===3) {break;}
             }
             if (w===1) {continue;}
             trace=trace*10+i;
             break;
         }
         trace*10+w",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(353532.0));

    let script = parse_script(
        "let i=0; let j=0; let n=0;
         for (;i<3;i=i+1) {
             for (j=0;j<3;j=j+1) {
                 if (j===0) {continue;}
                 n=n+1;
                 if (i===1) {break;}
             }
             if (i<2) {continue;}
             break;
         }
         n*100+i*10+j",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(523.0));
}

#[test]
fn clauses_see_enclosing_bindings_not_fresh_body_bindings() {
    let script = parse_script(
        "let i=0; let sum=0;
         if (true) {
             const step=1;
             for (i=step;i<step+3;i=i+step) {
                 const i=7;
                 let step=2;
                 sum=sum+i+step;
                 step=step+100;
                 continue;
             }
         }
         sum*10+i",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(274.0));
}

#[test]
fn let_header_has_its_own_scope_enclosing_a_distinct_body_scope() {
    let script = parse_script(
        "let i=9; let step=8; let sum=0; const seed=2;
         for (let i=seed-2, step=(i ? 99 : 2); i<5; i+=step) {
             const i=7;
             let step=3;
             sum+=i+step;
             step++;
         }
         sum*100+i*10+step",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(3098.0));
    assert_eq!(script.evaluate(), Ok(3098.0));

    // A sibling loop and the enclosing Script can both reuse the header name.
    assert_eq!(
        parse_script("for(let i;false;){} for(let i=1;i;i--){} let i=42; i")
            .unwrap()
            .evaluate(),
        Ok(42.0)
    );
}

#[test]
fn let_header_and_body_reinitialize_at_their_respective_entries() {
    let script = parse_script(
        "let trace=0;
         for (let w=0; w<2; ++w) {
             for (let i=1, step=i, spare; i<4; i+=step) {
                 let local;
                 trace=trace*10+(local ?? spare ?? i);
                 local=9;
                 if (i<3) { continue; }
                 spare=8;
                 break;
             }
         }
         trace",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(123123.0));
    assert_eq!(script.evaluate(), Ok(123123.0));
}

#[test]
fn let_headers_support_omitted_clauses_and_the_existing_jump_rules() {
    for (statement, expected) in [
        (
            "for(let i=0;;seen=i){if(i===2){i=99;break;}i++;continue;}",
            2.0,
        ),
        ("for(let i=0;i<2;){i++;seen=i;continue;}", 2.0),
        ("for(let i=2;;){seen=i;break;}", 2.0),
        ("for(let i=2, stop=false;stop;seen=99){seen=i;}", 0.0),
    ] {
        let source = format!("let seen=0;{statement}seen");
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn loop_undefined_can_shadow_the_root_without_affecting_implicit_initialization() {
    let script = parse_script(
        "let result=0; let sentinel;
         for (let undefined=40, x, y=undefined+2; !x; ++x) {
             result=y;
             x=0;
         }
         result===42 && undefined===sentinel",
    )
    .unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Boolean(true)));
    assert_error_at("let undefined;0", ErrorKind::Malformed, "undefined");
}

#[test]
fn const_header_list_reinitializes_on_reentry_not_on_each_iteration() {
    let script = parse_script(
        "let i=1; let trace=0; let step=9;
         for (let w=0;w<2;++w) {
             for (const start=i, end=start+2, step=end-start-1; i<end; i+=step) {
                 let step=start;
                 step++;
                 trace=trace*10+step;
                 continue;
             }
         }
         trace*100+i*10+step",
    )
    .unwrap();
    // The body records 22 on the first entry and 44 on the second. The
    // update uses the header's step, not the mutable body or Script binding.
    assert_eq!(script.evaluate(), Ok(224459.0));
    assert_eq!(script.evaluate(), Ok(224459.0));
}

#[test]
fn const_headers_support_omitted_clauses_and_mutable_updates() {
    for (statement, expected) in [
        (
            "for(const n=42;;seen+=n){if(seen===n){break;}continue;}",
            42.0,
        ),
        ("for(const n=42;seen<n;){seen+=n;continue;}", 42.0),
        ("for(const n=42;;){seen=n;break;}", 42.0),
        ("for(const n=42,stop=false;stop;seen=99){seen=n;}", 0.0),
    ] {
        let source = format!("let seen=0;{statement}seen");
        assert_eq!(
            parse_script(&source).unwrap().evaluate(),
            Ok(expected),
            "{source}"
        );
    }
}

fn assert_error_at(source: &str, kind: ErrorKind, marked: &str) -> swayze_js::Error {
    // A prefix before '|' disambiguates identical tokens at different sites.
    let (prefix, needle) = marked.split_once('|').unwrap_or(("", marked));
    let start = source.rfind(&[prefix, needle].concat()).unwrap() + prefix.len();
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source}: {error}");
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + needle.len()
        },
        "{source}"
    );
    error
}

#[test]
fn checks_inactive_headers_and_bodies_with_lexical_diagnostics() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, needle) in [
        ("for(a=0;false;){let a=1;}42", Unsupported, "for(|a"),
        ("for(;flag;){let flag=0;}42", Unsupported, ";|flag"),
        (
            "let i=0;for(;false;i=only){let only=0;}42",
            Unsupported,
            "i=|only",
        ),
        ("for(;false;missing=0){}42", Unsupported, "missing"),
        ("const i=0;for(i=1;false;){}42", Unsupported, "="),
        ("const i=0;for(;false;i=1){}42", Unsupported, "="),
        (
            "let x=0;if(false){for(x=1;false;){}let x=2;}42",
            Unsupported,
            "for(|x",
        ),
        ("let i=0;for(;false;i=i+1){let a=+;}42", Malformed, ";"),
        ("for(;false;){break; x=0;}42", Unsupported, "x"),
        ("for(;false;){const a=0;let a=1;}42", Malformed, "let |a"),
        ("for(;false;){let local=1;}local", Unsupported, "local"),
        ("for(;;){break;}continue;42", Malformed, "continue"),
    ] {
        assert_error_at(source, kind, needle);
    }
}

#[test]
fn requires_braces_delimiters_and_assignment_clauses() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, needle) in [
        ("for ;", Malformed, ";"),
        ("for(){}42", Malformed, ")"),
        ("for(;;;){}42", Malformed, ";;|;"),
        ("for(0;false;){}42", Unsupported, "0"),
        ("for(i in j){}42", Unsupported, "in"),
        ("for(i of j){}42", Unsupported, "of"),
        ("for(i**=1;false;){}42", Unsupported, "**="),
        ("for(i=0,j=1;false;){}42", Unsupported, ","),
        ("for(;i=1;){}42", Unsupported, "="),
        ("for(;false;i++){}42", Unsupported, "false;|i"),
        ("for(;false;1+2){}42", Unsupported, "1"),
        ("for(;false;i+1){}42", Unsupported, "+"),
        ("for(;false;i=0,i=1){}42", Unsupported, ","),
        ("for(i=;false;){}42", Malformed, "i=|;"),
        ("for(i=0\ntrue;){}42", Malformed, "true"),
        ("for(i=0){}42", Malformed, ")"),
        ("for(;true){}42", Malformed, ")"),
        ("for(;true;i=)", Malformed, ")"),
        ("for(;true;i=1;){}42", Malformed, "i=1|;"),
        ("for(;true;i=1", Malformed, ""),
        ("for(;true;i=1{}42", Malformed, "{"),
        ("for(;;)42", Malformed, "42"),
        ("for(;;){break;}", Unsupported, ""),
        ("42;for(;;){}", Unsupported, "for"),
        ("let for=0;42", Unsupported, "for"),
        ("(for)", Unsupported, "for"),
    ] {
        assert_error_at(source, kind, needle);
    }
    assert_eq!(
        parse_script("let for0=1;let for_=2;let for$=3;for(;for0;){for0=0;}for0+for_+for$")
            .unwrap()
            .evaluate(),
        Ok(5.0)
    );
}

#[test]
fn let_header_names_are_checked_even_in_inactive_code() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, marked) in [
        ("for(let i=0;false;){}i", Unsupported, "}|i"),
        ("for(let i=0;false;){}i=7;42", Unsupported, "}|i"),
        ("for(let i;false;){}for(;i;){}0", Unsupported, ";|i"),
        ("if(false){for(let i,i=0;;){}}0", Malformed, ",|i"),
        ("let i=42;for(let i=i;false;){}0", Unsupported, "i=|i"),
        (
            "let i=42;if(false){for(let x=false&&i,i;false;){}}0",
            Unsupported,
            "&&|i",
        ),
        (
            "for(let x=undefined,undefined;false;){}0",
            Unsupported,
            "x=|undefined",
        ),
        (
            "for(let undefined=undefined;false;){}0",
            Unsupported,
            "=|undefined",
        ),
        (
            "for(let i=outer;false;){}let outer=0;0",
            Unsupported,
            "i=|outer",
        ),
        (
            "for(let i=body;false;){let body=0;}0",
            Unsupported,
            "i=|body",
        ),
        ("for(let i;body;){let body=0;}0", Unsupported, ";|body"),
        (
            "for(let i;false;i=body){let body=0;}0",
            Unsupported,
            "i=|body",
        ),
        (
            "for(let i;false;i=0){break;i=1;let i;}0",
            Unsupported,
            "break;|i",
        ),
        ("for(let i;false;i=unknown){}0", Unsupported, "unknown"),
        ("const n=0;for(let i;false;n+=i){}0", Unsupported, "+="),
    ] {
        assert_error_at(source, kind, marked);
    }
}

#[test]
fn let_headers_require_declarators_and_both_for_semicolons() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, kind, marked) in [
        ("for(var i=0;false;){}0", Unsupported, "var"),
        ("for(let i in j){}0", Unsupported, "in"),
        ("for(let i of j){}0", Unsupported, "of"),
        ("for(let i=0 in j){}0", Unsupported, "in"),
        ("for(let {i}=x;false;){}0", Unsupported, "let |{"),
        ("for(let [i]=x;false;){}0", Unsupported, "["),
        ("for(let ;false;){}0", Malformed, "let |;"),
        ("for(let i,;false;){}0", Malformed, ",|;"),
        ("for(let i=0,;false;){}0", Malformed, ",|;"),
        ("for(let i=;false;){}0", Malformed, "i=|;"),
        ("for(let i+=0;false;){}0", Unsupported, "+="),
        ("for(let i=(0,);false;){}0", Malformed, ",|)"),
        ("for(let i=0){}0", Malformed, ")"),
        ("for(let i=0\ntrue;){}0", Malformed, "true"),
        ("for(let i\ntrue;){}0", Malformed, "true"),
        ("for(let i;false){}0", Malformed, ")"),
        ("for(let i;false;let j){}0", Unsupported, "false;|let"),
        ("for(let i;;)break;0", Malformed, "break"),
    ] {
        assert_error_at(source, kind, marked);
    }
}

#[test]
fn const_headers_require_every_initializer_and_disallow_trailing_commas() {
    use ErrorKind::{Malformed, Unsupported};
    let missing = "expected '=' after the binding name";
    for (source, kind, marked, message) in [
        ("for(const a;false;){}0", Malformed, "a|;", missing),
        ("for(const a,b=1;false;){}0", Malformed, "a|,", missing),
        ("for(const a=1,b;false;){}0", Malformed, "b|;", missing),
        (
            "for(const a=1,;false;){}0",
            Malformed,
            ",|;",
            "expected a binding name",
        ),
        (
            "for(const a=1,a=2;false;){}0",
            Malformed,
            ",|a",
            "duplicate binding declaration",
        ),
        (
            "for(const a+=1;false;){}0",
            Unsupported,
            "+=",
            "compound assignments cannot initialize declarations",
        ),
        (
            "for(const a=1\nfalse;){}0",
            Malformed,
            "false",
            "expected a binary operator",
        ),
        (
            "for(const a=1;false){}0",
            Malformed,
            ")",
            "expected ';' between for clauses",
        ),
        (
            "for(const {a}=1;false;){}0",
            Unsupported,
            "const |{",
            "destructuring is outside the subset",
        ),
    ] {
        assert_eq!(
            assert_error_at(source, kind, marked).message(),
            message,
            "{source}"
        );
    }

    for (source, marked) in [
        ("for(const [a]=1;false;){}0", "["),
        ("for(const a in b){}0", "in"),
        ("for(const a of b){}0", "of"),
        ("for(const a=1 in b){}0", "in"),
        ("for(const a=1 of b){}0", "of"),
        ("for(const a=1;false;const b=1){}0", "false;|const"),
    ] {
        assert_error_at(source, Unsupported, marked);
    }
}

#[test]
fn const_headers_resolve_names_statically_in_their_own_scope() {
    let tdz = "access before lexical binding initialization is outside the subset";
    let unbound = "unbound name is outside the subset";
    for (source, marked, message) in [
        ("let n=42;for(const n=n;false;){}0", "n=|n", tdz),
        (
            "let n=42;if(false){for(const m=false&&n,n=1;false;){}}0",
            "&&|n",
            tdz,
        ),
        (
            "for(const n=1;false;){break;n=2;let n=0;}0",
            "break;|n",
            tdz,
        ),
        ("for(const n=1;false;){}n", "}|n", unbound),
        ("for(const n=1;false;){}n=2;0", "}|n", unbound),
    ] {
        assert_eq!(
            assert_error_at(source, ErrorKind::Unsupported, marked).message(),
            message,
            "{source}"
        );
    }
}

#[test]
fn rejects_all_writes_to_header_consts_even_in_skipped_updates_and_bodies() {
    for (initial, write, marked) in [
        (1, "n=2", "n|="),
        (1, "n+=2", "+="),
        (0, "n&&=2", "&&="),
        (1, "n||=2", "n|||="),
        (1, "n??=2", "??="),
        (1, "++n", "++"),
        (1, "n--", "--"),
    ] {
        for source in [
            format!("let n=0;for(const n={initial};false;{write}){{let n;}}0"),
            format!("let n=0;for(const n={initial};false;){{break;{write};}}0"),
        ] {
            assert_eq!(
                assert_error_at(&source, ErrorKind::Unsupported, marked).message(),
                "assignment to const is outside the subset",
                "{source}"
            );
        }
    }
}

#[test]
fn for_header_and_body_share_existing_parse_ceilings() {
    let source = "for(;;){}0";
    let limits = Limits {
        max_tokens: 8,
        max_nesting_depth: 1,
        max_work: 17,
        ..Limits::default()
    };
    assert!(parse_script_with_limits(source, limits).is_ok());
    for (limits, ceiling, span) in [
        (
            Limits {
                max_tokens: 7,
                ..limits
            },
            Limit::Tokens,
            Span { start: 9, end: 10 },
        ),
        (
            Limits {
                max_work: 16,
                ..limits
            },
            Limit::Work,
            Span { start: 9, end: 10 },
        ),
        (
            Limits {
                max_nesting_depth: 0,
                ..limits
            },
            Limit::NestingDepth,
            Span { start: 3, end: 4 },
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(ceiling));
        assert_eq!(error.span(), span);
    }

    let source = "for(;(true?0:1);){}42";
    let limits = Limits {
        max_nesting_depth: 2,
        ..Limits::default()
    };
    let error = parse_script_with_limits(source, limits).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(&source[error.span().start..error.span().end], "?");
    assert!(
        parse_script_with_limits(
            source,
            Limits {
                max_nesting_depth: 3,
                ..limits
            }
        )
        .is_ok()
    );

    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    for depth in [128, 129] {
        let source = format!("{}break;{}0", "for(;;){".repeat(depth), "}".repeat(depth));
        let parsed = parse_script_with_limits(&source, limits);
        if depth == 128 {
            assert_eq!(parsed.unwrap().max_stack_depth(), 1);
        } else {
            let error = parsed.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
            assert_eq!(
                error.span(),
                Span {
                    start: 1024,
                    end: 1027
                }
            );
        }
    }
}

#[test]
fn meters_header_clauses_jumps_and_resolution_even_on_skipped_paths() {
    let source = "let i=0;for(i=1;false;i=2){break;}i";
    // In addition to the source bytes: three decimal conversions, one header
    // and one frame, 13 op emissions, one break patch, and three lookups of i
    // (each costs one candidate visit plus one compared byte).
    let limits = Limits {
        max_work: source.len() + 25,
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(1.0)
    );
    let error = parse_script_with_limits(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(
        error.span(),
        Span {
            start: source.len() - 1,
            end: source.len()
        }
    );
}

#[test]
fn skipped_update_and_body_count_toward_the_inclusive_prepared_ceiling() {
    let limits = Limits {
        max_tokens: 8192,
        ..Limits::default()
    };
    for (prefixes, expected) in [(4085, Ok(42.0)), (4086, Err(EvalError::LimitExceeded))] {
        let source = format!(
            "let i=0;for(;false;i={}0){{break;}}42",
            "+ ".repeat(prefixes)
        );
        let script = parse_script_with_limits(&source, limits).unwrap();
        assert_eq!(script.instruction_count(), 11 + prefixes);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn for_dispatches_share_the_inclusive_execution_ceiling() {
    // 11 dispatches per continuing iteration, 2 for setup, 4 for the last
    // (falsy) test, and 1 for the final load before the extra unary operators.
    for (prefixes, expected) in [
        (8, Ok(1488.0)),
        (9, Ok(1488.0)),
        (10, Err(EvalError::DispatchLimitExceeded)),
    ] {
        let source = format!(
            "let i=0;for(;i<1488;i=i+1){{continue;}}{}i",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
    for source in ["for(;;){}0", "let i=0;for(;;i=i+1){continue;}0"] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Err(EvalError::DispatchLimitExceeded)
        );
    }
}

#[test]
fn let_header_has_exact_source_token_nesting_and_work_costs() {
    let source = "for(let a,b=a;false;){break;}0";
    // In addition to the source bytes: one each for the header, header scope,
    // statement frame, decimal conversion and break patch; 9 emissions; 2
    // for duplicate checking b against a; 4 to look up a through b and a.
    let limits = Limits {
        max_source_bytes: source.len(),
        max_tokens: 17,
        max_nesting_depth: 1,
        max_work: source.len() + 20,
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(0.0)
    );
    for (limits, ceiling, start) in [
        (
            Limits {
                max_source_bytes: limits.max_source_bytes - 1,
                ..limits
            },
            Limit::SourceBytes,
            source.len() - 1,
        ),
        (
            Limits {
                max_tokens: limits.max_tokens - 1,
                ..limits
            },
            Limit::Tokens,
            source.len() - 1,
        ),
        (
            Limits {
                max_nesting_depth: 0,
                ..limits
            },
            Limit::NestingDepth,
            source.find('(').unwrap(),
        ),
        (
            Limits {
                max_work: limits.max_work - 1,
                ..limits
            },
            Limit::Work,
            source.find("=a").unwrap() + 1,
        ),
    ] {
        let error = parse_script_with_limits(source, limits).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded(ceiling));
        assert_eq!(
            error.span(),
            Span {
                start,
                end: start + 1
            }
        );
    }

    // The header, explicit group, and pending conditional share the same limit.
    let source = "for(let x=(true?0:1),y=x;false;){}0";
    let limits = Limits {
        max_nesting_depth: 3,
        ..Limits::default()
    };
    assert!(parse_script_with_limits(source, limits).is_ok());
    let error = parse_script_with_limits(
        source,
        Limits {
            max_nesting_depth: 2,
            ..limits
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
    assert_eq!(&source[error.span().start..error.span().end], "?");
}

#[test]
fn const_header_list_has_exact_parse_work_cost() {
    let source = "for(const a=1,b=a;false;){break;}0";
    // Besides source bytes: one each for the header, header scope, statement
    // frame and break patch; two decimal conversions; nine emissions; two
    // for checking b against a; four for looking up a through b and a.
    let limits = Limits {
        max_nesting_depth: 1,
        max_work: source.len() + 21,
        ..Limits::default()
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 9);
    assert_eq!(script.max_stack_depth(), 1);
    assert_eq!(script.evaluate(), Ok(0.0));
    let error = parse_script_with_limits(
        source,
        Limits {
            max_work: limits.max_work - 1,
            ..limits
        },
    )
    .unwrap_err();
    let start = source.find("=a").unwrap() + 1;
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::Work));
    assert_eq!(
        error.span(),
        Span {
            start,
            end: start + 1
        }
    );
}

#[test]
fn lexical_header_and_body_use_only_one_active_statement_frame() {
    let limits = Limits {
        max_nesting_depth: 1,
        ..Limits::default()
    };
    for (open, depth) in [
        ("for(let i;false;){", 128),
        ("for(let i;false;){", 129),
        ("for(const i=0;false;){", 128),
        ("for(const i=0;false;){", 129),
    ] {
        let source = format!("{}break;{}0", open.repeat(depth), "}".repeat(depth));
        let result = parse_script_with_limits(&source, limits);
        if depth == 128 {
            let script = result.unwrap();
            assert_eq!(script.max_stack_depth(), 1);
            assert_eq!(script.evaluate(), Ok(0.0));
        } else {
            let error = result.unwrap_err();
            let start = open.len() * 128;
            assert_eq!(error.kind(), ErrorKind::LimitExceeded(Limit::NestingDepth));
            assert_eq!(
                error.span(),
                Span {
                    start,
                    end: start + 3
                }
            );
        }
    }
}

#[test]
fn let_header_items_count_toward_prepared_instructions_and_binding_slots() {
    use std::fmt::Write;

    // 2,046 distinct bindings at two instructions each, plus four for
    // the test, loop back edge and final expression. Flat items add no nesting.
    let mut source = String::from("for(let a0000");
    for i in 1..2046 {
        write!(source, ",a{i:04}").unwrap();
    }
    let limits = Limits {
        max_tokens: 5_000,
        max_work: 13_000_000,
        max_nesting_depth: 1,
        ..Limits::default()
    };
    for (suffix, instructions, expected) in [
        (";false;){}42", 4096, Ok(42.0)),
        (";false;){}+42", 4097, Err(EvalError::LimitExceeded)),
        (",a2046;false;){}42", 4098, Err(EvalError::LimitExceeded)),
    ] {
        let script = parse_script_with_limits(&format!("{source}{suffix}"), limits).unwrap();
        assert_eq!(script.instruction_count(), instructions);
        assert_eq!(script.evaluate(), expected);
    }
}

#[test]
fn let_header_dispatches_share_the_inclusive_execution_ceiling() {
    // Four for the two items, 11 per continuing iteration, four for the last
    // (falsy) test and one for the final literal, before the extra unary ops.
    for (prefixes, expected) in [(7, Ok(0.0)), (8, Err(EvalError::DispatchLimitExceeded))] {
        let source = format!(
            "for(let i=0,step=1;i<1488;i+=step){{continue;}}{}0",
            "+ ".repeat(prefixes)
        );
        let script = parse_script(&source).unwrap();
        assert_eq!(script.evaluate(), expected);
        assert_eq!(script.evaluate(), expected);
    }
}
