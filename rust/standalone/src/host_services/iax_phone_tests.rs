//! Private IAX host regressions; only the external provider table is a fixture.
//! Actual host callbacks, account preparation and unique handle destruction run.
use crate::iax::{
    IaxError,
    accounts::{CallReceipt, ProviderCall},
    tests,
};
use crate::{
    abi,
    host_services::HostServicesOwner,
    phone_accounts::{AccountPreparer, PreparedAccount, PreparedAccounts},
    secrets::SecretsFile,
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::c_void,
    mem::size_of,
    net::SocketAddr,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone)]
enum Event {
    Reset(u64),
    Hangup,
    Failure,
    Digit(u8),
    Audio,
    UnqualifiedTone,
}
#[derive(Default)]
struct Provider {
    events: VecDeque<Event>,
    polls: usize,
    media_epoch: u64,
    source_cursor: u64,
    legacy_reads: usize,
    legacy_sends: usize,
    destroyed: usize,
    acknowledgements: Vec<u64>,
}
thread_local! { static PROVIDER: RefCell<Provider> = RefCell::default(); }

unsafe extern "C" fn destroy(_: *mut c_void) {
    PROVIDER.with_borrow_mut(|state| state.destroyed += 1);
}
unsafe extern "C" fn legacy_poll(
    _: *mut c_void,
    _: *mut f32,
    _: usize,
    _: *mut u8,
    _: usize,
    kind: *mut u32,
    length: *mut usize,
) -> i32 {
    PROVIDER.with_borrow_mut(|state| state.legacy_reads += 1);
    unsafe {
        kind.write(0);
        length.write(0);
    }
    0
}
unsafe extern "C" fn legacy_send(_: *mut c_void, _: *const u8, _: usize, _: u32, _: u32) -> i32 {
    PROVIDER.with_borrow_mut(|state| state.legacy_sends += 1);
    0
}
unsafe extern "C" fn timed_poll(
    _: *mut c_void,
    pcm: *mut f32,
    capacity: usize,
    text: *mut u8,
    text_capacity: usize,
    record: *mut abi::rptadv_iax2_timed_event_v1,
) -> i32 {
    let record = unsafe { &mut *record };
    if record.struct_size as usize != size_of::<abi::rptadv_iax2_timed_event_v1>()
        || record.abi_version != 1
        || record.host_clock_epoch != 9
        || record.reference_now_ns < 10_000
    {
        return -1;
    }
    PROVIDER.with_borrow_mut(|state| {
        state.polls += 1;
        state.acknowledgements.push(record.acknowledged_media_epoch);
        record.event_kind = 0;
        record.event_length = 0;
        record.media_epoch = state.media_epoch.max(1);
        record.source_start = state.source_cursor;
        record.flags = 0;
        record.born_ns = 0;
        match state.events.front().cloned() {
            Some(Event::Reset(target)) => {
                state.media_epoch = target;
                state.source_cursor = 0;
                record.source_start = 0;
                record.media_epoch = target;
                record.event_kind = 8;
                record.flags = 2;
                state.events.pop_front();
            }
            Some(Event::Digit(digit)) if text_capacity != 0 => {
                unsafe {
                    text.write(digit);
                }
                record.event_kind = 4;
                record.event_length = 1;
                state.events.pop_front();
            }
            Some(Event::Audio) if capacity >= 4 => {
                unsafe {
                    ptr::copy_nonoverlapping([0.25, 0.5, 0.75, 1.0].as_ptr(), pcm, 4);
                }
                record.event_kind = 1;
                record.event_length = 4;
                record.flags = 1;
                record.born_ns = 700;
                state.source_cursor += 4;
                state.events.pop_front();
            }
            Some(Event::Audio) => {
                record.event_length = 4;
                return 1;
            }
            Some(Event::UnqualifiedTone) if capacity >= 160 => {
                let samples = unsafe { std::slice::from_raw_parts_mut(pcm, 160) };
                for (index, sample) in samples.iter_mut().enumerate() {
                    let phase = (state.source_cursor + index as u64) as f32 * std::f32::consts::TAU
                        / 8000.0;
                    *sample = 0.25 * ((phase * 770.0).sin() + (phase * 1336.0).sin());
                }
                record.event_kind = 1;
                record.event_length = 160;
                state.source_cursor += 160;
                state.events.pop_front();
            }
            Some(Event::UnqualifiedTone) => {
                record.event_length = 160;
                return 1;
            }
            Some(Event::Failure) => return -1,
            Some(Event::Hangup) => {
                record.event_kind = 3;
                state.events.pop_front();
            }
            Some(Event::Digit(_)) => return -1,
            None => {}
        }
        0
    })
}
fn call(account: &PreparedAccount, local: SocketAddr) -> ProviderCall {
    call_with_timing(account, local, true)
}
fn call_with_timing(account: &PreparedAccount, local: SocketAddr, timed: bool) -> ProviderCall {
    let mut api = tests::descriptor();
    api.destroy = Some(destroy);
    api.poll = Some(legacy_poll);
    api.send_prepared_audio = Some(legacy_send);
    api.poll_timed = timed.then_some(timed_poll);
    ProviderCall {
        peer: tests::host_fixture_peer(api),
        receipt: CallReceipt {
            account_generation: account.generation(),
            call_generation: 17,
            format: 4,
            local_call: 11,
            remote_call: 23,
            local,
            remote: account.remote(),
        },
    }
}

