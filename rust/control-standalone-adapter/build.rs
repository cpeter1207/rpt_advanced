fn main() {
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_control_standalone_adapter.so.1");
}
