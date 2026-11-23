use super::{EXPONENT_MASK, FRACTION_MASK, MAX_UNITS, format_bits, ryu};

fn assert_format(bits: u64, expected: &str) {
    let mut buf = [u16::MAX; MAX_UNITS];
    let got = format_bits(bits, &mut buf);
    assert!(
        got.iter().copied().eq(expected.encode_utf16()),
        "{bits:016x}: expected {expected}, got {}",
        String::from_utf16_lossy(got)
    );
    assert!(buf[expected.len()..].iter().all(|&unit| unit == u16::MAX));
}

#[test]
fn special_values_from_raw_bits() {
    for (bits, expected) in [
        (0x0000_0000_0000_0000, "0"),
        (0x8000_0000_0000_0000, "0"),
        (0x7ff0_0000_0000_0000, "Infinity"),
        (0xfff0_0000_0000_0000, "-Infinity"),
        (0x7ff0_0000_0000_0001, "NaN"),
        (0xfff0_0000_0000_0010, "NaN"),
        (0x7ff8_0000_0000_0000, "NaN"),
        (0xffff_ffff_ffff_ffff, "NaN"),
    ] {
        assert_format(bits, expected);
    }
}

#[test]
fn halfway_decimal_candidates_choose_even() {
    // At 2^50, one ULP is 0.25. The shortest choices for the first case
    // end in .2 or .3; for the second, .7 or .8. Pick the even digit.
    assert_format(0x4310_0000_0000_0001, "1125899906842624.2");
    assert_format(0x4310_0000_0000_0003, "1125899906842624.8");
    assert_format(0xc310_0000_0000_0001, "-1125899906842624.2");
}

#[test]
fn binary_interval_endpoints_and_extremes() {
    for (bits, expected) in [
        (0x0000_0000_0000_0001, "5e-324"),
        (0x0000_0000_0000_0002, "1e-323"),
        (0x8000_0000_0000_0001, "-5e-324"),
        (0x000f_ffff_ffff_ffff, "2.225073858507201e-308"),
        (0x0010_0000_0000_0000, "2.2250738585072014e-308"),
        (0x0010_0000_0000_0001, "2.225073858507202e-308"),
        (0x7fef_ffff_ffff_ffff, "1.7976931348623157e+308"),
        (0xffef_ffff_ffff_ffff, "-1.7976931348623157e+308"),
        (0x434f_ffff_ffff_ffff, "18014398509481982"),
        (0x4350_0000_0000_0000, "18014398509481984"),
        (0x4350_0000_0000_0001, "18014398509481988"),
        // 1.9e22 is the lower midpoint of ...be18, not owned by ...be17.
        (0x4490_17f7_df96_be17, "1.8999999999999998e+22"),
        (0x4490_17f7_df96_be18, "1.9e+22"),
        // 10^23 is the exact binary midpoint between ...4af6 and ...4af7;
        // it belongs only to the even significand (...4af6).
        (0x44b5_2d02_c7e1_4af5, "9.999999999999997e+22"),
        (0x44b5_2d02_c7e1_4af6, "1e+23"),
        (0x44b5_2d02_c7e1_4af7, "1.0000000000000001e+23"),
    ] {
        assert_format(bits, expected);
    }
}

#[test]
fn notation_thresholds_and_maximum_utf16_length() {
    for (bits, expected) in [
        (0x3e7a_d7f2_9abc_af48, "1e-7"),
        (0x3eb0_c6f7_a0b5_ed8c, "9.999999999999997e-7"),
        (0x3eb0_c6f7_a0b5_ed8d, "0.000001"),
        (0x3eb0_c6f7_a0b5_ed8e, "0.0000010000000000000002"),
        (0x3fb9_9999_9999_999a, "0.1"),
        (0x3ff8_0000_0000_0000, "1.5"),
        (0x43ab_c16d_674e_c801, "1000000000000000100"),
        (0x4415_af1d_78b5_8c40, "100000000000000000000"),
        (0x444b_1ae4_d6e2_ef4f, "999999999999999900000"),
        (0x444b_1ae4_d6e2_ef50, "1e+21"),
        (0x444b_1ae4_d6e2_ef51, "1.0000000000000001e+21"),
    ] {
        assert_format(bits, expected);
    }
    let longest = "-0.0000010000000000000002";
    assert_eq!(longest.len(), MAX_UNITS);
    assert_format(0xbeb0_c6f7_a0b5_ed8e, longest);
}

fn assert_round_trip(bits: u64) {
    let exponent = ((bits >> 52) & EXPONENT_MASK) as i32;
    let fraction = bits & FRACTION_MASK;
    if exponent == 0x7ff || (exponent == 0 && fraction == 0) {
        // Raw specials are tested separately; zero deliberately drops its sign.
        return;
    }
    let (digits, _) = ryu::shortest(fraction, exponent);
    assert!(
        digits != 0 && digits < 100_000_000_000_000_000 && digits % 10 != 0,
        "{bits:016x}"
    );
    let mut buf = [0; MAX_UNITS];
    let units = format_bits(bits, &mut buf);
    let text = String::from_utf16(units).unwrap();
    assert_eq!(
        text.parse::<f64>().unwrap().to_bits(),
        bits,
        "{bits:016x}: {text}"
    );
}

#[test]
fn deterministic_boundary_round_trips() {
    // Include both even/odd endpoints and powers of two for every field,
    // including the subnormal bin and the highest finite exponent.
    for exponent in 0..0x7ff {
        for fraction in [
            0,
            1,
            2,
            3,
            (1 << 51) - 1,
            1 << 51,
            (1 << 51) + 1,
            FRACTION_MASK - 1,
            FRACTION_MASK,
        ] {
            let magnitude = (exponent << 52) | fraction;
            assert_round_trip(magnitude);
            assert_round_trip(magnitude | (1 << 63));
        }
    }
    // Powers of ten and their immediate finite neighbors; only the integer
    // exponent is formatted by Rust. Parsing here is just test scaffolding.
    for power in -323..=308 {
        let bits = format!("1e{power}").parse::<f64>().unwrap().to_bits();
        for magnitude in bits - 1..=bits + 1 {
            assert_round_trip(magnitude);
            assert_round_trip(magnitude | (1 << 63));
        }
    }
}

#[test]
fn deterministic_raw_bit_round_trips() {
    let mut bits = 0x5f37_59df_243f_6a88u64;
    for _ in 0..65_536 {
        bits ^= bits << 13;
        bits ^= bits >> 7;
        bits ^= bits << 17;
        assert_round_trip(bits);
    }
}

#[test]
#[ignore = "opt-in: supply a Node-verified SWAYZE_NUMBER_FORMAT_TSV file"]
fn node_oracle_tsv() {
    let path = std::env::var_os("SWAYZE_NUMBER_FORMAT_TSV").expect("set SWAYZE_NUMBER_FORMAT_TSV");
    let fixtures = std::fs::read_to_string(path).expect("read the oracle TSV");
    assert!(!fixtures.is_empty() && fixtures.ends_with('\n'));
    for line in fixtures.split_terminator('\n') {
        let (hex, expected) = line.split_once('\t').expect("bits<TAB>text");
        assert!(
            hex.len() == 16
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert!(!expected.is_empty() && expected.is_ascii() && !expected.contains('\t'));
        assert_format(u64::from_str_radix(hex, 16).unwrap(), expected);
    }
}
