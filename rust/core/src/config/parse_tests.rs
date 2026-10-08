use super::parse;

#[test]
fn empty_section_names_are_rejected_by_the_line_parser() {
    for text in ["[]", "[ ]", "[\t]"] {
        assert!(parse::parse_line(text).is_err());
    }
}

#[test]
fn signed_decimal_accepts_only_an_optional_minus_sign() {
    assert_eq!(parse::signed("-60", -60, 0), Some(-60));
    assert_eq!(
        parse::signed("-9223372036854775808", i64::MIN, i64::MAX),
        Some(i64::MIN)
    );
    assert_eq!(parse::signed("0", -60, 0), Some(0));

    for invalid in ["", "+1", "--1", " 1", "1 ", "1s", "9223372036854775808"] {
        assert_eq!(parse::signed(invalid, i64::MIN, i64::MAX), None);
    }
}

#[test]
fn ctcss_and_dcs_parsers_accept_only_supported_radio_codes() {
    assert_eq!(parse::ctcss_tone_tenths_hz("100.0"), Some(1_000));
    assert_eq!(parse::ctcss_tones("100,103.5"), Some(vec![1_000, 1_035]));
    for invalid in ["", "100.00", "100.1", "60.0", "100.0,100"] {
        assert_eq!(parse::ctcss_tones(invalid), None);
    }

    assert_eq!(parse::dcs_code("023N"), Some((0o23, false)));
    assert_eq!(parse::dcs_code("047i"), Some((0o47, true)));
    for invalid in ["", "23N", "028N", "023X", "023NI"] {
        assert_eq!(parse::dcs_code(invalid), None);
    }
}

#[test]
fn unsigned_decimal_and_boolean_grammar_match_the_configuration_contract() {
    assert_eq!(
        parse::unsigned("18446744073709551615", 0, u64::MAX),
        Some(u64::MAX)
    );
    assert_eq!(parse::unsigned("0010", 10, 10), Some(10));
    for invalid in ["", "-1", "+1", " 1", "1 ", "1s", "18446744073709551616"] {
        assert_eq!(parse::unsigned(invalid, 0, u64::MAX), None);
    }

    for yes in ["yes", "Yes", "yEs", "yeS", "YES"] {
        assert_eq!(parse::boolean(yes), Some(true));
    }
    for no in ["no", "No", "nO", "NO"] {
        assert_eq!(parse::boolean(no), Some(false));
    }
    for invalid in ["", "1", "true", "yes ", "no "] {
        assert_eq!(parse::boolean(invalid), None);
    }
}
