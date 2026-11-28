//! Dormant typed station plans and serialized physical ownership.
use super::{MAXIMUM_RADIO_FRAMES, PortLease, RadioHandle, Selection, StationPlan};
use crate::{abi, host_services::StandaloneHostContext};
use std::{
    collections::HashSet,
    ffi::c_void,
    ptr,
    sync::{Arc, Mutex},
};

pub(crate) struct Operations(pub(crate) abi::rptadv_station_binding_v1);
// SAFETY: all entries and the capability string have static lifetime. Each
// callback uses the host context and the individually serialized handle passed to it.
unsafe impl Sync for Operations {}

pub(crate) static OPERATIONS: Operations = Operations(abi::rptadv_station_binding_v1 {
    struct_size: size_of::<abi::rptadv_station_binding_v1>() as u32,
    abi_version: 1,
    capability_name: c"rptadv.station-binding1".as_ptr(),
    prepare: Some(prepare),
    open_prepared: Some(open),
    stage_update: Some(stage),
    release_plan: Some(release),
});

struct Binding {
    ports: Arc<Mutex<HashSet<[u8; 16]>>>,
    plan: Arc<StationPlan>,
}

unsafe extern "C" fn prepare(
    context: *mut c_void,
    request: *const abi::rptadv_station_request_v1,
    out: *mut *mut c_void,
) -> i32 {
    // SAFETY: the ABI supplies a writable output and at least a size word in a non-null request.
    let Some(out) = (unsafe { out.as_mut() }) else {
        return -1;
    };
    *out = ptr::null_mut();
    // SAFETY: the host owns the context through completion of all product calls.
    let Some(host) = (unsafe { context.cast::<StandaloneHostContext>().as_ref() }) else {
        return -1;
    };
    if request.is_null()
        || !request.is_aligned()
        // SAFETY: this aligned, non-null ABI pointer supplies at least the leading size word.
        || (unsafe { request.cast::<u32>().read() } as usize)
            < size_of::<abi::rptadv_station_request_v1>()
    {
        return -1;
    }
    // SAFETY: the complete readable request size was checked before borrowing its tail.
    let request = unsafe { &*request };
    if request.abi_version != 1 {
        return -2;
    }
    if request.product.is_null() || request.prepared.is_null() || request.candidate_identity == 0 {
        return -1;
    }
    let Ok(providers) = host.radio.station_providers() else {
        return -2;
    };
    let plan = match StationPlan::prepare(providers, request) {
        Ok(value) => value,
        Err(status) => return status,
    };
    *out = Box::into_raw(Box::new(Binding {
        ports: host.radio.typed_ports.clone(),
        plan: Arc::new(plan),
    }))
    .cast();
    0
}

unsafe extern "C" fn open(
    context: *mut c_void,
    plan: *const c_void,
    maximum: usize,
    out: *mut *mut c_void,
) -> i32 {
    // SAFETY: a non-null output is uniquely writable during this call.
    let Some(out) = (unsafe { out.as_mut() }) else {
        return -1;
    };
    *out = ptr::null_mut();
    // SAFETY: the product borrows its retained context and dormant plan for this call.
    let (Some(host), Some(binding)) = (unsafe {
        (
            context.cast::<StandaloneHostContext>().as_ref(),
            plan.cast::<Binding>().as_ref(),
        )
    }) else {
        return -1;
    };
    if !Arc::ptr_eq(&host.radio.typed_ports, &binding.ports)
        || !(1..=MAXIMUM_RADIO_FRAMES).contains(&maximum)
    {
        return -1;
    }
    let port = binding.plan.port;
    if !binding
        .ports
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(port)
    {
        return -5;
    }
    let lease = Arc::new(PortLease {
        registry: binding.ports.clone(),
        port,
    });
    let reservation = Selection {
        plan: binding.plan.clone(),
        maximum_frames: maximum as u32,
        lease,
    };
    *out = Box::into_raw(Box::new(RadioHandle {
        reservation,
        active: None,
        pending: None,
        metadata: None,
        feedback: None,
    }))
    .cast();
    0
}

