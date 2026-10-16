//! Select released functional libraries and reject incompatible installed ABIs.

use std::process::Command;

fn pkg_config(args: &[&str]) -> String {
    let output = Command::new("pkg-config")
        .args(args)
        .output()
        .expect("pkg-config is required for released providers");
    assert!(
        output.status.success(),
        "released provider development package is required: pkg-config {args:?}"
    );
    String::from_utf8(output.stdout)
        .expect("pkg-config UTF-8")
        .trim()
        .to_owned()
}

fn main() {
    pkg_config(&["--atleast-version=0.1.0~alpha2", "rptadvdtmf"]);
    for provider in [
        "rptadvtone",
        "rptadvmorse",
        "rptadvdtmf",
        "rptadvtemplate",
        "rptadvschedule",
    ] {
        assert_eq!(
            pkg_config(&["--variable=abi_version", provider]),
            "1",
            "{provider} ABI major"
        );
        let libdir = pkg_config(&["--variable=libdir", provider]);
        assert!(
            !libdir.is_empty(),
            "{provider} library directory is missing"
        );
        println!("cargo:rustc-link-search=native={libdir}");
        println!("cargo:rustc-link-lib=dylib={provider}");
    }
}
