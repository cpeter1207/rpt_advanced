//! Pending phone calls have private control ownership, never a radio-peer slot.
use super::*;
use crate::management::native::AccountCredentials;
use rpt_advanced_core::command::LinkAction;
use rpt_advanced_core::config::resources::{
    candidate::ResourceScope, profiles::credentials::CredentialSlot,
};

fn mailbox_host_with(media: NativeMediaPreparer) -> Host {
    let configuration = configuration(
        "usb",
        &[(
            Kind::Mailbox,
            "100",
            json!({"first_name":"Chris","callsign":"KG0BP"}),
        )],
    );
    let port = ResourceScope::Port(LOCAL.parse().unwrap());
    let mailbox = configuration.runtime().scopes[&port].mailboxes.effective["100"]
        .binding
        .id;
    let version = "ff000000-0000-4000-8000-000000000001".parse().unwrap();
    let pin = b"$argon2id$v=19$m=19456,t=2,p=1$c2l4dGVlbi1ieXRlLXNhbHQ$JJvcFomwgumB44bmQdOCEbN00saqrIis8c6S0WG6Kzw";
    let credentials = AccountCredentials::fixture(&[(mailbox, CredentialSlot::Pin, version, pin)]);
    Host::fixture_start_with_credentials(
        configuration,
        media,
        services(),
        Instant::now(),
        clock(),
        None,
        credentials,
    )
    .unwrap()
}

fn mailbox_host() -> Host {
    mailbox_host_with(media())
}

// These command-owner tests exercise private mailbox and prompt lifecycle below
// transport admission. Network and account qualification have independent native fixtures.
fn prepared_login(host: &mut Host, phone: PeerIo) -> Result<(), RuntimeError> {
    let source = host
        .phone_calls
        .admit(phone, clock().now_ms)
        .map_err(|_| RuntimeError::Rejected)?;
    let reply = host.phone_reply(&source).ok_or(RuntimeError::Rejected)?;
    if let Err(error) = host.prepare_login_prompt(
        &source,
        &reply,
        rpt_advanced_core::messages::PhoneAuthenticationPrompt::Login,
    ) {
        host.retire_phone(&source);
        return Err(error);
    }
    Ok(())
}

#[test]
fn accountless_phone_metadata_cannot_allocate_a_call_or_login_prompt() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    assert!(matches!(
        host.incoming_peer(phone, "203.0.113.9", clock()),
        Err(RuntimeError::Rejected)
    ));
    assert!(host.phone_source(call).is_none());
    assert!(host.peers.is_empty());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(host.fixture_stop(10));
}

#[test]
fn a_real_mailbox_and_pin_cannot_grant_an_unregistered_phone_authority() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    prepared_login(&mut host, phone).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    for pin in ["9999", "1234"] {
        assert_eq!(
            host.authenticate_phone(&source, &reply, "100", pin, clock()),
            Err(RuntimeError::Rejected)
        );
    }
    assert!(!reply.phone_authenticated());
    assert!(host.phone_mailbox(&source, &reply).is_none());
    let prompt = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    assert_eq!(prompt.source(), &source);
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    assert!(host.peers.is_empty());
    assert!(state.lock().unwrap().texts.is_empty());
    assert!(state.lock().unwrap().audio.is_empty());
    host.fixture_reload(configuration("usb", &[]), clock())
        .unwrap();
    pump_until(&mut host, |host| host.phone_source(call).is_none());
    assert!(reply.ended());
    assert!(
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .is_err()
    );
    assert!(host.fixture_stop(20));
}

#[test]
fn an_abandoned_login_retires_its_transport_and_reuse_gets_a_new_ingress() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    prepared_login(&mut host, phone).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    assert!(host.retire_phone(&source));
    assert!(reply.ended() && host.phone_source(call).is_none());
    assert_eq!(state.lock().unwrap().drops, 1);
    let (phone, fresh_state) = crate::link::session::tests::phone_peer_on(LOCAL);
    prepared_login(&mut host, phone).unwrap();
    let fresh = host.phone_source(call).unwrap().clone();
    assert_ne!(source.ingress(), fresh.ingress());
    let new_reply = host.phone_reply(&fresh).unwrap();
    let mut expired = clock();
    expired.now_ms = 60_010;
    let retired = host.phone_calls.expire_authentication(expired.now_ms);
    assert_eq!(retired, [new_reply.clone()]);
    assert!(new_reply.ended() && host.phone_source(call).is_none());
    assert_eq!(fresh_state.lock().unwrap().drops, 1);
    assert!(host.fixture_stop(60_020));
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
            .prepare_private_phone_prompt(LOCAL, Prompt::Login, &regular)
            .is_err()
    );
    assert!(
        host.runtime
            .prepare_phone_status(LOCAL, LinkAction::Status, None, clock(), &regular)
            .is_err()
    );
    let (phone, _) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    prepared_login(&mut host, phone).unwrap();
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
            .prepare_private_phone_prompt(LOCAL, Prompt::Login, &reply)
            .is_err()
    );
    assert!(
        host.prepare_login_prompt(&source, &reply, Prompt::Login)
            .is_err()
    );
    assert!(
        host.immediate(
            LOCAL,
            LinkEffect::Telemetry(LinkAction::Status, Some(reply)),
            clock()
        )
        .is_err()
    );
    assert!(host.fixture_stop(20));
}

