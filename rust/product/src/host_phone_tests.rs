//! Pending phone calls have private control ownership, never a radio-peer slot.
use super::*;
use crate::phone::PhoneLogin;
use rpt_advanced_core::command::LinkAction;

fn mailbox_host() -> Host {
    Host::start(
        document("usb", include_str!("tests/fixtures/phone_mailbox.conf")),
        media(),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap()
}

#[test]
fn provider_admission_prepares_an_unauthenticated_private_login_prompt() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    assert!(!reply.phone_authenticated());
    assert!(
        host.take_phone_reply(&source, &reply).unwrap().is_some(),
        "login must prepare privately before any command or attachment"
    );
    assert!(host.peers.is_empty());
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().audio.is_empty());
    assert!(host.stop(20));
}

#[test]
fn real_mailbox_verification_keeps_prompts_private_and_does_not_unlock_node_commands() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    assert_eq!(
        host.authenticate_phone(&source, &reply, "100", "9999", clock())
            .unwrap(),
        PhoneLogin::Retry
    );
    assert!(!reply.phone_authenticated());
    assert!(host.phone_mailbox(&source, &reply).is_none());
    assert_eq!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap(),
        PhoneLogin::Authenticated
    );
    assert_eq!(
        host.phone_mailbox(&source, &reply).unwrap().callsign,
        "KG0BP"
    );
    assert!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .is_err()
    );
    let mut prompts = 0;
    while let Some(prepared) = host.take_phone_reply(&source, &reply).unwrap() {
        assert_eq!(prepared.source(), &source);
        assert!(prepared.current());
        prompts += 1;
    }
    assert_eq!(
        prompts, 3,
        "login, generic retry and success all use the exact call outbox"
    );
    let links = host.runtime.node("1000").unwrap().links();
    assert!(
        "*804#"
            .chars()
            .filter_map(|digit| links.phone_digit(&source, digit, 10, reply.clone()))
            .next()
            .is_none(),
        "mailbox login never grants node unlock"
    );
    assert!(
        "*70#"
            .chars()
            .filter_map(|digit| links.phone_digit(&source, digit, 10, reply.clone()))
            .any(|outcome| matches!(outcome, DigitOutcome::Operation(_))),
        "unprotected commands are available after login"
    );
    assert!(host.peers.is_empty());
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().audio.is_empty());
    host.reload(document("usb", ""), clock()).unwrap();
    pump_until(&mut host, |host| host.phone_source(call).is_none());
    assert!(reply.ended());
    assert!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .is_err()
    );
    assert!(host.stop(20));
}

#[test]
fn exhausted_or_abandoned_login_retires_the_actual_provider_call() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    for expected in [PhoneLogin::Retry, PhoneLogin::Retry, PhoneLogin::Closed] {
        assert_eq!(
            host.authenticate_phone(&source, &reply, "unknown", "1234", clock())
                .unwrap(),
            expected
        );
    }
    assert!(reply.ended() && host.phone_source(call).is_none());
    assert_eq!(state.lock().unwrap().drops, 1);
    let (phone, fresh_state) = crate::link::session::tests::phone_peer();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let fresh = host.phone_source(call).unwrap().clone();
    assert_ne!(source.ingress(), fresh.ingress());
    let new_reply = host.phone_reply(&fresh).unwrap();
    let mut expired = clock();
    expired.now_ms = 60_010;
    host.pump(expired).unwrap();
    assert!(new_reply.ended() && host.phone_source(call).is_none());
    assert_eq!(fresh_state.lock().unwrap().drops, 1);
    assert!(host.stop(60_020));
}

#[test]
fn private_login_preparation_rejects_node_and_retired_tokens_without_rf_fallback() {
    use rpt_advanced_core::{link::PeerSignals, messages::PhoneAuthenticationPrompt as Prompt};
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let regular = PeerSignals::default().reply_target();
    assert!(
        host.runtime
            .prepare_private_phone_prompt("1000", Prompt::Login, &regular)
            .is_err()
    );
    assert!(
        host.runtime
            .prepare_phone_status("1000", LinkAction::Status, None, clock(), &regular)
            .is_err()
    );
    let (phone, _) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    assert!(matches!(
        host.runtime
            .prepare_private_phone_prompt("missing", Prompt::Login, &reply),
        Err(RuntimeError::MissingNode)
    ));
    assert!(host.retire_phone(&source));
    assert!(
        host.runtime
            .prepare_private_phone_prompt("1000", Prompt::Login, &reply)
            .is_err()
    );
    assert!(
        host.prepare_login_prompt(&source, &reply, Prompt::Login)
            .is_err()
    );
    assert!(
        host.immediate(
            "1000",
            LinkEffect::Telemetry(LinkAction::Status, Some(reply)),
            clock()
        )
        .is_err()
    );
    assert!(host.stop(20));
}

