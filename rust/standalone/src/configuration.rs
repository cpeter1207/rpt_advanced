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

/// Native provider protocol, independent of radio-peer identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhoneAccountProtocol {
    /// General IAX2 provider account.
    Iax,
    /// SIP provider account.
    Sip,
}

/// Owned public account settings; copying does not activate a provider.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedPhoneAccount {
    /// Local account label, distinct from provider username and mailbox.
    pub id: String,
    /// Explicit local node owning this account.
    pub local_node: String,
    /// Native signaling family.
    pub protocol: PhoneAccountProtocol,
    /// Disabled accounts are reported but must not open transports.
    pub enabled: bool,
    /// Provider host/IP and numeric port.
    pub server: String,
    /// Provider authentication username.
    pub username: String,
    /// Absolute or configuration-relative host path; no secrets are loaded.
    pub secrets_file: String,
    /// Whether to request provider registration.
    pub register: bool,
    /// Registration refresh interval in seconds.
    pub registration_interval_s: u16,
    /// Absolute authentication timeout in milliseconds.
    pub authentication_timeout_ms: u32,
}

impl fmt::Debug for ResolvedPhoneAccount {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("ResolvedPhoneAccount")
            .field("id", &self.id)
            .field("local_node", &self.local_node)
            .field("protocol", &self.protocol)
            .field("enabled", &self.enabled)
            .field("register", &self.register)
            .field("registration_interval_s", &self.registration_interval_s)
            .field("authentication_timeout_ms", &self.authentication_timeout_ms)
            .finish_non_exhaustive()
    }
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
    accounts: Vec<ResolvedPhoneAccount>,
    diagnostics: Vec<String>,
    secrets: Vec<(String, String)>,
    invalid_record: bool,
}

/// Validate without opening device providers and return the product's warnings.
pub fn check_config(text: &str) -> Result<Vec<String>, ConfigError> {
    inspect(text, true, false).map(|result| result.diagnostics)
}

/// Resolve copied records without retaining any pointer into the product's parser.
pub fn resolve_radio_nodes(text: &str) -> Result<Vec<ResolvedRadioNode>, ConfigError> {
    inspect(text, true, false).map(|result| result.nodes)
}

/// Copy public provider accounts through the existing product visitor, without activation.
pub fn resolve_phone_accounts(text: &str) -> Result<Vec<ResolvedPhoneAccount>, ConfigError> {
    inspect(text, true, false).map(|result| result.accounts)
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
        if node.is_null()
            || unsafe { node.cast::<u32>().read_unaligned() } as usize
                != size_of::<abi::rptadv_node_host_configuration>()
        {
            return Err(());
        }
        // SAFETY: the callback guarantees the advertised storage; the size was checked
        // before reading beyond the prefix, including for older product records.
        let node = unsafe { node.read_unaligned() };
        let local = unsafe { text(node.node, node.node_length) }?.to_owned();
        let accounts =
            unsafe { copy_phone_accounts(&local, node.phone_accounts, node.phone_account_count) }?;
        let radio = ResolvedRadioNode {
            node: local,
            channel: unsafe { text(node.channel, node.channel_length) }?.to_owned(),
            enabled: node.enabled != 0,
            iax_port: node.iax_port,
            registration_url: unsafe { text(node.registration_url, node.registration_url_length) }?
                .to_owned(),
            registration_interval_seconds: node.registration_interval_seconds,
            radio: unsafe { NativeRadioConfiguration::copy_from(node.radio) }.map_err(|_| ())?,
        };
        Ok((radio, accounts))
    })();
    match record {
        Ok((node, accounts)) => {
            output.nodes.push(node);
            output.accounts.extend(accounts);
            0
        }
        Err(()) => {
            output.invalid_record = true;
            -1
        }
    }
}