#[test]
fn a_full_private_outbox_rejects_an_additional_prompt_and_retires_with_its_owner() {
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
    let (phone, state) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    prepared_login(&mut host, phone).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    for _ in 1..16 {
        host.queue_phone_reply(&source, &reply, Box::new(Empty))
            .unwrap();
    }
    assert!(
        host.prepare_login_prompt(
            &source,
            &reply,
            rpt_advanced_core::messages::PhoneAuthenticationPrompt::Authenticated
        )
        .is_err()
    );
    assert!(!reply.phone_authenticated());
    assert!(host.retire_phone(&source));
    assert!(reply.ended() && host.phone_source(call).is_none());
    assert!(host.phone_mailbox(&source, &reply).is_none());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(host.fixture_stop(20));
}

#[test]
fn pending_station_prompts_allow_an_independent_login_without_radio_attachment() {
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
        output: *mut *mut c_void,
    ) -> i32 {
        // SAFETY: the media owner lends its initialized writable stream record.
        unsafe { *output = ptr::null_mut() };
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
    let mut host = mailbox_host_with(media);
    // Declared after Host: unwind releases the provider before Host joins it.
    let resume = Resume(Some(resume_tx));
    let (phone, _) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    prepared_login(&mut host, phone).unwrap();
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
                .prepare_private_phone_prompt(LOCAL, Prompt::Login, &reply)
                .unwrap(),
        );
    }
    assert!(host.retire_phone(&source));
    let (phone, replacement_state) = crate::link::session::tests::phone_peer_on(LOCAL);
    prepared_login(&mut host, phone).unwrap();
    let replacement = host.phone_source(call).unwrap().clone();
    assert_ne!(source.ingress(), replacement.ingress());
    assert!(host.phone_reply(&source).is_none());
    let replacement_reply = host.phone_reply(&replacement).unwrap();
    assert!(
        host.take_phone_reply(&replacement, &replacement_reply)
            .unwrap()
            .is_some()
    );
    assert!(host.retire_phone(&replacement));
    assert!(host.phone_source(call).is_none());
    assert_eq!(replacement_state.lock().unwrap().drops, 1);
    assert!(host.peers.is_empty());
    drop(pending);
    drop(initial);
    drop(resume);
    assert!(host.fixture_stop(20));
    drop(host);
    *GATE.lock().unwrap() = None;
}

#[test]
fn pending_phone_has_no_handshake_topology_or_radio_attachment_and_keeps_the_prompt_private() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (phone, state) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    prepared_login(&mut host, phone).unwrap();
    assert!(host.peers.is_empty());
    assert!(
        host.runtime
            .node(LOCAL)
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
            LOCAL,
            LinkEffect::Telemetry(LinkAction::Status, Some(reply.clone())),
            clock()
        )
        .is_err()
    );
    let prepared = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    assert_eq!(prepared.source(), &source);
    assert!(prepared.current());
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    assert!(
        host.status_text(LOCAL)
            .unwrap()
            .contains("radio.rx.muted=false active=false")
    );

    assert!(
        host.immediate(
            LOCAL,
            LinkEffect::Message {
                selector: "1".into(),
                reply: Some(reply.clone())
            },
            clock()
        )
        .is_err(),
        "unimplemented private message preparation must fail explicitly"
    );
    let (other, rejected) = crate::link::session::tests::phone_peer_on(LOCAL);
    assert!(
        host.attach_peer(LOCAL, "2000", Mode::TRANSCEIVE, other, clock())
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
    assert!(host.fixture_stop(20));
}

