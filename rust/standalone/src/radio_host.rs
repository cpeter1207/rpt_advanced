pub(crate) mod station_binding;
use crate::{
    providers::{PhysicalProviders, PhysicalSet, ProviderError},
    station_plan::StationPlan,
};
use std::{
    collections::HashSet,
    sync::{Arc, Weak},
};
use std::{
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Native audio bound used by the existing Asterisk host.
pub const MAXIMUM_RADIO_FRAMES: usize = 4096;

/// Process-lifetime data supplied to product radio callbacks.
pub struct RadioHostContext {
    providers: Option<&'static crate::providers::ProviderSet>,
    typed_ports: Arc<Mutex<HashSet<[u8; 16]>>>,
    physical: Mutex<Weak<PhysicalSet>>,
    next_generation: AtomicU64,
}

impl RadioHostContext {
    /// Reserve typed physical plans with the matched providers retained by the host.
    pub fn new(providers: Option<&'static crate::providers::ProviderSet>) -> Self {
        Self {
            providers,
            typed_ports: Arc::default(),
            physical: Mutex::default(),
            next_generation: AtomicU64::new(0),
        }
    }

    fn station_providers(&self) -> Result<PhysicalProviders, ProviderError> {
        if let Some(existing) = self.providers {
            return Ok(PhysicalProviders::Existing(existing));
        }
        let mut physical = self
            .physical
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = physical.upgrade() {
            return Ok(PhysicalProviders::Loaded(existing));
        }
        let loaded = Arc::new(PhysicalSet::load()?);
        *physical = Arc::downgrade(&loaded);
        Ok(PhysicalProviders::Loaded(loaded))
    }

    fn next_generation(&self) -> Option<u64> {
        self.next_generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .ok()?
            .checked_add(1)
    }
}

/// A typed port remains reserved until its last live/update owner is destroyed.
struct PortLease {
    registry: Arc<Mutex<HashSet<[u8; 16]>>>,
    port: [u8; 16],
}
impl Drop for PortLease {
    fn drop(&mut self) {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.port);
    }
}

struct Selection {
    plan: Arc<StationPlan>,
    maximum_frames: u32,
    lease: Arc<PortLease>,
}

struct RadioHandle {
    reservation: Selection,
    active: Option<crate::radio_activation::ActiveRadio>,
    pending: Option<Selection>,
    metadata: crate::abi::rptadv_radio_metadata_v1,
    feedback: Option<crate::abi::UrpRadioFeedbackPort>,
}

/// Stage/acknowledge, commit, or roll back a native radio configuration on its
/// existing callback and hardware owners. Unsupported settings select handoff.
///
/// # Safety
/// The context and live radio handle remain retained for this serialized call.
pub unsafe extern "C" fn radio_update(context: *mut c_void, radio: *mut c_void, phase: u32) -> i32 {
    // SAFETY: this control-only boundary borrows retained context and radio owners.
    let (host, handle) = unsafe {
        (
            context.cast::<RadioHostContext>().as_ref(),
            radio.cast::<RadioHandle>().as_mut(),
        )
    };
    let (Some(_host), Some(handle)) = (host, handle) else {
        return -1;
    };
    let Some(active) = handle.active.as_mut() else {
        return -7;
    };
    if !matches!(phase, 1 | 2) {
        return -1;
    }
    if handle.pending.is_none() {
        return if phase == 2 { 0 } else { -7 };
    }
    let status = active.finish_typed(phase == 1);
    if status == 0 {
        let candidate = handle.pending.take().expect("staged radio reservation");
        if phase == 1 {
            handle.reservation = candidate;
        }
    }
    status
}

