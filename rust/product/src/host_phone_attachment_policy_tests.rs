//! Real phone grammar dispatch checks both current local policy sides without media.
use super::*;
use crate::media::text::tests::SpeechFixture;
use rpt_advanced_core::{audio::PlaybackOutcome, link::PeerSignals, phone_dtmf::PhoneDtmfAction};

fn policy_document(local: &str, account: &str, target: &str) -> ConfigDocument {
    ConfigDocument::parse(&format!(
        "[{local}]\nradio_channel=usb\nduplex=full\n{target}\n\
         [2000]\nradio_channel=second\nduplex=full\n{target}\n\
         {}{account}\n{}\n[speech]\nvoice=voice.onnx\nlevel_db=-6\n",
        account_configuration(local),
        include_str!("tests/fixtures/phone_mailbox.conf"),
    ))
    .unwrap()
}

fn policy_host(local: &str, account: &str, target: &str) -> (Host, Arc<SpeechFixture>) {
    let speech = Arc::new(SpeechFixture::default());
    let host = Host::start(
        policy_document(local, account, target),
        crate::media::text::tests::preparer(Arc::clone(&speech)),
        services(),
        Instant::now(),
        clock(),
    )
    .unwrap();
    (host, speech)
}

fn finish_prompt(host: &mut Host, source: &PhoneSource, reply: &PeerReply) {
    let mut prompt = host.take_phone_reply(source, reply).unwrap().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut output = [0.0; 960];
    while prompt.outcome() == PlaybackOutcome::Active {
        assert!(Instant::now() < deadline, "private prompt did not complete");
        prompt.render(false, &mut output);
        std::thread::yield_now();
    }
    assert_eq!(prompt.outcome(), PlaybackOutcome::Succeeded);
    assert!(host.take_phone_reply(source, reply).unwrap().is_none());
}

fn feed(host: &mut Host, source: &PhoneSource, reply: &PeerReply, digits: &str) {
    for digit in digits.chars() {
        let actions = host.phone_calls.actions(source, reply, digit, 10).unwrap();
        host.private_phone_actions(source, reply, actions, clock())
            .unwrap();
    }
}

