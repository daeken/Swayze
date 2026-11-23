use swayze_js::{EvalError, EvalScratch, PrimitiveValue, Value, parse_script};

fn assert_units(source: &str, expected: &[u16]) {
    let script = parse_script(source).unwrap();
    let mut scratch = EvalScratch::new();
    assert_eq!(
        script.evaluate_primitive_with_scratch(&mut scratch),
        Ok(PrimitiveValue::String(expected)),
        "{source}"
    );
}

fn assert_errors(source: &str, expected: EvalError) {
    let script = parse_script(source).unwrap();
    assert_eq!(script.evaluate_primitive(), Err(expected));
    assert_eq!(script.evaluate_value(), Err(expected));
    assert_eq!(script.evaluate(), Err(expected));
    assert_eq!(
        script.evaluate_primitive_with_scratch(&mut EvalScratch::new()),
        Err(expected)
    );
}

#[test]
fn append_exact_utf16_units_including_cross_boundary_and_lone_surrogates() {
    assert_units(
        r"'\x00\uD800' + '\uDC00\uDFFF' + '😀' + 'e\u0301' + '\u00E9\uD801'",
        &[
            0, 0xD800, 0xDC00, 0xDFFF, 0xD83D, 0xDE00, 0x65, 0x301, 0xE9, 0xD801,
        ],
    );
}

#[test]
fn mixed_add_and_compound_add_spell_each_scalar_in_both_operand_orders() {
    for (scalar, text) in [
        ("true", "true"),
        ("false", "false"),
        ("null", "null"),
        ("undefined", "undefined"),
        ("0", "0"),
        ("-0", "0"),
        ("0 / 0", "NaN"),
        ("1 / 0", "Infinity"),
        ("-1 / 0", "-Infinity"),
        ("0x2a", "42"),
        ("-1.25", "-1.25"),
        ("1_000e0", "1000"),
        ("1e-6", "0.000001"),
        ("1e-7", "1e-7"),
        ("1000000000000000128", "1000000000000000100"),
        ("1e21", "1e+21"),
        ("5e-324", "5e-324"),
    ] {
        for (source, scalar_first) in [
            (format!(r"'\uD800' + ({scalar})"), false),
            (format!(r"({scalar}) + '\uD800'"), true),
            (format!(r"let x = '\uD800'; x += ({scalar}); x"), false),
            (format!(r"let x = {scalar}; (x += '\uD800')"), true),
        ] {
            let mut expected = text.encode_utf16().collect::<Vec<_>>();
            if scalar_first {
                expected.push(0xD800);
            } else {
                expected.insert(0, 0xD800);
            }
            assert_units(&source, &expected);
        }
    }
}

#[test]
fn scalar_addition_and_updates_stay_numeric() {
    for (source, expected) in [
        ("true + false + null + 41", 42.0),
        ("let x = true; x += null; x += 41; x", 42.0),
        ("-0 + -0", -0.0),
        ("-0 + null", 0.0),
        ("-0 + false", 0.0),
        ("undefined + true", f64::NAN),
        ("let x; (x += null)", f64::NAN),
        ("let x = '' + 41; (++x)", 42.0),
        ("let x = 42 + ''; (x++)", 42.0),
        ("let x = '' + 41; x++; x", 42.0),
        ("let x = 43 + ''; --x; x", 42.0),
    ] {
        let actual = parse_script(source).unwrap().evaluate().unwrap();
        if expected.is_nan() {
            assert!(actual.is_nan(), "{source}");
        } else {
            assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
        }
    }
    assert_units("let x = '-0'; '' + x++ + ':' + x", &[48, 58, 49]);
    assert_units("let x = 'oops'; '' + --x", &[78, 97, 78]);
    assert_units("1 + 2 + '3' + 4 * 5", &[51, 51, 50, 48]);
}

#[test]
fn scratch_and_legacy_result_paths_are_distinct() {
    let mut scratch = EvalScratch::default();
    for (source, expected) in [
        ("'a' + 'b'", &[97, 98][..]),
        ("'' + ''", &[][..]),
        ("'' + -0", &[48][..]),
        ("let x = 42; (x += '')", &[52, 50][..]),
    ] {
        let script = parse_script(source).unwrap();
        let result = script
            .evaluate_primitive_with_scratch(&mut scratch)
            .unwrap();
        let copy = result;
        assert_eq!(result, copy); // PrimitiveValue is Copy.
        assert_eq!(result, PrimitiveValue::String(expected));
        assert_eq!(
            script.evaluate_primitive(),
            Err(EvalError::RuntimeStringResultRequiresScratch)
        );
        assert_eq!(script.evaluate_value(), Err(EvalError::NonScalarResult));
        assert_eq!(script.evaluate(), Err(EvalError::NonNumericResult));
    }

    // An earlier dynamic evaluation does not make a final literal dynamic.
    for (source, expected) in [("'a' + 'b', 'c'", &[99][..]), ("'' + '', ''", &[][..])] {
        let script = parse_script(source).unwrap();
        assert_eq!(
            script.evaluate_primitive(),
            Ok(PrimitiveValue::String(expected))
        );
        assert_eq!(script.evaluate_value(), Err(EvalError::NonScalarResult));
        assert_eq!(script.evaluate(), Err(EvalError::NonNumericResult));
    }
    let script = parse_script("let x = '4' + '2'; +x").unwrap();
    assert_eq!(
        script.evaluate_primitive(),
        Ok(PrimitiveValue::Scalar(Value::Number(42.0)))
    );
    assert_eq!(script.evaluate_value(), Ok(Value::Number(42.0)));
    assert_eq!(script.evaluate(), Ok(42.0));
}