fn account() -> Arc<PreparedAccount> {
    use std::os::unix::fs::PermissionsExt;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "rptadv-private-iax-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    let secret = path.join("phone.secret");
    std::fs::write(&secret, "fixture-secret").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let records = crate::configuration::resolve_phone_accounts("[named-node]\n[phone_account provider]\nenabled=yes\nlocal_node=named-node\nserver=127.0.0.1:4000\nusername=login\nsecrets_file=phone.secret\n").unwrap();
    assert_eq!(records.len(), 1, "fixture account was not resolved");
    let prepared = AccountPreparer::default()
        .prepare(
            &path.join("rpt.conf"),
            records,
            &PreparedAccounts::default(),
            |_| panic!("numeric endpoint used DNS"),
        )
        .unwrap();
    std::fs::remove_file(secret).unwrap();
    std::fs::remove_dir(path).unwrap();
    Arc::clone(&prepared.as_slice()[0])
}
fn owner() -> HostServicesOwner {
    PROVIDER.with_borrow_mut(|state| *state = Provider::default());
    let providers = Box::leak(Box::new(crate::providers::ProviderSet::load().unwrap()));
    HostServicesOwner::new(Vec::new(), Some(providers), SecretsFile::parse("").unwrap()).unwrap()
}
struct Handle<'a> {
    owner: &'a HostServicesOwner,
    raw: *mut c_void,
}
impl Drop for Handle<'_> {
    fn drop(&mut self) {
        self.owner.destroy_peer(self.raw);
    }
}
fn handle<'a>(
    owner: &'a HostServicesOwner,
    account: &PreparedAccount,
    local: SocketAddr,
) -> Handle<'a> {
    Handle {
        owner,
        raw: owner
            .provider_peer_handle(call(account, local), account, local)
            .unwrap(),
    }
}
fn metadata(handle: &Handle<'_>) -> abi::rptadv_peer_info_v1 {
    let mut info: abi::rptadv_peer_info_v1 = unsafe { std::mem::zeroed() };
    info.struct_size = size_of::<abi::rptadv_peer_info_v1>() as u32;
    info.abi_version = 1;
    let api = handle.owner.descriptor();
    assert_eq!(
        unsafe { api.peer_info.unwrap()(api.context, handle.raw, &mut info) },
        0
    );
    info
}
unsafe fn bytes<'a>(value: abi::rptadv_peer_bytes_v1) -> &'a [u8] {
    if value.length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(value.data, value.length) }
    }
}

