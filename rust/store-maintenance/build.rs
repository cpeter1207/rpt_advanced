//! Select the same private product-library directory as the existing hosts.
use std::env;
fn main() {
    println!("cargo:rerun-if-env-changed=RPT_ADVANCED_LIBDIR");
    println!("cargo:rerun-if-env-changed=DEB_HOST_MULTIARCH");
    let multiarch = env::var("DEB_HOST_MULTIARCH").unwrap_or_else(|_| {
        match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("x86_64") => "x86_64-linux-gnu".to_owned(),
            Ok("aarch64") => "aarch64-linux-gnu".to_owned(),
            _ => panic!("unsupported Debian architecture"),
        }
    });
    let directory = env::var("RPT_ADVANCED_LIBDIR")
        .unwrap_or_else(|_| format!("/usr/lib/{multiarch}/rpt_advanced"));
    println!("cargo:rustc-env=RPT_ADVANCED_LIBDIR={directory}");
}
