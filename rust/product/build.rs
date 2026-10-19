use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=DEB_HOST_MULTIARCH");
    println!("cargo:rerun-if-env-changed=RPT_ADVANCED_LIBDIR");
    let multiarch = env::var("DEB_HOST_MULTIARCH").unwrap_or_else(|_| {
        match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("x86_64") => "x86_64-linux-gnu".to_owned(),
            Ok("aarch64") => "aarch64-linux-gnu".to_owned(),
            other => panic!("unsupported Debian multiarch: {other:?}"),
        }
    });
    let directory = env::var("RPT_ADVANCED_LIBDIR")
        .unwrap_or_else(|_| format!("/usr/lib/{multiarch}/rpt_advanced"));
    println!("cargo:rustc-env=RPT_ADVANCED_LIBDIR={directory}");
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=include/rptadv_product.h");
    println!("cargo:rerun-if-changed=../control-abi/include/rptadv_control_adapter.h");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_product.so.1");
    println!("cargo:rerun-if-env-changed=URP_PRODUCT_INCLUDE");
    let radio_include =
        env::var("URP_PRODUCT_INCLUDE").unwrap_or_else(|_| "/usr/include".to_owned());
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg("-I../media-support/include")
        .clang_arg("-I../control-abi/include")
        .clang_arg(format!("-I{radio_include}"))
        .allowlist_type("rptadv_.*")
        .allowlist_type("UrpNativeStationConfig")
        .allowlist_function("rpcr3_descriptor")
        .allowlist_function("rptadv_samplerate_adapter_descriptor")
        .allowlist_var("RPCR3_.*")
        .allowlist_var("RPTADV_MEDIA_.*")
        .allowlist_var("RPTADV_PEER_EVENT_.*")
        .allowlist_var("RPTADV_PEER_OBSERVATION_.*")
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate product ABI bindings");
    bindings
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs"))
        .expect("write product ABI bindings");
    println!("cargo:rustc-link-lib=rate_adjusting_pcm_ring3");
    println!("cargo:rustc-link-lib=rptadv_samplerate_adapter");
}
