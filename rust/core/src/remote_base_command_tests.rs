use super::*;

#[test]
fn familiar_status_51_is_detailed_and_52_is_short() {
    assert_eq!(
        parse_remote_command("51"),
        Some(RemoteCommand::Status { long: true })
    );
    assert_eq!(
        parse_remote_command("52"),
        Some(RemoteCommand::Status { long: false })
    );
}

#[test]
fn remote_commands_copy_exact_units_and_flags_into_the_shared_wire_request() {
    use RemoteCommand as C;
    let commands = [
        C::Recall(1),
        C::Frequency {
            hertz: 146_940_000,
            offset: RemoteOffset::Minus,
        },
        C::Tone {
            encode: true,
            tenths_hz: 1148,
        },
        C::ToneEnabled {
            encode: false,
            enabled: true,
        },
        C::PowerPreset(2),
        C::Status { long: true },
        C::Mode(3),
        C::Step(-500),
        C::ScanPreset {
            up: true,
            preset: 1,
        },
        C::Tune,
        C::GetSetting(101),
        C::SetSetting {
            selector: 102,
            value: DecimalValue {
                negative: true,
                magnitude: 12345,
                decimal_places: 3,
            },
        },
        C::StartScan(2),
        C::PauseScan,
        C::ResumeScan,
        C::StopScan,
        C::ScanDirection(true),
        C::Skip,
        C::Memory(3),
        C::Catalog {
            kind: CatalogKind::ScanLists,
            page: 2,
        },
        C::ScanStatus(4),
        C::Inhibit(true),
        C::Dwell(700),
        C::ResumeDelay(2300),
        C::Priority(Some(5)),
        C::CurrentStatus,
        C::Invoke(9),
    ];
    for (index, command) in commands.into_iter().enumerate() {
        let operation = command.wire();
        assert_eq!(operation.struct_size, 40);
        assert_eq!(operation.abi_version, 1);
        assert_eq!(operation.operation, index as u32 + 1);
    }
    let frequency = commands[1].wire();
    assert_eq!((frequency.magnitude, frequency.flags), (146_940_000, 1));
    let tone = commands[2].wire();
    assert_eq!((tone.magnitude, tone.flags), (1148, 1));
    assert_eq!(commands[7].wire().signed_value, -500);
    let scalar = commands[11].wire();
    assert_eq!(
        (
            scalar.selector,
            scalar.magnitude,
            scalar.decimal_places,
            scalar.flags
        ),
        (102, 12345, 3, 1)
    );
    let catalog = commands[19].wire();
    assert_eq!((catalog.selector, catalog.magnitude), (3, 2));
    assert_eq!(C::Priority(None).wire().selector, 0);
    assert_eq!(
        C::Frequency {
            hertz: 146520000,
            offset: RemoteOffset::Simplex
        }
        .wire()
        .flags,
        2
    );
    assert_eq!(
        C::Frequency {
            hertz: 146520000,
            offset: RemoteOffset::Plus
        }
        .wire()
        .flags,
        3
    );
}

#[test]
fn remote_dtmf_parses_familiar_operating_commands_without_floats() {
    use RemoteCommand as C;
    for (digits, expected) in [
        ("01", C::Recall(1)),
        (
            "1146*52*2",
            C::Frequency {
                hertz: 146_520_000,
                offset: RemoteOffset::Simplex,
            },
        ),
        (
            "1146*52000*1",
            C::Frequency {
                hertz: 146_520_000,
                offset: RemoteOffset::Minus,
            },
        ),
        (
            "2114*8",
            C::Tone {
                encode: false,
                tenths_hz: 1148,
            },
        ),
        (
            "3114*8",
            C::Tone {
                encode: true,
                tenths_hz: 1148,
            },
        ),
        (
            "40",
            C::ToneEnabled {
                encode: false,
                enabled: false,
            },
        ),
        (
            "41",
            C::ToneEnabled {
                encode: false,
                enabled: true,
            },
        ),
        (
            "42",
            C::ToneEnabled {
                encode: true,
                enabled: false,
            },
        ),
        (
            "43",
            C::ToneEnabled {
                encode: true,
                enabled: true,
            },
        ),
        ("44", C::PowerPreset(0)),
        ("45", C::PowerPreset(1)),
        ("46", C::PowerPreset(2)),
        ("51", C::Status { long: true }),
        ("52", C::Status { long: false }),
        ("61", C::Mode(1)),
        ("62", C::Mode(3)),
        ("63", C::Mode(4)),
        ("64", C::Mode(2)),
        ("711", C::Step(-20)),
        ("714", C::Step(-100)),
        ("717", C::Step(-500)),
        ("713", C::Step(20)),
        ("716", C::Step(100)),
        ("719", C::Step(500)),
        (
            "721",
            C::ScanPreset {
                up: false,
                preset: 0,
            },
        ),
        (
            "724",
            C::ScanPreset {
                up: false,
                preset: 1,
            },
        ),
        (
            "727",
            C::ScanPreset {
                up: false,
                preset: 2,
            },
        ),
        (
            "723",
            C::ScanPreset {
                up: true,
                preset: 0,
            },
        ),
        (
            "726",
            C::ScanPreset {
                up: true,
                preset: 1,
            },
        ),
        (
            "729",
            C::ScanPreset {
                up: true,
                preset: 2,
            },
        ),
        ("79", C::Tune),
    ] {
        assert_eq!(parse_remote_command(digits), Some(expected), "{digits}");
    }
}

