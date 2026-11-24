//! Generate only the public, vendor-neutral ABI.
use std::{env, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=RPTADV_SOFIA_PREFIX");
    println!("cargo:rerun-if-changed=native.c");
    println!("cargo:rerun-if-changed=native.h");
    let prefix = env::var("RPTADV_SOFIA_PREFIX").unwrap_or_else(|_| "/usr".into());
    cc::Build::new()
        .file("native.c")
        .flag("-isystem")
        .flag(format!("{prefix}/include/sofia-sip-1.13"))
        .flag("-std=c11")
        .flag("-Wall")
        .flag("-Wextra")
        .warnings_into_errors(true)
        .compile("sip_native");
    println!("cargo:rustc-link-search=native={prefix}/lib");
    println!("cargo:rustc-link-lib=dylib=sofia-sip-ua");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-z,defs");
    println!("cargo:rustc-cdylib-link-arg=-Wl,--no-allow-shlib-undefined");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_sip_adapter.so.1");
    println!("cargo:rerun-if-changed=include/rptadv_sip_adapter.h");
    bindgen::Builder::default()
        .header("include/rptadv_sip_adapter.h")
        .allowlist_type("rptadv_sip_.*")
        .allowlist_var("RPTADV_SIP_.*")
        .layout_tests(false)
        .generate_comments(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate neutral SIP ABI")
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs"))
        .expect("write SIP ABI");
}