#[test]
fn phone_attachment_policy_real_grammar_requires_both_exact_local_permissions() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // The last bool is permission, not readiness: even true must remain Unavailable.
    let cases = [
        ("missing", "1000", "", "", "*3#", false),
        (
            "empty",
            "1000",
            "attach_local_nodes=\n",
            "phone_allow_accounts=\n",
            "*3#",
            false,
        ),
        (
            "account only",
            "1000",
            "attach_local_nodes=1000\n",
            "",
            "*3#",
            false,
        ),
        (
            "target only",
            "1000",
            "",
            "phone_allow_accounts=provider\n",
            "*3#",
            false,
        ),
        (
            "default",
            "1000",
            "attach_local_nodes=1000\n",
            "phone_allow_accounts=provider\n",
            "*3#",
            true,
        ),
        (
            "explicit",
            "1000",
            "attach_local_nodes=1000\n",
            "phone_allow_accounts=provider\n",
            "*31000#",
            true,
        ),
        (
            "other hosted",
            "1000",
            "attach_local_nodes=1000\t2000\n",
            "phone_allow_accounts=other provider\n",
            "*32000#",
            true,
        ),
        (
            "other denied",
            "1000",
            "attach_local_nodes=1000\n",
            "phone_allow_accounts=provider\n",
            "*32000#",
            false,
        ),
        (
            "exact account",
            "1000",
            "attach_local_nodes=1000\n",
            "phone_allow_accounts=Provider\n",
            "*3#",
            false,
        ),
        (
            "no expansion",
            "1000",
            "attach_local_nodes=*\n",
            "phone_allow_accounts=provider\n",
            "*3#",
            false,
        ),
        (
            "invalid account list",
            "1000",
            "attach_local_nodes=1000 [bad]\n",
            "phone_allow_accounts=provider\n",
            "*3#",
            false,
        ),
        (
            "invalid target list",
            "1000",
            "attach_local_nodes=1000\n",
            "phone_allow_accounts=provider bad!\n",
            "*3#",
            false,
        ),
        (
            "named comma",
            "site,west",
            "attach_local_nodes=site,west\n",
            "phone_allow_accounts=provider\n",
            "*3#",
            true,
        ),
        (
            "comma is literal",
            "site,west",
            "attach_local_nodes=other,site,west\n",
            "phone_allow_accounts=provider\n",
            "*3#",
            false,
        ),
        (
            "unhosted",
            "1000",
            "attach_local_nodes=9999\n",
            "phone_allow_accounts=provider\n",
            "*39999#",
            false,
        ),
    ];
    let mut failures = Vec::new();
    for (name, local, account, target, digits, allowed) in cases {
        let (mut host, speech) = policy_host(local, account, target);
        let (phone, state) = crate::link::session::tests::phone_peer_named(local);
        let id = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
        let source = host.phone_source(id).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        finish_prompt(&mut host, &source, &reply);
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap();
        finish_prompt(&mut host, &source, &reply);
        feed(&mut host, &source, &reply, digits);
        finish_prompt(&mut host, &source, &reply);
        let expected = if allowed {
            "Requested operation is unavailable"
        } else {
            "Command failed"
        };
        let observed = speech.opened.lock().unwrap().last().unwrap().clone();
        if observed != expected {
            failures.push(format!("{name}: expected {expected:?}, got {observed:?}"));
        }
        assert!(reply.phone_authenticated());
        assert!(host.peers.is_empty() && host.operations.is_empty());
        {
            let state = state.lock().unwrap();
            assert!(state.bound_radio.is_none());
            assert!(state.texts.is_empty() && state.digits.is_empty() && state.audio.is_empty());
        }
        // Denial/readiness reporting must not revoke ordinary private control or
        // silently select the requested attachment destination.
        feed(&mut host, &source, &reply, "*4#*70");
        let [(selected, DigitOutcome::Operation(operation))] = host.operations.as_slice() else {
            panic!("one unchanged source-tagged local status operation");
        };
        assert_eq!(selected, local);
        assert_eq!(operation.reply.as_ref(), Some(&reply));
        assert!(host.stop(20));
        await_private_drop(&state);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn phone_attachment_policy_invalid_authority_cannot_receive_a_result() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut failures = Vec::new();
    for kind in ["unauthenticated", "foreign", "ended"] {
        let (mut host, speech) = policy_host(
            "1000",
            "attach_local_nodes=1000\n",
            "phone_allow_accounts=provider\n",
        );
        let (phone, state) = crate::link::session::tests::phone_peer();
        let id = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
        let source = host.phone_source(id).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        finish_prompt(&mut host, &source, &reply);
        if kind != "unauthenticated" {
            host.authenticate_phone(&source, &reply, "100", "1234", clock())
                .unwrap();
            finish_prompt(&mut host, &source, &reply);
        }
        let foreign = PeerSignals::for_phone(source.clone());
        assert!(foreign.authenticate_phone());
        let attempted = if kind == "foreign" {
            foreign.reply_target()
        } else {
            reply.clone()
        };
        if kind == "ended" {
            reply.retire();
        }
        let opened = speech.opened.lock().unwrap().len();
        let result = host.private_phone_actions(
            &source,
            &attempted,
            vec![PhoneDtmfAction::Attach(None)],
            clock(),
        );
        if result.is_ok() {
            failures.push(format!("{kind}: invalid source received a result"));
        }
        if !reply.ended() {
            let queued = host.take_phone_reply(&source, &reply).unwrap();
            if queued.is_some() {
                failures.push(format!("{kind}: queued a response"));
            }
            drop(queued);
        }
        assert!(host.peers.is_empty() && host.operations.is_empty());
        assert!(host.stop(20));
        await_private_drop(&state);
        if speech.opened.lock().unwrap().len() != opened {
            failures.push(format!("{kind}: prepared a response"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn phone_attachment_policy_reload_revokes_old_source_and_rechecks_new_call() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for (account, target) in [
        ("", "phone_allow_accounts=provider\n"),
        ("attach_local_nodes=1000\n", ""),
    ] {
        let (mut host, speech) = policy_host(
            "1000",
            "attach_local_nodes=1000\n",
            "phone_allow_accounts=provider\n",
        );
        let (phone, state) = crate::link::session::tests::phone_peer();
        let id = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
        let source = host.phone_source(id).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        finish_prompt(&mut host, &source, &reply);
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap();
        finish_prompt(&mut host, &source, &reply);
        feed(&mut host, &source, &reply, "*3#");
        finish_prompt(&mut host, &source, &reply);
        assert_eq!(
            speech.opened.lock().unwrap().last().unwrap(),
            "Requested operation is unavailable"
        );
        host.reload(policy_document("1000", account, target), clock())
            .unwrap();
        pump_until(&mut host, |host| host.phone_source(id).is_none());
        await_private_drop(&state);
        assert!(reply.ended());

        let (fresh, fresh_state) = crate::link::session::tests::phone_peer();
        assert_eq!(fresh.identity().call_id().unwrap(), id);
        host.incoming_peer(fresh, "203.0.113.9", clock()).unwrap();
        let fresh_source = host.phone_source(id).unwrap().clone();
        let fresh_reply = host.phone_reply(&fresh_source).unwrap();
        assert_ne!(source.ingress(), fresh_source.ingress());
        finish_prompt(&mut host, &fresh_source, &fresh_reply);
        host.authenticate_phone(&fresh_source, &fresh_reply, "100", "1234", clock())
            .unwrap();
        finish_prompt(&mut host, &fresh_source, &fresh_reply);
        assert!(
            host.private_phone_actions(
                &source,
                &reply,
                vec![PhoneDtmfAction::Attach(None)],
                clock()
            )
            .is_err()
        );
        assert!(
            host.take_phone_reply(&fresh_source, &fresh_reply)
                .unwrap()
                .is_none()
        );
        feed(&mut host, &fresh_source, &fresh_reply, "*3#");
        finish_prompt(&mut host, &fresh_source, &fresh_reply);
        assert_eq!(
            speech.opened.lock().unwrap().last().unwrap(),
            "Command failed"
        );
        assert!(fresh_reply.phone_authenticated());
        assert!(host.peers.is_empty() && host.operations.is_empty());
        assert!(fresh_state.lock().unwrap().bound_radio.is_none());
        assert!(host.stop(20));
        await_private_drop(&fresh_state);
    }
}

#[test]
fn prepared_phone_attachment_uses_a_typed_slot_and_preserves_private_ownership() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut host, _) = policy_host(
        "1000",
        "attach_local_nodes=1000\n",
        "phone_allow_accounts=provider\n",
    );
    let (phone, state) = crate::link::session::tests::phone_source_peer_at_rate(8000);
    let id = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(id).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    finish_prompt(&mut host, &source, &reply);
    host.authenticate_phone(&source, &reply, "100", "1234", clock())
        .unwrap();
    finish_prompt(&mut host, &source, &reply);
    host.reconcile_private_collections(clock());
    feed(&mut host, &source, &reply, "*3#");
    finish_prompt(&mut host, &source, &reply);
    host.reconcile_private_collections(clock());
    assert_eq!(host.phone_calls.attachments("1000").len(), 1);
    assert!(host.peers.is_empty(), "phone has no public node identity");
    assert!(host.phone_calls.media_refresh_nodes().is_empty());
    host.attach_phone(&source, &reply, "1000", clock()).unwrap();
    assert_eq!(host.phone_calls.attachments("1000").len(), 1);
    assert!(host.attach_phone(&source, &reply, "2000", clock()).is_err());
    assert_eq!(host.phone_calls.attachments("1000").len(), 1);
    host.phone_calls.retire_media(&source, &reply).unwrap();
    assert_eq!(host.phone_calls.media_refresh_nodes(), ["1000"]);
    host.reconcile_private_collections(clock());
    assert!(host.phone_calls.media_refresh_nodes().is_empty());
    assert!(!reply.ended());
    host.retire_phone(&source);
    assert!(host.phone_calls.attachments("1000").is_empty());
    assert!(reply.ended());
    assert!(host.stop(20));
    await_private_drop(&state);
}

#[test]
fn phone_attachment_reuses_ordinary_transport_without_losing_either_owner() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for mode in [Mode::TRANSCEIVE, Mode::MONITOR] {
        let (mut host, _) = policy_host(
            "1000",
            "attach_local_nodes=1000\n",
            "phone_allow_accounts=provider\n",
        );
        let ordinary = attach(&mut host, "3000", LinkAction::Transceive, mode);
        let (phone, state) = crate::link::session::tests::phone_source_peer_at_rate(8000);
        let id = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
        let source = host.phone_source(id).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        finish_prompt(&mut host, &source, &reply);
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap();
        finish_prompt(&mut host, &source, &reply);
        host.reconcile_private_collections(clock());
        let result = host.attach_phone(&source, &reply, "1000", clock());
        assert!(
            result.is_ok(),
            "authorized attachment reuses the ordinary negotiated transport"
        );
        assert_eq!(host.phone_calls.attachments("1000").len(), 1);
        assert!(!reply.ended() && reply.phone_authenticated());
        assert_eq!(host.peers.len(), 1);
        assert_eq!(ordinary.lock().unwrap().drops, 0);
        assert!(host.stop(20));
        await_private_drop(&state);
    }
}

#[test]
fn rf_operation_results_reach_only_current_attached_phone() {
    use rpt_advanced_core::messages::OperationResult;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (mut host, _) = policy_host(
        "1000",
        "attach_local_nodes=1000\n",
        "phone_allow_accounts=provider\n",
    );
    let (phone, state) = crate::link::session::tests::phone_source_peer_at_rate(8000);
    let call = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(call).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    finish_prompt(&mut host, &source, &reply);
    host.authenticate_phone(&source, &reply, "100", "1234", clock())
        .unwrap();
    finish_prompt(&mut host, &source, &reply);
    host.attach_phone(&source, &reply, "1000", clock()).unwrap();
    for result in [OperationResult::Completed, OperationResult::Unavailable] {
        host.node_operation_result("1000", None, result).unwrap();
        finish_prompt(&mut host, &source, &reply);
    }
    host.phone_calls.detach_media(&source, &reply);
    host.node_operation_result("1000", None, OperationResult::Completed)
        .unwrap();
    assert!(
        host.take_phone_reply(&source, &reply).unwrap().is_none(),
        "an authenticated but detached call does not receive RF operation results"
    );
    assert!(probe_public_phone_media(&mut host, "1000"));
    assert!(host.stop(20));
    await_private_drop(&state);
}

#[test]
fn retired_attachment_snapshot_cannot_suppress_rf_operation_result() {
    use rpt_advanced_core::messages::OperationResult;
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for explicit_caller in [false, true] {
        let (mut host, _) = policy_host(
            "1000",
            "attach_local_nodes=1000\n",
            "phone_allow_accounts=provider\n",
        );
        let (phone, state) = crate::link::session::tests::phone_source_peer_at_rate(8000);
        let call = phone.identity().call_id().unwrap();
        host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
        let source = host.phone_source(call).unwrap().clone();
        let reply = host.phone_reply(&source).unwrap();
        finish_prompt(&mut host, &source, &reply);
        host.authenticate_phone(&source, &reply, "100", "1234", clock())
            .unwrap();
        finish_prompt(&mut host, &source, &reply);
        host.attach_phone(&source, &reply, "1000", clock()).unwrap();
        let retired = reply.clone();
        BEFORE_OPERATION_RESULT
            .with_borrow_mut(|hook| *hook = Some(Box::new(move || retired.retire())));
        let delivered = host.node_operation_result(
            "1000",
            explicit_caller.then_some(&reply),
            OperationResult::Completed,
        );
        assert_eq!(delivered.is_ok(), !explicit_caller);
        assert!(reply.ended());
        assert_eq!(
            probe_public_phone_media(&mut host, "1000"),
            !explicit_caller,
            "RF-origin delivery survives a retired listener; retired explicit authority stays private"
        );
        assert!(host.stop(20));
        await_private_drop(&state);
    }
}

#[test]
fn phone_source_gap_rearm_preserves_buffered_ordinary_audio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, _) = policy_host(
        "1000",
        "attach_local_nodes=1000\n",
        "phone_allow_accounts=provider\n",
    );
    let ordinary = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let (phone, state) = crate::link::session::tests::phone_source_peer_at_rate(8000);
    state.lock().unwrap().continuous_async = true;
    let id = phone.identity().call_id().unwrap();
    host.incoming_peer(phone, "203.0.113.9", clock()).unwrap();
    let source = host.phone_source(id).unwrap().clone();
    let reply = host.phone_reply(&source).unwrap();
    finish_prompt(&mut host, &source, &reply);
    host.authenticate_phone(&source, &reply, "100", "1234", clock())
        .unwrap();
    finish_prompt(&mut host, &source, &reply);
    host.reconcile_private_collections(clock());
    host.attach_phone(&source, &reply, "1000", clock()).unwrap();
    pump_until(&mut host, |host| {
        host.runtime
            .node("1000")
            .unwrap()
            .status(10)
            .retiring
            .is_none()
    });
    let lease = Arc::clone(
        &host
            .leases
            .iter()
            .find(|(name, _)| name == "1000")
            .unwrap()
            .1,
    );
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let before = host.peers[0].control.reply_target();
    ordinary
        .lock()
        .unwrap()
        .input
        .extend((0..8).map(|_| crate::link::session::tests::Input::Audio(vec![0.25; 960])));
    pump_until(&mut host, |host| {
        host.peers[0]
            .control
            .snapshot()
            .unwrap()
            .ring
            .available_samples
            == 7680
    });
    let generation = host
        .runtime
        .node("1000")
        .unwrap()
        .status(10)
        .active
        .unwrap();
    state
        .lock()
        .unwrap()
        .input
        .push_back(crate::link::session::tests::Input::Gap(160));
    pump_until(&mut host, |host| {
        host.phone_calls.media_refresh_nodes() == ["1000"]
    });
    host.reconcile_private_collections(clock());
    pump_until(&mut host, |host| {
        // The physical fixture is deliberately stopped: adopt the new graph
        // without rendering, preserving a deterministic queued prefix.
        drop(owners.0.acquire());
        drop(owners.1.acquire());
        let status = host.runtime.node("1000").unwrap().status(10);
        status.active != Some(generation) && status.retiring.is_none()
    });
    let same_source = host.peers[0].control.reply_target() == before;
    let retained = host.peers[0]
        .control
        .snapshot()
        .unwrap()
        .ring
        .available_samples;
    ordinary
        .lock()
        .unwrap()
        .input
        .extend((0..7).map(|_| crate::link::session::tests::Input::Audio(vec![-0.25; 960])));
    pump_until(&mut host, |host| {
        host.peers[0]
            .control
            .snapshot()
            .unwrap()
            .ring
            .available_samples
            == retained + 6720
    });
    // Each side of the rearm is below 260ms. Only retaining both can prime
    // the real common ring. Two distinct signs identify both source intervals.
    let mut rendered = Vec::new();
    for block in 0..24 {
        let mut audio = [0.0; 960];
        let mut current = owners.1.acquire().unwrap();
        let generation = current.id();
        let transmit = current.state();
        transmit
            .adapter
            .process(
                &mut transmit.controller,
                false,
                &mut audio,
                rpt_advanced_core::link::SourcePosition {
                    generation,
                    sample: block * 960,
                    stream: 1,
                    born_ns: Some(host.epoch.elapsed().as_nanos() as u64),
                },
            )
            .unwrap();
        if transmit.adapter.rf_programme().active {
            rendered.extend(audio);
        }
        drop(current);
        host.pump(clock()).unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    let first_positive = rendered.iter().position(|value| *value > 0.1);
    let first_negative = rendered.iter().position(|value| *value < -0.1);
    let producer_live = !before.ended();
    assert!(reply.phone_authenticated() && !reply.ended());
    lease.lock().unwrap().owners = Some(owners);
    assert!(host.stop(20));
    await_private_drop(&state);
    assert!(
        same_source
            && producer_live
            && retained == 7680
            && first_positive
                .zip(first_negative)
                .is_some_and(|(first, second)| first < second),
        "phone-only rearm lost ordinary input: same_source={same_source}, producer_live={producer_live}, retained={retained}, first_positive={first_positive:?}, first_negative={first_negative:?}",
    );
}

