//! Dynamic descriptor fixtures never acquire radio or site hardware.
use super::*;
use crate::site::equipment_fixture::{endpoint, input, output, sensor};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "rpt-site-loader-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn provider(&self, name: &str, variant: &str) {
        let header =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../{name}-adapter/include"));
        let safe_type = if name == "gpiod" {
            "uint32_t"
        } else {
            "double"
        };
        let source = format!(
            r#"
#include "rptadv_{name}_adapter.h"
static uint32_t opens;
uint32_t fixture_open_count(void) {{ return opens; }}
static int32_t open_endpoint(const rptadv_{name}_endpoint_v1 *e, void **c) {{ ++opens; (void)e; *c = 0; return 1; }}
static int32_t read_endpoint(void *c, rptadv_{name}_observation_v1 *o) {{ (void)c; (void)o; return 1; }}
static int32_t write_endpoint(void *c, const rptadv_{name}_output_v1 *o) {{ (void)c; (void)o; return 1; }}
static int32_t capabilities(void *c, uint32_t *o) {{ (void)c; (void)o; return 1; }}
static void destroy(void *c) {{ (void)c; }}
static int32_t set_safe(void *c, {safe_type} level) {{ (void)c; (void)level; return 0; }}
static rptadv_{name}_descriptor_v1 table = {{
    .struct_size=sizeof(table), .abi_version=1, .capability="rptadv.{name}",
    .open=open_endpoint, .read=read_endpoint, .write=write_endpoint,
    .capabilities=capabilities, .destroy=destroy, .set_safe=set_safe
}};
const rptadv_{name}_descriptor_v1 *rptadv_{name}_adapter_descriptor_v1(void) {{ {variant} return &table; }}
"#
        );
        let input = self.0.join(format!("{name}.c"));
        fs::write(&input, source).unwrap();
        let result = Command::new("cc")
            .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"])
            .arg("-I")
            .arg(header)
            .arg(input)
            .arg("-o")
            .arg(self.0.join(format!("librptadv_{name}_adapter.so.1")))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn config(backend: &str, enabled: bool) -> ResolvedSiteSettings {
    match backend {
        "gpiod" => {
            let mut probe = input("probe", endpoint(SiteBackend::Gpiod, "/no-device", "0"));
            probe.enabled = enabled;
            ResolvedSiteSettings {
                inputs: vec![probe],
                ..Default::default()
            }
        }
        "hwmon" => {
            let mut probe = sensor(
                "probe",
                endpoint(SiteBackend::Hwmon, "/no-device", "temp1_input"),
            );
            probe.enabled = enabled;
            ResolvedSiteSettings {
                sensors: vec![probe],
                ..Default::default()
            }
        }
        _ => panic!("unexpected fixture backend"),
    }
}

#[test]
fn site_backends_absent_unselected_is_allowed_selected_is_required() {
    let fixture = Fixture::new();
    let empty = SiteBackends::load_scopes_at([&config("hwmon", false)], &fixture.0).unwrap();
    assert!(empty.hwmon.is_none() && empty.gpiod.is_none());
    assert!(SiteBackends::load_scopes_at([&config("hwmon", true)], &fixture.0).is_err());
    for name in ["hwmon", "gpiod"] {
        assert!(SiteBackends::load_scopes_at([&config(name, true)], &fixture.0).is_err());
    }
    let local = crate::fixture::station::LOCAL;
    let disabled = crate::fixture::station::prepare(
        &[(local, "1000", serde_json::json!({"enabled":false}))],
        &[(
            local,
            rpt_advanced_core::config::resources::candidate::ResourceKind::Input,
            "door",
            serde_json::json!({"enabled":true,"source":{"type":"gpiod","device":"/absent","line":"0"}}),
        )],
    );
    let settings = &disabled.runtime().acquisition;
    assert!(required(settings.values()).is_empty());
    assert!(SiteBackends::load_scopes_at(settings.values(), &fixture.0).is_ok());
    let relay = ResolvedSiteSettings {
        outputs: vec![output(
            "relay",
            endpoint(SiteBackend::Gpiod, "/absent", "0"),
        )],
        ..Default::default()
    };
    assert!(SiteBackends::load_scopes_at([&relay], &fixture.0).is_err());
    let iio = ResolvedSiteSettings {
        sensors: vec![sensor(
            "volt",
            endpoint(SiteBackend::Iio, "/absent", "in_voltage0"),
        )],
        ..Default::default()
    };
    assert!(SiteBackends::load_scopes_at([&iio], &fixture.0).is_ok());
}

