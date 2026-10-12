//! Select the released tone library and reject an incompatible installed ABI.

use std::process::Command;

fn pkg_config(args: &[&str]) -> String {
    let output = Command::new("pkg-config")
        .args(args)
        .output()
        .expect("pkg-config is required for librptadvtone-dev");
    assert!(
        output.status.success(),
        "released librptadvtone-dev is required: pkg-config {args:?}"
    );
    String::from_utf8(output.stdout)
        .expect("pkg-config UTF-8")
        .trim()
        .to_owned()
}

fn main() {
    assert_eq!(
        pkg_config(&["--variable=abi_version", "rptadvtone"]),
        "1",
        "tone ABI major"
    );
    let libdir = pkg_config(&["--variable=libdir", "rptadvtone"]);
    assert!(!libdir.is_empty(), "tone library directory is missing");
    println!("cargo:rustc-link-search=native={libdir}");
    println!("cargo:rustc-link-lib=dylib=rptadvtone");
}