#[test]
fn owned_provider_calls_have_private_identity_not_caller_numbers_or_listener_local_ids() {
    let owner = owner();
    let account = account();
    let first = handle(&owner, &account, "127.0.0.1:4569".parse().unwrap());
    let next = handle(&owner, &account, "127.0.0.1:4570".parse().unwrap());
    let a = metadata(&first);
    let b = metadata(&next);
    for info in [&a, &b] {
        assert_eq!((info.protocol, info.capabilities), (3, 51));
        assert_eq!(
            (info.account_generation, info.qso_generation),
            (account.generation(), 17)
        );
        assert_eq!(unsafe { bytes(info.provider_account) }, b"provider");
        assert_eq!(unsafe { bytes(info.local) }, b"named-node");
        let token = unsafe { bytes(info.remote) };
        assert_eq!(token.len(), 32);
        assert!(
            token
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        );
        assert!(token.iter().any(|byte| *byte != b'0'));
        assert!(unsafe { bytes(info.callsign) }.is_empty());
    }
    assert_ne!(unsafe { bytes(a.remote) }, unsafe { bytes(b.remote) });
    drop((first, next));
    PROVIDER.with_borrow(|state| assert_eq!(state.destroyed, 2));
}

#[test]
fn private_adoption_rejects_stale_account_or_wrong_listener_provider_receipt_once() {
    let owner = owner();
    let account = account();
    let local = "127.0.0.1:4569".parse().unwrap();
    for defect in 0..5 {
        let mut call = call(&account, local);
        match defect {
            0 => call.receipt.account_generation += 1,
            1 => call.receipt.call_generation = 0,
            2 => call.receipt.remote.set_port(4001),
            3 => call.receipt.local.set_port(4570),
            _ => call.receipt.format = 0,
        }
        let result = owner.provider_peer_handle(call, &account, local);
        if let Ok(raw) = result {
            owner.destroy_peer(raw);
        }
        assert!(matches!(result, Err(IaxError::InvalidParameters)));
        PROVIDER.with_borrow(|state| assert_eq!(state.destroyed, defect + 1));
    }
}

unsafe extern "C" fn ignore_event(_: *mut c_void, _: u32, _: *const c_void, _: usize) {}
#[test]
fn private_handles_never_enter_legacy_read_or_untimed_prepared_send() {
    let owner = owner();
    let account = account();
    let handle = handle(&owner, &account, "127.0.0.1:4569".parse().unwrap());
    let api = owner.descriptor();
    assert_eq!(
        unsafe {
            api.peer_read.unwrap()(api.context, handle.raw, Some(ignore_event), ptr::null_mut())
        },
        -1
    );
    assert_eq!(
        unsafe {
            api.peer_send_prepared.unwrap()(
                api.context,
                handle.raw,
                [0u8; 160].as_ptr(),
                160,
                160,
                0,
            )
        },
        -1
    );
    assert_eq!(
        unsafe { api.peer_send_text.unwrap()(api.context, handle.raw, c"T 1 524950".as_ptr(), 10) },
        -1
    );
    unsafe extern "C" fn idle_step(_: *mut c_void) -> i32 {
        0
    }
    let mut registration = ptr::dangling_mut::<c_void>();
    assert_eq!(
        unsafe {
            api.peer_step_attach.unwrap()(
                api.context,
                handle.raw,
                Some(idle_step),
                ptr::null_mut(),
                &mut registration,
            )
        },
        -1
    );
    assert!(
        registration.is_null(),
        "inactive private call was attached to ordinary jitter"
    );
    PROVIDER.with_borrow(|state| assert_eq!((state.legacy_reads, state.legacy_sends), (0, 0)));
}

