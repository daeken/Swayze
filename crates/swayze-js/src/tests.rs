use crate::parser::Instruction::{
    self, Add, Divide, Multiply, Negate, Number, Positive, Remainder, Subtract,
};
use crate::{ErrorKind, Limit, Limits, Span, parse_script, parse_script_with_limits};

fn assert_postfix(source: &str, expected: &[Instruction], depth: usize) {
    let script = parse_script(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    assert_eq!(script.instruction_count(), expected.len());
    assert_eq!(script.instructions(), expected);
    assert_eq!(script.max_stack_depth(), depth);
}

fn assert_error(source: &str, kind: ErrorKind, start: usize, end: usize) {
    let error = parse_script(source).unwrap_err();
    assert_eq!(error.kind(), kind, "{source:?}: {error}");
    assert_eq!(error.span(), Span { start, end }, "{source:?}: {error}");
}

fn assert_limit(source: &str, limits: Limits, limit: Limit, start: usize, end: usize) {
    let error = parse_script_with_limits(source, limits).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::LimitExceeded(limit), "{error}");
    assert_eq!(error.span(), Span { start, end }, "{error}");
}

#[test]
fn precedence_associativity_and_grouping() {
    assert_postfix(
        "1 + 2 * (3-4);",
        &[
            Number(1.),
            Number(2.),
            Number(3.),
            Number(4.),
            Subtract,
            Multiply,
            Add,
        ],
        4,
    );
    assert_postfix(
        "10 - 3 - 2 + 1",
        &[
            Number(10.),
            Number(3.),
            Subtract,
            Number(2.),
            Subtract,
            Number(1.),
            Add,
        ],
        2,
    );
    assert_postfix(
        "2*3*4",
        &[Number(2.), Number(3.), Multiply, Number(4.), Multiply],
        2,
    );
    assert_postfix(
        "(1+2)*3",
        &[Number(1.), Number(2.), Add, Number(3.), Multiply],
        2,
    );
    assert_postfix("((0))", &[Number(0.)], 1);
}

#[test]
fn postfix_orders_prefix_and_multiplicative_operators() {
    assert_postfix(
        "- + -(1+2)/-4%+5*6",
        &[
            Number(1.),
            Number(2.),
            Add,
            Negate,
            Positive,
            Negate,
            Number(4.),
            Negate,
            Divide,
            Number(5.),
            Positive,
            Remainder,
            Number(6.),
            Multiply,
        ],
        2,
    );
    assert_postfix(
        "8/-(-2)",
        &[Number(8.), Number(2.), Negate, Negate, Divide],
        2,
    );
    assert_postfix("1+ +2", &[Number(1.), Number(2.), Positive, Add], 2);
    assert_postfix("1- -2", &[Number(1.), Number(2.), Negate, Subtract], 2);
}

#[test]
fn entire_input_is_required() {
    use ErrorKind::{Malformed, Unsupported};
    assert_error("1; 2", Unsupported, 3, 4);
    assert_error("1;;", Unsupported, 2, 3);
    assert_error("1;\u{00A0}🚧", Unsupported, 4, 8);
    assert_error("1; let x", Unsupported, 3, 4);
    assert_error("1; // comment", Unsupported, 3, 5);
    assert_error("1;\0", Unsupported, 2, 3);
    assert_error("1 23", Malformed, 2, 4);
    assert_error("1\n2", Unsupported, 2, 3);
    assert_error("1\n(2)", Unsupported, 2, 3); // call, not ASI
    assert_error("1(2)", Unsupported, 1, 2);
    assert_error("(1)2", Malformed, 3, 4);
    assert_postfix("1\n-2\n;\t  ", &[Number(1.), Number(2.), Subtract], 2);
    assert_postfix("8\n/\n2", &[Number(8.), Number(2.), Divide], 2);
    assert_error("1\n++2", Unsupported, 2, 4);
    assert_error("1\n--2", Unsupported, 2, 4);
    assert_error("1;\n/2/", Unsupported, 3, 4);
}

#[test]
fn malformed_subset_and_unsupported_constructs() {
    use ErrorKind::{Malformed, Unsupported};
    assert_error("1+", Malformed, 2, 2);
    assert_error("1+;", Malformed, 2, 3);
    assert_error("1+*2", Malformed, 2, 3);
    assert_error("(1", Malformed, 2, 2);
    assert_error("1)", Malformed, 1, 2);
    assert_error("()", Malformed, 1, 2);
    assert_error("((1)", Malformed, 4, 4);
    assert_error("(1;)", Malformed, 2, 3);
    assert_error("(1\n2)", Malformed, 3, 4);
    assert_error("", Unsupported, 0, 0);
    assert_error(";", Unsupported, 0, 1);
    assert_error("-", Malformed, 1, 1);
    assert_error("(+)", Malformed, 2, 3);
    assert_error("1/", Malformed, 2, 2);
    assert_error("1%", Malformed, 2, 2);
    assert_error("1%%2", Malformed, 2, 3);
    assert_error("() => 1", Unsupported, 3, 5);
    assert_error("(()/*comment*/=>1)", Unsupported, 3, 5);
}