/// Dispatch typed maintenance through this radio's retained shared native owner.
///
/// # Safety
/// Handles and the initialized writable request remain live for this serialized
/// call. The optional sink borrows returned text only until its callback returns.
pub unsafe extern "C" fn radio_control(
    _: *mut c_void,
    radio: *mut c_void,
    request: *mut crate::abi::rptadv_radio_control_v1,
    sink: crate::abi::rptadv_text_sink_v1,
    sink_context: *mut c_void,
) -> i32 {
    if request.is_null() {
        return -1;
    }
    // SAFETY: the callback contract supplies at least the readable ABI header.
    let header = unsafe { request.cast::<[u32; 2]>().read_unaligned() };
    if (header[0] as usize) < std::mem::size_of::<crate::abi::rptadv_radio_control_v1>()
        || header[1] != 1
    {
        return -1;
    }
    // SAFETY: the caller lends the unchanged prefix and exclusive native reservation.
    let Some(handle) = (unsafe { radio.cast::<RadioHandle>().as_mut() }) else {
        return -1;
    };
    let operation = unsafe { std::ptr::addr_of!((*request).operation).read_unaligned() };
    if operation == crate::abi::RPTADV_RADIO_CONTROL_BIND_FEEDBACK {
        type Request = crate::abi::rptadv_radio_feedback_binding_v1;
        if header[0] as usize != std::mem::size_of::<Request>() || handle.active.is_some() {
            return -1;
        }
        // SAFETY: the checked prefix advertises this complete writable wrapper.
        let wrapped = unsafe { &mut *request.cast::<Request>() };
        if wrapped.reserved != 0
            || wrapped.accepted_abi_version != 0
            || wrapped.port.struct_size as usize != std::mem::size_of_val(&wrapped.port)
            || wrapped.port.abi_version != crate::abi::URP_RADIO_FEEDBACK_ABI_VERSION
        {
            return -1;
        }
        handle.feedback = Some(wrapped.port);
        wrapped.accepted_abi_version = 1;
        return 0;
    }
    if operation == crate::abi::RPTADV_RADIO_CONTROL_PHYSICAL_STATUS {
        type Request = crate::abi::rptadv_radio_physical_status_v1;
        if header[0] as usize != std::mem::size_of::<Request>() {
            return -1;
        }
        // SAFETY: the checked prefix advertises this complete writable wrapper.
        let wrapped = unsafe { &mut *request.cast::<Request>() };
        if wrapped.reserved != 0
            || wrapped.accepted_abi_version != 0
            || wrapped.status.struct_size as usize != std::mem::size_of_val(&wrapped.status)
            || wrapped.status.abi_version != 1
        {
            return -1;
        }
        let Some(active) = handle.active.as_mut() else {
            return -7;
        };
        let code = active.physical_status(&mut wrapped.status);
        if code == 0 {
            wrapped.accepted_abi_version = 1;
        }
        return code;
    }
    // SAFETY: all other operations only access the already validated base request.
    let request = unsafe { &mut *request };
    if request.operation == 7 {
        let command = &mut request.command;
        if command.struct_size as usize != std::mem::size_of_val(command)
            || command.abi_version != 1
            || command.command != crate::abi::URP_AST_COMMAND_GET_RECEIVE_POLICY
            || command.target != 0
        {
            return -1;
        }
        let candidate = handle.pending.as_ref().filter(|_| {
            handle
                .active
                .as_ref()
                .is_some_and(crate::radio_activation::ActiveRadio::candidate_acknowledged)
        });
        let reservation = candidate.unwrap_or(&handle.reservation);
        let (mode, frames) = reservation.plan.receive_policy;
        command.value = i64::from(mode);
        command.flags = frames;
        return 0;
    }
    let Some(active) = handle.active.as_mut() else {
        return -7;
    };
    active.maintenance(request, sink, sink_context)
}

/// Replace site claims on the already-active native radio, never a second device owner.
///
/// # Safety
/// `radio` is a live reserved handle and `binding` is readable for this synchronous call.
/// The caller serializes control operations with radio destruction.
pub unsafe extern "C" fn radio_site_bind(
    _: *mut c_void,
    radio: *mut c_void,
    bank: u32,
    binding: *const crate::abi::UrpAstSiteBinding,
) -> i32 {
    let (Some(handle), Some(binding)) = (unsafe { radio.cast::<RadioHandle>().as_mut() }, unsafe {
        binding.as_ref()
    }) else {
        return -1;
    };
    handle
        .active
        .as_mut()
        .map_or(-1, |active| active.site_bind(bank, binding))
}