#[derive(Default)]
struct Sink {
    digit_offers: Vec<u32>,
    audio_offers: Vec<(u64, Vec<f32>, u64, u64)>,
    reset_offers: usize,
    gaps: Vec<(u64, u32)>,
    refuse_reset: bool,
}
unsafe extern "C" fn digit(context: *mut c_void, symbol: u32) -> i32 {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    sink.digit_offers.push(symbol);
    i32::from(sink.digit_offers.len() == 1)
}
unsafe extern "C" fn reset(context: *mut c_void) -> i32 {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    sink.reset_offers += 1;
    i32::from(sink.refuse_reset)
}
unsafe extern "C" fn audio(
    context: *mut c_void,
    start: u64,
    pcm: *const f32,
    count: usize,
    concealed: u32,
    birth: *const abi::rptadv_source_birth_v1,
) -> usize {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    if birth.is_null() || concealed != 0 {
        return 0;
    }
    let birth = unsafe { &*birth };
    sink.audio_offers.push((
        start,
        unsafe { std::slice::from_raw_parts(pcm, count) }.to_vec(),
        birth.host_clock_epoch,
        birth.born_ns,
    ));
    match sink.audio_offers.len() {
        1 => 2.min(count),
        2 => 0,
        _ => count,
    }
}
unsafe extern "C" fn gap(context: *mut c_void, start: u64, count: u32) -> u32 {
    let sink = unsafe { &mut *context.cast::<Sink>() };
    sink.gaps.push((start, count));
    count
}
fn read(
    handle: &Handle<'_>,
    sink: Option<&mut Sink>,
    now: u64,
) -> abi::rptadv_peer_source_result_v1 {
    let mut output: abi::rptadv_peer_source_result_v1 = unsafe { std::mem::zeroed() };
    output.struct_size = size_of::<abi::rptadv_peer_source_result_v1>() as u32;
    output.abi_version = 1;
    let sink = sink.map(|sink| abi::rptadv_peer_source_sink_v1 {
        struct_size: size_of::<abi::rptadv_peer_source_sink_v1>() as u32,
        abi_version: 1,
        context: ptr::from_mut(sink).cast(),
        audio: None,
        gap: Some(gap),
        host_clock_epoch: 9,
        reference_now_ns: now,
        audio_timed: Some(audio),
        source_discontinuity: Some(reset),
        digit: Some(digit),
    });
    let api = handle.owner.descriptor();
    assert_eq!(
        unsafe {
            api.peer_read_source.unwrap()(
                api.context,
                handle.raw,
                0,
                sink.as_ref().map_or(ptr::null(), ptr::from_ref),
                None,
                ptr::null_mut(),
                &mut output,
            )
        },
        0
    );
    output
}
#[test]
fn timed_source_retains_refused_digit_then_exact_pcm_suffix_and_original_birth() {
    let owner = owner();
    let account = account();
    let handle = handle(&owner, &account, "127.0.0.1:4569".parse().unwrap());
    PROVIDER.with_borrow_mut(|state| {
        state.events = [Event::Reset(1), Event::Digit(b'5'), Event::Audio].into()
    });
    read(&handle, None, 10_000);
    PROVIDER.with_borrow(|state| assert_eq!(state.polls, 0));
    let mut sink = Sink::default();
    for turn in 0..12 {
        read(&handle, Some(&mut sink), 10_000 + turn * 1000);
        if sink.audio_offers.len() == 3 {
            break;
        }
        if sink.digit_offers.len() == 1 {
            assert!(sink.audio_offers.is_empty());
            PROVIDER.with_borrow(|state| assert_eq!(state.polls, 2));
        }
    }
    assert_eq!(sink.digit_offers, [b'5' as u32, b'5' as u32]);
    assert_eq!(
        sink.audio_offers,
        [
            (0, vec![0.25, 0.5, 0.75, 1.0], 9, 700),
            (2, vec![0.75, 1.0], 9, 700),
            (2, vec![0.75, 1.0], 9, 700),
        ]
    );
    PROVIDER.with_borrow(|state| {
        assert_eq!(state.legacy_reads, 0);
        assert_eq!(state.polls, 3);
        assert_eq!(state.acknowledgements, [0, 1, 1]);
    });
}
#[test]
fn provider_reset_is_acknowledged_only_after_exact_product_retirement() {
    let owner = owner();
    let account = account();
    let handle = handle(&owner, &account, "127.0.0.1:4569".parse().unwrap());
    PROVIDER.with_borrow_mut(|state| state.events = [Event::Reset(7), Event::Digit(b'5')].into());
    let mut sink = Sink {
        refuse_reset: true,
        ..Sink::default()
    };
    for _ in 0..3 {
        read(&handle, Some(&mut sink), 10_000);
    }
    assert!(sink.reset_offers >= 3);
    assert!(sink.digit_offers.is_empty());
    PROVIDER.with_borrow(|state| assert_eq!(state.acknowledgements, [0]));
    sink.refuse_reset = false;
    for _ in 0..3 {
        read(&handle, Some(&mut sink), 10_000);
        if !sink.digit_offers.is_empty() {
            break;
        }
    }
    PROVIDER.with_borrow(|state| assert_eq!(state.acknowledgements, [0, 7]));
}
#[test]
fn missing_timed_provider_slot_rejects_private_ownership_without_legacy_fallback() {
    let owner = owner();
    let account = account();
    let local = "127.0.0.1:4569".parse().unwrap();
    let call = call_with_timing(&account, local, false);
    let result = owner.provider_peer_handle(call, &account, local);
    if let Ok(raw) = result {
        owner.destroy_peer(raw);
    }
    assert!(matches!(result, Err(IaxError::IncompatibleAdapter)));
    PROVIDER.with_borrow(|state| assert_eq!(state.destroyed, 1));
}