#[test]
fn compound_snapshots_the_left_side_before_rhs_stores_and_yields_the_result() {
    assert_units(
        "let x = 'o' + 'ld'; let y = (x += x = 'n' + 'ew'); x + '|' + y",
        &"oldnew|oldnew".encode_utf16().collect::<Vec<_>>(),
    );
    assert_units(
        "let x = 'a' + 'b'; const before = x; x += x, x += 'c'; before + '|' + x",
        &"ab|ababc".encode_utf16().collect::<Vec<_>>(),
    );
    let part = "x".repeat(4094);
    assert_errors(
        &format!("let x = '{part}'; x += (x = 'y', 'y'); x"),
        EvalError::StringConcatenationLengthLimitExceeded,
    );
    assert_units(
        "let x = 'o' + 1; let y = (x += (x = 'new', 2)); x + '|' + y",
        &"o12|o12".encode_utf16().collect::<Vec<_>>(),
    );
    assert_units(
        "let x = 1, n = 0; let y = (x += (n++, x = 'n' + 'ew')); x + '|' + y + n",
        &"1new|1new1".encode_utf16().collect::<Vec<_>>(),
    );
    assert_units(
        "let x = 'a' + 1; const before = x; x += x; x += true; before + '|' + x",
        &"a1|a1a1true".encode_utf16().collect::<Vec<_>>(),
    );
    assert_units(
        "let x = 'old', n = 0; (n = n * 10 + 1, x) + (n = n * 10 + 2, x = 7) + n",
        &"old712".encode_utf16().collect::<Vec<_>>(),
    );
    assert_errors(
        &format!("let x = '{part}'; x += (x = 'y', 1); x"),
        EvalError::StringConcatenationLengthLimitExceeded,
    );
}

#[test]
fn skipping_does_not_append_or_raise_runtime_errors_but_still_validates_source() {
    let part = "x".repeat(4094);
    let script = parse_script(&format!(
        "let x = '{part}'; if (false) {{ x += 'y'; }} false && (x + 'y'); \
         let n = 0; x ||= (n++, x + -0); false && (undefined + (n++, x)); \
         if (false) {{ x += (n++, 1); }} x === '{part}' && n === 0 ? 42 : -1"
    ))
    .unwrap();
    assert_eq!(script.evaluate(), Ok(42.0));
    for source in [
        r"true || 'x' + '\u{}'",
        "false && 'x' + missing",
        "const x = 'a'; if (false) { x += 'b'; } 0",
    ] {
        assert!(parse_script(source).is_err(), "{source}");
    }
}

#[test]
fn runtime_strings_follow_existing_truthiness_comparison_and_conversion_paths() {
    for source in [
        r"('' + '') || ('\x00' + '' && 42)",
        r"('x' + '') ?? 0, ('x' + 'y') == 'xy' && 'xz' > ('x' + 'y') ? 42 : 0",
        r"'42' === ('4' + '2') && ('4' + '2') !== 42 && ('4' + '2') == 42 && 41 < ('4' + '2') ? 42 : 0",
        r"-('-' + '42')",
        r"('0x' + '2a') * ('' + '1')",
        r"let x = '0b' + '101010'; x &= '6' + '3'; x",
        r"let x = '4' + '1'; ++x; x",
        r"let x = '4' + '2'; x++ + 0",
        r"let x = '4' + '3'; --x + 0",
    ] {
        assert_eq!(
            parse_script(source).unwrap().evaluate(),
            Ok(42.0),
            "{source}"
        );
    }
}

#[test]
fn individual_and_cumulative_limits_are_inclusive_with_distinct_errors() {
    let part = "x".repeat(4094);
    assert_units(&format!("'{part}' + ''"), &vec![120; 4094]);
    assert_errors(
        &format!("'{part}' + 'y'"),
        EvalError::StringConcatenationLengthLimitExceeded,
    );

    // All loop results are discarded, but 8 * 4,094 + 16 = 32,768 units are retained.
    let prefix = format!(
        "const s = '{part}'; let i = 0; while (i++ < 8) {{ s + ''; }} '1234567890123456' + ''; "
    );
    assert_units(&format!("{prefix}'' + ''"), &[]);
    assert_errors(
        &format!("{prefix}s + 'y'"),
        EvalError::StringConcatenationLengthLimitExceeded,
    );
    assert_errors(
        &format!("{prefix}'y' + ''"),
        EvalError::StringConcatenationBudgetExceeded,
    );
    assert_errors(
        &format!("{prefix}let x = ''; x += 'y'; x"),
        EvalError::StringConcatenationBudgetExceeded,
    );
}