/// Submit site levels through the existing native owner; acceptance is not readback.
///
/// # Safety
/// `radio` is live and `request` is readable for the serialized control call.
pub unsafe extern "C" fn radio_site_publish(
    _: *mut c_void,
    radio: *mut c_void,
    bank: u32,
    request: *const crate::abi::UrpAstSiteRequest,
) -> i32 {
    let (Some(handle), Some(request)) = (unsafe { radio.cast::<RadioHandle>().as_mut() }, unsafe {
        request.as_ref()
    }) else {
        return -1;
    };
    handle
        .active
        .as_mut()
        .map_or(-1, |active| active.site_publish(bank, request))
}

/// Copy the native owner's site observation without changing its timestamps or quality.
///
/// # Safety
/// `radio` is live and `snapshot` is exclusively writable during this serialized control call.
pub unsafe extern "C" fn radio_site_snapshot(
    _: *mut c_void,
    radio: *mut c_void,
    bank: u32,
    snapshot: *mut crate::abi::UrpAstSiteSnapshot,
) -> i32 {
    let (Some(handle), Some(snapshot)) =
        (unsafe { radio.cast::<RadioHandle>().as_mut() }, unsafe {
            snapshot.as_mut()
        })
    else {
        return -1;
    };
    handle
        .active
        .as_mut()
        .map_or(-1, |active| active.site_snapshot(bank, snapshot))
}

/// Product host callback that starts one radio's GPIO and audio path.
/// Bind optional TX metadata before activation, without acquiring hardware.
///
/// # Safety
/// `radio` is this host's unique retained reservation on serialized control.
pub unsafe extern "C" fn radio_metadata_bind(
    _: *mut c_void,
    radio: *mut c_void,
    metadata: crate::abi::rptadv_radio_metadata_v1,
) -> i32 {
    // SAFETY: the caller lends the retained unique reservation.
    let Some(handle) = (unsafe { radio.cast::<RadioHandle>().as_mut() }) else {
        return -1;
    };
    if handle.active.is_some() {
        return -1;
    }
    handle.metadata = metadata;
    0
}

/// Product host callback that starts one radio's GPIO and audio path.
///
/// # Safety
/// `context` must remain live until all radios have been destroyed, and `radio` must be the
/// unique prepared station handle. The provider set must remain loaded through destroy.
pub unsafe extern "C" fn radio_activate(
    context: *mut c_void,
    radio: *mut c_void,
    receive: crate::abi::rptadv_radio_receive_v3,
    receive_context: *mut c_void,
    transmit: crate::abi::rptadv_radio_transmit_v3,
    transmit_context: *mut c_void,
) -> i32 {
    let Some(host) = (unsafe { context.cast::<RadioHostContext>().as_ref() }) else {
        return -1;
    };
    let Some(handle) = (unsafe { radio.cast::<RadioHandle>().as_mut() }) else {
        return -1;
    };
    if handle.active.is_some() {
        return -1;
    }
    let Some(generation) = host.next_generation() else {
        return -1;
    };
    let result = crate::radio_activation::ActiveRadio::open_prepared(
        &handle.reservation.plan,
        generation,
        handle.reservation.maximum_frames,
        crate::radio_activation::ProductRadioCallbacks {
            receive,
            receive_context,
            transmit,
            transmit_context,
            metadata: handle.metadata,
            feedback: handle.feedback,
        },
    );
    finish_activation(handle, result)
}

fn finish_activation(
    handle: &mut RadioHandle,
    result: Result<
        crate::radio_activation::ActiveRadio,
        crate::radio_activation::RadioActivationError,
    >,
) -> i32 {
    match result {
        Ok(active) => {
            handle.active = Some(active);
            0
        }
        Err(error) => {
            eprintln!("rpt-advanced radio activation failed: {error}");
            if let crate::radio_activation::RadioActivationError::Retained { active, .. } = error {
                handle.active = Some(*active);
            }
            -1
        }
    }
}

