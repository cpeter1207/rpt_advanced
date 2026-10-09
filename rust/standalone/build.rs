//! Set the Debian multiarch directory for the executable's private adapters.

use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rustc-link-lib=resolv");
    println!("cargo:rerun-if-env-changed=DEB_HOST_MULTIARCH");
    println!("cargo:rerun-if-env-changed=RPT_ADVANCED_LIBDIR");
    println!("cargo:rerun-if-env-changed=URP_PRODUCT_INCLUDE");
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=../product/include/rptadv_product.h");
    println!("cargo:rerun-if-changed=../control-abi/include/rptadv_control_adapter.h");
    println!("cargo:rerun-if-changed=../media-support/include/rptadv_media_types.h");
    println!("cargo:rerun-if-changed=../file-adapter/include/rptadv_file_adapter.h");
    println!("cargo:rerun-if-changed=../speech-adapter/include/rptadv_speech_adapter.h");
    println!(
        "cargo:rerun-if-changed=../control-standalone-adapter/include/rptadv_control_standalone_adapter.h"
    );
    println!("cargo:rerun-if-changed=/usr/include/rptadvradio/rptadvradio.h");
    println!(
        "cargo:rerun-if-changed=/usr/include/rptadv_portaudio_alsa_adapter/rptadv_portaudio_alsa_adapter.h"
    );
    println!("cargo:rerun-if-changed=/usr/include/rptadv_gpio_adapter/rptadv_gpio_adapter.h");
    println!("cargo:rerun-if-changed=/usr/include/rptadv_ffmpeg_adapter/rptadv_ffmpeg_adapter.h");
    let multiarch = env::var("DEB_HOST_MULTIARCH").unwrap_or_else(|_| {
        match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("x86_64") => "x86_64-linux-gnu".to_owned(),
            Ok("aarch64") => "aarch64-linux-gnu".to_owned(),
            Ok(architecture) => panic!("unsupported Debian multiarch: {architecture}"),
            Err(error) => panic!("Cargo did not provide the target architecture: {error}"),
        }
    });
    let library_directory = env::var("RPT_ADVANCED_LIBDIR")
        .unwrap_or_else(|_| format!("/usr/lib/{multiarch}/rpt_advanced"));
    println!("cargo:rustc-env=RPT_ADVANCED_LIBDIR={library_directory}");

    let bindings = bindgen::Builder::default()
        .derive_partialeq(true)
        .header("wrapper.h")
        .clang_arg("-I../product/include")
        .clang_arg("-I../control-abi/include")
        .clang_arg("-I../media-support/include")
        .clang_arg("-I../file-adapter/include")
        .clang_arg("-I../speech-adapter/include")
        .clang_arg("-I../control-standalone-adapter/include")
        .clang_arg("-I/usr/include")
        .clang_arg(format!(
            "-I{}",
            env::var("URP_PRODUCT_INCLUDE").unwrap_or_else(|_| "/usr/include".to_owned())
        ))
        .allowlist_type("rptadv_.*")
        .allowlist_type("Urp.*")
        .allowlist_function("rptadv_product_descriptor_v1")
        .allowlist_function("res_query")
        .allowlist_function("ns_initparse")
        .allowlist_function("ns_parserr")
        .allowlist_function("dn_expand")
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate standalone product ABI bindings");
    bindings
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs"))
        .expect("write standalone product ABI bindings");
}