#[test]
fn remote_dtmf_parses_extended_catalog_and_scan_operations() {
    use RemoteCommand as C;
    for (digits, expected) in [
        ("901", C::GetSetting(1)),
        (
            "91100*0*5*1",
            C::SetSetting {
                selector: 100,
                value: DecimalValue {
                    negative: false,
                    magnitude: 5,
                    decimal_places: 1,
                },
            },
        ),
        ("922", C::StartScan(2)),
        ("93", C::PauseScan),
        ("94", C::ResumeScan),
        ("95", C::StopScan),
        ("960", C::ScanDirection(false)),
        ("961", C::ScanDirection(true)),
        ("97", C::Skip),
        ("981", C::Memory(1)),
        (
            "9901",
            C::Catalog {
                kind: CatalogKind::Capabilities,
                page: 1,
            },
        ),
        (
            "9912",
            C::Catalog {
                kind: CatalogKind::Memories,
                page: 2,
            },
        ),
        (
            "9923",
            C::Catalog {
                kind: CatalogKind::ScanLists,
                page: 3,
            },
        ),
        ("9932", C::ScanStatus(2)),
        ("9940", C::Inhibit(false)),
        ("9941", C::Inhibit(true)),
        ("995500", C::Dwell(500)),
        ("9960", C::ResumeDelay(0)),
        ("9970", C::Priority(None)),
        ("9972", C::Priority(Some(2))),
        ("998", C::CurrentStatus),
        ("9991", C::Invoke(1)),
    ] {
        assert_eq!(parse_remote_command(digits), Some(expected), "{digits}");
    }
}

#[test]
fn remote_dtmf_rejects_partial_malformed_overflow_and_unsupported_precision() {
    for digits in [
        "",
        "0",
        "00",
        "1",
        "1146*52",
        "1146*520000*2",
        "1146*52*4",
        "1146**2",
        "10*00000*2",
        "2",
        "2114*88",
        "2114",
        "2114*8*1",
        "901x",
        "910*0*1*0",
        "911*2*1*0",
        "911*0*1*7",
        "911*0*18446744073709551616*0",
        "911*0*9223372036854775808*0",
        "911*1*9223372036854775809*0",
        "9900",
        "9950",
        "996",
        "997",
        "9990",
        "9942",
    ] {
        assert!(parse_remote_command(digits).is_none(), "{digits}");
    }
}

#[test]
fn remote_dtmf_fixed_point_is_exact_and_preserves_negative_zero_for_enum_validation() {
    let value = DecimalValue {
        negative: true,
        magnitude: 15,
        decimal_places: 1,
    };
    assert_eq!(value.at_decimal_places(2), Some(-150));
    assert_eq!(value.at_decimal_places(0), None);
    let value = DecimalValue {
        negative: true,
        magnitude: 1_u64 << 63,
        decimal_places: 0,
    };
    assert_eq!(value.at_decimal_places(0), Some(i64::MIN));
    assert_eq!(value.at_decimal_places(1), None);
    let value = DecimalValue {
        negative: true,
        magnitude: 0,
        decimal_places: 0,
    };
    assert_eq!(value.at_decimal_places(0), Some(0));
    assert!(value.negative);
}

#[test]
fn remote_dtmf_marks_queries_public_and_mutations_administrative() {
    for text in [
        "51", "52", "901", "981", "9901", "9911", "9921", "9931", "998",
    ] {
        assert!(
            !parse_remote_command(text).unwrap().administrative(),
            "{text}"
        );
    }
    for text in [
        "01",
        "1146*52*2",
        "2114*8",
        "43",
        "44",
        "61",
        "711",
        "721",
        "79",
        "911*0*1*0",
        "921",
        "93",
        "94",
        "95",
        "961",
        "97",
        "9940",
        "995500",
        "9960",
        "9970",
        "9991",
    ] {
        assert!(
            parse_remote_command(text).unwrap().administrative(),
            "{text}"
        );
    }
}
