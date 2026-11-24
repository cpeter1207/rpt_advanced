use std::{env, path::PathBuf};
fn main() {
    let path = |var, default: &str| {
        println!("cargo:rerun-if-env-changed={var}");
        env::var(var).unwrap_or_else(|_| default.into())
    };
    let bindings = bindgen::Builder::default()
        .header("../product/include/rptadv_product.h")
        .header(format!("{}/rptadv_iax2_client.h",path("RPTADV_IAX2_INCLUDE","/usr/include/rptadv_iax2")))
        .allowlist_type("rptadv_iax2_.*").allowlist_var("RPTADV_IAX2_SERVICE_ONLY")
        .header("../jitter-adapter/include/rptadv_jitter_adapter.h")
        .clang_arg(format!("-I{}", path("RPTADV_CODEC_INCLUDE", "/usr/include/rptadv-codec")))
        .clang_arg(format!("-I{}", path("URP_PRODUCT_INCLUDE", "/usr/include")))
        .clang_arg("-I../media-support/include")
        .allowlist_type("rptadv_(incoming_peer_v1|peer_info_v1|phone_.*|media_clock_reader_v1|jitter_descriptor_v1|peer_(source_.*|bytes_v1)|source_birth_v1|codec_encode_result_v1)")
        .layout_tests(false).generate_comments(false).parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate().expect("generate the native private call records");
    bindings.write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("abi.rs")).unwrap();
}