#[test]
fn failed_private_success_prompt_retires_login_authority_and_transport() {
    use rpt_advanced_core::audio::{PcmRead, PcmStreamReader};
    struct Empty;
    impl PcmStreamReader for Empty {
        fn render(&mut self, _: &mut [f32]) -> PcmRead {
            PcmRead::Finished
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    for _ in 1..16 {
        host.queue_phone_reply(&source, &reply, Box::new(Empty))
            .unwrap();
    }
    assert!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .is_err()
    );
    assert!(reply.ended() && host.phone_source(call).is_none());
    assert!(host.phone_mailbox(&source, &reply).is_none());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(host.stop(20));
}

#[test]
fn incoming_prompt_backpressure_reclaims_the_provider_call_without_attachment() {
    use crate::media::SpeechDescriptor;
    use rpt_advanced_core::messages::PhoneAuthenticationPrompt as Prompt;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    struct Gate {
        entered: mpsc::SyncSender<()>,
        resume: Mutex<mpsc::Receiver<()>>,
        once: AtomicBool,
    }
    static GATE: Mutex<Option<Arc<Gate>>> = Mutex::new(None);
    struct Resume(Option<mpsc::SyncSender<()>>);
    impl Drop for Resume {
        fn drop(&mut self) {
            if let Some(resume) = self.0.take() {
                let _ = resume.try_send(());
            }
        }
    }
    unsafe extern "C" fn open(
        _: *const c_void,
        _: *const abi::rptadv_media_speech_request,
        _: *const abi::rptadv_media_cancellation,
        output: *mut abi::rptadv_media_stream,
    ) -> i32 {
        // SAFETY: the media owner lends its initialized writable stream record.
        unsafe {
            *output = abi::rptadv_media_stream {
                handle: ptr::null_mut(),
                sample_rate_hz: 0,
            }
        };
        let gate = GATE.lock().unwrap().as_ref().unwrap().clone();
        if !gate.once.swap(true, Ordering::AcqRel) {
            let _ = gate.entered.try_send(());
            let _ = gate
                .resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
        }
        -6
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (entered_tx, entered) = mpsc::sync_channel(1);
    let (resume_tx, resume) = mpsc::sync_channel(1);
    *GATE.lock().unwrap() = Some(Arc::new(Gate {
        entered: entered_tx,
        resume: Mutex::new(resume),
        once: AtomicBool::new(false),
    }));
    // SAFETY: the real complete speech table is copied; only its replaceable
    // open boundary is controlled, with context destruction still delegated.
    let mut speech = unsafe {
        rptadv_speech_adapter::rptadv_speech_adapter_descriptor()
            .cast::<SpeechDescriptor>()
            .read()
    };
    speech.open_speech = Some(open);
    // SAFETY: both process-lifetime descriptor tables and reaper callbacks outlive Host.
    let media = unsafe {
        NativeMediaPreparer::from_descriptors(
            rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
            Box::leak(Box::new(speech)),
            Path::new("/usr/bin/ffmpeg"),
            Path::new("/usr/bin/piper"),
            1000,
            (reaper, reaper),
        )
    }
    .unwrap();
    let mut host = Host::start(
        document("usb", include_str!("tests/fixtures/phone_mailbox.conf")),
        media,
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    // Declared after Host: unwind releases the provider before Host joins it.
    let resume = Resume(Some(resume_tx));
    let (phone, _) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    let mut initial = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    initial.render(false, &mut [0.0; 64]);
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("actual speech provider entered");
    let mut pending = Vec::new();
    for _ in 0..4 {
        pending.push(
            host.runtime
                .prepare_private_phone_prompt("1000", Prompt::Login, &reply)
                .unwrap(),
        );
    }
    assert!(host.retire_phone(&source));
    let (phone, rejected) = crate::link::session::tests::phone_peer();
    assert_eq!(
        host.incoming_peer(phone, "203.0.113.9", clock()),
        Err(RuntimeError::Preparation)
    );
    assert!(host.phone_source(call).is_none());
    assert_eq!(rejected.lock().unwrap().drops, 1);
    assert!(host.peers.is_empty());
    drop(pending);
    drop(initial);
    drop(resume);
    assert!(host.stop(20));
    drop(host);
    *GATE.lock().unwrap() = None;
}

#[test]
fn pending_phone_has_no_handshake_topology_or_radio_attachment_and_queues_only_private_status() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    assert!(host.peers.is_empty());
    assert!(
        host.runtime
            .node("1000")
            .unwrap()
            .links()
            .manager()
            .snapshot()
            .is_empty()
    );
    let source = host.phone_source(call).unwrap().clone();
    assert_eq!(source.account_generation(), 1);
    assert_eq!(source.call_generation(), 2);
    assert_ne!(source.ingress(), 0);
    let reply = host.phone_reply(&source).unwrap();
    assert!(
        host.immediate(
            "1000",
            LinkEffect::Telemetry(LinkAction::Status, Some(reply.clone())),
            clock()
        )
        .is_err()
    );
    assert_eq!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap(),
        PhoneLogin::Authenticated
    );
    while host.take_phone_reply(&source, &reply).unwrap().is_some() {}
    host.immediate(
        "1000",
        LinkEffect::Telemetry(LinkAction::Status, Some(reply.clone())),
        clock(),
    )
    .unwrap();
    let prepared = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    assert_eq!(prepared.source(), &source);
    assert!(prepared.current());
    assert!(
        host.immediate(
            "1000",
            LinkEffect::Message {
                selector: "1".into(),
                reply: Some(reply.clone())
            },
            clock()
        )
        .is_err(),
        "unimplemented private message preparation must fail explicitly"
    );
    let (other, rejected) = crate::link::session::tests::phone_peer();
    assert!(
        host.attach_peer("1000", "2000", Mode::TRANSCEIVE, other, clock())
            .is_err()
    );
    assert!(rejected.lock().unwrap().bound_radio.is_none());
    assert!(host.retire_phone(&source));
    assert!(reply.ended());
    assert!(!prepared.current());
    assert!(host.phone_source(call).is_none());
    assert!(!host.retire_phone(&source));
    let state = state.lock().unwrap();
    assert!(
        state.texts.is_empty(),
        "no ASL protocol text may reach a phone"
    );
    assert!(state.audio.is_empty());
    assert!(state.bound_radio.is_none());
    assert_eq!(state.drops, 1);
    drop(state);
    assert!(host.stop(20));
}

#[test]
fn phone_reload_and_unload_retire_the_actual_call_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, _) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    host.reload(document("usb", "ctcss_encode=no\n"), clock())
        .unwrap();
    pump_until(&mut host, |host| host.phone_source(call).is_none());
    assert!(reply.ended());
    let (phone, _) = crate::link::session::tests::phone_peer();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let fresh = host.phone_source(call).unwrap().clone();
    assert_ne!(fresh.ingress(), source.ingress());
    let reply = host.phone_reply(&fresh).unwrap();
    assert!(host.stop(20));
    assert!(reply.ended());
}

