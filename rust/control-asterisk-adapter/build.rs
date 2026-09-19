use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_control_asterisk_adapter.so.1");
    bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg("-DAST_MODULE_SELF_SYM=__internal_app_rpt_advanced_self")
        .clang_arg("-fblocks")
        .allowlist_function("ast_taskprocessor_(get|unreference|is_task|seq_num)")
        .allowlist_function("__ast_taskprocessor_push")
        .allowlist_var("TPS_REF_DEFAULT")
        .opaque_type("ast_taskprocessor")
        .layout_tests(false)
        .generate_comments(false)
        .prepend_enum_name(false)
        .generate()
        .expect("generate installed Asterisk taskprocessor bindings")
        .write_to_file(
            PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
                .join("asterisk.rs"),
        )
        .expect("write taskprocessor bindings");
}
