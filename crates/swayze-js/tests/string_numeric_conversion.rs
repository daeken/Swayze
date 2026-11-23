use swayze_js::{
    ErrorKind, EvalError, Limit, Limits, PrimitiveValue, Value, parse_script,
    parse_script_with_limits,
};

fn unary(op: &str, text: &str) -> f64 {
    parse_script(&format!("{op}'{text}'"))
        .unwrap_or_else(|err| panic!("{op}{text:?}: {err}"))
        .evaluate()
        .unwrap()
}

fn positive(text: &str) -> f64 {
    unary("+", text)
}

fn assert_bits(text: &str, expected: f64) {
    assert_eq!(positive(text).to_bits(), expected.to_bits(), "{text:?}");
}

#[test]
fn trims_exactly_ecmascript_whitespace_and_line_terminators() {
    for unit in [
        0x0009, 0x000B, 0x000C, 0x0020, 0x00A0, 0x1680, 0x202F, 0x205F, 0x3000, 0xFEFF, 0x000A,
        0x000D, 0x2028, 0x2029,
    ]
    .into_iter()
    .chain(0x2000..=0x200A)
    {
        let ws = format!(r"\u{unit:04x}");
        assert_bits(&ws, 0.0);
        assert_bits(&format!("{ws}{ws}-0{ws}"), -0.0);
        assert_bits(&format!("{ws}0x2a{ws}"), 42.0);
        assert_bits(&format!("{ws}+Infinity{ws}"), f64::INFINITY);
        for misplaced in [format!("1{ws}2"), format!("-{ws}0"), format!("1e{ws}+2")] {
            assert!(positive(&misplaced).is_nan(), "{misplaced:?}");
        }
    }
    for other in [
        0x0000, 0x0008, 0x000E, 0x0085, 0x180E, 0x200B, 0x200C, 0x2060, 0xD800, 0xDFFF, 0xFEFE,
        0xFFFD, 0xFFFF,
    ] {
        let other = format!(r"\u{other:04x}");
        assert!(positive(&other).is_nan());
        assert!(positive(&format!("{other}1")).is_nan());
        assert!(positive(&format!("1{other}")).is_nan());
    }
    assert!(positive(r"\uD83D\uDE001").is_nan());
    assert!(positive("1😀").is_nan());
    assert_bits("\u{feff}\u{2028}.5\u{2029}", 0.5);
    assert_bits(r"\t\r\n\x20\u00a0", 0.0);
    assert_bits("4\\\r\n2", 42.0); // JS line continuation emits no code units.
}

#[test]
fn converts_only_the_numeric_text_produced_by_the_operand() {
    assert_bits("", 0.0);
    assert_bits("+0", 0.0);
    assert_bits("0008", 8.0);
    assert_bits("077", 77.0);
    assert_bits("+0007.", 7.0);
    assert_bits(".25E+2", 25.0);
    assert_bits("001.e-2", 0.01);
    assert_bits("-01.5", -1.5);
    assert_bits(r"\u0034\x32", 42.0);
    for text in ["Infinity", "+Infinity"] {
        assert_bits(text, f64::INFINITY);
    }
    assert_bits("-Infinity", f64::NEG_INFINITY);
    let script = parse_script("let count = 0; let x = '42'; +(count++, x) + count").unwrap();
    assert_eq!(script.evaluate_value(), Ok(Value::Number(43.0)));
    assert_eq!(script.evaluate(), Ok(43.0));
    let script = parse_script("const x = '42'; +x, x").unwrap();
    assert_eq!(script.evaluate(), Err(EvalError::NonNumericResult));
}

#[test]
fn unary_minus_negates_the_rounded_number_including_signed_zeros() {
    for (text, number) in [
        ("", 0.0),
        ("+0", 0.0),
        ("-0", -0.0),
        ("+1e-9999", 0.0),
        ("-1e-9999", -0.0),
        ("-2.4703282292062328e-324", -f64::from_bits(1)),
        ("+Infinity", f64::INFINITY),
        ("-Infinity", f64::NEG_INFINITY),
        ("0x20000000000003", 9_007_199_254_740_996.0),
    ] {
        assert_bits(text, number);
        assert_eq!(unary("-", text).to_bits(), (-number).to_bits(), "{text:?}");
    }
    for text in ["NaN", "+NaN", "-NaN", "-0x1", "1e+", r"\uD800"] {
        assert!(positive(text).is_nan(), "{text:?}");
        assert!(unary("-", text).is_nan(), "{text:?}");
        assert_eq!(unary("~", text), -1.0, "{text:?}");
    }
}

