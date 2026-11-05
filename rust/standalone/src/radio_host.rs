use crate::ResolvedRadioNode;
use std::{
    ffi::{c_char, c_void},
    str,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Native audio bound used by the existing Asterisk host.
pub const MAXIMUM_RADIO_FRAMES: usize = 4096;

/// A product radio request could not be mapped to one safe configured channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReservationError {
    /// No enabled node matches the requested channel or node number.
    UnknownRadio,
    /// The only matching node is disabled.
    Disabled,
    /// More than one configured node matches the name.
    AmbiguousRadio,
    /// The requested frame count is zero or exceeds the supported bound.
    InvalidFrameCount,
}

/// Resolved radio settings held while the product opens and activates a radio.
#[derive(Clone, Debug, PartialEq)]
pub struct RadioReservation {
    /// Node and hardware/signaling settings selected by the request.
    pub radio: ResolvedRadioNode,
    /// Largest native callback frame accepted by the product.
    pub maximum_frames: u32,
}

/// Process-lifetime data supplied to product radio callbacks.
pub struct RadioHostContext {
    /// All configured node radio selections.
    radios: Mutex<RadioSettings>,
    /// Dynamically loaded providers; absent only for callback validation tests.
    pub providers: Option<&'static crate::providers::ProviderSet>,
    next_generation: AtomicU64,
    pub(crate) observations: crate::processing::Observations,
}

#[derive(Default)]
/// Settings are read only while the serialized control path reserves/reopens hardware.
struct RadioSettings {
    active: Vec<ResolvedRadioNode>,
    staged: Option<StagedRadioSettings>,
}

struct StagedRadioSettings {
    candidate: Vec<ResolvedRadioNode>,
    attempted: Vec<String>,
}

impl RadioHostContext {
    /// Retain resolved node selections and the native runtime providers.
    pub fn new(
        radios: Vec<ResolvedRadioNode>,
        providers: Option<&'static crate::providers::ProviderSet>,
    ) -> Self {
        Self {
            radios: Mutex::new(RadioSettings {
                active: radios,
                staged: None,
            }),
            providers,
            next_generation: AtomicU64::new(0),
            observations: crate::processing::Observations::default(),
        }
    }

    /// Stage candidate radio settings for device handoff; the active snapshot remains available
    /// for rollback until the product confirms the configuration reload.
    pub fn stage_radios(&self, radios: Vec<ResolvedRadioNode>) -> bool {
        let mut state = self
            .radios
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.staged.is_some() {
            return false;
        }
        state.staged = Some(StagedRadioSettings {
            candidate: radios,
            attempted: Vec::new(),
        });
        true
    }

    /// Commit staged settings after every product/device handoff succeeds.
    pub fn commit_radios(&self) {
        let mut state = self
            .radios
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(staged) = state.staged.take() {
            state.active = staged.candidate;
        }
    }

    /// Discard candidate settings after a rejected reload; rollback opens use the active snapshot.
    pub fn discard_staged_radios(&self) {
        self.radios
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .staged = None;
    }

    fn update_reservation(
        &self,
        current: &RadioReservation,
    ) -> Result<RadioReservation, ReservationError> {
        let state = self
            .radios
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let radios = state
            .staged
            .as_ref()
            .map_or(&state.active, |staged| &staged.candidate);
        let candidate = reserve_radio(
            radios,
            current.radio.node.as_str(),
            current.maximum_frames as usize,
        )?;
        if candidate.radio.channel != current.radio.channel {
            return Err(ReservationError::UnknownRadio);
        }
        Ok(candidate)
    }

    fn reservation(
        &self,
        identity: &str,
        maximum_frames: usize,
    ) -> Result<RadioReservation, ReservationError> {
        let (name, channel) = identity
            .split_once('\0')
            .map_or((identity, None), |(node, channel)| (node, Some(channel)));
        if channel.is_some_and(str::is_empty) || name.is_empty() {
            return Err(ReservationError::UnknownRadio);
        }
        let mut state = self
            .radios
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(staged) = state.staged.as_mut() {
            if let Some(candidate) = staged
                .candidate
                .iter()
                .find(|radio| radio.node.as_str() == name)
            {
                if !staged
                    .attempted
                    .iter()
                    .any(|node| node == candidate.node.as_str())
                {
                    staged.attempted.push(candidate.node.as_str().to_owned());
                    if channel.is_some_and(|channel| channel != candidate.channel) {
                        return Err(ReservationError::UnknownRadio);
                    }
                    return reserve_radio(&[candidate.clone()], name, maximum_frames);
                }
            }
        }
        let active = reserve_radio(&state.active, name, maximum_frames)?;
        if channel.is_some_and(|channel| channel != active.radio.channel) {
            return Err(ReservationError::UnknownRadio);
        }
        Ok(active)
    }
}

