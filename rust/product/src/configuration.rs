//! Host configuration inspection without constructing a running controller.

use crate::abi;
use rpt_advanced_core::config::{
    Cm119Profile, ConfigDocument, NodeId, RadioDeviceSelection, RadioReceiveAudioSource,
    ResolvedNodeSettings, ResolvedRadioSettings, Schema,
};
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
        if node.is_some() {
            // Validate every node before exposing any host request; inspection is transactional.
            for name in document.nodes() {
                let identity = NodeId::new(name.to_owned()).map_err(|error| error.to_string())?;
                let settings = ResolvedNodeSettings::resolve(&document, &identity)
                    .map_err(|error| error.to_string())?
                    .value;
                let radio =
                    crate::radio_configuration::radio_session_config(&settings.radio, 1, 4096, 50)
                        .map_err(|error| error.to_string())?;
                nodes.push((identity, settings, radio));
            }
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
            for (identity, settings, radio) in nodes {
                let native = native_configuration(&settings.radio, &radio)?;
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
                    radio: &native,
                };
                if unsafe { sink(node_context, &record) } != 0 {
                    return Err("configuration visitor rejected a node".into());
                }
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

fn native_configuration(
    settings: &ResolvedRadioSettings,
    radio: &abi::rptadv_radio_session_config,
) -> Result<abi::UrpNativeStationConfig, String> {
    let length = |value: &str| {
        u32::try_from(value.len()).map_err(|_| "radio configuration value is too long".to_owned())
    };
    let (outputs, initial) = settings.cm119_gpio_output_masks();
    Ok(abi::UrpNativeStationConfig {
        struct_size: size_of::<abi::UrpNativeStationConfig>() as u32,
        abi_version: 1,
        radio,
        device_selection: match settings.device_selection {
            RadioDeviceSelection::Exact => 0,
            RadioDeviceSelection::AutomaticLowestAlsaCard => 1,
        },
        device_identifier: settings.device_identifier.as_ptr(),
        device_identifier_length: length(&settings.device_identifier)?,
        usb_serial: settings.usb_serial.as_ptr(),
        usb_serial_length: length(&settings.usb_serial)?,
        input_device_channels: settings.input_device_channels,
        output_device_channels: settings.output_device_channels,
        input_extra_buffer_ms: settings.input_extra_buffer_ms,
        output_extra_buffer_ms: settings.output_extra_buffer_ms,
        cm119_profile: match settings.cm119_profile {
            Cm119Profile::DudeUsb => 0,
            Cm119Profile::SphUsb => 1,
            Cm119Profile::Nhrc => 2,
            Cm119Profile::Custom => 3,
        },
        ptt_inverted: u32::from(settings.cm119_ptt_inverted),
        gpio_output_enable_mask: u32::from(outputs),
        gpio_output_initial_mask: u32::from(initial),
        clip_led_mask: settings.cm119_clip_led_gpio.map_or(0, |pin| 1 << (pin - 1)),
        receive_graph: settings.receive_graph.as_ptr(),
        receive_graph_length: length(&settings.receive_graph)?,
        transmit_graph: settings.transmit_graph.as_ptr(),
        transmit_graph_length: length(&settings.transmit_graph)?,
        receive_deemphasis: u32::from(
            settings.signaling.receive_audio_source == RadioReceiveAudioSource::Flat,
        ),
        receive_output_gain_db: settings.receive_output_gain_db as i32,
    })
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