#[test]
fn bitwise_not_uses_to_int32_of_the_rounded_number() {
    for (text, expected) in [
        ("-0", -1.0),
        ("+Infinity", -1.0),
        ("-Infinity", -1.0),
        ("2147483647", -2_147_483_648.0),
        ("2147483648", 2_147_483_647.0),
        ("4294967296", -1.0),
        ("4294967297.9", -2.0),
        ("-4294967297.9", 0.0),
        ("9007199254740993", -1.0), // Rounds down to 2^53 before wrapping.
        ("9007199254740995", -5.0), // Rounds up to 2^53 + 4.
        ("0x20000000000003", -5.0),
    ] {
        assert_eq!(
            unary("~", text).to_bits(),
            f64::to_bits(expected),
            "{text:?}"
        );
    }
}

#[test]
fn unary_conversions_evaluate_operands_once_in_source_order() {
    let script = parse_script(
        "let x = '2', n = 0; const result = -(n++, x) + ~(n++, x = '4'); \
         n === 2 && x === '4' ? result : 0",
    )
    .unwrap();
    assert_eq!(script.evaluate(), Ok(-7.0));
}

const BINARY_OPS: [&str; 4] = ["*", "/", "%", "**"];
// The expected results for the snapshotted operands '9' and '2'.
const BITWISE_OPS: [(&str, f64); 6] = [
    ("&", 0.0),
    ("^", 11.0),
    ("|", 11.0),
    ("<<", 36.0),
    (">>", 2.0),
    (">>>", 2.0),
];

fn assert_number(source: &str, expected: f64) {
    let script = parse_script(source).unwrap_or_else(|err| panic!("{source}: {err}"));
    let Ok(PrimitiveValue::Scalar(Value::Number(result))) = script.evaluate_primitive() else {
        panic!("expected Number from {source}");
    };
    if expected.is_nan() {
        assert!(result.is_nan(), "{source}: got {result}");
    } else {
        assert_eq!(result.to_bits(), expected.to_bits(), "{source}");
    }
}

#[test]
fn multiplicative_and_power_operands_coerce_mixed_tags_to_numbers() {
    for (source, expected) in [
        ("' 0x10 ' * '2.5'", 40.0),
        ("'6' * true", 6.0),
        ("false * '-2'", -0.0),
        ("'0b1100' / 2", 6.0),
        ("2 / '0b1100'", 1.0 / 6.0),
        ("'9' / '4'", 2.25),
        ("'8' / null", f64::INFINITY),
        ("null / '-8'", -0.0),
        ("'11' % 4", 3.0),
        ("4 % '11'", 4.0),
        ("'11' % '4'", 3.0),
        ("'6' % undefined", f64::NAN),
        ("undefined % '6'", f64::NAN),
        ("'3' ** 2", 9.0),
        ("2 ** '3'", 8.0),
        ("'2' ** '3' ** '2'", 512.0),
    ] {
        assert_number(source, expected);
    }
}

#[test]
fn multiplicative_and_power_preserve_number_special_cases() {
    for (source, expected) in [
        ("'-0' * '2'", -0.0),
        ("'-0' * '-2'", 0.0),
        ("'+Infinity' * '0'", f64::NAN),
        ("'not numeric' * '2'", f64::NAN),
        ("'1' / '-0'", f64::NEG_INFINITY),
        ("'1' / '-Infinity'", -0.0),
        ("'-0' / '-2'", 0.0),
        ("'Infinity' / 'Infinity'", f64::NAN),
        ("'-6' % '3'", -0.0),
        ("'6' % '-3'", 0.0),
        ("'-7' % '3'", -1.0),
        ("'-7' % 'Infinity'", -7.0),
        ("'Infinity' % '3'", f64::NAN),
        ("'1' % '-0'", f64::NAN),
        ("'not numeric' ** '-0'", 1.0),
        ("'0' ** 'not numeric'", f64::NAN),
        ("'not numeric' ** '2'", f64::NAN),
        ("'-1' ** 'Infinity'", f64::NAN),
        ("'-2' ** '.5'", f64::NAN),
        ("'-0' ** '3'", -0.0),
        ("'-0' ** '-3'", f64::NEG_INFINITY),
        ("'-0' ** '-2'", f64::INFINITY),
        ("'-Infinity' ** '3'", f64::NEG_INFINITY),
        ("'-Infinity' ** '-3'", -0.0),
        ("'-Infinity' ** '-2'", 0.0),
        ("'-1' ** '9007199254740991'", -1.0),
        ("'-1' ** '9007199254740993'", 1.0), // Rounded to even before testing parity.
    ] {
        assert_number(source, expected);
    }
}

