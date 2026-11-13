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
