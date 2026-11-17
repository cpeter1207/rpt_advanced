//! Current configured account authority at both real product admission callers.
use super::*;

const CALL: &[u8] = b"aabbccddeeff00112233445566778899";

unsafe extern "C" fn phone_info(
    context: *mut c_void,
    peer: *const c_void,
    output: *mut abi::rptadv_peer_info_v1,
) -> i32 {
    unsafe { crate::fixture::peer_info(context, peer, output) };
    let output = unsafe { &mut *output };
    output.protocol = 3;
    output.capabilities = 32;
    output.account_generation = 1;
    output.qso_generation = 2;
    output.remote = abi::rptadv_peer_bytes_v1 {
        data: CALL.as_ptr(),
        length: CALL.len(),
    };
    output.provider_account = abi::rptadv_peer_bytes_v1 {
        data: b"provider".as_ptr(),
        length: 8,
    };
    0
}

fn configuration(account: Option<(&str, bool)>) -> String {
    let mut source = active_configuration("[2000]\nradio_channel=usb2\nduplex=full\n");
    if let Some((local, enabled)) = account {
        source.push_str(&format!(
            "[phone_account provider]\nlocal_node={local}\nenabled={}\nserver=pbx.example:4569\nusername=fixture\nsecrets_file=unused.secret\n",
            if enabled { "yes" } else { "no" },
        ));
    }
    source
}

fn request() -> abi::rptadv_incoming_peer_v1 {
    let mut request = crate::fixture::incoming_request(
        c"1000".as_ptr(),
        4,
        CALL.as_ptr().cast(),
        CALL.len(),
        c"claimed-origin".as_ptr(),
        14,
    );
    request.protocol = 3;
    request
}

fn start_phone(table: &abi::rptadv_host_services_v10, source: &str) {
    assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
    SKIP_TICKER_THREAD.store(true, Ordering::Release);
    assert_eq!(
        unsafe {
            descriptor().start.unwrap()(
                table,
                crate::fixture::control_descriptor(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                source.as_ptr().cast(),
                source.len(),
            )
        },
        0,
    );
}

fn registered() -> bool {
    let engine = selected().unwrap().0;
    engine
        .run(|host| {
            let call = std::str::from_utf8(CALL).unwrap().parse().unwrap();
            Ok(host.phone_source(call).is_some())
        })
        .unwrap()
}

#[test]
fn phone_account_binding_borrowed_authorization_requires_current_account() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_info = Some(phone_info);
    let mut observed = Vec::new();
    for (name, account, expected) in [
        ("missing", None, -1),
        ("disabled", Some(("1000", false)), -1),
        ("wrong-local", Some(("2000", true)), -1),
        ("valid", Some(("1000", true)), 0),
    ] {
        start_phone(&table, &configuration(account));
        let peer = crate::fixture::peer();
        let before = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
        let result = unsafe { descriptor().authorize_incoming.unwrap()(&request(), peer) };
        let borrowed_drops = crate::fixture::PEER_DROPS.load(Ordering::Acquire) - before;
        let has_call = registered();
        unsafe { crate::fixture::release_peer(peer) };
        assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
        assert_eq!(
            crate::fixture::PEER_DROPS.load(Ordering::Acquire) - before,
            1
        );
        observed.push((name, result, expected, borrowed_drops, has_call));
    }
    for (name, result, expected, borrowed_drops, has_call) in &observed {
        assert_eq!(*borrowed_drops, 0, "{name}: borrowed handle was consumed");
        assert!(!has_call, "{name}: authorization created a call owner");
        assert_eq!(
            result, expected,
            "configured account authority: {observed:?}"
        );
    }
}

#[test]
fn phone_account_binding_owned_admission_requires_current_account() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_info = Some(phone_info);
    let mut observed = Vec::new();
    for (name, account, accepted) in [
        ("missing", None, false),
        ("disabled", Some(("1000", false)), false),
        ("wrong-local", Some(("2000", true)), false),
        ("valid", Some(("1000", true)), true),
    ] {
        start_phone(&table, &configuration(account));
        let peer = crate::fixture::peer();
        let before = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
        let result = unsafe { descriptor().incoming.unwrap()(&request(), peer) };
        let owned_drops = crate::fixture::PEER_DROPS.load(Ordering::Acquire) - before;
        let has_call = registered();
        assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
        assert_eq!(
            crate::fixture::PEER_DROPS.load(Ordering::Acquire) - before,
            1
        );
        observed.push((name, result, accepted, owned_drops, has_call));
    }
    for (name, result, accepted, owned_drops, has_call) in &observed {
        assert_eq!(*result, if *accepted { 0 } else { -1 }, "{observed:?}");
        assert_eq!(
            has_call, accepted,
            "{name}: rejected handle retained a call"
        );
        assert_eq!(
            *owned_drops,
            usize::from(!accepted),
            "{name}: exact ownership"
        );
    }
}

#[test]
fn phone_account_binding_owned_admission_rechecks_document_after_authorization() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.peer_info = Some(phone_info);
    let mut observed = Vec::new();
    for (name, next) in [
        ("removed", None),
        ("disabled", Some(("1000", false))),
        ("rebound", Some(("2000", true))),
    ] {
        start_phone(&table, &configuration(Some(("1000", true))));
        let peer = crate::fixture::peer();
        let before = crate::fixture::PEER_DROPS.load(Ordering::Acquire);
        assert_eq!(
            unsafe { descriptor().authorize_incoming.unwrap()(&request(), peer) },
            0,
        );
        assert_eq!(crate::fixture::PEER_DROPS.load(Ordering::Acquire), before);
        let next = configuration(next);
        assert_eq!(
            unsafe { descriptor().reload.unwrap()(next.as_ptr().cast(), next.len()) },
            0,
            "fixture reload must publish before the owned admission",
        );
        let result = unsafe { descriptor().incoming.unwrap()(&request(), peer) };
        let owned_drops = crate::fixture::PEER_DROPS.load(Ordering::Acquire) - before;
        let has_call = registered();
        assert_eq!(unsafe { descriptor().stop.unwrap()() }, 0);
        assert_eq!(
            crate::fixture::PEER_DROPS.load(Ordering::Acquire) - before,
            1
        );
        observed.push((name, result, owned_drops, has_call));
    }
    for (name, result, owned_drops, has_call) in &observed {
        assert_eq!(*result, -1, "current-document recheck: {observed:?}");
        assert_eq!(*owned_drops, 1, "{name}: owned rejection must destroy once");
        assert!(!has_call, "{name}: stale authorization retained a call");
    }
}