#[test]
fn binary_bitwise_coerces_strings_before_applying_32_bit_kernels() {
    for (source, expected) in [
        ("' 0x1f ' & '0o12'", 10.0),
        ("true & '3.9'", 1.0),
        ("'4294967299.9' ^ '-2.9'", -3.0),
        ("'0b101' ^ undefined", 5.0),
        ("'4294967295' | false", -1.0),
        ("null | '9007199254740993'", 0.0), // Rounded to 2^53 before wrapping.
        ("'0x20000000000003' | 0", 4.0),
        ("true << '-1.9'", -2_147_483_648.0),
        ("'1' << '4294967329.75'", 2.0),
        ("'8' >> '-4294967294.9'", 2.0),
        ("'2147483648' >> '32'", -2_147_483_648.0),
        ("'-2' >>> true", 2_147_483_647.0),
        ("'-1' >>> '-1'", 1.0),
        ("'-1' >>> '32'", 4_294_967_295.0),
        ("'-1' >>> '9007199254740993'", 4_294_967_295.0),
        ("'-0' | '-0'", 0.0),
        ("'Infinity' ^ '-Infinity'", 0.0),
        ("'1_0' & '-1'", 0.0),
        ("'5' ^ '-0x1'", 5.0),
        (r"'\uD800' | '7'", 7.0),
        ("'5' << '1e+'", 5.0),
        ("'junk' >> '2'", 0.0),
        ("'-7' >>> 'junk'", 4_294_967_289.0),
    ] {
        assert_number(source, expected);
    }
    assert_number("let x = '-1'; x >>>= '32'; x", 4_294_967_295.0);
    assert_number("let x = 'bad'; (x &= '-1'), x", 0.0);
}

#[test]
fn binary_expressions_run_once_left_to_right_and_compounds_snapshot_before_rhs() {
    for (op, expected) in BINARY_OPS
        .into_iter()
        .zip([18.0, 4.5, 1.0, 81.0])
        .chain(BITWISE_OPS)
    {
        assert_number(
            &format!(
                "let x = '9', order = 0; \
                 const result = (order = order * 10 + 1, x) {op} \
                                (order = order * 10 + 2, x = '2'); \
                 order === 12 && x === '2' ? result : -1"
            ),
            expected,
        );
        assert_number(
            &format!("let x = '9', n = 0; x {op}= (n++, x = '2'); n === 1 ? x : -1"),
            expected,
        );
        assert_number(
            &format!(
                "let x = '9', n = 0; const result = (x {op}= (n++, x = '2')); \
                 n === 1 && x === result ? result : -1"
            ),
            expected,
        );
    }
    // The inner power stores first; the outer division uses its earlier snapshot.
    assert_number(
        "let x = '16', y = '2'; const result = x /= y **= (x = '3'); \
         x === 2 && y === 8 ? result : -1",
        2.0,
    );
    // Invalid numeric text produces a Number NaN, which is still written.
    assert_number("let x = 'bad'; x *= 2; x", f64::NAN);
    assert_number("let x = '-0'; (x **= '-3'), x", f64::NEG_INFINITY);
}

#[test]
fn rejects_non_numeric_text_without_widening_source_syntax() {
    for text in [
        "+",
        "-",
        ".",
        "+.",
        ".e1",
        "1e",
        "1e+",
        "1e-",
        "1e.2",
        "1e2.3",
        "1ee2",
        "1.2.3",
        "++1",
        "--1",
        "+-1",
        "1+2",
        "1_0",
        "0.1_0",
        "1e1_0",
        "1n",
        "12px",
        "1/*ok*/",
        "//1",
        "/*1*/",
        "Infinity \\n0",
        "inf",
        "-inf",
        "infinity",
        "INFINITY",
        "Infinity0",
        "+NaN",
        "∞",
        "−1",
        "１",
        "١",
        "0x",
        "0b",
        "0o",
        "0x+1",
        "-0x0",
        "+0b1",
        "-0o1",
        "0x-1",
        "0o8",
        "0b2",
        "0x1p2",
        "0x1.0",
        "0x1_0",
        "0X１",
        "0xg",
        "00x1",
        "0b 1",
        "0xffn",
        "1e999999x",
    ] {
        assert!(positive(text).is_nan(), "{text:?}");
    }
    for source in [
        "+077",
        "+Infinity",
        "+0b2",
        "+'unterminated",
        "+'raw\nlf'",
        r"+'\xg0'",
        r"+'\u{}'",
    ] {
        assert!(parse_script(source).is_err(), "{source:?}");
    }
    for op in ["+", "-", "~"] {
        assert_eq!(
            parse_script(&format!(r"false && {op}'\u{{}}'"))
                .unwrap_err()
                .kind(),
            ErrorKind::Malformed
        );
    }
}

