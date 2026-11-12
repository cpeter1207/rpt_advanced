//! Typed private-call ownership for the existing standalone host callbacks.
//! Product admission borrows the actual handle before its one ownership transfer.
//! No registrar, dial worker or additional scheduler is created.
use super::{HostServicesOwner, IaxHandle, PeerHandle};
use crate::{
    abi,
    configuration::PhoneAccountProtocol,
    iax::{
        IaxError, IaxPeer,
        accounts::{CallReceipt, ProviderCall},
        timed::{Event, Kind},
    },
    phone_accounts::PreparedAccount,
};
use std::{
    ffi::c_void,
    mem::size_of,
    net::SocketAddr,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

/// Owned by the standalone host artifact, not by a separately linked support or
/// provider DSO. Every native private-call creator in this host uses this one
/// allocator. HostServicesOwner/listener replacement cannot reset its history.
static HOST_CALL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Private metadata and one bounded accepted-prefix source owner.
pub(super) struct Phone {
    pub account: String,
    pub receipt: CallReceipt,
    pending: Option<Event>,
    offset: usize,
    acknowledged: u64,
    host_epoch: Option<u64>,
    closed: bool,
}

impl HostServicesOwner {
    /// Consume an already owned transport-authenticated call. This does not admit
    /// the call to the product or grant mailbox/RF authority.
    ///
    /// The caller supplies the current committed account and exact bound listener,
    /// under the existing serialized admission/reload fence.
    pub fn provider_peer_handle(
        &self,
        call: ProviderCall,
        account: &PreparedAccount,
        listener: SocketAddr,
    ) -> Result<*mut c_void, IaxError> {
        self.provider_peer_handle_allocated(call, account, listener, &HOST_CALL_SEQUENCE)
    }

    /// Authorize the actual borrowed private handle, then transfer it only under
    /// the product's existing consumed/unconsumed incoming contract. This grants
    /// neither mailbox authentication nor transceive attachment.
    pub(crate) fn incoming_provider(
        &self,
        product: &abi::rptadv_product_descriptor_v1,
        call: ProviderCall,
        account: &PreparedAccount,
        listener: SocketAddr,
    ) -> Result<(), IaxError> {
        let (Some(authorize), Some(incoming)) = (product.authorize_incoming, product.incoming)
        else {
            return Err(IaxError::IncompatibleAdapter);
        };
        let source = call.receipt.remote.ip().to_string();
        let handle = self.provider_peer_handle(call, account, listener)?;
        // This constructor exclusively creates an IAX phone handle. Copy the
        // opaque identity so request bytes survive a consuming product callback.
        let remote = unsafe { super::iax_peer_ref(handle) }
            .expect("provider constructor returns an IAX handle")
            .remote
            .clone();
        let request =
            crate::runtime::incoming_request(3, &account.settings().local_node, &remote, &source);
        let result = unsafe {
            if authorize(&request, handle) == 0 {
                incoming(&request, handle)
            } else {
                1 // Borrowed authorization never consumes the handle.
            }
        };
        if !matches!(result, 0 | -1) {
            self.destroy_peer(handle);
        }
        if result == 0 {
            Ok(())
        } else {
            Err(IaxError::Rejected)
        }
    }

    pub(super) fn provider_peer_handle_allocated(
        &self,
        call: ProviderCall,
        account: &PreparedAccount,
        listener: SocketAddr,
        sequence: &AtomicU64,
    ) -> Result<*mut c_void, IaxError> {
        let receipt = call.receipt;
        if !account.settings().enabled
            || account.settings().protocol != PhoneAccountProtocol::Iax
            || receipt.account_generation != account.generation()
            || receipt.account_generation == 0
            || receipt.call_generation == 0
            || receipt.local != listener
            || receipt.remote != account.remote()
            || !matches!(receipt.format, 4 | 8)
            || !(1..=32767).contains(&receipt.local_call)
            || !(1..=32767).contains(&receipt.remote_call)
            || call.peer.sample_rate_hz() != 8000
        {
            return Err(IaxError::InvalidParameters);
        }
        if self.context.ingress.is_none() || !call.peer.supports_timed_source() {
            return Err(IaxError::IncompatibleAdapter);
        }
        let number = sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
                last.checked_add(1)
            })
            .map_err(|_| IaxError::Operation)?
            + 1;
        // Canonical opaque 128-bit text, not an unpredictable authentication token.
        // The checked 64-bit allocation domain is never wrapped or reused.
        let remote = format!("{number:032x}");
        let phone = Box::new(Phone {
            account: account.settings().id.clone(),
            receipt,
            pending: None,
            offset: 0,
            acknowledged: 0,
            host_epoch: None,
            closed: false,
        });
        Ok(Box::into_raw(Box::new(PeerHandle::Iax(IaxHandle {
            peer: call.peer,
            samples: vec![0.0; 160],
            local: account.settings().local_node.clone(),
            remote,
            phone: Some(phone),
        })))
        .cast())
    }
}

impl Phone {
    /// One source turn on the existing exclusive peer owner. A retained/refused
    /// item is offered before polling another source item. Released ABI2 service
    /// keeps ACK/retry progress bounded while leaving held PCM/digits untouched.
    /// Protocol retirement fences old PCM without discarding private control.
    pub(super) unsafe fn read(
        &mut self,
        peer: &mut IaxPeer,
        samples: &mut [f32],
        completion: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        output: &mut abi::rptadv_peer_source_result_v1,
    ) -> Result<(), IaxError> {
        output.progress = 0;
        output.activity = 0;
        output.finish_pending = 0;
        output.state = if self.closed { 3 } else { 1 };
        output.represented_node_number = 0;
        if self.closed {
            return Ok(());
        }
        let result = unsafe { self.read_open(peer, samples, completion, sink, output) };
        if result.is_err() {
            // Every terminal source failure retires this owner before a later
            // state query or provider access. Never revive a failed call.
            self.closed = true;
            self.pending = None;
            output.state = 3;
        }
        result
    }

