//! Generate only the sensor capability contract and its external bindings.
use std::{env, path::PathBuf};

/// Only a borrowed C stream pointer crosses the bindings; its layout is private.
#[derive(Debug)]
struct StreamName;
impl bindgen::callbacks::ParseCallbacks for StreamName {
    fn item_name(&self, item: bindgen::callbacks::ItemInfo<'_>) -> Option<String> {
        (item.name == "FILE").then(|| "SensorFile".to_owned())
    }
}

fn main() {
    println!("cargo:rustc-link-lib=sensors");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_hwmon_adapter.so.1");
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=include/rptadv_hwmon_adapter.h");
    bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_type("rptadv_hwmon_.*")
        .allowlist_var("RPTADV_HWMON_.*")
        .allowlist_function(
            "sensors_(init|cleanup|get_detected_chips|get_features|get_all_subfeatures|get_value)",
        )
        .allowlist_function("fopen|fclose")
        .allowlist_var("SENSORS_MODE_R")
        .opaque_type("_IO_FILE")
        .layout_tests(false)
        .generate_comments(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .parse_callbacks(Box::new(StreamName))
        .generate()
        .expect("generate hwmon capability ABI")
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs"))
        .expect("write hwmon ABI");
}
