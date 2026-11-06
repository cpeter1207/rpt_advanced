//! Host configuration inspection without constructing a running controller.
//!
//! The existing USBRadioPlus shared product owns radio defaults, parsing and
//! construction. All SDK resolutions succeed before any host record is visited;
//! borrowed native spans remain valid only during that synchronous callback.

use crate::abi;
use rpt_advanced_core::config::{
    ConfigDocument, EchoLinkSettings, NodeId, ResolvedNodeSettings, Schema,
};
#[path = "configuration_echolink.rs"]
mod echolink;
#[path = "configuration_radio.rs"]
pub(crate) mod radio;
use std::{
    ffi::{c_char, c_void},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
};

pub(crate) unsafe extern "C" fn inspect_configuration(
    configuration: *const c_char,
    length: usize,
    node: abi::rptadv_configuration_sink,
    node_context: *mut c_void,
    diagnostic: abi::rptadv_text_sink_v1,
    diagnostic_context: *mut c_void,
) -> i32 {
    boundary(diagnostic, diagnostic_context, || {
        let source = unsafe { input(configuration, length) }?;
        let document = ConfigDocument::parse(source).map_err(|error| error.to_string())?;
        let resolution = Schema::validate(&document).map_err(|error| error.to_string())?;
        let mut nodes = Vec::new();
        let mut radio_diagnostic = RadioDiagnostic {
            sink: diagnostic,
            context: diagnostic_context,
        };
        // Validate every radio even without a node visitor; config checks use this path.
        for name in document.nodes() {
            let identity = NodeId::new(name.to_owned()).map_err(|error| error.to_string())?;
            let settings = ResolvedNodeSettings::resolve(&document, &identity)
                .map_err(|error| error.to_string())?
                .value;
            let radio = radio::ResolvedRadio::resolve(
                source,
                identity.as_str(),
                Some(radio_diagnostic_sink),
                std::ptr::from_mut(&mut radio_diagnostic).cast(),
            )?;
            let account = EchoLinkSettings::resolve(&document, &identity)
                .map_err(|error| error.to_string())?
                .value;
            nodes.push((identity, settings, radio, account));
        }
        for warning in resolution.warnings {
            emit(
                diagnostic,
                diagnostic_context,
                &format!(
                    "line {} [{}] {}: {}",
                    warning.line, warning.section, warning.key, warning.message
                ),
            );
        }
        if let Some(sink) = node {
            for (identity, settings, radio, account) in nodes {
                let secrets_file = account
                    .secrets_file
                    .as_ref()
                    .map_or(std::borrow::Cow::Borrowed(""), |path| {
                        path.to_string_lossy()
                    });
                radio.visit(|native| {
                    let accepted = echolink::visit(identity.as_str(), &account, |account| {
                        let record = abi::rptadv_node_host_configuration {
                            struct_size: size_of::<abi::rptadv_node_host_configuration>() as u32,
                            node: identity.as_str().as_ptr().cast(),
                            node_length: identity.as_str().len(),
                            channel: settings.channel.as_ptr().cast(),
                            channel_length: settings.channel.len(),
                            enabled: u32::from(settings.enabled),
                            iax_port: settings.iax_local_port,
                            registration_url: settings.iax_registration_url.as_ptr().cast(),
                            registration_url_length: settings.iax_registration_url.len(),
                            registration_interval_seconds: settings.iax_registration_interval_s,
                            radio: native,
                            echolink: std::ptr::from_ref(account).cast(),
                            echolink_secrets_file: secrets_file.as_ptr(),
                            echolink_secrets_file_length: secrets_file.len(),
                        };
                        unsafe { sink(node_context, &record) }
                    });
                    if accepted == 0 {
                        Ok(())
                    } else {
                        Err("configuration visitor rejected a node".into())
                    }
                })?;
            }
        }
        Ok(())
    })
}

pub(crate) unsafe extern "C" fn inspect_secrets(
    configuration: *const c_char,
    length: usize,
    secret: abi::rptadv_secret_sink,
    secret_context: *mut c_void,
    diagnostic: abi::rptadv_text_sink_v1,
    diagnostic_context: *mut c_void,
) -> i32 {
    boundary(diagnostic, diagnostic_context, || {
        // Parser errors may contain input text. Never send those across the secrets boundary.
        let source = unsafe { input(configuration, length) }.map_err(|_| "invalid syntax")?;
        let document = ConfigDocument::parse(source).map_err(|_| "invalid syntax")?;
        for entry in document.entries() {
            if entry.key != "iax_secret" {
                return Err("unknown option".into());
            }
            if entry.value.is_empty() {
                return Err("empty secret".into());
            }
            if entry.section != "general"
                && (entry.section.len() > 63
                    || !entry.section.bytes().all(|byte| byte.is_ascii_digit()))
            {
                return Err("invalid section".into());
            }
        }
        if let Some(sink) = secret {
            for entry in document.entries() {
                if unsafe {
                    sink(
                        secret_context,
                        entry.section.as_ptr().cast(),
                        entry.section.len(),
                        entry.value.as_ptr().cast(),
                        entry.value.len(),
                    )
                } != 0
                {
                    return Err("secret visitor rejected a record".into());
                }
            }
        }
        Ok(())
    })
}

struct RadioDiagnostic {
    sink: abi::rptadv_text_sink_v1,
    context: *mut c_void,
}

unsafe extern "C" fn radio_diagnostic_sink(context: *mut c_void, text: *const u8, length: u32) {
    // SAFETY: the shared resolution retains this stack diagnostic adapter until return.
    let context = unsafe { &*context.cast::<RadioDiagnostic>() };
    // SAFETY: the shared SDK borrows exactly this diagnostic text for the callback.
    let text =
        unsafe { input(text.cast(), length as usize) }.unwrap_or("invalid shared radio diagnostic");
    emit(context.sink, context.context, text);
}

unsafe fn input<'a>(pointer: *const c_char, length: usize) -> Result<&'a str, String> {
    if length == 0 {
        return Ok("");
    }
    if pointer.is_null() || length > isize::MAX as usize {
        return Err("invalid configuration input".into());
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer.cast(), length) })
        .map_err(|_| "configuration is not UTF-8".into())
}

fn emit(sink: abi::rptadv_text_sink_v1, context: *mut c_void, text: &str) {
    if let Some(sink) = sink {
        unsafe { sink(context, text.as_ptr().cast(), text.len()) };
    }
}

fn boundary(
    sink: abi::rptadv_text_sink_v1,
    context: *mut c_void,
    operation: impl FnOnce() -> Result<(), String>,
) -> i32 {
    match catch_unwind(AssertUnwindSafe(operation))
        .unwrap_or_else(|_| Err("configuration inspection failed".into()))
    {
        Ok(()) => 0,
        Err(error) => {
            emit(sink, context, &error);
            -1
        }
    }
}

#[cfg(test)]
#[path = "configuration_tests.rs"]
mod tests;
