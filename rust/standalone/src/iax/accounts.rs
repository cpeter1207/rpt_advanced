//! Typed wrappers for the released provider-account listener tail.
//! Each synchronous operation runs on its listener owner, never an audio callback.

use super::{IaxClient, IaxError, IaxPeer, IaxServer};
use crate::abi;
use std::{
    mem::size_of,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
    ptr::{self, NonNull},
};

/// Borrowed provider identity; credentials are never formatted.
pub struct ProviderAccount<'a> {
    /// Stable configuration label, not a caller number or radio node.
    pub account: &'a str,
    /// Exact nonzero account generation.
    pub generation: u64,
    /// Configured local admission binding, including named nodes.
    pub local_binding: &'a str,
    /// Resolved numeric provider endpoint.
    pub remote: SocketAddr,
    /// Transport login, unrelated to mailbox authorization.
    pub username: &'a str,
    /// Literal transport secret, borrowed for the operation only.
    pub secret: &'a str,
    /// Maximum incoming authentication lifetime.
    pub authentication_timeout_ms: u32,
}

/// One explicit provider registration, refresh or release.
pub struct RegistrationRequest<'a> {
    /// Configured account label.
    pub account: &'a str,
    /// Exact account generation.
    pub generation: u64,
    /// Requested nonzero registration interval.
    pub refresh_seconds: u16,
    /// Request authenticated release instead of registration.
    pub release: bool,
    /// Total operation deadline in milliseconds.
    pub timeout_ms: u32,
}

/// Copied registration result, without credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistrationReceipt {
    /// Actual granted refresh interval.
    pub refresh_seconds: u16,
    /// Actual bound listener socket.
    pub local: SocketAddr,
    /// Provider's apparent address, when reported.
    pub apparent: Option<SocketAddr>,
}

/// One outgoing call using an already configured account.
pub struct DialRequest<'a> {
    /// Configured account label.
    pub account: &'a str,
    /// Exact account generation.
    pub generation: u64,
    /// Literal provider destination, including short numbers and leading zeros.
    pub destination: &'a str,
    /// Untrusted presentation number, never a product identity.
    pub caller_number: &'a str,
    /// Untrusted presentation name.
    pub caller_name: &'a str,
    /// Optional provider dial context.
    pub context: &'a str,
    /// Total setup and answer deadline in milliseconds.
    pub timeout_ms: u32,
}

/// Exact provider lifecycle receipt copied before borrowed metadata expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallReceipt {
    /// Configured account generation.
    pub account_generation: u64,
    /// Nonzero listener-local call generation.
    pub call_generation: u64,
    /// Negotiated IAX format bit.
    pub format: u64,
    /// Actual local IAX call number.
    pub local_call: u16,
    /// Actual remote IAX call number.
    pub remote_call: u16,
    /// Actual listener endpoint.
    pub local: SocketAddr,
    /// Actual provider endpoint.
    pub remote: SocketAddr,
}

/// Uniquely owned call and its verified transport receipt.
pub struct ProviderCall {
    /// Protocol handle retained until its serialized owner closes it.
    pub peer: IaxPeer,
    /// Transport provenance, not mailbox or RF permission.
    pub receipt: CallReceipt,
}

/// Transport-authenticated provider policy callback; caller fields remain untrusted.
pub type ProviderAuthorize = abi::rptadv_iax2_provider_authorize_v1;
/// Transfer a unique provider peer only when this callback returns zero.
pub type ProviderAccept = abi::rptadv_iax2_provider_accept_v1;