#[test]
fn decimals_preserve_signed_zero_and_round_at_binary64_boundaries() {
    for text in [
        "-0",
        "-0000.e2",
        "-.0",
        "-1e-99999",
        "-0e99999999999999999999",
    ] {
        assert_bits(text, -0.0);
    }
    assert_bits("1e-99999", 0.0);
    assert_bits("-1e99999", f64::NEG_INFINITY);
    assert_bits("9007199254740993", 9_007_199_254_740_992.0);
    assert_bits("9007199254740995", 9_007_199_254_740_996.0);
    // Around half the least subnormal, and on either side of the normal boundary.
    assert_bits("2.4703282292062327e-324", 0.0);
    assert_bits("-2.4703282292062327e-324", -0.0);
    assert_bits("2.4703282292062328e-324", f64::from_bits(1));
    assert_bits("-2.4703282292062328e-324", -f64::from_bits(1));
    assert_bits("2.2250738585072011e-308", f64::from_bits((1 << 52) - 1));
    assert_bits("2.2250738585072012e-308", f64::MIN_POSITIVE);
    assert_bits("1.7976931348623158e308", f64::MAX);
    assert_bits("1.7976931348623159e308", f64::INFINITY);
    // The exact overflow midpoint is 2^1024 - 2^970, ending in 2.
    let prefix = concat!(
        "17976931348623158079372897140530341507993413271003782693617377898044496829276475",
        "094664901797758720709633028641669288791094655554785194040263065748867150582068",
        "190890200070838367627385484581771153176447573027006985557136695962284291481986",
        "083493647529271907416844436551070434271155969950809304288017790417449779",
    );
    assert_bits(&format!("{prefix}1"), f64::MAX);
    assert_bits(&format!("{prefix}2"), f64::INFINITY);
}

#[test]
fn decimal_keeps_late_sticky_bits_and_cancelling_exponents() {
    // Exact midpoint between 1 and the next binary64 value. The last digit of
    // the full string, far beyond the prefix used by the fast path, breaks the tie.
    let midpoint = "1.00000000000000011102230246251565404236316680908203125";
    let almost_full = format!("{midpoint}{}", "0".repeat(4093 - midpoint.len()));
    assert_bits(&format!("{almost_full}0"), 1.0);
    assert_bits(
        &format!("{almost_full}1"),
        f64::from_bits(1.0_f64.to_bits() + 1),
    );
    assert!(positive(&format!("{almost_full}!")).is_nan());
    assert_bits(&format!("1{}e-4000", "0".repeat(4000)), 1.0);
    assert_bits(&format!(".{}2e4001", "0".repeat(4000)), 2.0);
    assert_bits(&format!("1e{}1", "0".repeat(4000)), 10.0);
}

#[test]
fn unsigned_radix_numbers_round_once_and_validate_the_whole_text() {
    for text in ["0x2A", "0X2a", "0o52", "0O052", "0b101010", "0B0101010"] {
        assert_bits(text, 42.0);
    }
    for prefix in ["0x", "0o", "0b"] {
        assert_bits(&format!("{prefix}{}", "0".repeat(4092)), 0.0);
        assert_bits(&format!("{prefix}{}1", "0".repeat(4091)), 1.0);
        assert_bits(&format!("{prefix}{}", "1".repeat(4092)), f64::INFINITY);
        for bad in ["_", "9z", "１", "!"] {
            assert!(positive(&format!("{prefix}{}{bad}", "1".repeat(2000))).is_nan());
        }
    }
    assert_bits("0x20000000000001", 9_007_199_254_740_992.0);
    assert_bits("0x20000000000003", 9_007_199_254_740_996.0);
    assert_bits(
        &format!("0x{}b{}", "f".repeat(13), "f".repeat(242)),
        f64::MAX,
    );
    assert_bits(
        &format!("0x{}c{}", "f".repeat(13), "0".repeat(242)),
        f64::INFINITY,
    );
}

#[test]
fn exact_literal_bound_and_unchanged_scalar_costs() {
    assert_bits(&"0".repeat(4094), 0.0);
    let too_long = format!("+'{}'", "0".repeat(4095));
    let error = parse_script(&too_long).unwrap_err();
    assert_eq!(
        error.kind(),
        ErrorKind::LimitExceeded(Limit::StringLiteralBytes)
    );
    assert_eq!(
        error.span(),
        swayze_js::Span {
            start: 4097,
            end: 4098
        }
    );

    let scalar_limits = Limits {
        max_tokens: 2,
        max_work: 6,
        ..Limits::default()
    };
    let one_short = Limits {
        max_work: 5,
        ..scalar_limits
    };
    for (source, expected) in [("+1", 1.0), ("-1", -1.0), ("~1", -2.0)] {
        let script = parse_script_with_limits(source, scalar_limits).unwrap();
        assert_eq!(script.instruction_count(), 2);
        assert_eq!(script.evaluate(), Ok(expected));
        assert_eq!(
            parse_script_with_limits(source, one_short)
                .unwrap_err()
                .kind(),
            ErrorKind::LimitExceeded(Limit::Work),
        );
    }
    // The final literal and seven unary instructions use exactly the dispatches left.
    let exact = "let n = 0; while (n++ < 3274) {} + - ~ + - ~ +0";
    assert_eq!(parse_script(exact).unwrap().evaluate(), Ok(2.0));
    assert_eq!(
        parse_script(&format!("{exact}, 0")).unwrap().evaluate(),
        Err(EvalError::DispatchLimitExceeded)
    );
}

