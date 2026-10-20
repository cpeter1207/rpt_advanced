//! Public typed site requests use the active shared policy, not link commands.
use crate::{abi, lifecycle::*};
use rpt_advanced_core::site::{FanOutput, SiteObservations};
use std::{mem::size_of, ptr, sync::atomic::Ordering};

fn request(operation: u32, target: &str, value: f64) -> abi::rptadv_site_command_v1 {
    abi::rptadv_site_command_v1 {
        struct_size: size_of::<abi::rptadv_site_command_v1>() as u32,
        abi_version: 1,
        operation,
        target: target.as_ptr().cast(),
        target_length: target.len(),
        value,
    }
}

#[test]
fn site_command_descriptor_validates_arguments_and_applies_current_policy() {
    let _serial = crate::fixture::LIFECYCLE.lock().unwrap();
    let api = unsafe { &*rptadv_product_descriptor_v1() };
    assert_eq!(unsafe { api.stop.unwrap()() }, 0);
    let invoke = |local: &str, request: &abi::rptadv_site_command_v1| unsafe {
        api.site_command.unwrap()(local.as_ptr().cast(), local.len(), request)
    };
    assert_eq!(invoke("1000", &request(9, "", 0.0)), -1);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let config = "[1000]\nradio_channel=usb\nduplex=full\n\
        [output 1000 relay]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=7\n\
        [output 1000 cooler]\nenabled=yes\nbackend=cm119\ndevice=1000\nline=8\n\
        [fan 1000 cooling]\nenabled=yes\noutput=cooler\nmode=ptt\nptt_nodes=1000\n\
        [sensor 1000 battery]\n";
    assert_eq!(
        unsafe {
            api.start.unwrap()(
                crate::fixture::host_descriptor(),
                crate::fixture::control_descriptor(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                config.as_ptr().cast(),
                config.len(),
            )
        },
        0
    );
    let (engine, call) = selected().unwrap();
    engine
        .run(|host| {
            let mut observations = SiteObservations::default();
            for name in ["relay", "cooler"] {
                observations.outputs.insert(
                    name.into(),
                    FanOutput {
                        available: true,
                        pwm: false,
                    },
                );
            }
            host.runtime
                .node("1000")
                .unwrap()
                .advance_site(0, &observations, &Default::default());
            Ok(())
        })
        .unwrap();
    for (operation, target, value) in [
        (1, "relay", 0.0),
        (2, "relay", 0.0),
        (3, "relay", 0.0),
        (6, "battery", 0.0),
        (8, "cooling", 0.0),
        (8, "cooling", 1.0),
        (8, "cooling", 0.75),
        (7, "cooling", 0.0),
        (9, "", 0.0),
        (10, "", 0.0),
    ] {
        assert_eq!(
            invoke("1000", &request(operation, target, value)),
            0,
            "{operation}"
        );
        let state = engine
            .run(|host| Ok(host.runtime.node("1000").unwrap().site().snapshot(0)))
            .unwrap();
        match operation {
            1 | 3 => assert!(state.outputs["relay"].requested_on),
            2 => assert!(!state.outputs["relay"].requested_on),
            8 => assert_eq!(state.fans["cooling"].duty, value),
            7 => assert_eq!(state.fans["cooling"].duty, 0.0),
            9 => assert!(state.transmit_inhibited),
            10 => assert!(!state.transmit_inhibited),
            _ => (),
        }
    }
    assert_eq!(invoke("missing", &request(9, "", 0.0)), -2);
    for operation in [1, 4, 5, 6] {
        assert_eq!(invoke("1000", &request(operation, "missing", 0.0)), -2);
    }
    assert_eq!(invoke("1000", &request(1, "cooler", 0.0)), -3);
    assert_eq!(invoke("1000", &request(7, "missing", 0.0)), -1);
    for (operation, name, value) in [
        (0, "relay", 0.0),
        (11, "relay", 0.0),
        (1, "", 0.0),
        (1, "relay", 1.0),
        (9, "relay", 0.0),
        (10, "", 1.0),
        (8, "cooling", -0.1),
        (8, "cooling", 1.1),
        (8, "cooling", f64::NAN),
        (8, "cooling", f64::INFINITY),
    ] {
        assert_eq!(invoke("1000", &request(operation, name, value)), -4);
    }
    for field in 0..5 {
        let mut invalid = request(1, "relay", 0.0);
        match field {
            0 => invalid.struct_size -= 1,
            1 => invalid.abi_version += 1,
            2 => invalid.target = ptr::null(),
            3 => invalid.target = c"\xff".as_ptr(),
            _ => invalid.struct_size += 1,
        }
        if field == 3 {
            invalid.target_length = 1;
        }
        assert_eq!(invoke("1000", &invalid), -4);
    }
    let mut unnamed = request(9, "", 0.0);
    unnamed.target = ptr::null();
    assert_eq!(invoke("1000", &unnamed), 0);
    assert!(
        engine
            .run(|host| Ok(host
                .runtime
                .node("1000")
                .unwrap()
                .site()
                .snapshot(0)
                .transmit_inhibited))
            .unwrap()
    );
    assert_eq!(
        unsafe { api.site_command.unwrap()(c"1000".as_ptr(), 4, ptr::null()) },
        -4
    );
    assert_eq!(
        unsafe { api.site_command.unwrap()(ptr::null(), 4, &request(9, "", 0.0)) },
        -4
    );
    crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
    assert_eq!(invoke("1000", &request(9, "", 0.0)), -1);
    crate::fixture::CONTROL_SUBMIT_MODE.store(0, Ordering::Release);
    drop(call);
    assert_eq!(unsafe { api.stop.unwrap()() }, 0);
}
