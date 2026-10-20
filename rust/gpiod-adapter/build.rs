//! Generate the public site-GPIO contract and isolated libgpiod bindings.
use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rustc-link-lib=gpiod");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_gpiod_adapter.so.1");
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=include/rptadv_gpiod_adapter.h");
    bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_type("rptadv_gpiod_.*")
        .allowlist_var("RPTADV_GPIOD_.*")
        .allowlist_function("gpiod_(chip_open|chip_close|chip_request_lines)")
        .allowlist_function(
            "gpiod_line_settings_(new|free|set_direction|set_active_low|set_output_value)",
        )
        .allowlist_function("gpiod_line_config_(new|free|add_line_settings)")
        .allowlist_function("gpiod_line_request_(release|get_value|set_value)")
        .layout_tests(false)
        .generate_comments(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate GPIO capability ABI")
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs"))
        .expect("write GPIO ABI");
}