fn retained_candidate(host: &mut Host) -> PreparedAdapter<Audio, GenerationControl> {
    let settings = host.runtime.settings("1000").unwrap().clone();
    for _ in 0..200 {
        match prepared(
            "1000",
            &settings,
            &host.peers,
            Some((
                &mut host.phone_calls,
                host.peer_io.source_clock_epoch(),
                true,
            )),
            media_worker(&mut host.media_workers, "1000").unwrap(),
            &mut host.text,
            host.services,
        ) {
            Ok(candidate) => return candidate,
            Err(RuntimeError::Busy) => std::thread::sleep(Duration::from_millis(1)),
            Err(error) => panic!("candidate failed: {error:?}"),
        }
    }
    panic!("prepared dispatcher did not release a slot");
}

#[test]
fn phone_rearm_candidate_drop_and_busy_preserve_live_ordinary_owners() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, _) = policy_host("1000", "", "");
    let ordinary = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    let lease = Arc::clone(
        &host
            .leases
            .iter()
            .find(|(name, _)| name == "1000")
            .unwrap()
            .1,
    );
    assert!(lease.lock().unwrap().quiesce());
    let mut owners = lease.lock().unwrap().owners.take().unwrap();
    let reply = host.peers[0].control.reply_target();
    ordinary
        .lock()
        .unwrap()
        .input
        .push_back(crate::link::session::tests::Input::Audio(vec![0.25; 960]));
    pump_until(&mut host, |host| {
        host.peers[0]
            .control
            .snapshot()
            .unwrap()
            .ring
            .available_samples
            == 960
    });
    drop(retained_candidate(&mut host));
    assert!(!reply.ended());
    assert_eq!(
        host.peers[0]
            .control
            .snapshot()
            .unwrap()
            .ring
            .available_samples,
        960
    );
    let held = owners.1.acquire().unwrap();
    let candidate = retained_candidate(&mut host);
    let mut called = false;
    assert_eq!(
        host.runtime
            .replace_adapter_transferring("1000", candidate, 20, |_, _| {
                called = true;
                true
            }),
        Err(RuntimeError::Busy)
    );
    assert!(!called, "protected callback must reject before transfer");
    drop(held);
    let candidate = retained_candidate(&mut host);
    assert_eq!(
        host.runtime
            .replace_adapter_transferring("1000", candidate, 21, |_, _| false),
        Err(RuntimeError::Preparation)
    );
    assert!(
        owners.1.acquire().is_some(),
        "refused transfer restores the old active graph"
    );
    assert_eq!(host.peers[0].control.reply_target(), reply);
    assert!(!reply.ended());
    assert_eq!(
        host.peers[0]
            .control
            .snapshot()
            .unwrap()
            .ring
            .available_samples,
        960
    );
    lease.lock().unwrap().owners = Some(owners);
    assert!(host.stop(22));
}

