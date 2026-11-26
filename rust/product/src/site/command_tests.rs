//! Public typed site requests use the active shared policy, not link commands.
use crate::{
    abi,
    fixture::station::{self, LOCAL},
    lifecycle::*,
};
use rpt_advanced_core::config::resources::candidate::ResourceKind;
use serde_json::json;
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
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let api = unsafe { &*rptadv_product_descriptor_v1() };
    assert_eq!(crate::lifecycle::tests::fixture_stop(), 0);
    let invoke = |local: &str, request: &abi::rptadv_site_command_v1| unsafe {
        api.site_command.unwrap()(local.as_ptr().cast(), local.len(), request)
    };
    assert_eq!(invoke(LOCAL, &request(9, "", 0.0)), -1);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    let config = station::prepare(
        &[(LOCAL, "1000", json!({}))],
        &[
            (
                LOCAL,
                ResourceKind::Interface,
                "",
                json!({"audio":{"device_identifier":"usb"}}),
            ),
            (
                LOCAL,
                ResourceKind::Output,
                "relay",
                json!({"enabled":true,"source":{"type":"gpiod","device":"fixture","line":"7"}}),
            ),
            (
                LOCAL,
                ResourceKind::Output,
                "cooler",
                json!({"enabled":true,"source":{"type":"hwmon","device":"fixture","channel":"pwm1"}}),
            ),
            (
                LOCAL,
                ResourceKind::Fan,
                "cooling",
                json!({"enabled":true,"output":"cooler","mode":"ptt","ptt_nodes":["1000"]}),
            ),
            (LOCAL, ResourceKind::Sensor, "battery", json!({})),
        ],
    );
    let _hardware = crate::site::backends::endpoint::tests::physical_fixture();
    // SAFETY: descriptors are static and every device callback uses the serialized fixture.
    let started = unsafe {
        crate::lifecycle::tests::fixture_engine_with_site(
            config,
            crate::fixture::host_descriptor(),
            crate::fixture::control_descriptor(),
            Some(crate::site::backends::endpoint::tests::backends()),
            None,
        )
    }
    .unwrap();
    let (engine, call) = selected().unwrap();
    assert!(std::sync::Arc::ptr_eq(&started, &engine));
    drop(started);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !engine
        .run(|host| {
            let observations = host.poll_sites(0);
            let Some(values) = observations.get(LOCAL) else {
                return Ok(false);
            };
            let ready = values
                .outputs
                .get("relay")
                .is_some_and(|out| out.available && !out.pwm)
                && values
                    .outputs
                    .get("cooler")
                    .is_some_and(|out| out.available && out.pwm);
            if ready {
                host.runtime
                    .advance_equipment(LOCAL, 0, values, &Default::default());
            }
            Ok(ready)
        })
        .unwrap()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "both simulated physical output owners are ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
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
            invoke(LOCAL, &request(operation, target, value)),
            0,
            "{operation}"
        );
        let state = engine
            .run(|host| Ok(host.runtime.node(LOCAL).unwrap().site().snapshot(0)))
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
        assert_eq!(invoke(LOCAL, &request(operation, "missing", 0.0)), -2);
    }
    assert_eq!(invoke(LOCAL, &request(1, "cooler", 0.0)), -3);
    assert_eq!(invoke(LOCAL, &request(7, "missing", 0.0)), -1);
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
        assert_eq!(invoke(LOCAL, &request(operation, name, value)), -4);
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
        assert_eq!(invoke(LOCAL, &invalid), -4);
    }
    let mut unnamed = request(9, "", 0.0);
    unnamed.target = ptr::null();
    assert_eq!(invoke(LOCAL, &unnamed), 0);
    assert!(
        engine
            .run(|host| Ok(host
                .runtime
                .node(LOCAL)
                .unwrap()
                .site()
                .snapshot(0)
                .transmit_inhibited))
            .unwrap()
    );
    assert_eq!(
        unsafe { api.site_command.unwrap()(LOCAL.as_ptr().cast(), LOCAL.len(), ptr::null()) },
        -4
    );
    assert_eq!(
        unsafe { api.site_command.unwrap()(ptr::null(), 4, &request(9, "", 0.0)) },
        -4
    );
    crate::fixture::CONTROL_SUBMIT_MODE.store(1, Ordering::Release);
    assert_eq!(invoke(LOCAL, &request(9, "", 0.0)), -1);
    crate::fixture::CONTROL_SUBMIT_MODE.store(0, Ordering::Release);
    drop(call);
    assert_eq!(unsafe { api.stop.unwrap()() }, 0);
}
