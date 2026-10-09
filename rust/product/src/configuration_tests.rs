use crate::{abi, lifecycle::rptadv_product_descriptor_v1};
use std::{
    ffi::{c_char, c_void},
    mem::size_of,
};

#[derive(Default, Debug)]
struct Collected {
    nodes: Vec<(String, String, bool, u16, String, u64)>,
    secrets: Vec<(String, String)>,
    diagnostics: Vec<String>,
    radios: Vec<CapturedRadio>,
}

#[derive(Debug)]
struct CapturedRadio {
    request: abi::UrpNativeStationConfig,
    session: abi::rptadv_radio_session_config,
    device: String,
    serial: String,
    receive_graph: String,
    transmit_graph: String,
}

unsafe fn text(pointer: *const c_char, length: usize) -> String {
    String::from_utf8(unsafe { std::slice::from_raw_parts(pointer.cast(), length) }.to_vec())
        .unwrap()
}

unsafe extern "C" fn node(
    context: *mut c_void,
    record: *const abi::rptadv_node_host_configuration,
) -> i32 {
    let record = unsafe { &*record };
    assert_eq!(
        record.struct_size as usize,
        size_of::<abi::rptadv_node_host_configuration>()
    );
    let output = unsafe { &mut *context.cast::<Collected>() };
    let native = unsafe { &*record.radio };
    output.radios.push(CapturedRadio {
        request: *native,
        session: unsafe { *native.radio },
        device: unsafe {
            text(
                native.device_identifier.cast(),
                native.device_identifier_length as usize,
            )
        },
        serial: unsafe { text(native.usb_serial.cast(), native.usb_serial_length as usize) },
        receive_graph: unsafe {
            text(
                native.receive_graph.cast(),
                native.receive_graph_length as usize,
            )
        },
        transmit_graph: unsafe {
            text(
                native.transmit_graph.cast(),
                native.transmit_graph_length as usize,
            )
        },
    });
    output.nodes.push((
        unsafe { text(record.node, record.node_length) },
        unsafe { text(record.channel, record.channel_length) },
        record.enabled != 0,
        record.iax_port,
        unsafe { text(record.registration_url, record.registration_url_length) },
        record.registration_interval_seconds,
    ));
    0
}

unsafe extern "C" fn secret(
    context: *mut c_void,
    name: *const c_char,
    name_length: usize,
    value: *const c_char,
    value_length: usize,
) -> i32 {
    unsafe { &mut *context.cast::<Collected>() }
        .secrets
        .push((unsafe { text(name, name_length) }, unsafe {
            text(value, value_length)
        }));
    0
}

unsafe extern "C" fn diagnostic(context: *mut c_void, pointer: *const c_char, length: usize) {
    unsafe { &mut *context.cast::<Collected>() }
        .diagnostics
        .push(unsafe { text(pointer, length) });
}

fn inspect(source: &str, secrets: bool) -> (i32, Collected) {
    let table = unsafe { &*rptadv_product_descriptor_v1() };
    assert_eq!(table.abi_version, 4);
    let mut output = Collected::default();
    let context = (&mut output as *mut Collected).cast();
    let result = unsafe {
        if secrets {
            table.inspect_secrets.unwrap()(
                source.as_ptr().cast(),
                source.len(),
                Some(secret),
                context,
                Some(diagnostic),
                context,
            )
        } else {
            table.inspect_configuration.unwrap()(
                source.as_ptr().cast(),
                source.len(),
                Some(node),
                context,
                Some(diagnostic),
                context,
            )
        }
    };
    (result, output)
}

#[test]
fn descriptor_resolves_inheritance_disabled_nodes_and_warnings_without_hardware() {
    let (result, output) = inspect(
        "[general]\niax_local_port=4570\niax_registration_url=https://register.example/\niax_registration_interval_s=90\n[1000]\nradio_channel=vhf\nunknown_setting=value\n[2000]\nnode_enabled=no\niax_local_port=4580\n",
        false,
    );
    assert_eq!(result, 0, "{output:?}");
    assert_eq!(
        output.nodes,
        vec![
            (
                "1000".into(),
                "vhf".into(),
                true,
                4570,
                "https://register.example/".into(),
                90
            ),
            (
                "2000".into(),
                "2000".into(),
                false,
                4580,
                "https://register.example/".into(),
                90
            ),
        ]
    );
    assert_eq!(output.diagnostics.len(), 1);
    assert!(output.diagnostics[0].contains("[1000] unknown_setting"));
}

