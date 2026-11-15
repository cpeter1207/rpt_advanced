//! Actual private SIP handles over the same product admission and source ABI as IAX.
use super::{HostServicesOwner, PeerHandle};
use crate::{
    abi,
    configuration::PhoneAccountProtocol,
    phone::{
        call_media::Media,
        media::MediaError,
        registration::{
            Error,
            calls::{Receipt, Shared},
        },
    },
};
use std::{
    ffi::c_void,
    sync::{Arc, atomic::Ordering},
};

/// Only immutable account pointers and a clone of the existing reactor sender.
pub(super) struct Originator {
    client: crate::phone::registration::calls::Client,
    accounts: crate::phone_accounts::PreparedAccounts,
    admitting: bool,
}

fn account_current(
    context: &super::StandaloneHostContext,
    account: &Arc<crate::phone_accounts::PreparedAccount>,
) -> bool {
    context
        .sip
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|owner| {
            owner
                .accounts
                .as_slice()
                .iter()
                .any(|current| Arc::ptr_eq(current, account))
        })
}

/// Ordinary control/lifecycle work only; never block the reactor or radio callback.
pub(super) unsafe extern "C" fn dial(
    context: *mut c_void,
    account: *const std::ffi::c_char,
    account_length: usize,
    local: *const std::ffi::c_char,
    local_length: usize,
    destination: *const std::ffi::c_char,
    destination_length: usize,
    maximum_frames: usize,
    current: abi::rptadv_current_v1,
    current_context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    unsafe {
        dial_progress(
            context,
            account,
            account_length,
            local,
            local_length,
            destination,
            destination_length,
            maximum_frames,
            current,
            current_context,
            None,
            std::ptr::null_mut(),
            output,
        )
    }
}

/// Ordinary control/lifecycle work only; never block the reactor or radio callback.
pub(super) unsafe extern "C" fn dial_progress(
    context: *mut c_void,
    account: *const std::ffi::c_char,
    account_length: usize,
    local: *const std::ffi::c_char,
    local_length: usize,
    destination: *const std::ffi::c_char,
    destination_length: usize,
    maximum_frames: usize,
    current: abi::rptadv_current_v1,
    current_context: *mut c_void,
    progress: abi::rptadv_phone_progress_v1,
    progress_context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    let result = super::boundary(-1, || {
        let Some(output) = (unsafe { output.as_mut() }) else {
            return -1;
        };
        *output = std::ptr::null_mut();
        let (Some(context), Some(account), Some(local), Some(destination), Some(current)) = (
            unsafe { context.cast::<super::StandaloneHostContext>().as_ref() },
            unsafe { super::input(account, account_length) },
            unsafe { super::input(local, local_length) },
            unsafe { super::input(destination, destination_length) },
            current,
        ) else {
            return -1;
        };
        if maximum_frames == 0
            || local.is_empty()
            || local.len() > 63
            || local.chars().any(char::is_control)
            || unsafe { current(current_context) } == 0
        {
            return -1;
        }
        let Some(ingress) = context.ingress.as_ref() else {
            return -1;
        };
        let (client, account) = {
            let binding = context
                .sip
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(owner) = binding.as_ref().filter(|owner| owner.admitting) else {
                return -1;
            };
            let Some(account) = owner
                .accounts
                .as_slice()
                .iter()
                .find(|candidate| {
                    let settings = candidate.settings();
                    settings.enabled
                        && settings.protocol == PhoneAccountProtocol::Sip
                        && settings.id == account
                })
                .cloned()
            else {
                return -1;
            };
            (owner.client.clone(), account)
        };
        if unsafe { current(current_context) } == 0 || !account_current(context, &account) {
            return -1;
        }
        let Ok(mut pending) =
            client.originate(&account, local, destination.to_owned(), ingress.jitter)
        else {
            return -1;
        };
        loop {
            if unsafe { current(current_context) } == 0 || !account_current(context, &account) {
                break -1;
            }
            match pending.try_complete() {
                Ok(Some(handle)) => {
                    if unsafe { current(current_context) } == 0
                        || !account_current(context, &account)
                        || !handle.shared.live()
                    {
                        break -1;
                    }
                    *output = Box::into_raw(Box::new(PeerHandle::Sip(handle))).cast();
                    break 0;
                }
                Err(_) => break -1,
                Ok(None) => {
                    if let Some(progress) = progress {
                        unsafe { progress(progress_context, pending.provisional_status()) };
                    }
                    std::thread::sleep(crate::phone::registration::SERVICE_INTERVAL);
                }
            }
        }
    });
    if let Some(progress) = progress {
        unsafe { progress(progress_context, 0) };
    }
    result
}

