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

impl HostServicesOwner {
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
