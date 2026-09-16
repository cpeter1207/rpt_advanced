use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,librptadv_asterisk_adapter.so.1");
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg("-I../media-support/include")
        .clang_arg("-DAST_MODULE_SELF_SYM=__internal_app_rpt_advanced_self")
        .clang_arg("-fblocks")
        .allowlist_function("ast_(get_channel_tech|format_cap_get_format|format_cache_get_slin_by_rate|format_get_sample_rate|format_cmp|request|hangup|set_read_format|set_write_format|read|write|indicate|frame_free|codec_get_max|codec_get_by_id|format_cache_get_by_codec|translate_path_steps|translator_build_path|translator_free_path|translate)")
        .allowlist_function("__ast_format_cap_(alloc|append)")
        .allowlist_function("__ao2_ref")
        .allowlist_function("ast_(config_load2|variable_retrieve|config_destroy|srv_lookup|srv_cleanup|sockaddr_resolve|sockaddr_stringify_fmt|sendtext|senddigit|waitfor)")
        .allowlist_function("ast_(replace_sigchld|unreplace_sigchld)")
        .allowlist_type("rptadv_.*")
        .allowlist_function("ast_(request_and_dial|channel_state)")
        .allowlist_function("ast_(channel_nativeformats|format_cap_count)")
        .allowlist_function("ast_(register_application2|unregister_application|cli_unregister_multiple|cli|call|channel_tech|channel_caller|channel_uniqueid|channel_move|answer|func_read)")
        .allowlist_function("(__ast_cli_register_multiple|__ast_channel_alloc|__ao2_unlock)")
        .allowlist_function("localtime_r")
        .allowlist_function("ast_log")
        .allowlist_var("__LOG_NOTICE")
        .allowlist_type("ast_(cli_entry|cli_args|cli_command|module)")
        .allowlist_var("(RESULT_.*|AST_MODULE_LOAD_.*|AST_AMA_NONE|ast_config_AST_CONFIG_DIR)")
        .allowlist_type("ast_channel_state")
        .allowlist_var("(PARSE_PORT_FORBID|AST_AF_UNSPEC|AST_SOCKADDR_STR_ADDR|AST_FRAME_(TEXT|DTMF_END))")
        .allowlist_type("ast_control_frame_type")
        .allowlist_type("ast_parse_flags")
        .allowlist_var("AST_(FRAME_(VOICE|CONTROL|NULL|CNG)|CONTROL_(RADIO_KEY|RADIO_UNKEY)|FORMAT_CMP_EQUAL|MEDIA_TYPE_AUDIO|FORMAT_CAP_FLAG_DEFAULT)")
        .opaque_type("ast_(channel|format|format_cap|assigned_ids|trans_pvt)")
        .layout_tests(false)
        .generate_comments(false)
        .prepend_enum_name(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generate bindings from installed public Asterisk headers");
    bindings
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("asterisk.rs"))
        .expect("write Asterisk bindings");
}
