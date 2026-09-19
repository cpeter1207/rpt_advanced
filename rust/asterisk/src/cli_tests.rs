use super::*;

thread_local! { static OUTPUT: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) }; }
// Production uses this variadic API with exactly one string argument.
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_cli(_: c_int, format: *const c_char, text: *const c_char) -> c_int {
    // SAFETY: output supplies static format and a live NUL-terminated string.
    unsafe {
        assert_eq!(CStr::from_ptr(format), c"%s");
        OUTPUT.with(|messages| {
            messages
                .borrow_mut()
                .push(CStr::from_ptr(text).to_str().unwrap().into())
        });
    }
    0
}

#[test]
fn callbacks_initialize_and_fail_closed_when_runtime_is_unloaded() {
    let _serial = crate::fixture::LIFECYCLE.lock().unwrap();
    OUTPUT.with(|messages| messages.borrow_mut().clear());
    output(1, "ignored\0message");
    // SAFETY: callbacks borrow only the valid local entry/argument storage below.
    unsafe {
        for callback in [link_callback, alias_callback, command_callback] {
            let mut entry: ffi::ast_cli_entry = std::mem::zeroed();
            assert!(callback(ptr::null_mut(), ffi::CLI_INIT, ptr::null_mut()).is_null());
            assert!(callback(&mut entry, ffi::CLI_INIT, ptr::null_mut()).is_null());
            assert!(!entry.command.is_null());
            assert!(!entry.usage.is_null());
            assert!(callback(&mut entry, ffi::CLI_GENERATE, ptr::null_mut()).is_null());
            assert_eq!(
                callback(&mut entry, 0, ptr::null_mut()),
                ffi::RESULT_SHOWUSAGE as *mut c_char
            );
        }
        let mut words = [
            c"rpt".as_ptr(),
            c"link".as_ptr(),
            c"status".as_ptr(),
            c"1000".as_ptr(),
        ];
        let mut args: ffi::ast_cli_args = std::mem::zeroed();
        args.argc = 4;
        args.argv = words.as_mut_ptr().cast();
        assert_eq!(
            link_callback(ptr::null_mut(), 0, &mut args),
            ffi::RESULT_FAILURE as *mut c_char
        );
        words[2] = c"invalid".as_ptr();
        assert_eq!(
            link_callback(ptr::null_mut(), 0, &mut args),
            ffi::RESULT_SHOWUSAGE as *mut c_char
        );
        words[2] = c"1000".as_ptr();
        words[3] = c"*99".as_ptr();
        assert_eq!(
            command_callback(ptr::null_mut(), 0, &mut args),
            ffi::RESULT_FAILURE as *mut c_char
        );
        for (count, local, digits) in [(3, c"1000", c"*99"), (4, c"", c"*99"), (4, c"1000", c"")] {
            args.argc = count;
            words[2] = local.as_ptr();
            words[3] = digits.as_ptr();
            assert_eq!(
                command_callback(ptr::null_mut(), 0, &mut args),
                ffi::RESULT_SHOWUSAGE as *mut c_char
            );
        }
    }
    OUTPUT.with(|messages| assert_eq!(messages.borrow().len(), 2));
}

#[test]
fn live_command_callback_reports_success_invalid_incomplete_and_operation_failure() {
    crate::lifecycle::tests::with_running(|| {
        // SAFETY: each callback borrows only the live argument vector and static strings.
        unsafe {
            let mut args: ffi::ast_cli_args = std::mem::zeroed();
            for (local, digits, success) in [
                (c"1000", c"*806", true),
                (c"1000", c"*99", false),
                (c"1000", c"invalid", false),
                (c"missing", c"*806", false),
                (c"1000", c"*32000", false),
            ] {
                let mut words = [
                    c"rpt_advanced".as_ptr(),
                    c"command".as_ptr(),
                    local.as_ptr(),
                    digits.as_ptr(),
                ];
                args.argc = words.len() as i32;
                args.argv = words.as_mut_ptr().cast();
                assert_eq!(
                    command_callback(ptr::null_mut(), 0, &mut args).is_null(),
                    success,
                    "{local:?} {digits:?}"
                );
            }
        }
    });
}

