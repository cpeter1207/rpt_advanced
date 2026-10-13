//! Generate resolver-only system bindings and the public descriptor layout.
use std::{env, path::PathBuf};
fn main() {
    println!("cargo:rustc-link-lib=resolv");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_resolver_adapter.so.1");
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=include/rptadv_resolver_adapter.h");
    bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_type("rptadv_resolver_.*")
        .allowlist_function("res_query")
        .allowlist_function("ns_initparse")
        .allowlist_function("ns_parserr")
        .allowlist_function("dn_expand")
        .layout_tests(false)
        .generate_comments(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate system resolver ABI")
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs"))
        .expect("write resolver ABI");
}