struct RadioHandle {
    reservation: RadioReservation,
    active: Option<crate::radio_activation::ActiveRadio>,
    pending: Option<RadioReservation>,
    metadata: crate::abi::rptadv_radio_metadata_v1,
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
    let (Some(host), Some(handle)) = (host, handle) else {
        return -1;
    };
    let Some(active) = handle.active.as_mut() else {
        return -7;
    };
    match phase {
        0 => {
            if handle.pending.is_some() {
                return -5;
            }
            let Ok(candidate) = host.update_reservation(&handle.reservation) else {
                return -1;
            };
            let generation = host.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
            match active.stage_update(
                &candidate.radio.radio.request(),
                generation,
                candidate.maximum_frames,
            ) {
                Ok(true) => {
                    handle.pending = Some(candidate);
                    0
                }
                Ok(false) => 1,
                Err(error) => {
                    handle.pending = Some(candidate);
                    error
                }
            }
        }
        1 | 2 => {
            if handle.pending.is_none() {
                return if phase == 2 { 0 } else { -7 };
            }
            match active.finish_update(phase == 1) {
                Ok(()) => {
                    let candidate = handle.pending.take().expect("staged radio reservation");
                    if phase == 1 {
                        handle.reservation = candidate;
                    }
                    0
                }
                Err(error) => error,
            }
        }
        _ => -1,
    }
}