    /// Advance only a live source; the outer boundary latches every terminal error.
    unsafe fn read_open(
        &mut self,
        peer: &mut IaxPeer,
        samples: &mut [f32],
        completion: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        output: &mut abi::rptadv_peer_source_result_v1,
    ) -> Result<(), IaxError> {
        if completion != 0 {
            return Err(IaxError::Operation);
        }
        if sink.is_null() {
            return Ok(());
        }
        if unsafe { ptr::addr_of!((*sink).struct_size).read() }
            < size_of::<abi::rptadv_peer_source_sink_v1>() as u32
            || unsafe { ptr::addr_of!((*sink).abi_version).read() } != 1
        {
            return Err(IaxError::Operation);
        }
        let sink = unsafe { &*sink };
        if sink.host_clock_epoch == 0
            || sink.audio_timed.is_none()
            || sink.gap.is_none()
            || sink.digit.is_none()
            || sink.source_discontinuity.is_none()
            || self
                .host_epoch
                .is_some_and(|epoch| epoch != sink.host_clock_epoch)
        {
            return Err(IaxError::Operation);
        }
        self.host_epoch = Some(sink.host_clock_epoch);
        let event = if self.pending.is_some() {
            peer.service_source(
                sink.host_clock_epoch,
                sink.reference_now_ns,
                self.acknowledged,
            )?
        } else {
            peer.poll_source(
                samples,
                sink.host_clock_epoch,
                sink.reference_now_ns,
                self.acknowledged,
            )?
        };
        if (event.discontinuity && event.media_epoch <= self.acknowledged)
            || (!event.discontinuity && event.media_epoch != self.acknowledged)
        {
            return Err(IaxError::Operation);
        }
        if self.pending.is_none() {
            if matches!(event.kind, Kind::None) && !event.discontinuity {
                return Ok(());
            }
            self.pending = Some(event);
            self.offset = 0;
        } else if event.discontinuity {
            // Media retirement may supersede refused PCM/gaps, never a private
            // control symbol that the downstream owner has not accepted.
            let pending = self.pending.as_mut().unwrap();
            if matches!(pending.kind, Kind::Digit(_)) {
                pending.media_epoch = event.media_epoch;
                pending.discontinuity = true;
            } else {
                *pending = event;
                self.offset = 0;
            }
        }
        let event = self.pending.as_mut().unwrap();
        if event.discontinuity {
            match unsafe { sink.source_discontinuity.unwrap()(sink.context) } {
                0 => {
                    self.acknowledged = event.media_epoch;
                    event.discontinuity = false;
                }
                1 => {
                    output.progress = 2;
                    return Ok(());
                }
                _ => return Err(IaxError::Operation),
            }
        }
        let accepted = match event.kind {
            Kind::Audio(count) if event.born_ns.is_some() => {
                let birth = abi::rptadv_source_birth_v1 {
                    struct_size: size_of::<abi::rptadv_source_birth_v1>() as u32,
                    abi_version: 1,
                    host_clock_epoch: sink.host_clock_epoch,
                    born_ns: event.born_ns.unwrap(),
                };
                if birth.born_ns > sink.reference_now_ns {
                    return Err(IaxError::Operation);
                }
                let remaining = count - self.offset;
                let accepted = unsafe {
                    sink.audio_timed.unwrap()(
                        sink.context,
                        event.source_start + self.offset as u64,
                        samples[self.offset..count].as_ptr(),
                        remaining,
                        0,
                        &birth,
                    )
                };
                if accepted > remaining {
                    return Err(IaxError::Operation);
                }
                self.offset += accepted;
                self.offset == count
            }
            Kind::Audio(count) => {
                // Absent birth does not identify real media versus native PLC.
                // Discard unavailable usable audio as exact counted source loss,
                // never send ambiguous PCM to analysis or invent a concealment flag.
                unsafe { self.offer_gap(sink, count as u32)? }
            }
            Kind::Gap(count) => unsafe { self.offer_gap(sink, count)? },
            Kind::Digit(symbol) => {
                match unsafe { sink.digit.unwrap()(sink.context, symbol as u32) } {
                    0 => true,
                    1 => false,
                    _ => return Err(IaxError::Operation),
                }
            }
            Kind::Hangup => {
                self.closed = true;
                output.state = 3;
                true
            }
            Kind::None | Kind::Ignored => true,
        };
        output.progress = if accepted { 1 } else { 2 };
        if accepted {
            self.pending = None;
        }
        Ok(())
    }

    /// Credit only an exact accepted prefix; a refused gap retains its coordinates.
    unsafe fn offer_gap(
        &mut self,
        sink: &abi::rptadv_peer_source_sink_v1,
        count: u32,
    ) -> Result<bool, IaxError> {
        let event = self.pending.as_ref().unwrap();
        let remaining = count - self.offset as u32;
        let accepted = unsafe {
            sink.gap.unwrap()(
                sink.context,
                event.source_start + self.offset as u64,
                remaining,
            )
        };
        if accepted > remaining {
            return Err(IaxError::Operation);
        }
        self.offset += accepted as usize;
        Ok(self.offset == count as usize)
    }
}
