//! Set the Debian multiarch directory for the executable's private adapters.

use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=DEB_HOST_MULTIARCH");
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
    println!("cargo:rustc-env=RPT_ADVANCED_LIBDIR=/usr/lib/{multiarch}/rpt_advanced");

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg("-I../product/include")
        .clang_arg("-I../control-abi/include")
        .clang_arg("-I../media-support/include")
        .clang_arg("-I../file-adapter/include")
        .clang_arg("-I../speech-adapter/include")
        .clang_arg("-I../control-standalone-adapter/include")
        .clang_arg("-I/usr/include")
        .allowlist_type("rptadv_.*")
        .allowlist_function("rptadv_product_descriptor_v1")
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate standalone product ABI bindings");
    bindings
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs"))
        .expect("write standalone product ABI bindings");
}