unsafe extern "C" fn stage(context: *mut c_void, radio: *mut c_void, plan: *const c_void) -> i32 {
    // SAFETY: the product serializes the unique radio, and retains the plan throughout the call.
    let (Some(host), Some(handle), Some(binding)) = (unsafe {
        (
            context.cast::<StandaloneHostContext>().as_ref(),
            radio.cast::<RadioHandle>().as_mut(),
            plan.cast::<Binding>().as_ref(),
        )
    }) else {
        return -1;
    };
    let Selection {
        plan: current,
        maximum_frames,
        lease,
    } = &handle.reservation;
    if !Arc::ptr_eq(&host.radio.typed_ports, &binding.ports)
        || !Arc::ptr_eq(&binding.ports, &lease.registry)
        || current.port != binding.plan.port
        || current.candidate == binding.plan.candidate
    {
        return -1;
    }
    let Some(active) = handle.active.as_mut() else {
        return -7;
    };
    if let Some(pending) = &handle.pending {
        return if Arc::ptr_eq(&pending.plan, &binding.plan) {
            active.poll_typed_stage()
        } else {
            -5
        };
    }
    let Some(generation) = host.radio.next_generation() else {
        return -1;
    };
    let status = active.stage_typed(&binding.plan, generation, *maximum_frames);
    if active.has_typed_update() {
        handle.pending = Some(Selection {
            plan: binding.plan.clone(),
            maximum_frames: *maximum_frames,
            lease: lease.clone(),
        });
    }
    status
}