/// Advance only the already-owned native control mailbox.
///
/// # Safety
/// `radio` is a live reservation serialized by the product control executor.
pub unsafe extern "C" fn radio_service(_: *mut c_void, radio: *mut c_void) -> i32 {
    // SAFETY: the caller retains the unique radio reservation through this call.
    let Some(handle) = (unsafe { radio.cast::<RadioHandle>().as_mut() }) else {
        return -1;
    };
    handle.active.as_mut().map_or(0, |active| active.service())
}

/// Observe initial remote admission; no hardware I/O runs on this caller.
///
/// # Safety
/// `radio` is a live reservation serialized by the product control executor.
pub unsafe extern "C" fn radio_ready(_: *mut c_void, radio: *mut c_void) -> i32 {
    // SAFETY: the product retains this unique reservation until checked retirement.
    let Some(handle) = (unsafe { radio.cast::<RadioHandle>().as_mut() }) else {
        return -1;
    };
    handle.active.as_mut().map_or(-7, |active| active.ready())
}

/// Poll retirement while preserving callbacks and the partially started owner.
///
/// # Safety
/// The caller retains `radio` and all callback contexts until this returns zero.
pub unsafe extern "C" fn radio_quiesce(_: *mut c_void, radio: *mut c_void) -> i32 {
    // SAFETY: same serialized reservation; this does not transfer its ownership.
    let Some(handle) = (unsafe { radio.cast::<RadioHandle>().as_mut() }) else {
        return -1;
    };
    handle.active.as_mut().map_or(0, |active| active.quiesce())
}

/// Submit copied control intent and transfer an independent SDK ticket.
///
/// # Safety
/// `radio` is live; operation/output are complete borrowed records with unique output.
pub unsafe extern "C" fn radio_remote_submit(
    _: *mut c_void,
    radio: *mut c_void,
    operation: *const crate::abi::UrpRemoteOperation,
    flags: u32,
    output: *mut crate::abi::rptadv_remote_ticket_v1,
) -> i32 {
    // SAFETY: the synchronous caller lends exclusively writable complete output storage.
    let Some(output) = (unsafe { output.as_mut() }) else {
        return -1;
    };
    // SAFETY: a zero record contains only valid empty scalar/pointer values.
    *output = unsafe { std::mem::zeroed() };
    // SAFETY: the retained reservation and borrowed input remain valid through dispatch.
    let (Some(handle), Some(operation)) =
        (unsafe { radio.cast::<RadioHandle>().as_mut() }, unsafe {
            operation.as_ref()
        })
    else {
        return -1;
    };
    handle
        .active
        .as_mut()
        .map_or(-7, |active| active.remote_submit(operation, flags, output))
}

/// Product host callback that releases an inactive radio reservation.
///
/// # Safety
/// A non-null `radio` must be the unique handle pointer previously returned by
/// the typed station binding and must not have been destroyed already.
pub unsafe extern "C" fn radio_destroy(_: *mut c_void, radio: *mut c_void) {
    if !radio.is_null() {
        drop(unsafe { Box::from_raw(radio.cast::<RadioHandle>()) });
    }
}

/// Confirm a peer is attached to a live standalone radio.
///
/// The portable product owns link mixing and already routes it through the product's transmit
/// callback; unlike the Asterisk adapter, standalone has no external channel-driver hook to set.
///
/// # Safety
/// `peer` and `radio` must refer to live handles from this host, and neither may be destroyed
/// during this call.
pub unsafe extern "C" fn peer_bind_radio(
    _: *mut c_void,
    peer: *mut c_void,
    radio: *mut c_void,
) -> i32 {
    let (Some(_peer), Some(radio)) = (unsafe {
        (
            (!peer.is_null()).then_some(()),
            radio.cast::<RadioHandle>().as_ref(),
        )
    }) else {
        return -1;
    };
    if radio.active.is_some() { 0 } else { -1 }
}
