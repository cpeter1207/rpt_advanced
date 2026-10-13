//! Generate the neutral descriptor and private SpeexDSP system bindings.
use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rustc-link-lib=speexdsp");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_jitter_adapter.so.1");
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=include/rptadv_jitter_adapter.h");
    bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_type("rptadv_jitter_.*")
        .allowlist_type("JitterBuffer.*")
        .allowlist_function("jitter_buffer_.*")
        .allowlist_var("JITTER_BUFFER_.*")
        .allowlist_var("RPTADV_JITTER_.*")
        .layout_tests(false)
        .generate_comments(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate jitter adapter ABI")
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs"))
        .expect("write jitter adapter ABI");
}