#[test]
fn phone_rearm_acknowledgment_requires_exact_new_graph_and_connection() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, _) = policy_host("1000", "", "");
    let graph = host
        .runtime
        .node("1000")
        .unwrap()
        .status(10)
        .active
        .unwrap();
    let control = host.runtime.adapter_control_mut("1000", graph).unwrap();
    control.awaiting_outbound.push(("3000".into(), 17));
    for (remote, connection, received, active) in [
        ("3000", 17, graph, graph + 1),
        ("3000", 18, graph + 1, graph + 1),
        ("4000", 17, graph + 1, graph + 1),
    ] {
        control.acknowledge_outbound(remote, connection, received, active);
        assert_eq!(control.awaiting_outbound.len(), 1);
    }
    control.acknowledge_outbound("3000", 17, graph + 1, graph + 1);
    assert!(control.awaiting_outbound.is_empty());
    control.acknowledge_outbound("3000", 17, graph + 1, graph + 1);
    control.awaiting_outbound.push(("3000".into(), 17));
    control.acknowledge_outbound("3000", 17, graph + 1, graph + 2);
    assert_eq!(
        control.awaiting_outbound.len(),
        1,
        "old ACK cannot clear a later graph"
    );
    control.awaiting_outbound.clear();
    assert!(host.stop(20));
}