#[test]
fn private_prompt_queue_requires_the_exact_live_token_and_never_uses_radio_media() {
    struct Prompt;
    impl rpt_advanced_core::audio::PcmStreamReader for Prompt {
        fn render(&mut self, output: &mut [f32]) -> rpt_advanced_core::audio::PcmRead {
            let count = output.len().min(8);
            output[..count].fill(0.25);
            rpt_advanced_core::audio::PcmRead::FinalSamples(count)
        }
    }
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    let foreign = rpt_advanced_core::link::PeerSignals::for_phone(source.clone()).reply_target();
    assert!(matches!(
        host.immediate(
            "1000",
            LinkEffect::Telemetry(LinkAction::Status, Some(foreign.clone())),
            clock()
        ),
        Err(RuntimeError::Rejected)
    ));
    assert!(
        host.queue_phone_reply(&source, &foreign, Box::new(Prompt))
            .is_err()
    );
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_some());
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    for _ in 0..16 {
        host.queue_phone_reply(&source, &reply, Box::new(Prompt))
            .unwrap();
    }
    assert!(
        host.queue_phone_reply(&source, &reply, Box::new(Prompt))
            .is_err()
    );
    let mut prepared = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    let mut output = [0.0; 16];
    assert_eq!(prepared.render(false, &mut output), 8);
    assert_eq!(&output[..8], &[0.25; 8]);
    assert_eq!(&output[8..], &[0.0; 8]);
    assert!(host.peers.is_empty());
    assert!(state.lock().unwrap().audio.is_empty());
    assert!(state.lock().unwrap().bound_radio.is_none());
    assert!(host.retire_phone(&source));
    assert!(!prepared.current());
    assert!(host.take_phone_reply(&source, &reply).is_err());
    assert!(
        host.queue_phone_reply(&source, &reply, Box::new(Prompt))
            .is_err()
    );
    assert!(host.stop(20));
}

#[test]
fn removed_node_reload_retires_its_pending_private_call_before_reclaiming_the_node() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, state) = crate::link::session::tests::phone_peer();
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "127.0.0.1", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    host.reload(
        ConfigDocument::parse("[general]\nenabled=no\n").unwrap(),
        clock(),
    )
    .unwrap();
    assert!(host.runtime.node("1000").is_none());
    assert!(reply.ended());
    assert!(host.phone_source(call).is_none());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(state.lock().unwrap().bound_radio.is_none());
    pump_until(&mut host, |host| host.runtime.status(20).is_empty());
    assert!(host.stop(20));
}