/// Copy only complete public records; the caller supplies their advertised storage.
unsafe fn copy_phone_accounts(
    local: &str,
    records: *const abi::rptadv_phone_account_configuration_v1,
    count: usize,
) -> Result<Vec<ResolvedPhoneAccount>, ()> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if records.is_null()
        || count > isize::MAX as usize / size_of::<abi::rptadv_phone_account_configuration_v1>()
    {
        return Err(());
    }
    let mut accounts = Vec::new();
    for index in 0..count {
        let pointer = unsafe { records.add(index) };
        if unsafe { pointer.cast::<u32>().read_unaligned() } as usize
            != size_of::<abi::rptadv_phone_account_configuration_v1>()
        {
            return Err(());
        }
        // SAFETY: the complete record size was checked before reading its fields.
        let account = unsafe { pointer.read_unaligned() };
        let protocol = match account.protocol {
            abi::RPTADV_PHONE_ACCOUNT_IAX => PhoneAccountProtocol::Iax,
            abi::RPTADV_PHONE_ACCOUNT_SIP => PhoneAccountProtocol::Sip,
            _ => return Err(()),
        };
        if account.enabled > 1
            || account.register_account > 1
            || account.registration_interval_s == 0
            || !(1..=120000).contains(&account.authentication_timeout_ms)
        {
            return Err(());
        }
        let copy = |span: abi::rptadv_peer_bytes_v1| {
            // SAFETY: spans are borrowed for this callback and copied before return.
            unsafe { text(span.data.cast(), span.length) }.map(str::to_owned)
        };
        accounts.push(ResolvedPhoneAccount {
            id: copy(account.id)?,
            local_node: local.to_owned(),
            protocol,
            enabled: account.enabled == 1,
            server: copy(account.server)?,
            username: copy(account.username)?,
            secrets_file: copy(account.secrets_file)?,
            register: account.register_account == 1,
            registration_interval_s: account.registration_interval_s,
            authentication_timeout_ms: account.authentication_timeout_ms,
        });
    }
    Ok(accounts)
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
    fn configured_native_profiles_are_resolved_once_without_hardware() {
        let valid = "[1000]\n[2000]\n[link 1000]\nlimiter_enabled=yes\n";
        assert!(check_config(valid).unwrap().is_empty());
        for role in ["local", "link", "voice_telemetry"] {
            let source = format!("[1000]\n{role}_profile=selected\n[{role} selected]\n");
            let warnings = check_config(&format!(
                "{source}limiter_enabled=invalid\nunknown_processing_setting=yes\n"
            ))
            .unwrap();
            assert_eq!(warnings.len(), 2, "{role}: {warnings:?}");
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("limiter_enabled"))
            );
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("unknown_processing_setting"))
            );
            assert!(
                check_config(&format!(
                    "{source}limiter_low_crossover_hz=2000\nlimiter_high_crossover_hz=1000\n"
                ))
                .is_err()
            );
        }
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

        assert_eq!(unsafe { text(std::ptr::null(), 0) }, Ok(""));
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

    #[test]
    fn phone_account_inspection_copies_owned_redacted_records() {
        let source = String::from(
            "[1000]\n[phone_account office]\nlocal_node=1000\nprotocol=sip\nenabled=yes\nserver=provider.example:5060\nusername=private-user\nsecrets_file=private/path.conf\nregister=yes\nregistration_interval_s=42\nauthentication_timeout_ms=1234\n",
        );
        let radios = resolve_radio_nodes(&source).unwrap();
        let accounts = resolve_phone_accounts(&source).unwrap();
        assert_eq!(resolve_radio_nodes(&source).unwrap(), radios);
        drop(source);
        assert_eq!(accounts.len(), 1);
        let account = &accounts[0];
        assert_eq!(account.local_node, "1000");
        assert_eq!(account.id, "office");
        assert_eq!(account.protocol, PhoneAccountProtocol::Sip);
        assert!(account.enabled && account.register);
        assert_eq!(account.server, "provider.example:5060");
        assert_eq!(account.username, "private-user");
        assert_eq!(account.secrets_file, "private/path.conf");
        assert_eq!(account.registration_interval_s, 42);
        assert_eq!(account.authentication_timeout_ms, 1234);
        let debug = format!("{accounts:?}");
        for private in ["provider.example", "private-user", "private/path.conf"] {
            assert!(!debug.contains(private), "{debug}");
        }
    }

    #[test]
    fn phone_account_copy_rejects_malformed_public_records() {
        // SAFETY: this ABI record contains only integers and nullable pointers.
        let mut record: abi::rptadv_phone_account_configuration_v1 = unsafe { std::mem::zeroed() };
        record.struct_size = size_of::<abi::rptadv_phone_account_configuration_v1>() as u32;
        record.protocol = abi::RPTADV_PHONE_ACCOUNT_IAX;
        record.registration_interval_s = 300;
        record.authentication_timeout_ms = 30000;
        assert_eq!(
            unsafe { copy_phone_accounts("1000", std::ptr::null(), 0) },
            Ok(Vec::new())
        );
        assert!(unsafe { copy_phone_accounts("1000", std::ptr::null(), 1) }.is_err());
        assert!(unsafe { copy_phone_accounts("1000", &record, usize::MAX) }.is_err());
        let valid = record;
        let accounts = unsafe { copy_phone_accounts("1000", &record, 1) }.unwrap();
        assert_eq!(accounts[0].protocol, PhoneAccountProtocol::Iax);
        assert!(!accounts[0].enabled && !accounts[0].register);
        for field in 0..10 {
            record = valid;
            match field {
                0 => record.struct_size -= 1,
                1 => record.protocol = 99,
                2 => record.enabled = 2,
                3 => record.register_account = 2,
                4 => record.id.length = 1,
                5 => record.server.length = 1,
                6 => record.username.length = 1,
                7 => record.secrets_file.length = 1,
                8 => record.registration_interval_s = 0,
                _ => record.authentication_timeout_ms = 120001,
            }
            assert!(
                unsafe { copy_phone_accounts("1000", &record, 1) }.is_err(),
                "field {field}"
            );
        }
        let old_prefix = 4_u32;
        assert!(
            unsafe { copy_phone_accounts("1000", (&old_prefix as *const u32).cast(), 1) }.is_err()
        );
        let mut output = Inspection::default();
        let context = (&mut output as *mut Inspection).cast();
        assert_eq!(
            unsafe { copy_node(context, (&old_prefix as *const u32).cast()) },
            -1
        );
        assert!(output.invalid_record);
        output.invalid_record = false;
        // SAFETY: the host record contains only scalar fields and nullable pointers.
        let mut node: abi::rptadv_node_host_configuration = unsafe { std::mem::zeroed() };
        node.struct_size = size_of::<abi::rptadv_node_host_configuration>() as u32;
        node.phone_account_count = 1;
        assert_eq!(unsafe { copy_node(context, &node) }, -1);
        assert!(output.invalid_record);
        assert!(output.accounts.is_empty());
    }
}