impl HostServicesOwner {
    /// Publish only a committed SIP cohort; no reactor callback is invoked here.
    pub(crate) fn publish_sip(
        &self,
        worker: Option<&crate::phone::registration::Worker>,
        accounts: &crate::phone_accounts::PreparedAccounts,
    ) {
        let originator = worker.map(|worker| Originator {
            client: worker.client(),
            accounts: accounts.clone(),
            admitting: true,
        });
        *self
            .context
            .sip
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = originator;
    }
    /// Suspend only new dials during product/account publication, not live calls.
    pub(crate) fn set_sip_dial_admission(&self, admitting: bool) {
        if let Some(owner) = self
            .context
            .sip
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
        {
            owner.admitting = admitting;
        }
    }
    pub(crate) fn incoming_sip(
        &self,
        product: &abi::rptadv_product_descriptor_v1,
        receipt: Receipt,
    ) -> Result<(), Error> {
        let (Some(authorize), Some(incoming)) = (product.authorize_incoming, product.incoming)
        else {
            return Err(Error::Call);
        };
        let settings = receipt.account.settings();
        if !settings.enabled || settings.protocol != PhoneAccountProtocol::Sip {
            return Err(Error::Call);
        }
        let ingress = self.context.ingress.as_ref().ok_or(Error::Call)?;
        let number = super::iax_phone::HOST_CALL_SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
                last.checked_add(1)
            })
            .map_err(|_| Error::Call)?
            + 1;
        let handle = receipt.prepare(ingress.jitter, format!("{number:032x}"))?;
        let shared = Arc::clone(&handle.shared);
        let source = shared.source.ip().to_string();
        let request = crate::runtime::incoming_request(3, &shared.local, &shared.remote, &source);
        let raw = Box::into_raw(Box::new(PeerHandle::Sip(handle))).cast();
        let result = unsafe {
            if authorize(&request, raw) == 0 {
                incoming(&request, raw)
            } else {
                1
            }
        };
        if !matches!(result, 0 | -1) {
            self.destroy_peer(raw);
        }
        if result != 0 {
            return Err(Error::Call);
        }
        shared.answer().inspect_err(|_| shared.end())
    }
}

pub(super) unsafe fn shared<'a>(peer: *const c_void) -> Option<&'a Shared> {
    match unsafe { peer.cast::<PeerHandle>().as_ref() }? {
        PeerHandle::Sip(handle) => Some(&handle.shared),
        _ => None,
    }
}
pub(super) fn media<T>(
    shared: &Shared,
    name: &'static str,
    operation: impl FnOnce(&mut Media) -> Result<T, MediaError>,
) -> Result<T, ()> {
    if !shared.live() {
        return Err(());
    }
    let result = shared
        .media
        .lock()
        .map_err(|_| MediaError::Owner)
        .and_then(|mut media| operation(&mut media));
    result.map_err(|error| {
        shared.end();
        // Fixed operation labels and a payload-free enum; never format SIP,
        // RTP, audio, digits, account identifiers or a foreign error string.
        eprintln!("phone SIP: media {name} failed ({error:?})");
    })
}
pub(super) unsafe fn read(
    shared: &Shared,
    completion: u32,
    sink: *const abi::rptadv_peer_source_sink_v1,
    output: &mut abi::rptadv_peer_source_result_v1,
) -> i32 {
    if !shared.live() {
        output.progress = 0;
        output.activity = 0;
        output.finish_pending = 0;
        output.state = 3;
        output.represented_node_number = 0;
        return 0;
    }
    media(shared, "read", |media| unsafe {
        media.read(completion, sink, output)
    })
    .map_or(-1, |()| 0)
}
