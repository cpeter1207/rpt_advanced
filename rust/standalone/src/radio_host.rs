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
    }))
    .cast();
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
            -1
        }
    }
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