#[test]
fn public_cli_arguments_reject_invalid_counts_pointers_and_utf8() {
    // SAFETY: all non-null argument pointers below reference the owned local arrays.
    unsafe {
        assert!(arguments(ptr::null_mut()).is_none());
        let mut args: ffi::ast_cli_args = std::mem::zeroed();
        for count in [-1, 33, 0] {
            args.argc = count;
            assert!(arguments(&mut args).is_none());
        }
        let mut words = [ptr::null(), c"\xff".as_ptr(), c"rpt".as_ptr()];
        args.argc = 1;
        for index in 0..3 {
            args.argv = words.as_mut_ptr().add(index).cast();
            let result = arguments(&mut args);
            if index < 2 {
                assert!(result.is_none());
            } else {
                assert_eq!(result.unwrap().1, vec!["rpt"]);
            }
        }
    }
}

#[test]
fn status_sink_validates_provider_bytes_and_link_command_reports_result() {
    crate::lifecycle::tests::with_running(|| unsafe {
        for (local, success) in [
            (c"null-context", true),
            (c"null-text", false),
            (c"empty", true),
            (c"invalid", false),
            (c"missing", false),
        ] {
            let mut words = [
                c"rpt".as_ptr(),
                c"link".as_ptr(),
                c"status".as_ptr(),
                local.as_ptr(),
            ];
            let mut args: ffi::ast_cli_args = std::mem::zeroed();
            args.argc = 4;
            args.argv = words.as_mut_ptr().cast();
            assert_eq!(
                link_callback(ptr::null_mut(), 0, &mut args).is_null(),
                success
            );
        }
        for (local, success) in [(c"1000", true), (c"missing", false)] {
            let mut words = [
                c"rpt".as_ptr(),
                c"link".as_ptr(),
                c"connect".as_ptr(),
                local.as_ptr(),
                c"2000".as_ptr(),
            ];
            let mut args: ffi::ast_cli_args = std::mem::zeroed();
            args.argc = 5;
            args.argv = words.as_mut_ptr().cast();
            assert_eq!(
                link_callback(ptr::null_mut(), 0, &mut args).is_null(),
                success
            );
        }
    });
}

#[test]
fn command_validates_the_entire_input_before_touching_the_collector() {
    let mut seen = String::new();
    assert_eq!(
        execute_digits("*3123a", |digit| {
            seen.push(digit);
            Ok(false)
        }),
        Err(DigitError::Invalid('a'))
    );
    assert!(seen.is_empty());
    assert_eq!(
        execute_digits("*3123", |digit| {
            seen.push(digit);
            Ok(digit == '#')
        }),
        Ok(())
    );
    assert_eq!(seen, "*3123#");
    seen.clear();
    assert_eq!(
        execute_digits("*3123#", |digit| {
            seen.push(digit);
            Ok(digit == '#')
        }),
        Ok(())
    );
    assert_eq!(seen, "*3123#");
    assert_eq!(
        execute_digits("", |_| panic!("empty input")),
        Err(DigitError::Usage)
    );
    assert_eq!(
        execute_digits("*99", |_| Ok(false)),
        Err(DigitError::Incomplete)
    );
    assert_eq!(execute_digits("1", |_| Err(())), Err(DigitError::Operation));
}

#[test]
fn link_grammar_retains_alias_and_exact_arity() {
    for prefix in ["rpt", "rpt_advanced"] {
        assert_eq!(
            parse_link(&[prefix, "link", "status", "1000"]),
            Some(LinkRequest::Status("1000"))
        );
        for (verb, action) in [
            ("connect", LinkAction::Transceive),
            ("monitor", LinkAction::Monitor),
            ("local-monitor", LinkAction::LocalMonitor),
            ("disconnect", LinkAction::Disconnect),
        ] {
            assert_eq!(
                parse_link(&[prefix, "link", verb, "1000", "2000"]),
                Some(LinkRequest::Command {
                    local: "1000",
                    remote: "2000",
                    action
                })
            );
        }
    }
    assert_eq!(
        parse_link(&["rpt_advanced", "link", "status", "1000", "2000"]),
        None
    );
    assert_eq!(
        parse_link(&["rpt_advanced", "link", "CONNECT", "1000", "2000"]),
        None
    );
    assert_eq!(
        parse_link(&["rpt_advanced", "link", "connect", "1000"]),
        None
    );
}
