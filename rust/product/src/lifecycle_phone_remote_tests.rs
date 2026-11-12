//! Actual phone grammar and serialized radio tickets; every physical callback is a fixture.
use super::*;
use std::sync::atomic::AtomicUsize;

static READY: AtomicBool = AtomicBool::new(false);
static SUBMITTED: AtomicUsize = AtomicUsize::new(0);
static DESTROYED: AtomicUsize = AtomicUsize::new(0);
static LOCAL: AtomicBool = AtomicBool::new(false);

const STATUS: &[u8] = br#"{"version":1,"kind":"status","requested":{"rx_hz":146530000,"tx_hz":146530000,"mode":1,"power_mw":5000},"applied":{"rx_hz":146520000,"tx_hz":146520000,"mode":1,"power_mw":5000,"bandwidth_hz":12500},"observed_values":null,"scan":{},"tx_inhibited":false,"receive_permitted":true,"serviced_ptt":false,"failure_code":0}"#;

unsafe extern "C" fn source(_: *mut c_void, _: u32) -> i32 {
    0
}
unsafe extern "C" fn take(
    _: *mut c_void,
    sink: abi::rptadv_remote_sink_v1,
    context: *mut c_void,
) -> i32 {
    if !READY.load(Ordering::Acquire) {
        return 1;
    }
    // SAFETY: the product provides this synchronous copied-result sink.
    unsafe { sink.unwrap()(context, STATUS.as_ptr(), STATUS.len() as u32) };
    0
}
unsafe extern "C" fn destroy(handle: *mut c_void) {
    // SAFETY: submit transfers exactly one boxed fixture owner to the product.
    drop(unsafe { Box::from_raw(handle.cast::<u8>()) });
    DESTROYED.fetch_add(1, Ordering::AcqRel);
}
unsafe extern "C" fn service(_: *mut c_void, _: *mut c_void) -> i32 {
    0
}
unsafe extern "C" fn bind_metadata(
    _: *mut c_void,
    _: *mut c_void,
    _: abi::rptadv_radio_metadata_v1,
) -> i32 {
    0
}
unsafe extern "C" fn submit(
    _: *mut c_void,
    _: *mut c_void,
    request: *const abi::UrpRemoteOperation,
    flags: u32,
    output: *mut abi::rptadv_remote_ticket_v1,
) -> i32 {
    // SAFETY: the product lends a complete wire request and writable ticket.
    let request = unsafe { &*request };
    assert_eq!(
        request.operation, 6,
        "only the existing read-only status operation"
    );
    assert!(request.flags <= 1);
    assert_eq!(
        flags & 1,
        u32::from(LOCAL.load(Ordering::Acquire)),
        "source identity must not change"
    );
    SUBMITTED.fetch_add(1, Ordering::AcqRel);
    unsafe {
        output.write(abi::rptadv_remote_ticket_v1 {
            struct_size: size_of::<abi::rptadv_remote_ticket_v1>() as u32,
            abi_version: 1,
            context: Box::into_raw(Box::new(0_u8)).cast(),
            source: Some(source),
            take: Some(take),
            destroy: Some(destroy),
        })
    };
    0
}
fn services() -> HostServices {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.radio_remote_submit = Some(submit);
    table.radio_metadata_bind = Some(bind_metadata);
    table.radio_service = Some(service);
    table.radio_quiesce = Some(service);
    table.radio_ready = Some(service);
    // Existing fixture radio open/close never opens a device. Only the remote
    // ticket boundary is replaced, leaving real control ownership in place.
    unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap()
}
fn configured(access: &str) -> String {
    format!(
        "{}\n[remote_base 1000]\nenabled=yes\nbackend=hamlib\nhamlib_model=RIG_MODEL_DUMMY\n\
        dtmf_access={access}\nstartup_memory=local\nallowed_modes=FM\n\
        allowed_rx_ranges_hz=144000000-148000000\nallowed_tx_ranges_hz=144000000-148000000\n\
        [remote_memory 1000 local]\nselector=1\nrx_frequency_hz=146520000\n\
        tx_frequency_hz=146520000\nmode=FM\n",
        configuration()
    )
}
fn reset() {
    LOCAL.store(false, Ordering::Release);
    READY.store(false, Ordering::Release);
    SUBMITTED.store(0, Ordering::Release);
    DESTROYED.store(0, Ordering::Release);
}