#[test]
fn exact_conversion_budget_is_shared_inclusive_and_fresh() {
    let radix = format!("0x{}", "f".repeat(1021));
    let decimal = format!(" -1{}! ", "0".repeat(444));
    let nonnumeric = format!(r" \uD800{}  ", "x".repeat(190));
    // Each iteration costs: radix 1,024; signed decimal with invalid suffix
    // 450 + 256 + 3*446 = 2,044; invalid initial unit still pays
    // 195 + 256 + 3*191 = 1,024; whitespace 4.
    let prefix = format!(
        "const r = '{radix}', d = '{decimal}', x = '{nonnumeric}'; let i = 0; \
         while (i++ < 64) {{ +r; -d; ~x; ~'   '; }} "
    );
    // At exactly 262,144 credits, all three calls get their own budget;
    // comparison, plain String operands and scalar unary operations don't charge it.
    let exact = parse_script(&format!(
        "{prefix}x === x && x > '' ? (void x, + - ~41) : 0"
    ))
    .unwrap();
    for _ in 0..2 {
        assert_eq!(exact.evaluate(), Ok(42.0));
        assert_eq!(exact.evaluate_value(), Ok(Value::Number(42.0)));
        assert_eq!(
            exact.evaluate_primitive(),
            Ok(swayze_js::PrimitiveValue::Scalar(Value::Number(42.0)))
        );
    }
    let skipped = parse_script(&format!(
        "{prefix}let s = 0; if (false) {{ -'0'; }} while (false) {{ +'0'; }} \
         for (; false; ~'0') {{}} false && -'0'; true || ~''; '' ?? +''; \
         s &&= -''; s ??= ~''; true ? 42 : +''"
    ))
    .unwrap();
    assert_eq!(skipped.evaluate(), Ok(42.0));
    let error = EvalError::StringNumericConversionLimitExceeded;
    for op in ["+", "-", "~"] {
        let over = parse_script(&format!("{prefix}{op}''")).unwrap();
        assert_eq!(over.evaluate(), Err(error));
        assert_eq!(over.evaluate_value(), Err(error));
        assert_eq!(over.evaluate_primitive(), Err(error));
    }
}

fn unary_budget_prefix(padding: usize) -> String {
    // 64 * 4,095 credits for the loop, plus padding + 1.
    format!(
        "const ws = '{}'; let i = 0; while (i++ < 64) {{ +ws; }} ~'{}'; ",
        " ".repeat(4094),
        " ".repeat(padding),
    )
}

#[test]
fn name_updates_charge_once_and_share_credits_with_other_operators() {
    let credit_error = EvalError::StringNumericConversionLimitExceeded;
    for (form, new, result) in [
        ("++x", 1.0, 1.0),
        ("--x", -1.0, -1.0),
        ("x++", 1.0, 0.0),
        ("x--", -1.0, 0.0),
    ] {
        for (update, exact) in [(format!("{form}; x"), new), (format!("({form})"), result)] {
            for padding in [60, 61] {
                // Unary conversions leave three or two credits, then division
                // spends one. The update's one UTF-16 unit costs two credits.
                let prefix = format!(
                    "{}'' / true; let x = '\\uFEFF'; ",
                    unary_budget_prefix(padding)
                );
                let script = parse_script(&format!("{prefix}{update}")).unwrap();
                let expected = if padding == 60 {
                    Ok(exact)
                } else {
                    Err(credit_error)
                };
                assert_eq!(script.evaluate(), expected, "{update}");
                assert_eq!(script.evaluate(), expected, "{update}");
                if padding == 60 {
                    let over = parse_script(&format!("{prefix}{update}, '' == 0")).unwrap();
                    assert_eq!(over.evaluate_value(), Err(credit_error), "{update}");
                }
            }
        }
    }
}

#[test]
fn skipped_string_updates_do_not_convert_or_store_even_with_no_credits() {
    let source = format!(
        "{}let x = ''; false && x++; true || --x; 'keep' ?? ++x; true ? 0 : x--; \
         if (false) {{ x++; }} for (; false; ++x) {{}} for (;; x--) {{break;}} \
         x === ''",
        unary_budget_prefix(63),
    );
    assert_eq!(
        parse_script(&source).unwrap().evaluate_value(),
        Ok(Value::Boolean(true))
    );
}