#[test]
fn failed_host_identity_allocation_rejects_and_destroys_the_owned_call_once() {
    let owner = owner();
    let account = account();
    let local = "127.0.0.1:4569".parse().unwrap();
    let result = owner.provider_peer_handle_allocated(
        call(&account, local),
        &account,
        local,
        &AtomicU64::new(u64::MAX),
    );
    if let Ok(raw) = result {
        owner.destroy_peer(raw);
    }
    assert_eq!(result, Err(IaxError::Operation));
    PROVIDER.with_borrow(|state| assert_eq!(state.destroyed, 1));
}

#[test]
fn concurrent_host_replacements_do_not_reuse_listener_local_call_identity() {
    let account = account();
    let ids = std::thread::scope(|scope| {
        let joins: Vec<_> = (0..2)
            .map(|worker| {
                let account = Arc::clone(&account);
                scope.spawn(move || {
                    let mut ids = Vec::new();
                    for replacement in 0..2 {
                        let owner = owner();
                        let local =
                            SocketAddr::from(([127, 0, 0, 1], 4569 + worker * 2 + replacement));
                        let handle = handle(&owner, &account, local);
                        let info = metadata(&handle);
                        ids.push(unsafe { bytes(info.remote) }.to_vec());
                        drop(handle);
                        PROVIDER.with_borrow(|state| assert_eq!(state.destroyed, 1));
                    }
                    ids
                })
            })
            .collect();
        joins
            .into_iter()
            .flat_map(|join| join.join().unwrap())
            .collect::<Vec<_>>()
    });
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        4,
        "host replacements reused provider-local identity"
    );
}

#[test]
fn ambiguous_tone_pcm_never_reaches_analysis_but_native_control_stays_ordered() {
    let owner = owner();
    let account = account();
    let handle = handle(&owner, &account, "127.0.0.1:4569".parse().unwrap());
    assert_eq!(
        metadata(&handle).capabilities & 64,
        0,
        "in-band authority was advertised"
    );
    PROVIDER.with_borrow_mut(|state| {
        state.events = [
            Event::Reset(1),
            Event::UnqualifiedTone,
            Event::UnqualifiedTone,
            Event::UnqualifiedTone,
            Event::UnqualifiedTone,
            Event::Digit(b'7'),
        ]
        .into();
    });
    let mut sink = Sink::default();
    for turn in 0..12 {
        read(&handle, Some(&mut sink), 10_000 + turn * 1000);
        if sink.digit_offers.len() == 2 {
            break;
        }
    }
    // The source analyzer receives no ambiguous PCM, so this 80ms DTMF-like
    // waveform cannot become an in-band command or acquire a claimed PLC flag.
    assert!(sink.audio_offers.is_empty());
    assert_eq!(sink.gaps, [(0, 160), (160, 160), (320, 160), (480, 160)]);
    assert_eq!(sink.digit_offers, [b'7' as u32, b'7' as u32]);
    PROVIDER.with_borrow(|state| assert_eq!(state.legacy_reads, 0));
}