#[test]
fn private_radio_status_existing_local_query_baseline_uses_same_radio_owner() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    LOCAL.store(true, Ordering::Release);
    let voice = speech();
    let engine = engine(configured("disabled"), services(), Arc::clone(&voice));
    let clock = engine.clock();
    engine
        .run(move |host| {
            let operation = "*652#"
                .chars()
                .filter_map(|digit| {
                    host.runtime.digit(
                        "1000",
                        rpt_advanced_core::runtime::dtmf::DigitEvent::Digit {
                            digit,
                            now_ms: clock.now_ms,
                        },
                    )
                })
                .find_map(|outcome| match outcome {
                    DigitOutcome::Operation(operation) => Some(operation),
                    _ => None,
                })
                .unwrap();
            let effect = host.runtime.command("1000", operation, clock, false)?;
            assert!(matches!(effect, LinkEffect::RemoteBase(_)));
            host.immediate("1000", effect, clock)
        })
        .unwrap();
    assert_eq!(SUBMITTED.load(Ordering::Acquire), 1);
    READY.store(true, Ordering::Release);
    engine.pump(engine.clock(), false).unwrap();
    let public = engine
        .run(|host| Ok(crate::host::tests::probe_public_phone_media(host, "1000")))
        .unwrap();
    let texts = voice.opened.lock().unwrap().clone();
    assert!(engine.stop());
    assert!(
        public,
        "ordinary local status keeps its existing RF audience"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("Applied settings.")
                && text.contains("Receive hertz 146520000.")),
        "{texts:?}"
    );
    assert_eq!(DESTROYED.load(Ordering::Acquire), 1);
}

#[test]
fn private_radio_status_copied_operation_retains_exact_collector_through_actual_host_pump() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    let engine = engine(configured("disabled"), services(), speech());
    let (source, reply, state) = admit(&engine);
    wait_for(&engine, || state.lock().unwrap().audio.len() >= 320);
    let clock = engine.clock();
    let (before, after) = engine
        .run(move |host| {
            let node = host.runtime.node("1000").unwrap();
            let operation = "*652#"
                .chars()
                .filter_map(|digit| {
                    node.links()
                        .phone_digit(&source, digit, clock.now_ms, reply.clone())
                })
                .find_map(|outcome| match outcome {
                    DigitOutcome::Operation(operation) => Some(operation),
                    _ => None,
                })
                .unwrap();
            let before = host
                .runtime
                .command("1000", operation.clone(), clock, false)
                .map(|effect| matches!(effect, LinkEffect::RemoteBase(_)));
            host.pump(clock)?;
            let after = host
                .runtime
                .command("1000", operation, clock, false)
                .map(|effect| matches!(effect, LinkEffect::RemoteBase(_)));
            Ok((before, after))
        })
        .unwrap();
    assert!(engine.stop());
    assert_eq!(
        before,
        Ok(true),
        "same exact private query is authorized before the control boundary"
    );
    assert_eq!(
        after,
        Ok(true),
        "actual pump must not reclaim a source required by copied pending command"
    );
    assert_eq!(
        SUBMITTED.load(Ordering::Acquire),
        0,
        "diagnostic exercises admission only, not hardware submission"
    );
}

#[test]
fn ordinary_peer_radio_status_copied_operation_survives_actual_host_pump() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    reset();
    let engine = engine(configured("disabled"), services(), speech());
    let clock = engine.clock();
    let (before, after) = engine
        .run(move |host| {
            host.runtime.incoming("1000", "2000", true)?;
            let signals = rpt_advanced_core::link::PeerSignals::new();
            let reply = signals.reply_target();
            let node = host.runtime.node("1000").unwrap();
            let operation = "*652#"
                .chars()
                .filter_map(|digit| {
                    node.links()
                        .peer_digit("2000", digit, clock.now_ms, reply.clone())
                })
                .find_map(|outcome| match outcome {
                    DigitOutcome::Operation(operation) => Some(operation),
                    _ => None,
                })
                .unwrap();
            let before = host
                .runtime
                .command("1000", operation.clone(), clock, false)
                .map(|effect| matches!(effect, LinkEffect::RemoteBase(_)));
            host.pump(clock)?;
            let after = host
                .runtime
                .command("1000", operation, clock, false)
                .map(|effect| matches!(effect, LinkEffect::RemoteBase(_)));
            signals.end();
            host.runtime.node("1000").unwrap().links().ended("2000");
            host.runtime.peer_detached("1000", "2000", clock.now_ms);
            Ok((before, after))
        })
        .unwrap();
    assert!(engine.stop());
    assert_eq!(before, Ok(true));
    assert_eq!(
        after,
        Ok(true),
        "idle source reclamation must not invalidate a queued ordinary peer query"
    );
    assert_eq!(SUBMITTED.load(Ordering::Acquire), 0);
}

