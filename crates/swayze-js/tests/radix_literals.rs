use swayze_js::{ErrorKind, Limit, Limits, Span, parse_script, parse_script_with_limits};

fn assert_number(source: &str, expected: f64) {
    let script = parse_script(source).unwrap_or_else(|error| panic!("{source:?}: {error}"));
    assert_eq!(
        script.evaluate().unwrap().to_bits(),
        expected.to_bits(),
        "{source:?}"
    );
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
fn bases_prefixes_and_lexical_boundaries() {
    for (source, expected) in [
        ("0x000aBcDeF", 11_259_375.0),
        ("0XfF", 255.0),
        ("0o00017", 15.0),
        ("0O710", 456.0),
        ("0b00001101", 13.0),
        ("0B110", 6.0),
        ("0x000", 0.0),
        ("-0O000", -0.0),
        ("0B0", 0.0),
        ("0x1e+0x2", 32.0), // e is a hex digit, not an exponent marker.
        ("0xB-0xE", -3.0),
        ("0b0_1_0", 2.0),
        ("0B1_1", 3.0),
        ("0o0_1_7", 15.0),
        ("0O7_1_0", 456.0),
        ("0x0_a_B", 171.0),
        ("0Xf_F", 255.0),
        ("0x1_e+0x2", 32.0),
        ("-0O0_0", -0.0),
        ("\u{FEFF}0b10/*🦀*/+0O7\u{2028}+0x10;", 25.0),
    ] {
        assert_number(source, expected);
    }
}

#[test]
fn missing_digits_invalid_digits_and_unsupported_suffixes_have_exact_spans() {
    use ErrorKind::{Malformed, Unsupported};
    for (source, start, end) in [
        ("0x", 2, 2),
        ("0B+1", 2, 2),
        ("0o;", 2, 2),
        ("0x/*no join*/1", 2, 2),
        ("0b\u{00A0}1", 2, 2),
        ("0Xg", 2, 3),
        ("0B2", 2, 3),
        ("0b102", 4, 5),
        ("\u{FEFF}0o708", 7, 8),
        ("0b1\u{00A0}2", 5, 6),
        ("0x_1", 2, 3),
        ("0b_1", 2, 3),
        ("0o_1", 2, 3),
        ("0B1__0", 3, 4),
        ("0O7_", 3, 4),
        ("0xA__f", 3, 4),
        ("0b1_2", 3, 4),
        ("0o1_8", 3, 4),
        ("0xf_G", 3, 4),
        ("0x0_.1", 3, 4),
        ("0b0_/*no join*/1", 3, 4),
        ("0o1_\u{00A0}2", 3, 4),
        ("0xA_Ｆ", 3, 4),
        ("0b1_02", 5, 6),
        ("0o7_08", 5, 6),
    ] {
        assert_error(source, Malformed, start, end);
    }
    for (source, start, end) in [
        ("0x1n", 3, 4),
        ("0B10n", 4, 5),
        ("0O777n", 5, 6),
        ("0xdeadbeefz", 10, 11),
        ("0b1_0n", 5, 6),
        ("0o7_0n", 5, 6),
        ("0xE_Fn", 5, 6),
        ("0xA_Btail", 5, 6),
        ("0xA_B.1", 5, 6),
        ("0b1_0e+2", 5, 6),
        ("\u{FEFF}0b1_0🔑", 8, 12),
        ("0b1e+2", 3, 4),
        ("0x1.5", 3, 4),
        ("0b1$tail", 3, 4),
        ("0x1\\u0061", 3, 4),
        ("0o7é", 3, 5),
        ("0xＦ", 2, 5),
        ("\u{FEFF}0b1🔑", 6, 10),
        ("0b1\u{2028}2", 6, 7),
        ("0o7;0x1", 4, 7),
        ("077", 1, 2),
        ("00x1", 1, 2),
    ] {
        assert_error(source, Unsupported, start, end);
    }
}

fn separate_radix_digits(source: &str) -> String {
    let (prefix, digits) = source.split_at(2);
    let mut separated = prefix.to_owned();
    for (i, digit) in digits.chars().enumerate() {
        if i != 0 {
            separated.push('_');
        }
        separated.push(digit);
    }
    separated
}

#[test]
fn rounds_once_around_two_to_the_53rd() {
    let two53 = 1_u64 << 53;
    for (integer, rounded) in [
        (two53 - 1, two53 - 1),
        (two53, two53),
        (two53 + 1, two53),
        (two53 + 2, two53 + 2),
        (two53 + 3, two53 + 4),
        (two53 + 4, two53 + 4),
        (2 * two53 - 1, 2 * two53), // Tie carries into the next binade.
        (2 * two53 + 3, 2 * two53 + 4),
    ] {
        for source in [
            format!("0x{integer:x}"),
            format!("0o{integer:o}"),
            format!("0b{integer:b}"),
        ] {
            assert_number(&source, rounded as f64);
            assert_number(&separate_radix_digits(&source), rounded as f64);
        }
    }
}

fn assert_binary_in_all_bases(bits: &str, expected: f64) {
    for (prefix, width) in [("0b", 1), ("0o", 3), ("0x", 4)] {
        let mut source = prefix.to_owned();
        for group in bits.as_bytes().rchunks(width).rev() {
            let group = std::str::from_utf8(group).unwrap();
            let digit = u32::from_str_radix(group, 2).unwrap();
            source.push(char::from_digit(digit, 16).unwrap());
        }
        assert_number(&source, expected);
        assert_number(&separate_radix_digits(&source), expected);
    }
}

#[test]
fn deep_midpoints_use_guard_sticky_and_low_significand_bit() {
    // Check each digit alignment, so guard can be inside an octal/hex digit.
    for tail_len in 840..852 {
        let zeros = "0".repeat(tail_len);
        let ones = "1".repeat(tail_len);
        let even_prefix = format!("1{}", "0".repeat(52));
        let odd_prefix = format!("1{}1", "0".repeat(51));
        let exponent = 53 + tail_len;
        let even = (1023 + exponent as u64) << 52;
        let above = "0".repeat(tail_len - 1) + "1";
        assert_binary_in_all_bases(&format!("{even_prefix}0{ones}"), f64::from_bits(even));
        assert_binary_in_all_bases(&format!("{even_prefix}1{zeros}"), f64::from_bits(even));
        assert_binary_in_all_bases(&format!("{even_prefix}1{above}"), f64::from_bits(even + 1));
        assert_binary_in_all_bases(&format!("{odd_prefix}1{zeros}"), f64::from_bits(even + 2));
    }
}

#[test]
fn rounds_at_the_overflow_midpoint() {
    // MAX = 2^1024 - 2^971; halfway to 2^1024 is 2^1024 - 2^970.
    let ones = "1".repeat(53);
    let prev_max = f64::from_bits(f64::MAX.to_bits() - 1);
    for (bits, expected) in [
        (format!("{}{}", "1".repeat(52), "0".repeat(972)), prev_max),
        (format!("{ones}{}", "0".repeat(971)), f64::MAX),
        (format!("{ones}0{}", "1".repeat(970)), f64::MAX),
        (format!("{ones}1{}", "0".repeat(970)), f64::INFINITY),
        (format!("{ones}1{}1", "0".repeat(969)), f64::INFINITY),
        (format!("1{}", "0".repeat(1024)), f64::INFINITY),
    ] {
        assert_binary_in_all_bases(&bits, expected);
    }
    let negative_overflow = format!("-0b{ones}1{}", "0".repeat(970));
    assert_number(&negative_overflow, f64::NEG_INFINITY);
}

#[test]
fn exact_byte_work_and_token_limits() {
    let source = "\u{00A0}0x1_0";
    let limits = Limits {
        max_tokens: 1,
        max_source_bytes: 7,
        max_work: 8, // Seven UTF-8 bytes and one emitted instruction.
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(source, limits).unwrap().evaluate(),
        Ok(16.0)
    );
    for (work, start, end) in [
        (1, 0, 2),
        (2, 2, 3),
        (3, 3, 4),
        (4, 4, 5),
        (5, 5, 6),
        (6, 6, 7),
        (7, 2, 7),
    ] {
        assert_limit(
            source,
            Limits {
                max_work: work,
                ..limits
            },
            Limit::Work,
            start,
            end,
        );
    }
    assert_limit(
        source,
        Limits {
            max_tokens: 0,
            ..limits
        },
        Limit::Tokens,
        2,
        3,
    );
    assert_limit(
        source,
        Limits {
            max_source_bytes: 1,
            ..limits
        },
        Limit::SourceBytes,
        0,
        2,
    );
    assert_limit(
        "0x1🔑",
        Limits {
            max_work: 6,
            ..Limits::default()
        },
        Limit::Work,
        3,
        7,
    );
    assert_limit(
        "0b12",
        Limits {
            max_work: 3,
            ..Limits::default()
        },
        Limit::Work,
        3,
        4,
    );
}

#[test]
fn long_literals_are_bounded_by_source_and_work_not_the_decimal_cap() {
    let bytes = Limits::default().max_source_bytes;
    let at_cap = format!("0x0{}1", "_0".repeat((bytes - 4) / 2));
    let limits = Limits {
        max_work: bytes + 1,
        ..Limits::default()
    };
    assert_eq!(
        parse_script_with_limits(&at_cap, limits)
            .unwrap()
            .evaluate(),
        Ok(1.0)
    );
    assert_limit(
        &at_cap,
        Limits {
            max_work: bytes - 1,
            ..limits
        },
        Limit::Work,
        bytes - 1,
        bytes,
    );
    let above_cap = at_cap + "0";
    assert_limit(&above_cap, limits, Limit::SourceBytes, bytes, bytes + 1);
    assert_number("0o00000000", 0.0);

    let overflow = format!("0b1{}", "0".repeat(5000));
    let end = overflow.len();
    assert_number(&overflow, f64::INFINITY);
    assert_error(
        &format!("{overflow}n"),
        ErrorKind::Unsupported,
        end,
        end + 1,
    );
    assert_error(&format!("{overflow}2"), ErrorKind::Malformed, end, end + 1);
    assert_limit(
        &overflow,
        Limits {
            max_work: end - 1,
            ..Limits::default()
        },
        Limit::Work,
        end - 1,
        end,
    );
}

#[test]
fn separated_overflow_rejects_invalid_tails_in_all_bases() {
    for (prefix, digit, digit_count, out_of_base) in [
        ("0b", '1', 1025, '2'),
        ("0o", '7', 342, '8'),
        ("0x", 'f', 257, 'g'),
    ] {
        let overflow = separate_radix_digits(&format!(
            "{prefix}{}",
            digit.to_string().repeat(digit_count)
        ));
        let end = overflow.len();
        assert_number(&overflow, f64::INFINITY);
        assert_error(&format!("{overflow}_"), ErrorKind::Malformed, end, end + 1);
        assert_error(
            &format!("{overflow}_{out_of_base}"),
            ErrorKind::Malformed,
            end,
            end + 1,
        );
        assert_error(
            &format!("{overflow}n"),
            ErrorKind::Unsupported,
            end,
            end + 1,
        );
        assert_error(
            &format!("{overflow}é"),
            ErrorKind::Unsupported,
            end,
            end + 2,
        );
    }
}