#[test]
fn phone_rearm_stale_redirect_cannot_reach_a_same_named_successor() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (mut host, _) = policy_host("1000", "", "");
    let (old_io, _) = crate::link::session::tests::peer(48000);
    let (_, old_output) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
    let (old_session, old_control) =
        PeerSession::prepare(old_io, old_output, "1000", "3000", true).unwrap();
    let old_connection = old_control.connection_generation();
    drop(old_session);
    drop(old_control);
    let ordinary = attach(&mut host, "3000", LinkAction::Transceive, Mode::TRANSCEIVE);
    assert_ne!(
        host.peers[0].control.connection_generation(),
        old_connection
    );
    let graph = host
        .runtime
        .node("1000")
        .unwrap()
        .status(10)
        .active
        .unwrap();
    let (_, outbound) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
    let control = host.runtime.adapter_control_mut("1000", graph).unwrap();
    // Model an older connection's prepared command still pending when the same
    // remote name belongs to a new transport. Only the captured identity differs.
    control.redirects.push((
        "3000".into(),
        PeerCommand::RedirectOutbound {
            generation: 0,
            connection: old_connection,
            outbound,
        },
    ));
    control
        .awaiting_outbound
        .push(("3000".into(), old_connection));
    host.pump(clock()).unwrap();
    host.peers[0]
        .control
        .send(PeerCommand::Text {
            text: CString::new("successor remains usable").unwrap(),
            advisory: false,
        })
        .ok()
        .unwrap();
    pump_until(&mut host, |_| {
        let state = ordinary.lock().unwrap();
        state.drops != 0
            || state
                .texts
                .iter()
                .any(|text| text == b"successor remains usable")
    });
    let (dropped, marker) = {
        let state = ordinary.lock().unwrap();
        (
            state.drops,
            state
                .texts
                .iter()
                .any(|text| text == b"successor remains usable"),
        )
    };
    let peer_count = host.peers.len();
    assert!(host.stop(20));
    assert!(
        dropped == 0 && marker && peer_count == 1,
        "stale redirect reached successor: drops={dropped}, marker={marker}, peers={peer_count}"
    );
}