#[test]
fn invalid_configuration_emits_no_partial_records() {
    let (result, output) = inspect("[1000]\n[permanent unknown 2000]\n", false);
    assert_eq!(result, -1);
    assert!(output.nodes.is_empty());
    assert_eq!(output.diagnostics.len(), 1);
    assert!(output.diagnostics[0].contains("unknown node"));
}

#[test]
fn secret_records_keep_node_overrides_and_redact_all_invalid_input() {
    let (result, output) = inspect(
        "[general]\niax_secret=global-secret\n[1000]\niax_secret=node-secret\n",
        true,
    );
    assert_eq!(result, 0);
    assert_eq!(
        output.secrets,
        vec![
            ("general".into(), "global-secret".into()),
            ("1000".into(), "node-secret".into())
        ]
    );
    for (source, message) in [
        ("private-secret", "invalid syntax"),
        ("[general]\nprivate=private-secret\n", "unknown option"),
        ("[general]\niax_secret=\n", "empty secret"),
        (
            "[private-secret]\niax_secret=private-secret\n",
            "invalid section",
        ),
    ] {
        let (result, output) = inspect(source, true);
        assert_eq!(result, -1);
        assert!(output.secrets.is_empty());
        assert_eq!(output.diagnostics, vec![message]);
    }
}

#[test]
fn native_requests_preserve_explicit_gains_graphs_hardware_and_signaling() {
    let (result, output) = inspect(
        "[radio]\ndevice_identifier=3-1\ncm119_profile=sphusb\ncm119_ptt_inverted=yes\ncm119_gpio_1_mode=out1\ncm119_clip_led_gpio=2\nreceive_input_gain_db=-2\nreceive_output_gain_db=-6\nreceive_graph=volume=0.5\ntransmit_graph=anull\n[1000]\n[radio 1000]\ncm119_profile=nhrc\ncm119_ptt_inverted=no\ncm119_gpio_1_mode=out0\nreceive_signaling=ctcss\nctcss_source=dsp\nreceive_ctcss_tones_hz=100.0,103.5\ntransmit_signaling=ctcss\ntransmit_ctcss_tones_hz=100.0,103.5\ntransmit_ctcss_default_hz=103.5\ntransmit_ctcss_turnoff_mode=tail_tone\ntransmit_ctcss_tail_tone_hz=55\n[2000]\n[radio 2000]\nusb_serial=SECOND\n",
        false,
    );
    assert_eq!(result, 0, "{output:?}");
    let first = &output.radios[0];
    let second = &output.radios[1];
    assert_eq!(first.device, "3-1");
    assert_eq!(second.device, "3-1");
    assert_eq!(second.serial, "SECOND");
    assert_eq!(first.receive_graph, "volume=0.5");
    assert_eq!(first.transmit_graph, "anull");
    assert_eq!(first.request.cm119_profile, 2);
    assert_eq!(first.request.ptt_inverted, 0);
    assert_eq!(first.request.gpio_output_initial_mask, 0);
    assert_eq!(second.request.cm119_profile, 1);
    assert_eq!(second.request.ptt_inverted, 1);
    assert_eq!(second.request.gpio_output_initial_mask, 1);
    assert_eq!(first.request.gpio_output_enable_mask, 1);
    assert_eq!(first.request.clip_led_mask, 2);
    assert_eq!(first.request.receive_output_gain_db, -6);
    assert!((first.session.receive_input_gain - 0.794_328_2).abs() < 0.000_001);
    assert_eq!(first.session.receive.ctcss_tone_mask, (1 << 11) | (1 << 12));
    assert_eq!(first.session.qualification.subaudible_source, 3);
    assert_eq!(
        first.session.transmit.default_ctcss_frequency_tenths_hz,
        1035
    );
    assert_eq!(first.session.transmit.tone_off_mode, 3);
    assert_eq!(first.session.transmit.ctcss_turnoff_tail_tone_hz, 55.0);
}