/// Dispatch typed maintenance through this radio's retained shared native owner.
///
/// # Safety
/// Handles and the initialized writable request remain live for this serialized
/// call. The optional sink borrows returned text only until its callback returns.
pub unsafe extern "C" fn radio_control(
    context: *mut c_void,
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
    // SAFETY: this boundary borrows the caller's live handle and complete request.
    let (handle, request) = unsafe { (radio.cast::<RadioHandle>().as_mut(), request.as_mut()) };
    let (Some(handle), Some(request)) = (handle, request) else {
        return -1;
    };
    let Some(active) = handle.active.as_mut() else {
        return -7;
    };
    if request.operation == 6 {
        let Some(sink) = sink else {
            return -1;
        };
        let source = handle.reservation.radio.radio.processing_request();
        // SAFETY: the reservation owns the complete source configuration while
        // the synchronous sink copies it. No normalized aliases replace source.
        unsafe {
            sink(
                sink_context,
                source.configuration.cast(),
                source.configuration_length as usize,
            )
        };
        return 0;
    }
    let processing = if request.operation == 5 {
        // SAFETY: this process-lifetime registry is retained by the host callback.
        let Some(host) = (unsafe { context.cast::<RadioHostContext>().as_ref() }) else {
            return -1;
        };
        let Some(sink) = sink else {
            return -1;
        };
        Some((host, sink))
    } else {
        None
    };
    let status = active.maintenance(request, sink, sink_context);
    if status == 0 {
        if let Some((host, sink)) = processing {
            let text = host
                .observations
                .text(handle.reservation.radio.node.as_str());
            // SAFETY: this complete borrowed text lasts through the synchronous sink.
            unsafe { sink(sink_context, text.as_ptr().cast(), text.len()) };
        }
    }
    status
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

/// Resolve a product radio name without opening audio or GPIO hardware.
pub fn reserve_radio(
    radios: &[ResolvedRadioNode],
    name: &str,
    maximum_frames: usize,
) -> Result<RadioReservation, ReservationError> {
    let maximum_frames = u32::try_from(maximum_frames)
        .ok()
        .filter(|frames| *frames > 0 && *frames as usize <= MAXIMUM_RADIO_FRAMES)
        .ok_or(ReservationError::InvalidFrameCount)?;
    let mut matches = radios
        .iter()
        .filter(|radio| radio.node.as_str() == name || radio.channel == name);
    let radio = matches.next().ok_or(ReservationError::UnknownRadio)?;
    if matches.next().is_some() {
        return Err(ReservationError::AmbiguousRadio);
    }
    if !radio.enabled {
        return Err(ReservationError::Disabled);
    }
    Ok(RadioReservation {
        radio: radio.clone(),
        maximum_frames,
    })
}

/// Product host callback that reserves a configured radio channel without opening hardware.
///
/// `context` points to a process-lifetime [`RadioHostContext`].
///
/// # Safety
/// The context and output pointers must be valid for this call. `name` must reference
/// `name_length` readable bytes, and the context must outlive all returned radio handles.
pub unsafe extern "C" fn radio_open(
    context: *mut c_void,
    name: *const c_char,
    name_length: usize,
    maximum_frames: usize,
    output: *mut *mut c_void,
) -> i32 {
    let Some(output) = (unsafe { output.as_mut() }) else {
        return -1;
    };
    *output = std::ptr::null_mut();
    let Some(host) = (unsafe { context.cast::<RadioHostContext>().as_ref() }) else {
        return -1;
    };
    if name.is_null() {
        return -1;
    }
    let name = unsafe { std::slice::from_raw_parts(name.cast::<u8>(), name_length) };
    let Ok(name) = str::from_utf8(name) else {
        return -1;
    };
    let Ok(reservation) = host.reservation(name, maximum_frames) else {
        return -1;
    };
    *output = Box::into_raw(Box::new(RadioHandle {
        reservation,
        active: None,
        pending: None,
        metadata: None,
    }))
    .cast();
    0
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
/// unique handle returned by [`radio_open`]. The provider set must remain loaded through destroy.
pub unsafe extern "C" fn radio_activate(
    context: *mut c_void,
    radio: *mut c_void,
    receive: crate::abi::rptadv_radio_receive_v2,
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
    let Some(providers) = host.providers.as_ref() else {
        return -1;
    };
    // SAFETY: HostServicesOwner borrows the provider box owned by ProductRuntime; product.stop
    // synchronously destroys this radio before either owner can be released.
    let providers: &'static crate::providers::ProviderSet = providers;
    let generation = host.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
    let result = crate::radio_activation::ActiveRadio::open(
        providers,
        &handle.reservation.radio,
        generation,
        handle.reservation.maximum_frames,
        crate::radio_activation::ProductRadioCallbacks {
            receive,
            receive_context,
            transmit,
            transmit_context,
            metadata: handle.metadata,
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
/// [`radio_open`] and must not have been destroyed already.
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

#[cfg(test)]
mod tests {
    use super::{
        RadioHandle, RadioHostContext, ReservationError, finish_activation, radio_activate,
        radio_destroy, radio_open, reserve_radio,
    };
    use crate::resolve_radio_nodes;

    #[test]
    fn maintenance_validates_the_request_prefix_and_returns_original_active_source() {
        unsafe extern "C" fn append(
            context: *mut std::ffi::c_void,
            text: *const std::ffi::c_char,
            length: usize,
        ) {
            // SAFETY: the synchronous caller owns the output and complete source span.
            unsafe { &mut *context.cast::<String>() }.push_str(
                std::str::from_utf8(unsafe {
                    std::slice::from_raw_parts(text.cast::<u8>(), length)
                })
                .unwrap(),
            );
        }
        let source = "[1000]\n; original comment\n[radio 1000]\nreceive_output_gain_db=-8\n";
        let radios = resolve_radio_nodes(source).unwrap();
        let mut handle = RadioHandle {
            reservation: reserve_radio(&radios, "1000", 960).unwrap(),
            active: None,
            pending: None,
            metadata: None,
        };
        // SAFETY: the request is a scalar-only ABI record initialized before use.
        let mut request: crate::abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
        request.struct_size = std::mem::size_of_val(&request) as u32;
        request.abi_version = 1;
        request.operation = 6;
        let mut output = String::new();
        let radio = std::ptr::from_mut(&mut handle).cast();
        // SAFETY: stack owners remain live and the callback copies each borrowed span.
        unsafe {
            assert_eq!(
                super::radio_control(
                    std::ptr::null_mut(),
                    radio,
                    &mut request,
                    Some(append),
                    std::ptr::from_mut(&mut output).cast()
                ),
                -7
            );
            handle.active = Some(crate::radio_activation::tests::empty_active_radio());
            assert_eq!(
                super::radio_control(
                    std::ptr::null_mut(),
                    radio,
                    &mut request,
                    Some(append),
                    std::ptr::from_mut(&mut output).cast()
                ),
                0
            );
            assert_eq!(output, source);
            for invalid in [0, request.struct_size - 1] {
                let mut header = [invalid, 1_u32];
                assert_eq!(
                    super::radio_control(
                        std::ptr::null_mut(),
                        radio,
                        header.as_mut_ptr().cast(),
                        None,
                        std::ptr::null_mut()
                    ),
                    -1
                );
            }
            assert_eq!(
                super::radio_control(
                    std::ptr::null_mut(),
                    radio,
                    &mut request,
                    None,
                    std::ptr::null_mut()
                ),
                -1
            );
            request.abi_version = 2;
            assert_eq!(
                super::radio_control(
                    std::ptr::null_mut(),
                    radio,
                    &mut request,
                    None,
                    std::ptr::null_mut()
                ),
                -1
            );
        }
    }

    #[test]
    fn radio_update_keeps_active_source_until_commit_and_does_not_consume_fallback_open() {
        use std::ffi::c_void;
        thread_local! {
            static REJECT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
            static FINISH_STATUS: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
        }
        unsafe extern "C" fn prepare(
            _: *mut c_void,
            _: *const crate::abi::UrpNativeCreateArgs,
            output: *mut *mut c_void,
        ) -> i32 {
            // SAFETY: the caller owns this writable output; token is an opaque fixture address.
            unsafe { *output = std::ptr::NonNull::<u8>::dangling().as_ptr().cast() };
            0
        }
        unsafe extern "C" fn begin(_: *mut c_void, _: *mut c_void) -> i32 {
            if REJECT.get() { -9 } else { 0 }
        }
        unsafe extern "C" fn accept(_: *mut c_void) -> i32 {
            0
        }
        unsafe extern "C" fn finish(_: *mut c_void, _: u32) -> i32 {
            FINISH_STATUS.get()
        }
        unsafe extern "C" fn destroy(_: *mut c_void) {}
        unsafe extern "C" fn requires_handoff(
            _: *mut c_void,
            _: *const crate::abi::UrpNativeCreateArgs,
            output: *mut *mut c_void,
        ) -> i32 {
            // SAFETY: structural incompatibility transfers no prepared owner.
            unsafe { *output = std::ptr::null_mut() };
            -3
        }
        let old = "[1000]\n[radio 1000]\nreceive_output_gain_db=-8\n";
        let next = "[1000]\n[radio 1000]\nreceive_output_gain_db=-3\n";
        let radios = resolve_radio_nodes(old).unwrap();
        let mut host = RadioHostContext::new(radios.clone(), None);
        assert!(host.stage_radios(resolve_radio_nodes(next).unwrap()));
        // SAFETY: this descriptor contains only scalar metadata and nullable pointers.
        let mut api: crate::abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
        api.native_prepare_update = Some(requires_handoff);
        api.native_begin_update = Some(begin);
        api.native_accept_update = Some(accept);
        api.native_finish_update = Some(finish);
        api.native_destroy_update = Some(destroy);
        let mut handle = RadioHandle {
            reservation: reserve_radio(&radios, "1000", 960).unwrap(),
            active: Some(crate::radio_activation::tests::fixture_active_radio(api)),
            pending: None,
            metadata: None,
        };
        let context = std::ptr::from_mut(&mut host).cast();
        let radio = std::ptr::from_mut(&mut handle).cast();
        // SAFETY: malformed ownership is rejected without dereferencing absent owners.
        unsafe {
            assert_eq!(super::radio_update(std::ptr::null_mut(), radio, 0), -1);
            assert_eq!(super::radio_update(context, std::ptr::null_mut(), 0), -1);
            let active = handle.active.take();
            assert_eq!(super::radio_update(context, radio, 0), -7);
            handle.active = active;
        }
        for source in ["[2000]\n", "[1000]\nradio_channel=different\n"] {
            let mut other = RadioHostContext::new(resolve_radio_nodes(source).unwrap(), None);
            // SAFETY: a different configured reservation cannot replace this live identity.
            assert_eq!(
                unsafe { super::radio_update(std::ptr::from_mut(&mut other).cast(), radio, 0) },
                -1
            );
        }
        // SAFETY: these retained stack owners remain serially controlled throughout the test.
        unsafe { assert_eq!(super::radio_update(context, radio, 0), 1) };
        assert_eq!(
            host.reservation("1000", 960).unwrap().radio,
            resolve_radio_nodes(next).unwrap().remove(0)
        );
        api.native_prepare_update = Some(prepare);
        api.native_begin_update = Some(begin);
        api.native_accept_update = Some(accept);
        api.native_finish_update = Some(finish);
        api.native_destroy_update = Some(destroy);
        handle.active = Some(crate::radio_activation::tests::fixture_active_radio(api));
        for commit in [false, true] {
            // SAFETY: the fixture retains all request and native-owner addresses.
            unsafe {
                assert_eq!(super::radio_update(context, radio, 0), 0);
                assert_eq!(handle.reservation.radio, radios[0]);
                assert_eq!(super::radio_update(context, radio, 0), -5);
                FINISH_STATUS.set(-9);
                assert_eq!(super::radio_update(context, radio, 2), -9);
                assert!(handle.pending.is_some());
                FINISH_STATUS.set(0);
                assert_eq!(
                    super::radio_update(context, radio, if commit { 1 } else { 2 }),
                    0
                );
            }
            let expected = if commit {
                resolve_radio_nodes(next).unwrap().remove(0)
            } else {
                radios[0].clone()
            };
            assert_eq!(handle.reservation.radio, expected);
            assert!(handle.pending.is_none());
        }
        handle.reservation = reserve_radio(&radios, "1000", 960).unwrap();
        REJECT.set(true);
        // SAFETY: rejected stage retains the exact previous source and abort owner.
        unsafe {
            assert_eq!(super::radio_update(context, radio, 0), -9);
            assert!(handle.pending.is_some());
            assert_eq!(handle.reservation.radio, radios[0]);
            assert_eq!(super::radio_update(context, radio, 2), 0);
            assert!(handle.pending.is_none());
            assert_eq!(super::radio_update(context, radio, 2), 0);
            assert_eq!(super::radio_update(context, radio, 1), -7);
            assert_eq!(super::radio_update(context, radio, 3), -1);
        }
    }

    #[test]
    fn processing_status_uses_the_retained_registry_and_rejects_invalid_owners_before_collection() {
        use std::ffi::{c_char, c_void};
        thread_local! {
            static STATUS: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
            static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        }
        unsafe extern "C" fn counters(
            _: *mut c_void,
            sink: crate::abi::UrpNativeProcessingDiagnostic,
            context: *mut c_void,
        ) -> i32 {
            READS.set(READS.get() + 1);
            let bytes = b"radio_receive_calls=7\n";
            // SAFETY: the bridge retains this synchronous borrowed-span callback.
            unsafe { sink.unwrap()(context, bytes.as_ptr(), bytes.len() as u32) };
            STATUS.get()
        }
        unsafe extern "C" fn append(context: *mut c_void, text: *const c_char, length: usize) {
            // SAFETY: the caller retains the destination and the complete fixture text span.
            unsafe { &mut *context.cast::<String>() }.push_str(
                std::str::from_utf8(unsafe {
                    std::slice::from_raw_parts(text.cast::<u8>(), length)
                })
                .unwrap(),
            );
        }
        let radios = resolve_radio_nodes("[1000]\n").unwrap();
        let mut host = RadioHostContext::new(radios.clone(), None);
        // SAFETY: this descriptor contains only scalar metadata and nullable operations.
        let mut api: crate::abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
        api.native_processing_status_text = Some(counters);
        api.native_status_text = Some(counters);
        let mut handle = RadioHandle {
            reservation: reserve_radio(&radios, "1000", 960).unwrap(),
            active: Some(crate::radio_activation::tests::fixture_active_radio(api)),
            pending: None,
            metadata: None,
        };
        // SAFETY: the request is a scalar-only ABI record initialized before dispatch.
        let mut request: crate::abi::rptadv_radio_control_v1 = unsafe { std::mem::zeroed() };
        request.struct_size = std::mem::size_of_val(&request) as u32;
        request.abi_version = 1;
        request.operation = 5;
        let context = std::ptr::from_mut(&mut host).cast();
        let radio = std::ptr::from_mut(&mut handle).cast();
        let mut output = String::new();
        let sink_context = std::ptr::from_mut(&mut output).cast();
        // SAFETY: stack owners remain retained for all synchronous validation/dispatch calls.
        unsafe {
            assert_eq!(
                super::radio_control(
                    context,
                    radio,
                    std::ptr::null_mut(),
                    Some(append),
                    sink_context
                ),
                -1
            );
            assert_eq!(
                super::radio_control(
                    context,
                    std::ptr::null_mut(),
                    &mut request,
                    Some(append),
                    sink_context
                ),
                -1
            );
            assert_eq!(
                super::radio_control(
                    std::ptr::null_mut(),
                    radio,
                    &mut request,
                    Some(append),
                    sink_context
                ),
                -1
            );
            assert_eq!(
                super::radio_control(context, radio, &mut request, None, sink_context),
                -1
            );
            assert_eq!(READS.get(), 0);
            assert_eq!(
                super::radio_control(context, radio, &mut request, Some(append), sink_context),
                0
            );
            assert_eq!(
                output,
                "radio_receive_calls=7\nlink_processors_retained=0\n"
            );
            output.clear();
            STATUS.set(-9);
            assert_eq!(
                super::radio_control(context, radio, &mut request, Some(append), sink_context),
                -9
            );
            assert_eq!(output, "radio_receive_calls=7\n");
            STATUS.set(0);
            request.operation = 1;
            assert_eq!(
                super::radio_control(context, radio, &mut request, Some(append), sink_context),
                0
            );
        }
    }

    #[test]
    fn site_bridge_rejects_null_or_inactive_handles_without_opening_hardware() {
        use super::{radio_site_bind, radio_site_publish, radio_site_snapshot};
        let radios = resolve_radio_nodes("[1000]\n[radio 1000]\n").unwrap();
        let mut handle = RadioHandle {
            reservation: reserve_radio(&radios, "1000", 960).unwrap(),
            active: None,
            pending: None,
            metadata: None,
        };
        let binding: crate::abi::UrpAstSiteBinding = unsafe { std::mem::zeroed() };
        let request: crate::abi::UrpAstSiteRequest = unsafe { std::mem::zeroed() };
        let mut snapshot: crate::abi::UrpAstSiteSnapshot = unsafe { std::mem::zeroed() };
        snapshot.generation = 123;
        for active in [false, true] {
            handle.active = active.then(crate::radio_activation::tests::empty_active_radio);
            let radio = std::ptr::from_mut(&mut handle).cast();
            for radio in [std::ptr::null_mut(), radio] {
                // SAFETY: the stack owner and records stay live; null inputs test rejection.
                unsafe {
                    for input in [std::ptr::null(), &raw const binding] {
                        assert_eq!(radio_site_bind(std::ptr::null_mut(), radio, 0, input), -1);
                    }
                    for input in [std::ptr::null(), &raw const request] {
                        assert_eq!(
                            radio_site_publish(std::ptr::null_mut(), radio, 0, input),
                            -1
                        );
                    }
                    for output in [std::ptr::null_mut(), &raw mut snapshot] {
                        assert_eq!(
                            radio_site_snapshot(std::ptr::null_mut(), radio, 0, output),
                            -1
                        );
                    }
                }
                assert_eq!(snapshot.generation, 123);
            }
        }
    }

    #[test]
    fn reserves_an_enabled_radio_by_channel_or_node_without_opening_hardware() {
        let document = "[1000]\nradio_channel=vhf\n[radio 1000]\n[2000]\nradio_channel=uhf\nnode_enabled=no\n[radio 2000]\n";
        let radios = resolve_radio_nodes(document).unwrap();

        let by_channel = reserve_radio(&radios, "vhf", 960).unwrap();
        assert_eq!(by_channel.radio.node.as_str(), "1000");
        assert_eq!(by_channel.maximum_frames, 960);
        let by_node = reserve_radio(&radios, "1000", 480).unwrap();
        assert_eq!(by_node.radio.node.as_str(), "1000");
        assert_eq!(by_node.maximum_frames, 480);
        assert_eq!(
            reserve_radio(&radios, "uhf", 960),
            Err(ReservationError::Disabled)
        );
    }

    #[test]
    fn rejects_unknown_ambiguous_and_out_of_range_radio_reservations() {
        let document = "[1000]\nradio_channel=shared\n[radio 1000]\n[2000]\nradio_channel=shared\n[radio 2000]\n";
        let radios = resolve_radio_nodes(document).unwrap();

        assert_eq!(
            reserve_radio(&radios, "missing", 960),
            Err(ReservationError::UnknownRadio)
        );
        assert_eq!(
            reserve_radio(&radios, "shared", 960),
            Err(ReservationError::AmbiguousRadio)
        );
        assert_eq!(
            reserve_radio(&radios, "1000", 0),
            Err(ReservationError::InvalidFrameCount)
        );
        assert_eq!(
            reserve_radio(&radios, "1000", 4097),
            Err(ReservationError::InvalidFrameCount)
        );
    }

    #[test]
    fn staged_radio_reservations_are_attempted_once_and_validate_channel_names() {
        let initial = "[1000]\nradio_channel=vhf\n[2000]\nradio_channel=uhf\n";
        let candidate = "[1000]\nradio_channel=uhf\n";
        let host = RadioHostContext::new(resolve_radio_nodes(initial).unwrap(), None);
        assert!(host.stage_radios(resolve_radio_nodes(candidate).unwrap()));

        let selected = host.reservation("1000", 960).unwrap();
        assert_eq!(selected.radio.channel, "uhf");
        assert_eq!(host.reservation("2000", 960).unwrap().radio.channel, "uhf");
        assert_eq!(
            host.reservation("1000\0wrong", 960),
            Err(ReservationError::UnknownRadio)
        );
        assert_eq!(
            host.reservation("1000\0uhf", 960),
            Err(ReservationError::UnknownRadio)
        );
        assert_eq!(
            host.reservation("1000\0", 960),
            Err(ReservationError::UnknownRadio)
        );
        assert_eq!(
            host.reservation("", 960),
            Err(ReservationError::UnknownRadio)
        );

        let mismatch = RadioHostContext::new(resolve_radio_nodes(initial).unwrap(), None);
        assert!(mismatch.stage_radios(resolve_radio_nodes(candidate).unwrap()));
        assert_eq!(
            mismatch.reservation("1000\0wrong", 960),
            Err(ReservationError::UnknownRadio)
        );
    }

    #[test]
    fn radio_settings_stage_commit_and_discard_have_single_candidate_semantics() {
        let initial = "[1000]\nradio_channel=vhf\n";
        let candidate = "[1000]\nradio_channel=uhf\n";
        let host = RadioHostContext::new(resolve_radio_nodes(initial).unwrap(), None);
        host.commit_radios();
        host.discard_staged_radios();

        assert!(host.stage_radios(resolve_radio_nodes(candidate).unwrap()));
        assert!(!host.stage_radios(Vec::new()));
        host.discard_staged_radios();
        assert_eq!(host.reservation("1000", 960).unwrap().radio.channel, "vhf");

        assert!(host.stage_radios(resolve_radio_nodes(candidate).unwrap()));
        host.commit_radios();
        assert_eq!(host.reservation("1000", 960).unwrap().radio.channel, "uhf");
    }

    #[test]
    fn product_open_callback_returns_a_reservation_and_destroy_releases_it() {
        use std::ffi::c_void;

        let document = "[1000]\nradio_channel=vhf\n";
        let host = RadioHostContext::new(resolve_radio_nodes(document).unwrap(), None);
        let mut reservation: *mut c_void = std::ptr::null_mut();
        let context = std::ptr::from_ref(&host).cast_mut().cast::<c_void>();

        let result =
            unsafe { radio_open(context, b"vhf".as_ptr().cast(), 3, 960, &mut reservation) };

        assert_eq!(result, 0);
        assert!(!reservation.is_null());
        let handle = unsafe { &*reservation.cast::<RadioHandle>() };
        assert_eq!(handle.reservation.radio.node.as_str(), "1000");
        assert_eq!(handle.reservation.maximum_frames, 960);
        unsafe { radio_destroy(context, reservation) };
    }

    #[test]
    fn product_open_rejects_invalid_pointers_names_utf8_and_reservations() {
        use std::ffi::c_void;

        let document = "[1000]\nradio_channel=vhf\n";
        let host = RadioHostContext::new(resolve_radio_nodes(document).unwrap(), None);
        let context = std::ptr::from_ref(&host).cast_mut().cast::<c_void>();
        let mut handle = 1_usize as *mut c_void;

        assert_eq!(
            unsafe { radio_open(context, std::ptr::null(), 0, 960, &mut handle) },
            -1
        );
        assert!(handle.is_null());
        assert_eq!(
            unsafe { radio_open(context, [0xff].as_ptr().cast(), 1, 960, &mut handle) },
            -1
        );
        assert_eq!(
            unsafe { radio_open(context, b"".as_ptr().cast(), 0, 960, &mut handle) },
            -1
        );
        assert_eq!(
            unsafe { radio_open(context, b"missing".as_ptr().cast(), 7, 960, &mut handle) },
            -1
        );
        assert_eq!(
            unsafe { radio_open(context, b"vhf".as_ptr().cast(), 3, 0, &mut handle) },
            -1
        );
        assert_eq!(
            unsafe { radio_open(context, b"vhf".as_ptr().cast(), 3, 960, &mut handle) },
            0
        );
        unsafe { radio_destroy(context, handle) };
        assert_eq!(
            unsafe {
                radio_open(
                    std::ptr::null_mut(),
                    b"vhf".as_ptr().cast(),
                    3,
                    960,
                    &mut handle,
                )
            },
            -1
        );
        assert!(handle.is_null());
        assert_eq!(
            unsafe {
                radio_open(
                    context,
                    b"vhf".as_ptr().cast(),
                    3,
                    960,
                    std::ptr::null_mut(),
                )
            },
            -1
        );
    }

    #[test]
    fn destroy_accepts_a_null_handle_and_peer_bind_requires_a_live_radio() {
        use std::ffi::c_void;

        unsafe { radio_destroy(std::ptr::null_mut(), std::ptr::null_mut()) };
        let document = "[1000]\nradio_channel=vhf\n";
        let host = RadioHostContext::new(resolve_radio_nodes(document).unwrap(), None);
        let mut handle = std::ptr::null_mut();
        let context = std::ptr::from_ref(&host).cast_mut().cast::<c_void>();
        assert_eq!(
            unsafe { radio_open(context, b"vhf".as_ptr().cast(), 3, 960, &mut handle) },
            0
        );
        assert_eq!(
            unsafe { super::peer_bind_radio(context, std::ptr::null_mut(), handle) },
            -1
        );
        assert_eq!(
            unsafe {
                super::peer_bind_radio(context, 1_usize as *mut c_void, std::ptr::null_mut())
            },
            -1
        );
        assert_eq!(
            unsafe { super::peer_bind_radio(context, 1_usize as *mut c_void, handle) },
            -1
        );
        unsafe { radio_destroy(context, handle) };
    }

    #[test]
    fn updated_radio_settings_are_used_by_candidate_and_committed_after_reload() {
        use std::ffi::c_void;

        let initial = "[1000]\n[radio 1000]\ncm119_profile=sphusb\n";
        let updated = "[1000]\n[radio 1000]\ncm119_profile=nhrc\n";
        let host = RadioHostContext::new(resolve_radio_nodes(initial).unwrap(), None);
        assert!(host.stage_radios(resolve_radio_nodes(updated).unwrap()));
        let context = std::ptr::from_ref(&host).cast_mut().cast::<c_void>();
        let mut reservation = std::ptr::null_mut();

        assert_eq!(
            unsafe { radio_open(context, b"1000".as_ptr().cast(), 4, 960, &mut reservation) },
            0
        );
        let handle = unsafe { &*reservation.cast::<RadioHandle>() };
        assert_eq!(
            handle.reservation.radio.radio.request().cm119_profile,
            crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_NHRC
        );
        unsafe { radio_destroy(context, reservation) };
        host.commit_radios();
        let mut reservation = std::ptr::null_mut();
        assert_eq!(
            unsafe { radio_open(context, b"1000".as_ptr().cast(), 4, 960, &mut reservation) },
            0
        );
        let handle = unsafe { &*reservation.cast::<RadioHandle>() };
        assert_eq!(
            handle.reservation.radio.radio.request().cm119_profile,
            crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_NHRC
        );
        unsafe { radio_destroy(context, reservation) };
    }

    #[test]
    fn failed_candidate_handoff_reopens_with_active_radio_settings() {
        use std::ffi::c_void;

        let initial = "[1000]\n[radio 1000]\ncm119_profile=sphusb\n";
        let updated = "[1000]\n[radio 1000]\ncm119_profile=nhrc\n";
        let host = RadioHostContext::new(resolve_radio_nodes(initial).unwrap(), None);
        assert!(host.stage_radios(resolve_radio_nodes(updated).unwrap()));
        let context = std::ptr::from_ref(&host).cast_mut().cast::<c_void>();
        let mut candidate = std::ptr::null_mut();
        assert_eq!(
            unsafe { radio_open(context, b"1000".as_ptr().cast(), 4, 960, &mut candidate) },
            0
        );
        assert_eq!(
            unsafe { &*candidate.cast::<RadioHandle>() }
                .reservation
                .radio
                .radio
                .request()
                .cm119_profile,
            crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_NHRC
        );
        unsafe { radio_destroy(context, candidate) };

        // Product's failed candidate open triggers a rollback open before reload returns.
        let mut rollback = std::ptr::null_mut();
        assert_eq!(
            unsafe { radio_open(context, b"1000".as_ptr().cast(), 4, 960, &mut rollback) },
            0
        );
        assert_eq!(
            unsafe { &*rollback.cast::<RadioHandle>() }
                .reservation
                .radio
                .radio
                .request()
                .cm119_profile,
            crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_SPHUSB
        );
        unsafe { radio_destroy(context, rollback) };
        host.discard_staged_radios();
    }

    #[test]
    fn rollback_uses_node_identity_when_channel_changes() {
        use std::ffi::c_void;

        let initial = "[1000]\nradio_channel=old\n[radio 1000]\ncm119_profile=sphusb\n";
        let updated = "[1000]\nradio_channel=new\n[radio 1000]\ncm119_profile=nhrc\n";
        let host = RadioHostContext::new(resolve_radio_nodes(initial).unwrap(), None);
        assert!(host.stage_radios(resolve_radio_nodes(updated).unwrap()));
        let context = std::ptr::from_ref(&host).cast_mut().cast::<c_void>();
        let mut candidate = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                radio_open(
                    context,
                    b"1000\0new".as_ptr().cast(),
                    8,
                    960,
                    &mut candidate,
                )
            },
            0
        );
        assert_eq!(
            unsafe { &*candidate.cast::<RadioHandle>() }
                .reservation
                .radio
                .radio
                .request()
                .cm119_profile,
            crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_NHRC
        );
        unsafe { radio_destroy(context, candidate) };

        let mut rollback = std::ptr::null_mut();
        assert_eq!(
            unsafe { radio_open(context, b"1000\0old".as_ptr().cast(), 8, 960, &mut rollback) },
            0
        );
        let handle = unsafe { &*rollback.cast::<RadioHandle>() };
        assert_eq!(handle.reservation.radio.channel, "old");
        assert_eq!(
            handle.reservation.radio.radio.request().cm119_profile,
            crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_SPHUSB
        );
        unsafe { radio_destroy(context, rollback) };
        host.discard_staged_radios();
    }

    #[test]
    fn activation_without_providers_fails_closed_and_preserves_reservation() {
        use std::ffi::c_void;

        unsafe extern "C" fn receive(_: *mut c_void, _: u32, _: *mut f32, _: u32) -> i32 {
            0
        }
        unsafe extern "C" fn transmit(
            _: *mut c_void,
            _: *mut f32,
            _: u32,
            _: *mut u32,
            _: *mut u32,
        ) -> i32 {
            0
        }

        let document = "[1000]\nradio_channel=vhf\n";
        let host = RadioHostContext::new(resolve_radio_nodes(document).unwrap(), None);
        let context = std::ptr::from_ref(&host).cast_mut().cast::<c_void>();
        let mut handle = std::ptr::null_mut();
        assert_eq!(
            unsafe { radio_open(context, b"vhf".as_ptr().cast(), 3, 960, &mut handle) },
            0
        );

        assert_eq!(
            unsafe {
                radio_activate(
                    context,
                    handle,
                    Some(receive),
                    std::ptr::null_mut(),
                    Some(transmit),
                    std::ptr::null_mut(),
                )
            },
            -1
        );
        assert!(unsafe { &*handle.cast::<RadioHandle>() }.active.is_none());
        unsafe { radio_destroy(context, handle) };
    }

    #[test]
    fn radio_activation_rejects_invalid_contexts_handles_and_duplicate_activation() {
        use std::ffi::c_void;

        unsafe extern "C" fn receive(_: *mut c_void, _: u32, _: *mut f32, _: u32) -> i32 {
            0
        }
        unsafe extern "C" fn transmit(
            _: *mut c_void,
            _: *mut f32,
            _: u32,
            _: *mut u32,
            _: *mut u32,
        ) -> i32 {
            0
        }

        let document = "[1000]\n";
        let host = RadioHostContext::new(resolve_radio_nodes(document).unwrap(), None);
        let context = std::ptr::from_ref(&host).cast_mut().cast::<c_void>();
        let empty = || unsafe {
            radio_activate(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(empty(), -1);
        assert_eq!(
            unsafe {
                radio_activate(
                    context,
                    std::ptr::null_mut(),
                    None,
                    std::ptr::null_mut(),
                    None,
                    std::ptr::null_mut(),
                )
            },
            -1
        );

        let mut reservation = std::ptr::null_mut();
        assert_eq!(
            unsafe { radio_open(context, b"1000".as_ptr().cast(), 4, 960, &mut reservation) },
            0
        );
        unsafe { &mut *reservation.cast::<RadioHandle>() }.active =
            Some(crate::radio_activation::tests::empty_active_radio());
        assert_eq!(
            unsafe {
                radio_activate(
                    context,
                    reservation,
                    Some(receive),
                    std::ptr::null_mut(),
                    Some(transmit),
                    std::ptr::null_mut(),
                )
            },
            -1
        );
        unsafe { radio_destroy(context, reservation) };
    }

    #[test]
    fn activation_completion_commits_only_successful_radio_ownership() {
        let document = "[1000]\n";
        let reservation =
            reserve_radio(&resolve_radio_nodes(document).unwrap(), "1000", 960).unwrap();
        let mut handle = RadioHandle {
            reservation,
            active: None,
            pending: None,
            metadata: None,
        };

        assert_eq!(
            finish_activation(
                &mut handle,
                Ok(crate::radio_activation::tests::empty_active_radio())
            ),
            0
        );
        assert!(handle.active.is_some());
        assert_eq!(
            unsafe {
                super::peer_bind_radio(
                    std::ptr::null_mut(),
                    1_usize as *mut std::ffi::c_void,
                    std::ptr::from_mut(&mut handle).cast(),
                )
            },
            0
        );

        let mut failed = RadioHandle {
            reservation: handle.reservation.clone(),
            active: None,
            pending: None,
            metadata: None,
        };
        assert_eq!(
            finish_activation(
                &mut failed,
                Err(crate::radio_activation::RadioActivationError::InvalidCallbacks)
            ),
            -1
        );
        assert!(failed.active.is_none());
    }

    #[test]
    fn provider_backed_activation_fails_safely_without_cm119_hardware() {
        use std::ffi::c_void;

        unsafe extern "C" fn receive(_: *mut c_void, _: u32, _: *mut f32, _: u32) -> i32 {
            0
        }
        unsafe extern "C" fn transmit(
            _: *mut c_void,
            _: *mut f32,
            _: u32,
            _: *mut u32,
            _: *mut u32,
        ) -> i32 {
            0
        }

        let providers = Box::leak(Box::new(crate::providers::ProviderSet::load().unwrap()));
        let document = "[1000]\n";
        let host = RadioHostContext::new(resolve_radio_nodes(document).unwrap(), Some(providers));
        let context = std::ptr::from_ref(&host).cast_mut().cast::<c_void>();
        let mut reservation = std::ptr::null_mut();
        assert_eq!(
            unsafe { radio_open(context, b"1000".as_ptr().cast(), 4, 960, &mut reservation) },
            0
        );
        assert_eq!(
            unsafe {
                radio_activate(
                    context,
                    reservation,
                    Some(receive),
                    std::ptr::null_mut(),
                    Some(transmit),
                    std::ptr::null_mut(),
                )
            },
            -1
        );
        assert!(
            unsafe { &*reservation.cast::<RadioHandle>() }
                .active
                .is_none()
        );
        unsafe { radio_destroy(context, reservation) };
    }
}