#[test]
fn string_update_dispatch_boundaries_are_exact_and_dispatch_precedes_conversion() {
    let credit_error = EvalError::StringNumericConversionLimitExceeded;
    let dispatch_error = EvalError::DispatchLimitExceeded;
    for padding in [62, 63] {
        let prefix = format!(
            "{}let x = ''; let k = 0; while (k++ < 3169) {{}} ",
            unary_budget_prefix(padding),
        );
        // 525 dispatches for the credit prelude and x declaration, then
        // 6 + 5 * 3169 for the empty scalar loop, leaving eight dispatches.
        let cases = [
            // Padding costs seven, then the expression update is dispatch 16,384.
            (
                "+ + + + +0; (x++)",
                if padding == 62 {
                    Ok(0.0)
                } else {
                    Err(credit_error)
                },
            ),
            ("+ + + + + +0; (x++)", Err(dispatch_error)),
            // Padding costs three; four consuming dispatches plus the final load fit.
            (
                "+0; --x; x",
                if padding == 62 {
                    Ok(-1.0)
                } else {
                    Err(credit_error)
                },
            ),
            (
                "+ +0; --x; x",
                if padding == 62 {
                    Err(dispatch_error)
                } else {
                    Err(credit_error)
                },
            ),
            // Padding costs five or six. Conversion is the third consuming
            // dispatch and must run at 16,384, but not at 16,385.
            (
                "+ + +0; --x; x",
                if padding == 62 {
                    Err(dispatch_error)
                } else {
                    Err(credit_error)
                },
            ),
            ("+ + + +0; --x; x", Err(dispatch_error)),
        ];
        for (tail, expected) in cases {
            let script = parse_script(&format!("{prefix}{tail}")).unwrap();
            assert_eq!(script.evaluate(), expected, "padding {padding}; {tail}");
        }
    }
}

#[test]
fn unary_arithmetic_and_bitwise_share_credits_and_charge_both_strings_once() {
    // After unary conversions and one credit for division, two credits are
    // enough for two empty Strings, but one is not. Use the same range twice
    // to ensure aliasing doesn't waive the second charge.
    let exact = format!("{}'' / true; ", unary_budget_prefix(60));
    let one_short = format!("{}'' / true; ", unary_budget_prefix(61));
    let error = EvalError::StringNumericConversionLimitExceeded;
    for (op, expected) in BINARY_OPS
        .into_iter()
        .zip([0.0, f64::NAN, f64::NAN, 1.0])
        .chain(BITWISE_OPS.map(|(op, _)| (op, 0.0)))
    {
        for expr in [format!("empty {op} empty"), format!("(empty {op}= empty)")] {
            let suffix = format!("let empty = ''; {expr}");
            assert_number(&format!("{exact}{suffix}"), expected);
            let over = parse_script(&format!("{one_short}{suffix}")).unwrap();
            assert_eq!(over.evaluate(), Err(error), "{suffix}");
            // A subsequent unary conversion must see the credits spent by the binary op.
            let over = parse_script(&format!("{exact}{suffix}, -''")).unwrap();
            assert_eq!(over.evaluate_primitive(), Err(error), "{suffix}");
        }
    }
}

#[test]
fn rhs_errors_precede_coercion_and_skipped_binary_operators_do_not_convert() {
    let exhausted = unary_budget_prefix(63);
    for op in BINARY_OPS.into_iter().chain(BITWISE_OPS.map(|(op, _)| op)) {
        for source in [
            format!("{exhausted}'' {op} ('' + '')"),
            format!("{exhausted}let x = ''; x {op}= ('' + ''); x"),
        ] {
            assert_eq!(
                parse_script(&source).unwrap().evaluate(),
                Err(EvalError::UnsupportedStringOperation),
                "{op}: the RHS fails before coercing the left operand"
            );
        }
        assert_number(
            &format!(
                "{exhausted}let x = 'untouched', n = 0; \
                 false && '' {op} (n++, ''); true || (x {op}= ''); '' ?? '' {op} ''; \
                 true ? 0 : x {op}= ''; if (false) {{ x {op}= ''; }} \
                 for (; false; x {op}= '') {{}} x ||= '' {op} (n++, ''); \
                 n === 0 && x === 'untouched' ? 42 : -1"
            ),
            42.0,
        );
        assert_eq!(
            parse_script(&format!(r"true || '' {op} '\u{{}}'"))
                .unwrap_err()
                .kind(),
            ErrorKind::Malformed
        );
        for source in [
            format!("const x = ''; x {op}= ''; x"),
            format!("const x = ''; true || (x {op}= '')"),
        ] {
            assert_eq!(
                parse_script(&source).unwrap_err().kind(),
                ErrorKind::Unsupported,
                "{source}"
            );
        }
    }
}