#[test]
fn check_only_uses_no_node_visitor_and_validates_the_shipped_example() {
    let table = unsafe { &*rptadv_product_descriptor_v1() };
    let source = include_str!("../../../examples/rpt_advanced.conf");
    let mut output = Collected::default();
    let result = unsafe {
        table.inspect_configuration.unwrap()(
            source.as_ptr().cast(),
            source.len(),
            None,
            std::ptr::null_mut(),
            Some(diagnostic),
            (&mut output as *mut Collected).cast(),
        )
    };
    assert_eq!(result, 0, "{output:?}");
    assert!(output.diagnostics.is_empty());
}

#[test]
fn inspection_rejects_malformed_spans_and_consumer_rejection_without_partial_secret_output() {
    let api = unsafe { &*rptadv_product_descriptor_v1() };
    let invalid = [0xff_u8];
    for (pointer, length) in [
        (std::ptr::null(), 1),
        (invalid.as_ptr().cast(), 1),
        (invalid.as_ptr().cast(), usize::MAX),
    ] {
        assert_eq!(
            unsafe {
                api.inspect_configuration.unwrap()(
                    pointer,
                    length,
                    None,
                    std::ptr::null_mut(),
                    None,
                    std::ptr::null_mut(),
                )
            },
            -1
        );
        let mut output = Collected::default();
        assert_eq!(
            unsafe {
                api.inspect_secrets.unwrap()(
                    pointer,
                    length,
                    None,
                    std::ptr::null_mut(),
                    Some(diagnostic),
                    (&mut output as *mut Collected).cast(),
                )
            },
            -1
        );
        assert_eq!(output.diagnostics, ["invalid syntax"]);
    }
    assert_eq!(
        unsafe {
            api.inspect_secrets.unwrap()(
                std::ptr::null(),
                0,
                None,
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
            )
        },
        0
    );
    let source = "[1000]\n";
    unsafe extern "C" fn reject_node(
        _: *mut c_void,
        _: *const abi::rptadv_node_host_configuration,
    ) -> i32 {
        -1
    }
    assert_eq!(
        unsafe {
            api.inspect_configuration.unwrap()(
                source.as_ptr().cast(),
                source.len(),
                Some(reject_node),
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
            )
        },
        -1
    );
    unsafe extern "C" fn reject_secret(
        _: *mut c_void,
        _: *const c_char,
        _: usize,
        _: *const c_char,
        _: usize,
    ) -> i32 {
        -1
    }
    let source = "[general]\niax_secret=private\n";
    assert_eq!(
        unsafe {
            api.inspect_secrets.unwrap()(
                source.as_ptr().cast(),
                source.len(),
                Some(reject_secret),
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
            )
        },
        -1
    );
    let (result, output) = inspect(
        "[general]\niax_secret=private\n[invalid]\niax_secret=private\n",
        true,
    );
    assert_eq!(result, -1);
    assert!(output.secrets.is_empty());
}

#[test]
fn native_request_selectors_preserve_all_hardware_profiles_and_speaker_input() {
    for (profile, value) in [("dudeusb", 0), ("sphusb", 1), ("nhrc", 2), ("custom", 3)] {
        let (result, output) = inspect(
            &format!(
                "[1000]\n[radio]\ncm119_profile={profile}\ndevice_selection=automatic_lowest_alsa_card\nreceive_audio=speaker\n"
            ),
            false,
        );
        assert_eq!(result, 0, "{output:?}");
        let native = &output.radios[0].request;
        assert_eq!(native.cm119_profile, value);
        assert_eq!(native.device_selection, 1);
        assert_eq!(native.receive_deemphasis, 0);
        assert_eq!(native.clip_led_mask, 0);
    }
}

#[test]
fn inspection_contains_internal_panics_without_exposing_their_payload() {
    let mut output = Collected::default();
    let status = super::boundary(
        Some(diagnostic),
        (&mut output as *mut Collected).cast(),
        || panic!("private-input"),
    );
    assert_eq!(status, -1);
    assert_eq!(output.diagnostics, ["configuration inspection failed"]);
}