#[test]
fn operator_lexical_boundaries_are_conservative() {
    use ErrorKind::Unsupported;
    for source in ["1--2", "1++2", "1**2", "1-=2", "1/=2", "1%=2"] {
        assert_error(source, Unsupported, 1, 3);
    }
    assert_error("++2", Unsupported, 0, 2);
    assert_error("--2", Unsupported, 0, 2);
    assert_error("1**=2", Unsupported, 1, 4);
    assert_error("-1**2", Unsupported, 2, 4);
    assert_error("(-1)**2", Unsupported, 4, 6);
    assert_error("1\n**2", Unsupported, 2, 4);
    assert_error("1/*comment*/+2", Unsupported, 1, 3);
    assert_error("1//2", Unsupported, 1, 3);
    assert_error("1\n//2", Unsupported, 2, 4);
    assert_error("-/*comment*/1", Unsupported, 1, 3);
    assert_error("/2/", Unsupported, 0, 1);
    assert_error("8 / /2/", Unsupported, 4, 5);
    assert_error("+ /2/", Unsupported, 2, 3);
}

#[test]
fn whitespace_and_utf8_spans() {
    use ErrorKind::{Malformed, Unsupported};
    let white = "\u{FEFF}\t\u{000B}\u{000C} \u{00A0}\u{1680}\u{2000}\u{200A}\u{202F}\u{205F}\u{3000}\n\r\u{2028}\u{2029}";
    assert_postfix(
        &format!("{white}1{white}+{white}2{white};{white}"),
        &[Number(1.), Number(2.), Add],
        2,
    );
    assert_error(white, Unsupported, white.len(), white.len());
    assert_error("1\u{2028}2", Unsupported, 4, 5);
    assert_error("1+\u{0085}2", Unsupported, 2, 4); // NEL is not a JS line terminator
    assert_error("1+\u{180E}2", Unsupported, 2, 5); // not ECMAScript WhiteSpace
    assert_error("1+\u{200B}2", Unsupported, 2, 5);
    assert_error("1+\u{00A0}", Malformed, 4, 4);
    assert_error("1+３", Unsupported, 2, 5); // ASCII decimal digits only
    let error = parse_script("1+🔑").unwrap_err();
    assert!(error.to_string().contains("bytes 2..6"));
    assert!(!format!("{error:?} {error}").contains('🔑'));
}

#[test]
fn non_slice_numeric_lexemes_are_not_split() {
    use ErrorKind::Unsupported;
    for input in [
        "00", "08", "0x10", "0o10", "0b10", "1.0", "1.", "1e2", "1E+2", "1n", "1_000", "1foo",
        "1$foo", "1\\u0061",
    ] {
        assert_error(input, Unsupported, 1, 2);
    }
    assert_error(".5", Unsupported, 0, 1);
    assert_error("10n", Unsupported, 2, 3);
    assert_error("1é", Unsupported, 1, 3);
}

#[test]
fn decimal_precision_and_overflow() {
    for (input, expected) in [
        ("9007199254740991", 9_007_199_254_740_991.), // 2^53 - 1
        ("9007199254740992", 9_007_199_254_740_992.),
        ("9007199254740993", 9_007_199_254_740_992.), // midpoint rounds down
        ("9007199254740995", 9_007_199_254_740_996.), // midpoint rounds up
        ("18446744073709551616", 18_446_744_073_709_551_616.), // 2^64
    ] {
        assert_postfix(input, &[Number(expected)], 1);
    }
    // The 309-digit overflow midpoint is 2^1024 - 2^970. The preceding
    // integer rounds to the largest finite Number; the midpoint overflows.
    let prefix = concat!(
        "17976931348623158079372897140530341507993413271003782693617377898044496829276475",
        "094664901797758720709633028641669288791094655554785194040263065748867150582068",
        "190890200070838367627385484581771153176447573027006985557136695962284291481986",
        "083493647529271907416844436551070434271155969950809304288017790417449779",
    );
    assert_postfix(&format!("{prefix}1"), &[Number(f64::MAX)], 1);
    assert_postfix(&format!("{prefix}2"), &[Number(f64::INFINITY)], 1);
    let above = format!("1{}", "0".repeat(309));
    assert_postfix(&above, &[Number(f64::INFINITY)], 1);
    assert_error(&format!("{above}n"), ErrorKind::Unsupported, 310, 311);
}