#[test]
fn enabled_port_adds_its_provider_and_poll_demand_to_the_shared_site_sensor() {
    use crate::fixture::station::{LOCAL, SITE, prepare};
    use rpt_advanced_core::config::resources::candidate::{ResourceKind, ResourceScope};
    use serde_json::json;

    let site = ResourceScope::Site(SITE.parse().unwrap());
    let port = ResourceScope::Port(LOCAL.parse().unwrap());
    for enabled in [false, true] {
        let configuration = prepare(
            &[(LOCAL, "1000", json!({"enabled": enabled}))],
            &[
                (
                    SITE,
                    ResourceKind::Sensor,
                    "temperature",
                    json!({
                        "enabled": true, "source": {"type":"hwmon","device":"/board","channel":"temp1_input"},
                        "timing":{"poll_interval_ms":1000}
                    }),
                ),
                (
                    LOCAL,
                    ResourceKind::Sensor,
                    "temperature",
                    json!({"timing":{"poll_interval_ms":75}}),
                ),
                (
                    LOCAL,
                    ResourceKind::Input,
                    "door",
                    json!({
                        "enabled":true,"source":{"type":"gpiod","device":"/absent","line":"0"}
                    }),
                ),
            ],
        );
        let runtime = configuration.runtime();
        let effective = &runtime.scopes[&port].equipment.settings;
        assert!(
            effective
                .inputs
                .iter()
                .any(|input| input.name == "door" && input.enabled)
        );
        assert_eq!(effective.sensors[0].poll_interval_ms, 75);
        let plan = &runtime.acquisition;
        let selected = required(plan.values());
        assert!(selected.contains(&SiteBackend::Hwmon));
        assert_eq!(selected.contains(&SiteBackend::Gpiod), enabled);
        assert_eq!(plan.values().map(|v| v.sensors.len()).sum::<usize>(), 1);
        assert_eq!(
            plan[&site].sensors[0].poll_interval_ms,
            if enabled { 75 } else { 1000 }
        );
    }
}

#[test]
fn modular_site_plan_admits_only_enabled_physical_owners_before_opening_devices() {
    use rpt_advanced_core::config::{Calibration, SensorSettings, SiteEndpoint};
    let fixture = Fixture::new();
    let mut equipment = ResolvedSiteSettings::default();
    equipment.sensors.push(SensorSettings {
        name: "cabinet temperature".into(),
        enabled: false,
        endpoint: Some(SiteEndpoint {
            backend: SiteBackend::Hwmon,
            device: "/absent-device".into(),
            channel: "temp1_input".into(),
        }),
        interface: None,
        label_message_id: String::new(),
        unit: "C".into(),
        precision: 1,
        poll_interval_ms: 1000,
        stale_after_ms: 5000,
        smoothing_ms: 0,
        calibration: Calibration::None,
        dtmf_id: None,
    });
    let old = SiteBackends::load_scopes_at([&equipment], &fixture.0).unwrap();
    assert!(old.hwmon.is_none());
    equipment.sensors[0].enabled = true;
    assert!(SiteBackends::load_scopes_at([&equipment], &fixture.0).is_err());
    fixture.provider("hwmon", "");
    let new = SiteBackends::load_scopes_at([&equipment], &fixture.0).unwrap();
    let loaded = new.hwmon.as_ref().unwrap();
    // SAFETY: this fixture provides the exact function and retains its DSO while it runs.
    let opens = unsafe {
        loaded
            ._library
            .get::<unsafe extern "C" fn() -> u32>(b"fixture_open_count")
            .unwrap()()
    };
    assert_eq!(opens, 0);
    assert!(new.gpiod.is_none());
    assert!(old.hwmon.is_none());
}

#[test]
fn site_backends_complete_selected_tables_load_and_reload_without_new_code() {
    let fixture = Fixture::new();
    for name in ["hwmon", "gpiod"] {
        fixture.provider(name, "");
        let providers = SiteBackends::load_scopes_at([&config(name, true)], &fixture.0).unwrap();
        fs::remove_file(fixture.0.join(format!("librptadv_{name}_adapter.so.1"))).unwrap();
        let retained = match name {
            "gpiod" => &providers.gpiod.as_ref().unwrap()._library,
            "hwmon" => &providers.hwmon.as_ref().unwrap()._library,
            _ => unreachable!(),
        };
        // SAFETY: the retained library provides this exact fixture function even after unlink.
        let opens = unsafe {
            retained
                .get::<unsafe extern "C" fn() -> u32>(b"fixture_open_count")
                .unwrap()()
        };
        assert_eq!(opens, 0);
        drop(providers);
        assert!(SiteBackends::load_scopes_at([&config(name, false)], &fixture.0).is_ok());
        assert!(SiteBackends::load_scopes_at([&config(name, true)], &fixture.0).is_err());
    }
}

#[test]
fn site_backends_reject_incomplete_or_incompatible_descriptor() {
    for name in ["hwmon", "gpiod"] {
        for variant in [
            "return 0;",
            "table.struct_size=4;",
            "table.abi_version=2;",
            "table.capability[0]='?';",
            "table.open=0;",
            "table.read=0;",
            "table.write=0;",
            "table.capabilities=0;",
            "table.destroy=0;",
            "table.set_safe=0;",
        ] {
            let fixture = Fixture::new();
            fixture.provider(name, variant);
            assert!(
                SiteBackends::load_scopes_at([&config(name, true)], &fixture.0).is_err(),
                "{name}: {variant}"
            );
        }
    }
}