#[test]
fn host_identity_issues_final_counter_once_then_remains_exhausted() {
    let owner = owner();
    let account = account();
    let local = "127.0.0.1:4569".parse().unwrap();
    let sequence = AtomicU64::new(u64::MAX - 1);
    let raw = owner
        .provider_peer_handle_allocated(call(&account, local), &account, local, &sequence)
        .unwrap();
    let final_call = Handle { owner: &owner, raw };
    assert_eq!(
        unsafe { bytes(metadata(&final_call).remote) },
        b"0000000000000000ffffffffffffffff"
    );
    drop(final_call);
    for _ in 0..2 {
        assert_eq!(
            owner.provider_peer_handle_allocated(call(&account, local), &account, local, &sequence),
            Err(IaxError::Operation)
        );
        assert_eq!(sequence.load(Ordering::Relaxed), u64::MAX);
    }
    PROVIDER.with_borrow(|state| assert_eq!(state.destroyed, 3));
}

#[test]
fn hangup_remains_closed_for_null_queries_and_repeated_source_reads() {
    let owner = owner();
    let account = account();
    let handle = handle(&owner, &account, "127.0.0.1:4569".parse().unwrap());
    PROVIDER.with_borrow_mut(|state| {
        state.events = [
            Event::Reset(1),
            Event::Hangup,
            Event::Audio,
            Event::Digit(b'7'),
        ]
        .into();
    });
    let mut sink = Sink::default();
    read(&handle, Some(&mut sink), 10_000);
    let ended = read(&handle, Some(&mut sink), 11_000);
    let query = read(&handle, None, 12_000);
    let repeated = read(&handle, Some(&mut sink), 13_000);
    let again = read(&handle, None, 14_000);
    assert_eq!(
        [ended.state, query.state, repeated.state, again.state],
        [3; 4],
        "terminal closure must survive a later state query or source turn"
    );
    PROVIDER
        .with_borrow(|state| assert_eq!(state.polls, 2, "closed call polled later provider input"));
    assert!(sink.audio_offers.is_empty());
    assert!(sink.digit_offers.is_empty());
    drop(handle);
    PROVIDER.with_borrow(|state| assert_eq!(state.destroyed, 1));
}

#[test]
fn terminal_provider_error_remains_closed_without_repolling() {
    let owner = owner();
    let account = account();
    let handle = handle(&owner, &account, "127.0.0.1:4569".parse().unwrap());
    PROVIDER.with_borrow_mut(|state| {
        state.events = [Event::Reset(1), Event::Failure, Event::Audio].into();
    });
    let mut sink = Sink::default();
    read(&handle, Some(&mut sink), 10_000);
    let api = owner.descriptor();
    let callbacks = abi::rptadv_peer_source_sink_v1 {
        struct_size: size_of::<abi::rptadv_peer_source_sink_v1>() as u32,
        abi_version: 1,
        context: ptr::from_mut(&mut sink).cast(),
        audio: None,
        gap: Some(gap),
        host_clock_epoch: 9,
        reference_now_ns: 11_000,
        audio_timed: Some(audio),
        source_discontinuity: Some(reset),
        digit: Some(digit),
    };
    let mut output: abi::rptadv_peer_source_result_v1 = unsafe { std::mem::zeroed() };
    output.struct_size = size_of::<abi::rptadv_peer_source_result_v1>() as u32;
    output.abi_version = 1;
    assert_eq!(
        unsafe {
            api.peer_read_source.unwrap()(
                api.context,
                handle.raw,
                0,
                &callbacks,
                None,
                ptr::null_mut(),
                &mut output,
            )
        },
        -1
    );
    let query = read(&handle, None, 12_000);
    assert_eq!(query.state, 3, "terminal provider error was forgotten");
    let repeated = read(&handle, Some(&mut sink), 13_000);
    assert_eq!(repeated.state, 3);
    PROVIDER.with_borrow(|state| assert_eq!(state.polls, 2));
    assert!(sink.audio_offers.is_empty());
    drop(handle);
    PROVIDER.with_borrow(|state| assert_eq!(state.destroyed, 1));
}