impl IaxServer {
    /// Bind a provider-capable listener, including a phone-only listener without ASL nodes.
    pub fn bind_with_accounts(
        client: &IaxClient,
        address: SocketAddr,
        local_nodes: &[String],
        accounts: &[ProviderAccount<'_>],
    ) -> Result<Self, IaxError> {
        let api = super::load_server_descriptor(client)?;
        check_account_support(&api)?;
        let address = address.to_string();
        let nodes = local_nodes.join(",");
        let handle = with_accounts(accounts, |records| {
            let options = super::ServerOptions {
                struct_size: size_of::<super::ServerOptions>() as u32,
                abi_version: super::ABI_VERSION,
                bind_address: address.as_ptr(),
                bind_address_length: address.len(),
                local_nodes: nodes.as_ptr(),
                local_nodes_length: nodes.len(),
                catalog: client.0.catalog.as_ptr(),
                accounts: records.as_ptr(),
                account_count: records.len(),
            };
            let mut handle = ptr::null_mut();
            // SAFETY: all borrowed spans and the retained catalog live through bind.
            if unsafe { api.bind.unwrap()(&options, &mut handle) } != 0 {
                return Err(IaxError::Bind);
            }
            NonNull::new(handle).ok_or(IaxError::Bind)
        })?;
        Ok(Self {
            _owner: client.clone(),
            api,
            handle,
        })
    }

    /// Replace one listener's accounts atomically inside the provider.
    ///
    /// This operation is not a multi-listener/product reload transaction.
    pub fn configure_accounts(&mut self, accounts: &[ProviderAccount<'_>]) -> Result<(), IaxError> {
        let configure = self
            .api
            .configure_accounts
            .ok_or(IaxError::IncompatibleAdapter)?;
        with_accounts(accounts, |records| {
            // SAFETY: the provider copies every span before returning.
            status(unsafe { configure(self.handle.as_ptr(), records.as_ptr(), records.len()) })
        })
    }

    /// Register/refresh/release on the listener's existing source port.
    ///
    /// Requires async listener activation. Call off-executor without product locks.
    pub fn register_account(
        &mut self,
        request: &RegistrationRequest<'_>,
    ) -> Result<RegistrationReceipt, IaxError> {
        let register = self
            .api
            .register_account
            .ok_or(IaxError::IncompatibleAdapter)?;
        let options = abi::rptadv_iax2_registration_options_v1 {
            struct_size: size_of::<abi::rptadv_iax2_registration_options_v1>() as u32,
            abi_version: 1,
            account: bytes(request.account),
            generation: request.generation,
            refresh: request.refresh_seconds,
            release: u16::from(request.release),
            timeout_ms: request.timeout_ms,
        };
        // SAFETY: the ABI record contains only integers and fixed integer arrays.
        let mut receipt: abi::rptadv_iax2_registration_result_v1 = unsafe { std::mem::zeroed() };
        receipt.struct_size = size_of::<abi::rptadv_iax2_registration_result_v1>() as u32;
        status(unsafe { register(self.handle.as_ptr(), &options, &mut receipt) })?;
        if receipt.struct_size as usize != size_of::<abi::rptadv_iax2_registration_result_v1>()
            || receipt.abi_version != 1
            || receipt.refresh == 0
            || receipt.reserved != 0
        {
            return Err(IaxError::Operation);
        }
        Ok(RegistrationReceipt {
            refresh_seconds: receipt.refresh,
            local: endpoint(receipt.local)?.ok_or(IaxError::Operation)?,
            apparent: endpoint(receipt.apparent)?,
        })
    }

    /// Dial with the configured account rather than a numeric ASL identity.
    ///
    /// Requires async listener activation. Call off-executor without product locks.
    pub fn dial_provider_account(
        &mut self,
        request: &DialRequest<'_>,
    ) -> Result<ProviderCall, IaxError> {
        let dial = self
            .api
            .dial_provider_account
            .ok_or(IaxError::IncompatibleAdapter)?;
        let options = abi::rptadv_iax2_provider_dial_options_v1 {
            struct_size: size_of::<abi::rptadv_iax2_provider_dial_options_v1>() as u32,
            abi_version: 1,
            generation: request.generation,
            account: bytes(request.account),
            destination: bytes(request.destination),
            caller_number: bytes(request.caller_number),
            caller_name: bytes(request.caller_name),
            context: bytes(request.context),
            timeout_ms: request.timeout_ms,
            reserved: 0,
        };
        // SAFETY: this sized ABI record has no references or invalid zero bit patterns.
        let mut receipt: abi::rptadv_iax2_provider_call_receipt_v1 = unsafe { std::mem::zeroed() };
        receipt.struct_size = size_of::<abi::rptadv_iax2_provider_call_receipt_v1>() as u32;
        let mut handle = ptr::null_mut();
        status(unsafe { dial(self.handle.as_ptr(), &options, &mut handle, &mut receipt) })?;
        // Adopt before checking metadata so a malformed success still destroys its unique peer.
        let peer = unsafe { self._owner.clone().adopt_inbound(handle) }?;
        let receipt = call_receipt(receipt, request.generation)?;
        Ok(ProviderCall { peer, receipt })
    }

    /// Poll exactly one mixed numeric/provider admission stream.
    ///
    /// # Safety
    /// Context and callbacks must remain valid for this synchronous call and may not unwind.
    pub unsafe fn poll_accounts(
        &mut self,
        now_seconds: u32,
        authorize: super::ServerAuthorize,
        accept: super::ServerAccept,
        provider_authorize: ProviderAuthorize,
        provider_accept: ProviderAccept,
        context: *mut std::ffi::c_void,
    ) -> Result<bool, IaxError> {
        let poll = self
            .api
            .poll_accounts
            .ok_or(IaxError::IncompatibleAdapter)?;
        match unsafe {
            poll(
                self.handle.as_ptr(),
                now_seconds,
                authorize,
                accept,
                provider_authorize,
                provider_accept,
                context,
            )
        } {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(IaxError::Operation),
        }
    }
}

/// Borrow UTF-8 without converting credentials to NUL-terminated or printable storage.
pub(super) fn bytes(value: &str) -> abi::rptadv_iax2_bytes_v1 {
    abi::rptadv_iax2_bytes_v1 {
        data: value.as_ptr(),
        length: value.len(),
    }
}

/// Keep numeric endpoint storage alive through the synchronous provider copy.
fn with_accounts<T>(
    accounts: &[ProviderAccount<'_>],
    action: impl FnOnce(&[abi::rptadv_iax2_provider_account_v1]) -> Result<T, IaxError>,
) -> Result<T, IaxError> {
    let addresses: Vec<_> = accounts
        .iter()
        .map(|account| account.remote.to_string())
        .collect();
    let records: Vec<_> = accounts
        .iter()
        .zip(&addresses)
        .map(|(account, address)| account_record(account, address))
        .collect();
    action(&records)
}

/// Borrow strings only for the synchronous provider copy; callers retain both inputs.
pub(super) fn account_record(
    account: &ProviderAccount<'_>,
    address: &str,
) -> abi::rptadv_iax2_provider_account_v1 {
    abi::rptadv_iax2_provider_account_v1 {
        struct_size: size_of::<abi::rptadv_iax2_provider_account_v1>() as u32,
        abi_version: 1,
        generation: account.generation,
        account: bytes(account.account),
        local_binding: bytes(account.local_binding),
        remote_address: bytes(address),
        username: bytes(account.username),
        secret: bytes(account.secret),
        authentication_timeout_ms: account.authentication_timeout_ms,
        reserved: 0,
    }
}

/// Require the complete provider-account capability before binding any listener.
fn check_account_support(api: &super::ServerDescriptor) -> Result<(), IaxError> {
    if api.configure_accounts.is_none()
        || api.register_account.is_none()
        || api.poll_accounts.is_none()
        || api.dial_provider_account.is_none()
    {
        return Err(IaxError::IncompatibleAdapter);
    }
    Ok(())
}

/// Preserve useful non-secret provider failures instead of treating all failures alike.
pub(super) fn status(result: i32) -> Result<(), IaxError> {
    match result {
        0 => Ok(()),
        -1 => Err(IaxError::InvalidParameters),
        -2 => Err(IaxError::Network),
        -3 => Err(IaxError::Timeout),
        -4 => Err(IaxError::Rejected),
        -5 => Err(IaxError::Codec),
        -6 => Err(IaxError::Protocol),
        _ => Err(IaxError::Operation),
    }
}

/// Validate a copied fixed address; an absent endpoint must be fully zero.
fn endpoint(value: abi::rptadv_iax2_endpoint_v1) -> Result<Option<SocketAddr>, IaxError> {
    match value.family {
        0 if value.port == 0
            && value.address == [0; 16]
            && value.flowinfo == 0
            && value.scope_id == 0 =>
        {
            Ok(None)
        }
        4 if value.port != 0
            && value.address[4..] == [0; 12]
            && value.flowinfo == 0
            && value.scope_id == 0 =>
        {
            Ok(Some(SocketAddr::new(
                Ipv4Addr::new(
                    value.address[0],
                    value.address[1],
                    value.address[2],
                    value.address[3],
                )
                .into(),
                value.port,
            )))
        }
        6 if value.port != 0 => Ok(Some(SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::from(value.address),
            value.port,
            value.flowinfo,
            value.scope_id,
        )))),
        _ => Err(IaxError::Operation),
    }
}

/// Check generation and negotiated transport identities before exposing a call.
fn call_receipt(
    receipt: abi::rptadv_iax2_provider_call_receipt_v1,
    generation: u64,
) -> Result<CallReceipt, IaxError> {
    if receipt.struct_size as usize != size_of::<abi::rptadv_iax2_provider_call_receipt_v1>()
        || receipt.abi_version != 1
        || receipt.account_generation != generation
        || generation == 0
        || receipt.call_generation == 0
        || !receipt.format.is_power_of_two()
        || !(1..=32767).contains(&receipt.local_call)
        || !(1..=32767).contains(&receipt.remote_call)
        || receipt.reserved != 0
    {
        return Err(IaxError::Operation);
    }
    Ok(CallReceipt {
        account_generation: receipt.account_generation,
        call_generation: receipt.call_generation,
        format: receipt.format,
        local_call: receipt.local_call,
        remote_call: receipt.remote_call,
        local: endpoint(receipt.local)?.ok_or(IaxError::Operation)?,
        remote: endpoint(receipt.remote)?.ok_or(IaxError::Operation)?,
    })
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
