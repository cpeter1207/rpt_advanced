use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=include/rptadv_product.h");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_product.so.1");
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg("-I../media-support/include")
        .allowlist_type("rptadv_.*")
        .allowlist_function("rpcr2_descriptor")
        .allowlist_function("rptadv_samplerate_adapter_descriptor")
        .allowlist_var("RPCR2_.*")
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate product ABI bindings");
    bindings
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs"))
        .expect("write product ABI bindings");
    println!("cargo:rustc-link-lib=rate_adjusting_pcm_ring2");
    println!("cargo:rustc-link-lib=rptadv_samplerate_adapter");
}