#[test]
fn remote_operation_custody_layout_and_lazy_allocation_probe() {
    use rpt_advanced_core::{
        access::AccessPolicy,
        command::DtmfCommandMap,
        link::PeerSignals,
        remote_base_command::RemoteDtmfAccess,
        runtime::{
            dtmf::{DigitEvent, DigitOperation, DtmfCommands},
            links::NodeLinkControl,
        },
    };
    fn collector() -> NodeLinkControl {
        let mut links = NodeLinkControl::new(
            "1000",
            AccessPolicy::new("", "").unwrap(),
            DtmfCommandMap::standard(),
            None,
        )
        .unwrap();
        links
            .configure_remote("6", RemoteDtmfAccess::Disabled)
            .unwrap();
        links.accept("2000", true).unwrap();
        links
    }
    fn peer_query(links: &mut NodeLinkControl, reply: &PeerReply) -> (DigitOperation, usize) {
        for digit in "*652".chars() {
            assert!(links.peer_digit("2000", digit, 1, reply.clone()).is_none());
        }
        let (outcome, allocations) =
            crate::fixture::allocations(|| links.peer_digit("2000", '#', 1, reply.clone()));
        let Some(DigitOutcome::Operation(operation)) = outcome else {
            panic!("real peer status completion")
        };
        (operation, allocations)
    }
    let mut links = collector();
    let reply = PeerSignals::new().reply_target();
    let (first, first_allocations) = peer_query(&mut links, &reply);
    let (second, shared_allocations) = peer_query(&mut links, &reply);
    let (_, clone_allocations) = crate::fixture::allocations(|| first.clone());
    drop((first, second));
    let (fresh, fresh_allocations) = peer_query(&mut links, &reply);
    drop(fresh);
    let mut local = DtmfCommands::new(DtmfCommandMap::standard());
    local
        .configure_remote("6", RemoteDtmfAccess::Disabled)
        .unwrap();
    for digit in "*652".chars() {
        assert!(local.feed(DigitEvent::Digit { digit, now_ms: 1 }).is_none());
    }
    let (outcome, local_allocations) = crate::fixture::allocations(|| {
        local.feed(DigitEvent::Digit {
            digit: '#',
            now_ms: 1,
        })
    });
    assert!(matches!(outcome, Some(DigitOutcome::Operation(_))));
    assert_eq!(
        (
            first_allocations,
            shared_allocations,
            fresh_allocations,
            clone_allocations,
            local_allocations
        ),
        (5, 4, 5, 1, 1),
        "one lazy custody allocation per original collector while work is pending; copies share it"
    );
    println!(
        "custody layout: DigitOperation={} DigitOutcome={} DtmfCommands={}; allocations: first={first_allocations} shared={shared_allocations} fresh={fresh_allocations} clone={clone_allocations} local={local_allocations}",
        std::mem::size_of::<DigitOperation>(),
        std::mem::size_of::<DigitOutcome>(),
        std::mem::size_of::<DtmfCommands>()
    );
}

#[test]
fn ordinary_peer_radio_status_native_pending_keeps_custody_until_completion_or_retirement() {
    for boundary in ["completion", "retirement", "reload"] {
        let _serial = crate::fixture::LIFECYCLE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        reset();
        let engine = engine(configured("disabled"), services(), speech());
        let clock = engine.clock();
        let signals = engine
            .run(move |host| {
                host.runtime.incoming("1000", "2000", true)?;
                let signals = rpt_advanced_core::link::PeerSignals::new();
                let reply = signals.reply_target();
                let node = host.runtime.node("1000").unwrap();
                let operation = "*652#"
                    .chars()
                    .filter_map(|digit| {
                        node.links()
                            .peer_digit("2000", digit, clock.now_ms, reply.clone())
                    })
                    .find_map(|outcome| match outcome {
                        DigitOutcome::Operation(operation) => Some(operation),
                        _ => None,
                    })
                    .unwrap();
                let effect = host.runtime.command("1000", operation, clock, false)?;
                host.immediate("1000", effect, clock)?;
                Ok(signals)
            })
            .unwrap();
        for _ in 0..3 {
            engine.pump(engine.clock(), false).unwrap();
            assert_eq!(
                DESTROYED.load(Ordering::Acquire),
                0,
                "native Pending alone must keep the collector discoverable"
            );
        }
        assert_eq!(SUBMITTED.load(Ordering::Acquire), 1);
        match boundary {
            "completion" => READY.store(true, Ordering::Release),
            "retirement" => signals.end(),
            "reload" => {
                let clock = engine.clock();
                engine
                    .run(move |host| host.reload(document(&configured("disabled")).unwrap(), clock))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        engine.pump(engine.clock(), false).unwrap();
        assert_eq!(DESTROYED.load(Ordering::Acquire), 1, "{boundary}");
        signals.end();
        engine
            .run(move |host| {
                host.runtime.node("1000").unwrap().links().ended("2000");
                host.runtime.peer_detached("1000", "2000", clock.now_ms);
                Ok(())
            })
            .unwrap();
        assert!(engine.stop());
        assert_eq!(
            DESTROYED.load(Ordering::Acquire),
            1,
            "native receipt is released exactly once"
        );
    }
}
