//! Copy resolved host requirements across the dynamically loaded product boundary.

use crate::{abi, native_configuration::NativeRadioConfiguration, providers::ProductInspection};
use std::{
    ffi::{c_char, c_void},
    fmt,
    mem::size_of,
};

/// One node's host requirements, resolved by the controller product.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedRadioNode {
    /// Configured node identity.
    pub node: String,
    /// Channel used when the controller reserves its radio.
    pub channel: String,
    /// Disabled nodes are reported but never opened.
    pub enabled: bool,
    /// Effective IAX listener port.
    pub iax_port: u16,
    /// HTTPS registration endpoint; empty disables registration.
    pub registration_url: String,
    /// Effective HTTPS registration refresh interval.
    pub registration_interval_seconds: u64,
    /// Owned native station request, without controller parser types.
    pub radio: NativeRadioConfiguration,
}

/// Safely printable product configuration diagnostic.
#[derive(Debug, PartialEq, Eq)]
pub struct ConfigError(pub(crate) String);

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for ConfigError {}

#[derive(Debug, Default)]
struct Inspection {
    nodes: Vec<ResolvedRadioNode>,
    diagnostics: Vec<String>,
    secrets: Vec<(String, String)>,
    invalid_record: bool,
}

/// Validate without opening device providers and return the product's warnings.
pub fn check_config(text: &str) -> Result<Vec<String>, ConfigError> {
    inspect(text, false, false).map(|result| result.diagnostics)
}

/// Resolve copied records without retaining any pointer into the product's parser.
pub fn resolve_radio_nodes(text: &str) -> Result<Vec<ResolvedRadioNode>, ConfigError> {
    inspect(text, true, false).map(|result| result.nodes)
}

pub(crate) fn secrets(text: &str) -> Result<Vec<(String, String)>, ConfigError> {
    inspect(text, false, true).map(|result| result.secrets)
}

fn inspect(text: &str, nodes: bool, secrets: bool) -> Result<Inspection, ConfigError> {
    let product = ProductInspection::open().map_err(|error| ConfigError(error.to_string()))?;
    let mut output = Inspection::default();
    let context = (&mut output as *mut Inspection).cast();
    // All callback arguments are borrowed only for this call; callbacks copy them.
    let result = unsafe {
        if secrets {
            product.api().inspect_secrets.unwrap()(
                text.as_ptr().cast(),
                text.len(),
                Some(copy_secret),
                context,
                Some(copy_diagnostic),
                context,
            )
        } else {
            product.api().inspect_configuration.unwrap()(
                text.as_ptr().cast(),
                text.len(),
                nodes.then_some(copy_node),
                context,
                Some(copy_diagnostic),
                context,
            )
        }
    };
    finish_inspection(output, result)
}

fn finish_inspection(mut output: Inspection, result: i32) -> Result<Inspection, ConfigError> {
    if result != 0 || output.invalid_record {
        return Err(ConfigError(output.diagnostics.pop().unwrap_or_else(|| {
            "product configuration inspection failed".into()
        })));
    }
    Ok(output)
}

unsafe fn text<'a>(pointer: *const c_char, length: usize) -> Result<&'a str, ()> {
    if length == 0 {
        return Ok("");
    }
    if pointer.is_null() || length > isize::MAX as usize {
        return Err(());
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer.cast(), length) })
        .map_err(|_| ())
}

unsafe extern "C" fn copy_node(
    context: *mut c_void,
    node: *const abi::rptadv_node_host_configuration,
) -> i32 {
    let output = unsafe { &mut *context.cast::<Inspection>() };
    let record = (|| {
        let node = unsafe { node.as_ref() }.ok_or(())?;
        if node.struct_size as usize != size_of::<abi::rptadv_node_host_configuration>() {
            return Err(());
        }
        Ok(ResolvedRadioNode {
            node: unsafe { text(node.node, node.node_length) }?.to_owned(),
            channel: unsafe { text(node.channel, node.channel_length) }?.to_owned(),
            enabled: node.enabled != 0,
            iax_port: node.iax_port,
            registration_url: unsafe { text(node.registration_url, node.registration_url_length) }?
                .to_owned(),
            registration_interval_seconds: node.registration_interval_seconds,
            radio: unsafe { NativeRadioConfiguration::copy_from(node.radio) }.map_err(|_| ())?,
        })
    })();
    match record {
        Ok(node) => {
            output.nodes.push(node);
            0
        }
        Err(()) => {
            output.invalid_record = true;
            -1
        }
    }
}