#[test]
fn phone_reload_and_unload_retire_the_actual_call_generation() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, _) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    prepared_login(&mut host, phone).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    host.fixture_reload(
        configuration(
            "usb",
            &[(Kind::TxSignaling, "", json!({"ctcss":{"enabled":false}}))],
        ),
        clock(),
    )
    .unwrap();
    pump_until(&mut host, |host| host.phone_source(call).is_none());
    assert!(reply.ended());
    let (phone, _) = crate::link::session::tests::phone_peer_on(LOCAL);
    prepared_login(&mut host, phone).unwrap();
    let fresh = host.phone_source(call).unwrap().clone();
    assert_ne!(fresh.ingress(), source.ingress());
    let reply = host.phone_reply(&fresh).unwrap();
    assert!(host.fixture_stop(20));
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
    let (phone, state) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    prepared_login(&mut host, phone).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    let foreign = rpt_advanced_core::link::PeerSignals::for_phone(source.clone()).reply_target();
    assert!(matches!(
        host.immediate(
            LOCAL,
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
    assert!(host.fixture_stop(20));
}

fn authenticated_command_phone(host: &mut Host) -> (PhoneSource, PeerReply) {
    use crate::phone::PhoneLogin;

    let (phone, _) = crate::link::session::tests::phone_peer_on(LOCAL);
    let source = host.phone_calls.admit(phone, clock().now_ms).unwrap();
    let reply = host.phone_reply(&source).unwrap();
    let port = ResourceScope::Port(LOCAL.parse().unwrap());
    let mailboxes = &host.runtime.modular_configuration().unwrap().scopes[&port].mailboxes;
    let credentials = host.radio_preparation.credentials.as_ref().unwrap();
    assert_eq!(
        host.phone_calls
            .authenticate_scoped(
                &source,
                &reply,
                mailboxes,
                |mailbox| credentials.get(mailbox.binding.id, CredentialSlot::Pin),
                ("100", "1234"),
                clock().now_ms,
            )
            .unwrap(),
        PhoneLogin::Authenticated
    );
    (source, reply)
}

#[test]
fn phone_commands_queue_private_mute_pause_and_failure_results() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (source, reply) = authenticated_command_phone(&mut host);
    assert!(reply.rx_muted());

    let mut now = clock();
    // Time is a valid command, but cannot complete without a civil-time sample.
    now.civil = None;
    let mut missing = Vec::new();
    for (command, muted, expected_reply) in [
        ("*99#", false, true),
        ("*99#", true, true),
        ("*806#", true, true),
        ("*722#", true, true),
        ("*0000000#", true, false),
    ] {
        for digit in command.chars() {
            now.now_ms += 1;
            let outcome = host.runtime.node(LOCAL).unwrap().links().phone_digit(
                &source,
                digit,
                now.now_ms,
                reply.clone(),
            );
            if let Some(outcome) = outcome {
                host.immediate(LOCAL, LinkEffect::Digit(outcome), now)
                    .unwrap();
            }
        }
        assert_eq!(reply.rx_muted(), muted, "{command}");
        let prepared = host.take_phone_reply(&source, &reply).unwrap();
        if expected_reply && prepared.is_none() {
            missing.push(command);
        } else if let Some(prepared) = prepared {
            assert!(expected_reply, "unexpected reply to {command}");
            assert_eq!(prepared.source(), &source);
            assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
        }
    }
    assert!(host.fixture_stop(40));
    assert!(
        missing.is_empty(),
        "missing private replies for {missing:?}"
    );
}

#[test]
fn phone_link_feedback_waits_for_matching_ack_and_fences_reused_calls() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = mailbox_host();
    let (source, reply) = authenticated_command_phone(&mut host);
    let generation = host.runtime.node(LOCAL).unwrap().command_work().unwrap();
    let receipt = || PendingLinkDial {
        phone: Some(PhoneLinkReply {
            reply: reply.clone(),
            generation: Arc::clone(&generation),
            report_failure: true,
        }),
        ..PendingLinkDial::default()
    };

    host.pending_link_dials.insert(23, receipt());
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    host.finish_link_dial(24, LOCAL, "2000", true);
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());
    host.finish_link_dial(23, LOCAL, "2000", true);
    let result = host.take_phone_reply(&source, &reply).unwrap().unwrap();
    assert_eq!(result.source(), &source);
    host.finish_link_dial(23, LOCAL, "2000", true);
    assert!(host.take_phone_reply(&source, &reply).unwrap().is_none());

    host.pending_link_dials.insert(24, receipt());
    assert!(host.retire_phone(&source));
    let (replacement, replacement_reply) = authenticated_command_phone(&mut host);
    assert_eq!(replacement.call_id(), source.call_id());
    assert_ne!(replacement.ingress(), source.ingress());
    host.finish_link_dial(24, LOCAL, "2000", true);
    assert!(!result.current());
    assert!(
        host.take_phone_reply(&replacement, &replacement_reply)
            .unwrap()
            .is_none()
    );
    assert!(host.fixture_stop(40));
}

#[test]
fn removed_node_reload_retires_its_pending_private_call_before_reclaiming_the_node() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut host = host();
    let (phone, state) = crate::link::session::tests::phone_peer_on(LOCAL);
    let call = phone.identity().call_id().unwrap();
    prepared_login(&mut host, phone).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    host.fixture_reload(stations(&[], &[]), clock()).unwrap();
    assert!(host.runtime.node(LOCAL).is_none());
    assert!(reply.ended());
    assert!(host.phone_source(call).is_none());
    assert_eq!(state.lock().unwrap().drops, 1);
    assert!(state.lock().unwrap().bound_radio.is_none());
    pump_until(&mut host, |host| host.runtime.status(20).is_empty());
    assert!(host.fixture_stop(20));
}