#[test]
fn binary_dispatch_precedes_conversion_at_the_exact_boundary() {
    for (padding, succeeds) in [(61, true), (62, false)] {
        let prefix = format!(
            "{}let k = 0; while (k++ < 3170) {{}} ",
            unary_budget_prefix(padding)
        );
        // One literal statement then a binary expression use the last five
        // dispatches; only the extra unary `+` puts the binary op over the limit.
        for (op, expected) in [("**", 1.0)]
            .into_iter()
            .chain(BITWISE_OPS.map(|(op, _)| (op, 0.0)))
        {
            let exact = format!("{prefix}0; '' {op} ''");
            if succeeds {
                assert_number(&exact, expected);
            } else {
                assert_eq!(
                    parse_script(&exact).unwrap().evaluate(),
                    Err(EvalError::StringNumericConversionLimitExceeded),
                    "{op}"
                );
            }
            let over = parse_script(&format!("{prefix}+0; '' {op} ''")).unwrap();
            assert_eq!(
                over.evaluate(),
                Err(EvalError::DispatchLimitExceeded),
                "{op}"
            );
        }
    }
}

const MIXED_COMPARISONS: [&str; 6] = ["<", "<=", ">", ">=", "==", "!="];

#[test]
fn mixed_comparisons_share_exact_conversion_credits_with_unary_and_binary_ops() {
    let ws = " ".repeat(1024);
    // 32 trips use all 65,536 pairs, split between equality and ordering.
    let pairs = format!(
        "const ws_pair = '{ws}'; let j = 0; \
         while (j++ < 32) {{ ws_pair === ws_pair; ws_pair >= ws_pair; }} "
    );
    // 63 * 4,095 unary credits, then 3,894 or 3,895 more, then 1 for
    // division. The comparison of ' -0 ' needs 5 + 256 + 3 = 264, no pairs.
    let prefix = |padding: usize| {
        format!(
            "{pairs}const ws_num = '{}'; let i = 0; \
             while (i++ < 63) {{ +ws_num; }} ~'{}'; '' / true; ",
            " ".repeat(4094),
            " ".repeat(padding),
        )
    };
    let exact = prefix(3893);
    let one_short = prefix(3894);
    let credit_error = EvalError::StringNumericConversionLimitExceeded;
    for (scalar, results) in [
        ("0", [false, true, false, true, true, false]),
        ("false", [false, true, false, true, true, false]),
        ("null", [false, true, false, true, false, true]),
        ("undefined", [false, false, false, false, false, true]),
        ("(0 / 0)", [false, false, false, false, false, true]),
    ] {
        for (index, (op, expected)) in MIXED_COMPARISONS.into_iter().zip(results).enumerate() {
            if index >= 4 && matches!(scalar, "null" | "undefined") {
                continue; // Noncoercing paths are tested with exhausted credits below.
            }
            for (lhs, rhs) in [("' -0 '", scalar), (scalar, "' -0 '")] {
                let expr = format!("{lhs} {op} {rhs}");
                for (prefix, result) in [
                    (&exact, Ok(Value::Boolean(expected))),
                    (&one_short, Err(credit_error)),
                ] {
                    let script = parse_script(&format!("{prefix}{expr}")).unwrap();
                    assert_eq!(script.evaluate_value(), result, "{expr}");
                }
                assert_eq!(
                    parse_script(&format!("{exact}{expr}, +''"))
                        .unwrap()
                        .evaluate(),
                    Err(credit_error),
                    "{expr} must spend the last credit"
                );
            }
        }
    }
    assert_eq!(
        parse_script(&format!("{exact}' -0 ' == 0; 'x' <= 'y'"))
            .unwrap()
            .evaluate_value(),
        Err(EvalError::StringComparisonLimitExceeded)
    );
}

