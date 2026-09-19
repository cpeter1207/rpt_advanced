fn main() {
    let package = std::env::var("CARGO_PKG_NAME").unwrap();
    let capability = if package == "rptadv-file-adapter" {
        "file"
    } else {
        "speech"
    };
    println!("cargo:rustc-check-cfg=cfg(file_adapter)");
    println!("cargo:rustc-check-cfg=cfg(speech_adapter)");
    println!("cargo:rustc-cfg={capability}_adapter");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_{capability}_adapter.so.1");
}