#[test]
fn mixed_limits_charge_both_exact_emitted_lengths_including_scalar_units() {
    let part = "x".repeat(4069);
    let number = "-0.0000010000000000000002";
    let expected = format!("{part}{number}").encode_utf16().collect::<Vec<_>>();
    assert_eq!(expected.len(), 4094);
    assert_units(&format!("'{part}' + {number}"), &expected);
    assert_units(&format!("let x = '{part}'; x += {number}; x"), &expected);
    assert_errors(
        &format!("{number} + '{part}x'"),
        EvalError::StringConcatenationLengthLimitExceeded,
    );

    // The mixed loop appends 8 * (4,085 + 9) units. Skip an overlength
    // Number attempt and an append before filling the remaining 16 units.
    let wide = "x".repeat(4085);
    let prefix = format!(
        "const wide = '{wide}'; let i = 0; while (i++ < 8) {{ undefined + wide; }} \
         false && (wide + {number}); let x = ''; x &&= false + wide; x = '12345678901'; "
    );
    assert_units(
        &format!("{prefix}(x += false)"),
        &"12345678901false".encode_utf16().collect::<Vec<_>>(),
    );
    assert_errors(
        &format!("{prefix}(x += false), '' + -0"),
        EvalError::StringConcatenationBudgetExceeded,
    );
    assert_errors(
        &format!("{prefix}let y = false; y += '123456789012'; y"),
        EvalError::StringConcatenationBudgetExceeded,
    );
}

#[test]
fn final_mixed_dispatch_checks_length_before_cumulative_budget() {
    // 4 init + 1,819 * 9 taken + 4 final-test dispatches = 16,379.
    // The loop appends 1,819 * 18 = 32,742 units. After the comma
    // prepends two dispatches, the final Add is dispatch 16,384.
    let prefix = "const s = '12345678901234567'; let i = 0; while (i++ < 1819) { s + 1; } 0, ";
    let over_length = "a".repeat(4094);
    assert_units(
        &format!("{prefix}'1234567890123456789012345' + 1e0000000"),
        &"12345678901234567890123451"
            .encode_utf16()
            .collect::<Vec<_>>(),
    );
    assert_errors(
        &format!("{prefix}'{over_length}' + 1"),
        EvalError::StringConcatenationLengthLimitExceeded,
    );
    assert_errors(
        &format!("{prefix}'12345678901234567890123456' + 1"),
        EvalError::StringConcatenationBudgetExceeded,
    );
    assert_errors(
        &format!("{prefix}'{over_length}' + (+1)"),
        EvalError::DispatchLimitExceeded,
    );
}

#[test]
fn dynamic_pairs_and_conversion_credits_are_separate_from_append_budget() {
    let ws = " ".repeat(4094);
    let last_pairs = "x".repeat(32);
    let last_credits = " ".repeat(63);
    let prefix = format!(
        "const ws = '{ws}' + '', p = '' + '{last_pairs}', c = '' + '{last_credits}'; \
         let i = 0; while (i++ < 16) {{ ws <= ws; }} p === p; \
         i = 0; while (i++ < 64) {{ +ws; }} +c; "
    );
    // 16 * 4,094 + 32 == 65,536 pairs; 64 * 4,095 + 64 == 262,144 credits.
    // Equal-length aliases still charge, and concatenation can run after both are exhausted.
    assert_units(&format!("{prefix}'o' + 'k'"), &[111, 107]);
    assert_units(&format!("{prefix}'o' + 1"), &[111, 49]);
    assert_errors(
        &format!("{prefix}('' + 'x') === 'x'"),
        EvalError::StringComparisonLimitExceeded,
    );
    assert_errors(
        &format!("{prefix}+('' + '')"),
        EvalError::StringNumericConversionLimitExceeded,
    );
}

#[test]
fn empty_concatenation_uses_dispatch_fuel_but_no_append_budget() {
    // Two initialization dispatches, nine per taken iteration, four in the
    // failed test, and a load with six unary operators: 13 + 1,819 * 9 == 16,384.
    let prefix = "let i = 0; while (i++ < 1819) { '' + ''; } + + + + + + ";
    assert_eq!(
        parse_script(&format!("{prefix}i")).unwrap().evaluate(),
        Ok(1820.0)
    );
    assert_errors(&format!("{prefix}+i"), EvalError::DispatchLimitExceeded);
}
