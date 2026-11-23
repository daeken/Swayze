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
fn scratch_and_legacy_result_paths_are_distinct() {
    let mut scratch = EvalScratch::default();
    for (source, expected) in [("'a' + 'b'", &[97, 98][..]), ("'' + ''", &[][..])] {
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
    assert_errors(
        &format!("let x = '{part}'; x += (x = 'y', 1 + 'y'); x"),
        EvalError::UnsupportedStringOperation,
    );
}

#[test]
fn skipping_does_not_append_or_raise_runtime_errors_but_still_validates_source() {
    let part = "x".repeat(4094);
    let script = parse_script(&format!(
        "let x = '{part}'; if (false) {{ x += 'y'; }} false && (x + 'y'); \
         x ||= x + 'y'; x === '{part}' ? 42 : -1"
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