#[test]
fn noncoercing_and_skipped_comparisons_spend_no_conversion_credits() {
    let exhausted = unary_budget_prefix(63);
    for (expr, expected) in [
        ("'01' == '1'", false),
        ("'01' != '1'", true),
        ("'01' === '1'", false),
        ("'01' !== '1'", true),
        ("'01' < '1'", true),
        ("'01' <= '1'", true),
        ("'01' > '1'", false),
        ("'01' >= '1'", false),
        ("false == 0 && null == undefined && 0 < true", true),
    ] {
        assert_eq!(
            parse_script(&format!("{exhausted}{expr}"))
                .unwrap()
                .evaluate_value(),
            Ok(Value::Boolean(expected)),
            "{expr}"
        );
    }
    for scalar in ["0", "false", "null", "undefined"] {
        for (lhs, rhs) in [("''", scalar), (scalar, "''")] {
            for (op, expected) in [("===", false), ("!==", true), ("==", false), ("!=", true)] {
                if matches!(op, "==" | "!=") && !matches!(scalar, "null" | "undefined") {
                    continue;
                }
                let expr = format!("{lhs} {op} {rhs}");
                assert_eq!(
                    parse_script(&format!("{exhausted}{expr}"))
                        .unwrap()
                        .evaluate_value(),
                    Ok(Value::Boolean(expected)),
                    "{expr}"
                );
            }
        }
    }
    for op in MIXED_COMPARISONS {
        assert_number(
            &format!(
                "{exhausted}let n = 0; \
                 false && '' {op} (n++, 0); true || (n++, 0) {op} ''; \
                 '' ?? '' {op} (n++, 0); if (false) {{ (n++, 0) {op} ''; }} \
                 for (; false; '' {op} (n++, 0)) {{}} true ? n : (n++, 0) {op} ''"
            ),
            0.0,
        );
        assert_eq!(
            parse_script(&format!("{exhausted}'' {op} (0 + '')"))
                .unwrap()
                .evaluate_value(),
            Err(EvalError::UnsupportedStringOperation),
            "{op}: the RHS fails before the comparison can try to convert the left operand"
        );
    }
}

#[test]
fn mixed_comparison_dispatch_precedes_conversion_at_the_exact_boundary() {
    for (padding, succeeds) in [(62, true), (63, false)] {
        let prefix = format!(
            "{}let k = 0; while (k++ < 3170) {{}} ",
            unary_budget_prefix(padding)
        );
        // The scalar statement and comparison take the remaining five dispatches.
        for (op, expected) in MIXED_COMPARISONS
            .into_iter()
            .zip([false, true, false, true, true, false])
        {
            for (lhs, rhs) in [("''", "0"), ("0", "''")] {
                let expr = format!("{lhs} {op} {rhs}");
                for (pad, expected) in [
                    (
                        "0",
                        if succeeds {
                            Ok(Value::Boolean(expected))
                        } else {
                            Err(EvalError::StringNumericConversionLimitExceeded)
                        },
                    ),
                    ("+0", Err(EvalError::DispatchLimitExceeded)),
                ] {
                    assert_eq!(
                        parse_script(&format!("{prefix}{pad}; {expr}"))
                            .unwrap()
                            .evaluate_value(),
                        expected,
                        "{pad}; {expr}"
                    );
                }
            }
        }
    }
}

#[test]
fn sign_only_candidate_must_pay_the_surcharge_before_returning_nan() {
    // The loop spends 63 * 4095 credits. The next base-only conversion
    // leaves 258 or 257; `'-'` can pay its base of two in either case.
    for (padding, succeeds) in [(3900, true), (3901, false)] {
        let source = format!(
            "const ws = '{}'; let i = 0; while (i++ < 63) {{ +ws; }} +'{}'; +'-'",
            " ".repeat(4094),
            " ".repeat(padding),
        );
        let result = parse_script(&source).unwrap().evaluate();
        if succeeds {
            assert!(result.unwrap().is_nan());
        } else {
            assert_eq!(result, Err(EvalError::StringNumericConversionLimitExceeded));
        }
    }
}

#[test]
fn dispatch_precedes_conversion_and_comparisons_have_an_independent_budget() {
    for padding in [62, 63] {
        // 64 * (4094 + 1) + (padding + 1) credits, leaving either one or zero.
        let prefix = format!(
            "const x = '{}'; let i = 0; while (i++ < 64) {{ +x; }} +'{}'; \
             let k = 0; while (k++ < 3170) {{}} ",
            " ".repeat(4094),
            " ".repeat(padding)
        );
        for (op, expected) in [("+", 0.0), ("-", -0.0), ("~", -1.0)] {
            let exact = parse_script(&format!("{prefix}+0; {op}''")).unwrap();
            if padding == 62 {
                assert_eq!(exact.evaluate().unwrap().to_bits(), f64::to_bits(expected));
            } else {
                assert_eq!(
                    exact.evaluate(),
                    Err(EvalError::StringNumericConversionLimitExceeded)
                );
            }
            // Only the extra scalar `+` pushes the final conversion past dispatch 16,384.
            let over = parse_script(&format!("{prefix}+ +0; {op}''")).unwrap();
            assert_eq!(over.evaluate(), Err(EvalError::DispatchLimitExceeded));
        }
    }
    let prefix = format!(
        "const x = '{}'; let i = 0; while (i++ < 64) {{ x === x; }} ",
        "z".repeat(1024)
    );
    for expr in ["+'42'", "'84' / '2'", "'21' << '33'"] {
        assert_number(&format!("{prefix}{expr}"), 42.0);
        assert_eq!(
            parse_script(&format!("{prefix}{expr}, x < x"))
                .unwrap()
                .evaluate(),
            Err(EvalError::StringComparisonLimitExceeded)
        );
    }
}