unsafe extern "C" fn copy_secret(
    context: *mut c_void,
    node: *const c_char,
    node_length: usize,
    secret: *const c_char,
    secret_length: usize,
) -> i32 {
    let output = unsafe { &mut *context.cast::<Inspection>() };
    match unsafe { (text(node, node_length), text(secret, secret_length)) } {
        (Ok(node), Ok(secret)) => {
            output.secrets.push((node.to_owned(), secret.to_owned()));
            0
        }
        _ => {
            output.invalid_record = true;
            -1
        }
    }
}

unsafe extern "C" fn copy_diagnostic(context: *mut c_void, pointer: *const c_char, length: usize) {
    let output = unsafe { &mut *context.cast::<Inspection>() };
    match unsafe { text(pointer, length) } {
        Ok(text) => output.diagnostics.push(text.to_owned()),
        Err(()) => output.invalid_record = true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_without_a_diagnostic_reports_a_safe_fallback() {
        assert_eq!(
            finish_inspection(Inspection::default(), -1).unwrap_err(),
            ConfigError("product configuration inspection failed".into())
        );
        assert_eq!(
            finish_inspection(
                Inspection {
                    invalid_record: true,
                    ..Inspection::default()
                },
                0
            )
            .unwrap_err(),
            ConfigError("product configuration inspection failed".into())
        );
    }

    #[test]
    fn hardware_free_inspection_uses_the_dynamic_product_and_copies_native_records() {
        let source = String::from(
            "[general]\niax_local_port=4570\n[radio]\ndevice_identifier=3-1\nreceive_input_gain_db=-2\nreceive_output_gain_db=-6\n[1000]\nradio_channel=vhf\n[2000]\nnode_enabled=no\n",
        );
        let nodes = resolve_radio_nodes(&source).unwrap();
        drop(source);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].node, "1000");
        assert_eq!(nodes[0].channel, "vhf");
        assert_eq!(nodes[0].iax_port, 4570);
        assert!(!nodes[1].enabled);
        let copied = nodes[0].clone();
        drop(nodes);
        let native = copied.radio.request();
        assert_eq!(
            unsafe {
                text(
                    native.device_identifier.cast(),
                    native.device_identifier_length as usize,
                )
            },
            Ok("3-1")
        );
        assert_eq!(native.receive_output_gain_db, -6);
        assert!((unsafe { (*native.radio).receive_input_gain } - 0.794_328_2).abs() < 0.000_001);
    }

    #[test]
    fn check_only_preserves_schema_warnings_and_errors() {
        assert!(
            check_config(include_str!("../../../examples/rpt_advanced.conf"))
                .unwrap()
                .is_empty()
        );
        let warnings = check_config("[1000]\nunknown_setting=value\n").unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("[1000] unknown_setting"));
        assert!(
            check_config("[node 1000]\n[permanent remote 2000]\n")
                .unwrap_err()
                .to_string()
                .contains("unknown node")
        );
    }

    #[test]
    fn inspection_callbacks_reject_malformed_borrowed_records() {
        let mut output = Inspection::default();
        let context = (&mut output as *mut Inspection).cast();
        assert_eq!(unsafe { copy_node(context, std::ptr::null()) }, -1);
        assert!(output.invalid_record);
        output.invalid_record = false;
        // SAFETY: the C record contains only scalar fields and nullable pointers.
        let node: abi::rptadv_node_host_configuration = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { copy_node(context, &node) }, -1);
        assert!(output.invalid_record);
        output.invalid_record = false;

        assert_eq!(unsafe { text(std::ptr::null(), 1) }, Err(()));
        assert_eq!(
            unsafe { text(c"x".as_ptr(), isize::MAX as usize + 1) },
            Err(())
        );
        assert_eq!(
            unsafe { copy_secret(context, std::ptr::null(), 1, c"x".as_ptr(), 1) },
            -1
        );
        assert!(output.invalid_record);
        output.invalid_record = false;
        unsafe { copy_diagnostic(context, std::ptr::null(), 1) };
        assert!(output.invalid_record);
    }
}
