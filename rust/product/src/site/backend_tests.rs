//! Dynamic descriptor fixtures never acquire radio or site hardware.
use super::*;
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
static int32_t open_endpoint(const rptadv_{name}_endpoint_v1 *e, void **c) {{ (void)e; *c = 0; return 1; }}
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
fn config(backend: &str, enabled: bool) -> ConfigDocument {
    let (kind, key, channel) = if backend == "gpiod" {
        ("input", "line", "0")
    } else {
        ("sensor", "channel", "temp1_input")
    };
    ConfigDocument::parse(&format!("[1000]\n[{kind} 1000 probe]\nenabled={}\nbackend={backend}\ndevice=/no-device\n{key}={channel}\n", if enabled { "yes" } else { "no" })).unwrap()
}

#[test]
fn site_backends_absent_unselected_is_allowed_selected_is_required() {
    let fixture = Fixture::new();
    let empty = SiteBackends::load_at(&config("hwmon", false), &fixture.0).unwrap();
    assert!(empty.validate(&config("hwmon", false)).is_ok());
    assert!(empty.validate(&config("hwmon", true)).is_err());
    for name in ["hwmon", "gpiod"] {
        assert!(SiteBackends::load_at(&config(name, true), &fixture.0).is_err());
    }
    let disabled = ConfigDocument::parse("[1000]\nnode_enabled=no\n[input 1000 door]\nenabled=yes\nbackend=gpiod\ndevice=/absent\nline=0\n").unwrap();
    assert!(SiteBackends::load_at(&disabled, &fixture.0).is_ok());
    assert!(crate::site::configured(&disabled).unwrap().is_empty());
    let output = ConfigDocument::parse(
        "[1000]\n[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=/absent\nline=0\n",
    )
    .unwrap();
    assert!(SiteBackends::load_at(&output, &fixture.0).is_err());
    let iio = ConfigDocument::parse("[1000]\n[sensor 1000 volt]\nenabled=yes\nbackend=iio\ndevice=/absent\nchannel=in_voltage0\n").unwrap();
    assert!(SiteBackends::load_at(&iio, &fixture.0).is_ok());
}

#[test]
fn site_backends_complete_selected_tables_load_and_reload_without_new_code() {
    let fixture = Fixture::new();
    for name in ["hwmon", "gpiod"] {
        fixture.provider(name, "");
        let providers = SiteBackends::load_at(&config(name, true), &fixture.0).unwrap();
        fs::remove_file(fixture.0.join(format!("librptadv_{name}_adapter.so.1"))).unwrap();
        assert!(providers.validate(&config(name, true)).is_ok());
        assert!(providers.validate(&config(name, false)).is_ok());
        assert!(
            providers
                .validate(&config(
                    if name == "hwmon" { "gpiod" } else { "hwmon" },
                    true
                ))
                .is_err()
        );
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
                SiteBackends::load_at(&config(name, true), &fixture.0).is_err(),
                "{name}: {variant}"
            );
        }
    }
}