unsafe extern "C" fn release(_context: *mut c_void, plan: *mut c_void) {
    if !plan.is_null() {
        // SAFETY: prepare transferred this unique wrapper; live radios have independent Arc owners.
        drop(unsafe { Box::from_raw(plan.cast::<Binding>()) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_services::HostServicesOwner;
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Scenario {
        Dormant,
        Ticket,
        Pending,
        FailedPrepare,
        FailedBegin,
    }
    #[test]
    fn standalone_dormant_plans_survive_source_release_and_open_only_their_own_ports() {
        exercise(Scenario::Dormant);
    }
    #[test]
    fn standalone_station_ticket_retains_lazy_group_after_quiesced_radio_destroy() {
        exercise(Scenario::Ticket);
    }
    #[test]
    fn standalone_station_pending_stage_and_finish_yield_and_reuse_one_transaction() {
        exercise(Scenario::Pending);
    }
    #[test]
    fn standalone_station_failed_prepare_keeps_previous_policy_and_refuses_commit() {
        exercise(Scenario::FailedPrepare);
    }

    #[test]
    fn standalone_station_partial_begin_retains_recovery_without_exposing_candidate() {
        exercise(Scenario::FailedBegin);
    }

    use std::cell::Cell;
    thread_local! {
        static PREPARE_STATUS: Cell<i32> = const { Cell::new(0) };
        static ACCEPT_STATUS: Cell<i32> = const { Cell::new(0) };
        static FINISH_STATUS: Cell<i32> = const { Cell::new(0) };
        static BEGIN_STATUS: Cell<i32> = const { Cell::new(0) };
        static BEGINS: Cell<usize> = const { Cell::new(0) };
    }
    unsafe extern "C" fn physical_prepare(
        _: *mut c_void,
        _: *const c_void,
        _: *const abi::UrpNativeCreateArgs,
        out: *mut *mut c_void,
    ) -> i32 {
        if PREPARE_STATUS.get() == 0 {
            // SAFETY: caller supplies unique writable storage. This test token has no backing state.
            unsafe { *out = ptr::dangling_mut::<u32>().cast() };
        }
        PREPARE_STATUS.get()
    }
    unsafe extern "C" fn begin(_: *mut c_void, _: *mut c_void) -> i32 {
        BEGINS.set(BEGINS.get() + 1);
        BEGIN_STATUS.get()
    }
    unsafe extern "C" fn accept(_: *mut c_void) -> i32 {
        ACCEPT_STATUS.get()
    }
    unsafe extern "C" fn finish(_: *mut c_void, _: u32) -> i32 {
        FINISH_STATUS.get()
    }
    unsafe extern "C" fn destroy_token(_: *mut c_void) {}

    fn exercise(scenario: Scenario) {
        // SAFETY: both versioned getters belong to this retained, matched product library.
        let producer = unsafe { libloading::Library::new("libusbradioplus_product.so.1") }.unwrap();
        let host = HostServicesOwner::new(None).unwrap();
        let other = HostServicesOwner::new(None).unwrap();
        let table = host.descriptor();

        // SAFETY: the getters and public fixture structures are retained for all synchronous calls.
        unsafe {
            let product = &*producer
                .get::<unsafe extern "C" fn() -> *const abi::UrpAstDescriptor>(
                    b"usbradioplus_product_descriptor_v1\0",
                )
                .unwrap()();
            let physical = &*producer
                .get::<unsafe extern "C" fn() -> *const abi::UrpPreparationDescriptor>(
                    b"usbradioplus_physical_preparation_descriptor\0",
                )
                .unwrap()();
            let mut radio: abi::rptadv_radio_session_config = std::mem::zeroed();
            radio.struct_size = std::mem::size_of_val(&radio) as u32;
            radio.abi_version = 5;
            radio.generation_id = 1;
            radio.native_sample_rate_hz = 48_000;
            radio.interleaved_channels = 2;
            radio.maximum_receive_frame_count = 960;
            radio.maximum_transmit_frame_count = 960;
            radio.publication_interval_ms = 50;
            radio.receive_input_gain = 1.;
            radio.receive.ctcss_decoder_gain = 1.;
            radio.receive.vox_threshold = 1000;
            radio.receive.vox_hang_ms = 500;
            radio.receive.vad_external_delay = 1;
            radio.qualification.advanced_transport = 1;
            radio.qualification.radio_duplex = 1;
            radio.qualification.carrier_source = 4;
            radio.transmit.output_a_route = 1;
            radio.transmit.output_a_tone_gain = 1.;
            radio.transmit.output_b_tone_gain = 1.;
            let mut native: abi::UrpNativeStationConfig = std::mem::zeroed();
            native.struct_size = std::mem::size_of_val(&native) as u32;
            native.abi_version = 4;
            native.radio = ptr::from_ref(&radio).cast();
            native.device_selection = 1;
            native.input_device_channels = 2;
            native.output_device_channels = 2;
            native.receive_output_gain_db = -6;
            native.transmit_preemphasis_corner_hz = 300.;
            native.receive_deemphasis_corner_hz = 300.;
            let settings=br#"{"physical":{"ordinary":{"audio_source":"cm119","ptt_source":"usb"},"parallel_carrier_inverted":false,"parallel_ctcss_inverted":false}}"#;
            let args = abi::UrpTypedPreparation {
                struct_size: size_of::<abi::UrpTypedPreparation>() as u32,
                abi_version: 1,
                station: &native,
                settings: settings.as_ptr(),
                settings_length: settings.len() as u32,
            };
            let mut prepared = ptr::null_mut();
            assert_eq!(
                physical.prepare.unwrap()(&args, None, ptr::null_mut(), &mut prepared),
                0
            );
            let mut request = abi::rptadv_station_request_v1 {
                struct_size: size_of::<abi::rptadv_station_request_v1>() as u32,
                abi_version: 1,
                port_uuid: [18; 16],
                candidate_identity: 41,
                product,
                prepared,
            };
            let ops = &*table.station_binding;
            let context = &*table.context.cast::<StandaloneHostContext>();
            let physical_refs = || context.radio.physical.lock().unwrap().strong_count();
            assert_eq!(physical_refs(), 0);
            let mut first = ptr::null_mut();
            let mut second = ptr::null_mut();
            request.product = ptr::null();
            assert_ne!(ops.prepare.unwrap()(table.context, &request, &mut first), 0);
            assert!(first.is_null());
            assert_eq!(physical_refs(), 0);
            request.product = product;
            assert_eq!(ops.prepare.unwrap()(table.context, &request, &mut first), 0);
            request.port_uuid = [39; 16];
            assert_eq!(
                ops.prepare.unwrap()(table.context, &request, &mut second),
                0
            );
            product.native_configuration_destroy.unwrap()(prepared);
            let mut next = ptr::null_mut();
            if matches!(
                scenario,
                Scenario::Pending | Scenario::FailedPrepare | Scenario::FailedBegin
            ) {
                radio.receive.vad_mode = 2;
                std::hint::black_box(&radio);
                assert_eq!(
                    physical.prepare.unwrap()(&args, None, ptr::null_mut(), &mut prepared),
                    0
                );
                request.port_uuid = [18; 16];
                request.candidate_identity = 42;
                request.prepared = prepared;
                assert_eq!(ops.prepare.unwrap()(table.context, &request, &mut next), 0);
                product.native_configuration_destroy.unwrap()(prepared);
                let next_plan = Arc::get_mut(&mut (*next.cast::<Binding>()).plan).unwrap();
                next_plan.physical.prepare_update = Some(physical_prepare);
            }
            let mut a = ptr::null_mut();
            let mut b = ptr::null_mut();
            let mut rejected = ptr::dangling_mut();
            assert_ne!(
                ops.open_prepared.unwrap()(other.descriptor().context, first, 960, &mut rejected),
                0
            );
            assert!(rejected.is_null());
            assert_eq!(
                ops.open_prepared.unwrap()(table.context, first, 960, &mut a),
                0
            );
            assert_ne!(
                ops.open_prepared.unwrap()(table.context, first, 960, &mut rejected),
                0
            );
            assert_ne!(ops.stage_update.unwrap()(table.context, a, second), 0);
            assert_eq!(
                ops.open_prepared.unwrap()(table.context, second, 960, &mut b),
                0
            );
            let mut ticket: abi::rptadv_remote_ticket_v1 = std::mem::zeroed();
            if scenario != Scenario::Dormant {
                let mut api = crate::radio_activation::tests::remote_fixture_api();
                api.native_begin_update = Some(begin);
                api.native_accept_update = Some(accept);
                api.native_finish_update = Some(finish);
                api.native_destroy_update = Some(destroy_token);
                let providers = first
                    .cast::<Binding>()
                    .as_ref()
                    .unwrap()
                    .plan
                    .providers
                    .clone();
                (*a.cast::<RadioHandle>()).active = Some(
                    crate::radio_activation::tests::fixture_station_radio(api, providers),
                );
            }
            if scenario == Scenario::Ticket {
                let operation = crate::radio_activation::tests::remote_operation();
                assert_eq!(
                    table.radio_remote_submit.unwrap()(
                        table.context,
                        a,
                        &operation,
                        3,
                        &mut ticket
                    ),
                    0
                );
                assert_eq!(table.radio_quiesce.unwrap()(table.context, a), 0);
            }
            if !next.is_null() {
                check_update(scenario, table, a, next);
                ops.release_plan.unwrap()(table.context, next);
            }
            let release = ops.release_plan.unwrap();
            release(table.context, first);
            release(table.context, second);
            assert!(physical_refs() > 0);
            table.radio_destroy.unwrap()(table.context, a);
            table.radio_destroy.unwrap()(table.context, b);
            drop(producer);
            if scenario == Scenario::Ticket {
                assert!(
                    physical_refs() > 0,
                    "the independent ticket must retain the provider group after all radio and plan owners are gone"
                );
                assert_eq!(ticket.source.unwrap()(ticket.context, 2), 0);
                assert_eq!(
                    ticket.take.unwrap()(ticket.context, None, ptr::null_mut()),
                    0
                );
                ticket.destroy.unwrap()(ticket.context);
            }
            assert_eq!(physical_refs(), 0);
        }
    }
    unsafe fn check_update(
        scenario: Scenario,
        table: &abi::rptadv_host_services_v11,
        radio: *mut c_void,
        next: *mut c_void,
    ) {
        // SAFETY: the fixture owns the radio, exact candidate and complete host callbacks through all polls.
        unsafe {
            let ops = &*table.station_binding;
            let policy = || {
                let mut cmd: abi::rptadv_radio_control_v1 = std::mem::zeroed();
                cmd.struct_size = size_of_val(&cmd) as u32;
                cmd.abi_version = 1;
                cmd.operation = 7;
                cmd.command.struct_size = size_of_val(&cmd.command) as u32;
                cmd.command.abi_version = 1;
                cmd.command.command = abi::URP_AST_COMMAND_GET_RECEIVE_POLICY;
                assert_eq!(
                    table.radio_control.unwrap()(
                        table.context,
                        radio,
                        &mut cmd,
                        None,
                        ptr::null_mut()
                    ),
                    0
                );
                cmd.command.value
            };
            let finish_update = table.radio_update.unwrap();
            BEGINS.set(0);
            BEGIN_STATUS.set(if scenario == Scenario::FailedBegin {
                -6
            } else {
                0
            });
            PREPARE_STATUS.set(if scenario == Scenario::FailedPrepare {
                -11
            } else {
                0
            });
            ACCEPT_STATUS.set(1);
            FINISH_STATUS.set(1);
            if scenario == Scenario::FailedPrepare {
                assert_eq!(ops.stage_update.unwrap()(table.context, radio, next), -11);
                assert_eq!(policy(), 0);
                assert_ne!(finish_update(table.context, radio, 1), 0);
                assert_eq!(finish_update(table.context, radio, 2), 0);
                assert_eq!(BEGINS.get(), 0);
            } else if scenario == Scenario::FailedBegin {
                assert_eq!(ops.stage_update.unwrap()(table.context, radio, next), -6);
                assert_eq!(BEGINS.get(), 1);
                assert_eq!(policy(), 0);
                assert_ne!(finish_update(table.context, radio, 1), 0);
                assert_ne!(ops.stage_update.unwrap()(table.context, radio, next), 0);
                FINISH_STATUS.set(-11);
                assert_eq!(finish_update(table.context, radio, 2), -11);
                FINISH_STATUS.set(1);
                assert_eq!(finish_update(table.context, radio, 2), 2);
                assert_ne!(finish_update(table.context, radio, 1), 0);
                FINISH_STATUS.set(0);
                assert_eq!(finish_update(table.context, radio, 2), 0);
                assert_eq!(policy(), 0);
            } else {
                assert_eq!(ops.stage_update.unwrap()(table.context, radio, next), 2);
                assert_eq!(policy(), 0);
                assert_eq!(table.radio_service.unwrap()(table.context, radio), -31);
                assert_ne!(finish_update(table.context, radio, 1), 0);
                assert_eq!(ops.stage_update.unwrap()(table.context, radio, next), 2);
                ACCEPT_STATUS.set(0);
                assert_eq!(ops.stage_update.unwrap()(table.context, radio, next), 0);
                assert_eq!(BEGINS.get(), 1);
                assert_eq!(policy(), 2);
                assert_eq!(finish_update(table.context, radio, 1), 2);
                assert_eq!(table.radio_service.unwrap()(table.context, radio), -31);
                FINISH_STATUS.set(0);
                assert_eq!(finish_update(table.context, radio, 1), 0);
                assert_eq!(policy(), 2);
            }
        }
    }
}