#[test]
fn individual_limits_are_inclusive() {
    let defaults = Limits::default();
    let source_limit = Limits {
        max_source_bytes: 4,
        ..defaults
    };
    assert!(parse_script_with_limits("\u{FEFF}1", source_limit).is_ok());
    assert_limit("1 + 🔑", source_limit, Limit::SourceBytes, 4, 8);
    assert_limit(
        "1\u{3000}+ 2",
        Limits {
            max_source_bytes: 2,
            ..defaults
        },
        Limit::SourceBytes,
        1,
        4,
    );

    let token_limit = Limits {
        max_tokens: 6,
        ..defaults
    };
    assert!(parse_script_with_limits(" ( 1+2 ) ;", token_limit).is_ok());
    assert_limit(
        "(1+2);",
        Limits {
            max_tokens: 5,
            ..defaults
        },
        Limit::Tokens,
        5,
        6,
    );
    assert_limit(
        "🔑",
        Limits {
            max_tokens: 0,
            ..defaults
        },
        Limit::Tokens,
        0,
        4,
    );

    let depth_limit = Limits {
        max_nesting_depth: 1,
        ..defaults
    };
    assert!(parse_script_with_limits("(1)+(2)", depth_limit).is_ok());
    assert_limit("( (1))", depth_limit, Limit::NestingDepth, 2, 3);
    assert!(
        parse_script_with_limits(
            "1",
            Limits {
                max_nesting_depth: 0,
                ..defaults
            }
        )
        .is_ok()
    );

    assert!(
        parse_script_with_limits(
            "1+2",
            Limits {
                max_work: 9,
                ..defaults
            }
        )
        .is_ok()
    );
    assert_limit(
        "1+2",
        Limits {
            max_work: 8,
            ..defaults
        },
        Limit::Work,
        1,
        2,
    ); // operator emission
    assert_limit(
        "12",
        Limits {
            max_work: 3,
            ..defaults
        },
        Limit::Work,
        0,
        2,
    ); // literal conversion
    assert_limit(
        "1\u{FEFF}",
        Limits {
            max_work: 5,
            ..defaults
        },
        Limit::Work,
        1,
        4,
    ); // trailing trivia
}

#[test]
fn mixed_operators_charge_token_and_work_budgets() {
    let source = "-(1)/+2%3";
    let limits = Limits {
        max_tokens: 9,
        max_work: 24,
        max_nesting_depth: 1,
        ..Limits::default()
    };
    let script = parse_script_with_limits(source, limits).unwrap();
    assert_eq!(script.instruction_count(), 7);
    assert_eq!(script.max_stack_depth(), 2);
    assert_limit(
        source,
        Limits {
            max_tokens: 8,
            ..limits
        },
        Limit::Tokens,
        8,
        9,
    );
    assert_limit(
        source,
        Limits {
            max_work: 23,
            ..limits
        },
        Limit::Work,
        7,
        8,
    );
}

#[test]
fn long_prefix_chains_are_iterative_without_using_nesting_budget() {
    let count = 10_000;
    let source = "- ".repeat(count) + "0";
    let limits = Limits {
        max_nesting_depth: 0,
        ..Limits::default()
    };
    assert_limit(
        &source,
        limits,
        Limit::Tokens,
        2 * limits.max_tokens,
        2 * limits.max_tokens + 1,
    );
    let script = parse_script_with_limits(
        &source,
        Limits {
            max_tokens: count + 1,
            ..limits
        },
    )
    .unwrap();
    assert_eq!(script.instruction_count(), count + 1);
    assert_eq!(script.max_stack_depth(), 1);
}

#[test]
fn long_literals_chains_and_nesting_are_bounded() {
    let defaults = Limits::default();
    let max_source = "9".repeat(defaults.max_source_bytes);
    assert_postfix(&max_source, &[Number(f64::INFINITY)], 1);
    assert_limit(
        &format!("{max_source};"),
        defaults,
        Limit::SourceBytes,
        defaults.max_source_bytes,
        defaults.max_source_bytes + 1,
    );

    let nesting = 10_000;
    let parentheses = format!("{}1{}", "(".repeat(nesting), ")".repeat(nesting));
    assert_limit(
        &parentheses,
        defaults,
        Limit::NestingDepth,
        defaults.max_nesting_depth,
        defaults.max_nesting_depth + 1,
    );
    let limits = Limits {
        max_nesting_depth: nesting,
        max_tokens: 2 * nesting + 1,
        ..defaults
    };
    let script = parse_script_with_limits(&parentheses, limits).unwrap();
    assert_eq!(script.instructions(), [Number(1.)]);

    let chain = format!("{}1", "1+".repeat(nesting));
    assert_limit(
        &chain,
        defaults,
        Limit::Tokens,
        defaults.max_tokens,
        defaults.max_tokens + 1,
    );
    let script = parse_script_with_limits(&chain, limits).unwrap();
    assert_eq!(script.instruction_count(), 2 * nesting + 1);
    assert_eq!(script.max_stack_depth(), 2);
}
