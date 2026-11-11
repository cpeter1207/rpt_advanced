//! Private calls own bounded media replies before any radio attachment exists.
use super::*;
use crate::{abi, services::HostServices};
use rpt_advanced_core::{
    audio::{PcmRead, PcmStreamReader, PlaybackOutcome},
    config::{ConfigDocument, PhoneMailbox, resolve_phone_mailboxes},
};
use std::{ffi::c_void, mem::size_of, ptr};

thread_local! {
    static BEFORE_AUTHENTICATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const {
        std::cell::RefCell::new(None)
    };
}

/// One-shot scheduling checkpoint; the real shared owner performs retirement.
pub(super) fn before_authenticate() {
    let callback = BEFORE_AUTHENTICATE.with_borrow_mut(Option::take);
    if let Some(callback) = callback {
        callback();
    }
}

#[test]
fn concurrent_owner_retirement_wins_between_admission_and_authentication_publish() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut calls = PhoneCalls::new(1, 1).unwrap();
    let source = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    let owner_reply = reply.clone();
    let (retire, request) = std::sync::mpsc::channel();
    let (completed, completion) = std::sync::mpsc::channel();
    let owner = std::thread::spawn(move || {
        request.recv().unwrap();
        owner_reply.retire();
        completed.send(()).unwrap();
    });
    BEFORE_AUTHENTICATE.with_borrow_mut(|callback| {
        *callback = Some(Box::new(move || {
            retire.send(()).unwrap();
            completion
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
        }));
    });
    assert!(!reply.ended());
    assert_eq!(
        calls.authenticate(&source, &reply, &records(), "100", "1234", 0),
        Err(Error::Admission)
    );
    owner.join().unwrap();
    assert!(reply.ended());
    assert!(!reply.phone_authenticated());
    assert!(calls.queue_reply(&source, &reply, media()).is_err());
    assert!(calls.take_reply(&source, &reply).is_err());
    assert_eq!(calls.retire(&source), Some(reply));
    assert!(calls.is_empty());
}

fn records() -> Vec<PhoneMailbox> {
    resolve_phone_mailboxes(
        &ConfigDocument::parse(include_str!("tests/fixtures/phone_mailbox.conf")).unwrap(),
    )
    .value
}

#[test]
fn login_collection_bounds_digits_and_time_without_replaying_credentials() {
    for (input, expected_mailbox, expected_pin) in [
        ("100#1234#".to_owned(), "100", "1234"),
        ("#".to_owned(), "", ""),
        ("*41000#".to_owned(), "", ""),
        (format!("{}#", "1".repeat(33)), "", ""),
        (format!("100#{}#", "1".repeat(33)), "", ""),
        ("100#A#".to_owned(), "", ""),
        (
            format!("{}#{}#", "1".repeat(32), "2".repeat(32)),
            "11111111111111111111111111111111",
            "22222222222222222222222222222222",
        ),
    ] {
        let mut collector = LoginCollector::default();
        let result = input
            .chars()
            .filter_map(|digit| collector.digit(digit, 1))
            .last()
            .unwrap();
        let LoginInput::Credentials { mailbox, pin } = result else {
            panic!("completed credentials")
        };
        assert_eq!(
            (mailbox.as_str(), pin.as_str()),
            (expected_mailbox, expected_pin)
        );
        assert!(collector.mailbox.is_empty() && collector.pin.is_empty());
        assert!(!collector.pin_phase && !collector.invalid && collector.last_ms.is_none());
    }
    let mut collector = LoginCollector::default();
    assert!(collector.tick(u64::MAX).is_none());
    collector.digit('1', 1);
    assert!(collector.tick(3000).is_none());
    assert!(matches!(collector.tick(3001), Some(LoginInput::Retry)));
    assert!(collector.mailbox.is_empty());
    collector.digit('1', 10);
    assert!(
        collector.tick(0).is_none(),
        "clock rollback cannot finish credentials"
    );
}

#[test]
fn private_input_helpers_preserve_bounds_and_retire_authenticated_ended_owners() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut calls = PhoneCalls::new(1, 16).unwrap();
    let source = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    assert!(calls.event().is_none());
    assert!(calls.actions(&source, &reply, '*', 0).is_err());
    calls.cancel_grammar(&source, &reply);
    calls.transfer_replies();
    assert!(calls.grammar_timeouts(3000).is_empty());
    assert!(
        calls
            .login_digit(&source, &reply, '1', 0)
            .unwrap()
            .is_none()
    );
    assert!(calls.login_timeouts(2999).is_empty());
    assert_eq!(
        calls.login_timeouts(3000),
        [(source.clone(), reply.clone())]
    );
    calls.login_digit(&source, &reply, '1', 0).unwrap();
    assert!(matches!(
        calls.login_digit(&source, &reply, '2', 3000).unwrap(),
        Some(LoginInput::Retry)
    ));
    calls
        .authenticate(&source, &reply, &records(), "100", "1234", 3000)
        .unwrap();
    assert!(calls.grammar_timeouts(6000).is_empty());
    assert!(calls.login_timeouts(6000).is_empty());
    reply.retire();
    calls.cancel_grammar(&source, &reply);
    assert_eq!(calls.expire_authentication(6000), [reply.clone()]);
    assert!(calls.is_empty());
}

#[test]
fn pending_private_handoff_keeps_the_control_queue_when_the_owner_is_full() {
    let (mut calls, mut session, control, _state, source, reply) = private_session(false, false);
    calls.calls[0].owner = Some(control);
    for _ in 0..16 {
        calls.queue_reply(&source, &reply, media()).unwrap();
    }
    calls.transfer_replies();
    assert!(calls.calls[0].replies.is_empty());
    calls.queue_reply(&source, &reply, media()).unwrap();
    calls.transfer_replies();
    assert_eq!(calls.calls[0].replies.len(), 1);
    use crate::link::session::SerialSession;
    session.step(0).unwrap();
    calls.transfer_replies();
    assert!(calls.calls[0].replies.is_empty());
}

pub(crate) fn private_session(
    async_enabled: bool,
    prepared: bool,
) -> (
    PhoneCalls,
    crate::link::private_phone::PrivatePhoneSession,
    crate::link::private_phone::PrivatePhoneControl,
    std::sync::Arc<std::sync::Mutex<crate::link::session::tests::State>>,
    PhoneSource,
    PeerReply,
) {
    let (io, state) = crate::link::session::tests::phone_with_egress(async_enabled, prepared);
    let mut calls = PhoneCalls::new(1, 16).unwrap();
    let source = calls.admit(io, 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    let io = calls.calls[0].io.take().unwrap();
    let (session, control) =
        crate::link::private_phone::PrivatePhoneSession::prepare(io, reply.clone()).unwrap();
    (calls, session, control, state, source, reply)
}

pub(crate) fn pending_digits(calls: &PhoneCalls, source: &PhoneSource) -> usize {
    calls
        .calls
        .iter()
        .find(|call| &call.source == source)
        .unwrap()
        .owner
        .as_ref()
        .unwrap()
        .pending_digits()
}

fn peer(second: bool) -> PeerIo {
    unsafe extern "C" fn info(
        context: *mut c_void,
        peer: *const c_void,
        output: *mut abi::rptadv_peer_info_v1,
    ) -> i32 {
        unsafe { crate::fixture::peer_info(context, peer, output) };
        let output = unsafe { &mut *output };
        output.protocol = 3;
        output.capabilities = 32;
        output.account_generation = 7;
        output.qso_generation = 9;
        output.remote = abi::rptadv_peer_bytes_v1 {
            data: if context.is_null() {
                b"aabbccddeeff00112233445566778899".as_ptr()
            } else {
                b"112233445566778899aabbccddeeff00".as_ptr()
            },
            length: 32,
        };
        output.provider_account = abi::rptadv_peer_bytes_v1 {
            data: b"provider".as_ptr(),
            length: 8,
        };
        0
    }
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(usize::from(second));
    table.peer_info = Some(info);
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    unsafe { services.peer(crate::fixture::peer()) }.unwrap()
}

fn media() -> Box<dyn PcmStreamReader> {
    struct Empty;
    impl PcmStreamReader for Empty {
        fn render(&mut self, _: &mut [f32]) -> PcmRead {
            PcmRead::Finished
        }
    }
    Box::new(Empty)
}

#[test]
fn unconfigured_mailbox_cannot_authenticate_a_provider_admitted_call() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut calls = PhoneCalls::new(1, 1).unwrap();
    let source = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    let result = calls.authenticate(&source, &reply, &[], "999", "1234", 0);
    assert_eq!(
        result.unwrap(),
        PhoneLogin::Retry,
        "provider admission is not a mailbox/PIN verifier"
    );
    assert!(!reply.phone_authenticated());
}

#[test]
fn mailbox_login_bounds_attempts_and_does_not_transfer_profiles_to_other_calls() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let records = records();
    let mut calls = PhoneCalls::new(2, 2).unwrap();
    let first = calls.admit(peer(false), 100).unwrap();
    let second = calls.admit(peer(true), 100).unwrap();
    let reply = calls.reply(&first).unwrap();
    let other = calls.reply(&second).unwrap();
    assert_eq!(calls.local(&first, &reply), Some("1000"));
    assert!(calls.local(&first, &other).is_none());
    assert!(
        calls
            .authenticate(&first, &other, &records, "100", "1234", 100)
            .is_err()
    );
    for (mailbox, pin, expected) in [
        ("100", "9999", PhoneLogin::Retry),
        ("unknown", "1234", PhoneLogin::Retry),
        ("100", "1234#", PhoneLogin::Closed),
    ] {
        assert_eq!(
            calls
                .authenticate(&first, &reply, &records, mailbox, pin, 101)
                .unwrap(),
            expected
        );
    }
    assert!(reply.ended());
    assert!(
        calls
            .authenticate(&first, &reply, &records, "100", "1234", 102)
            .is_err()
    );
    assert!(!other.phone_authenticated());
    assert_eq!(
        calls
            .authenticate(&second, &other, &records, "100", "1234", 59_999)
            .unwrap(),
        PhoneLogin::Authenticated
    );
    let profile = calls.mailbox(&second, &other).unwrap();
    assert_eq!(
        (&*profile.first_name, &*profile.callsign),
        ("Chris", "KG0BP")
    );
    assert!(calls.mailbox(&first, &other).is_none());
    assert_eq!(calls.expire_authentication(60_100), [reply]);
    assert!(
        other.phone_authenticated(),
        "login timeout does not end an authenticated call"
    );
}

#[test]
fn mailbox_login_deadline_is_inclusive_and_replacement_never_inherits_attempts_or_identity() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let records = records();
    let mut calls = PhoneCalls::new(1, 2).unwrap();
    let source = calls.admit(peer(false), 100).unwrap();
    let reply = calls.reply(&source).unwrap();
    assert_eq!(
        calls
            .authenticate(&source, &reply, &records, "100", "1234", 60_100)
            .unwrap(),
        PhoneLogin::Closed
    );
    calls.retire(&source).unwrap();
    let fresh = calls.admit(peer(false), 60_100).unwrap();
    assert_ne!(source.ingress(), fresh.ingress());
    let new_reply = calls.reply(&fresh).unwrap();
    assert!(
        calls
            .authenticate(&fresh, &reply, &records, "100", "1234", 60_101)
            .is_err()
    );
    assert_eq!(
        calls
            .authenticate(&fresh, &new_reply, &records, "100", "1234", 60_101)
            .unwrap(),
        PhoneLogin::Authenticated
    );
    assert!(calls.expire_authentication(u64::MAX).is_empty());
    calls.retire(&fresh).unwrap();
    let abandoned = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&abandoned).unwrap();
    assert!(calls.expire_authentication(59_999).is_empty());
    reply.retire();
    assert!(calls.local(&abandoned, &reply).is_none());
    assert_eq!(calls.expire_authentication(59_999), [reply.clone()]);
    let abandoned = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&abandoned).unwrap();
    assert_eq!(calls.expire_authentication(60_000), [reply.clone()]);
    assert!(reply.ended() && calls.is_empty());
}

#[test]
fn private_call_reply_queue_is_exact_bounded_and_revoked_on_retirement() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(PhoneCalls::new(0, 1).is_err());
    assert!(PhoneCalls::new(1, 0).is_err());
    let mut calls = PhoneCalls::new(2, 2).unwrap();
    let first = calls.admit(peer(false), 0).unwrap();
    assert!(
        calls.admit(peer(false), 0).is_err(),
        "a duplicate cannot replace its call"
    );
    let second = calls.admit(peer(true), 0).unwrap();
    assert!(
        calls.admit(peer(true), 0).is_err(),
        "transport capacity is bounded"
    );
    let reply = calls.reply(&first).unwrap();
    let other = calls.reply(&second).unwrap();
    assert!(!reply.phone_authenticated());
    assert!(
        calls
            .authenticate(&first, &other, &records(), "100", "1234", 0)
            .is_err()
    );
    calls
        .authenticate(&first, &reply, &records(), "100", "1234", 0)
        .unwrap();
    assert!(reply.phone_authenticated());
    assert!(
        calls
            .authenticate(&first, &reply, &records(), "101", "1234", 0)
            .is_err(),
        "mailbox changes cannot inherit a live source grant"
    );
    assert!(!other.phone_authenticated());
    assert!(calls.queue_reply(&first, &other, media()).is_err());
    calls.queue_reply(&first, &reply, media()).unwrap();
    calls.queue_reply(&first, &reply, media()).unwrap();
    assert!(calls.queue_reply(&first, &reply, media()).is_err());
    let mut prepared = calls.take_reply(&first, &reply).unwrap().unwrap();
    assert_eq!(prepared.source(), &first);
    assert!(prepared.current());
    assert!(calls.take_reply(&first, &other).is_err());
    assert_eq!(calls.retire(&first), Some(reply.clone()));
    assert!(reply.ended());
    assert!(!prepared.current());
    assert_eq!(prepared.render(false, &mut [0.0; 16]), 0);
    assert_eq!(prepared.outcome(), PlaybackOutcome::Cancelled);
    assert!(calls.queue_reply(&first, &reply, media()).is_err());
    assert!(
        calls
            .authenticate(&first, &reply, &records(), "100", "1234", 0)
            .is_err()
    );
    assert!(calls.retire(&first).is_none());
    let replaced = calls.admit(peer(false), 0).unwrap();
    assert_ne!(replaced.ingress(), first.ingress());
    assert!(calls.queue_reply(&replaced, &reply, media()).is_err());
    let fresh = calls.reply(&replaced).unwrap();
    calls.queue_reply(&replaced, &fresh, media()).unwrap();
    let mut prepared = calls.take_reply(&replaced, &fresh).unwrap().unwrap();
    assert_eq!(prepared.render(false, &mut [0.0; 16]), 0);
    assert_eq!(prepared.outcome(), PlaybackOutcome::Succeeded);
    assert_eq!(calls.retire_all().len(), 2);
    assert!(fresh.ended() && other.ended());
    assert!(calls.is_empty());
    assert_eq!(
        size_of::<abi::rptadv_peer_info_v1>(),
        std::mem::offset_of!(abi::rptadv_peer_info_v1, provider_account)
            + size_of::<abi::rptadv_peer_bytes_v1>()
    );
}

#[test]
fn dropping_the_call_owner_revokes_retained_private_replies() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut calls = PhoneCalls::new(1, 1).unwrap();
    let source = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    drop(calls);
    assert!(reply.ended());
}

#[test]
fn ingress_sequence_exhaustion_never_wraps_into_a_reused_source() {
    let sequence = std::sync::atomic::AtomicU64::new(u64::MAX);
    assert_eq!(next_ingress(&sequence), Err(Error::Admission));
    assert_eq!(
        sequence.load(std::sync::atomic::Ordering::Relaxed),
        u64::MAX
    );
}

#[test]
fn retirement_during_private_render_revokes_the_copied_pcm_before_delivery() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    struct Retire(rpt_advanced_core::link::PeerReply);
    impl PcmStreamReader for Retire {
        fn render(&mut self, output: &mut [f32]) -> PcmRead {
            output.fill(0.25);
            self.0.retire();
            PcmRead::FinalSamples(output.len())
        }
    }
    let mut calls = PhoneCalls::new(1, 1).unwrap();
    let source = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    calls
        .queue_reply(&source, &reply, Box::new(Retire(reply.clone())))
        .unwrap();
    let mut prepared = calls.take_reply(&source, &reply).unwrap().unwrap();
    let mut output = [1.0; 16];
    assert_eq!(prepared.render(false, &mut output), 0);
    assert_eq!(output, [0.0; 16]);
    assert_eq!(prepared.outcome(), PlaybackOutcome::Cancelled);
    assert!(calls.queue_reply(&source, &reply, media()).is_err());
    assert!(calls.take_reply(&source, &reply).is_err());
}

#[test]
fn prepared_phone_media_api_requires_current_mailbox_and_serial_owner_identity() {
    use crate::link::{private_phone::PrivatePhoneSession, session::SerialSession};
    let (io, _state) = crate::link::session::tests::phone_source_peer();
    let mut calls = PhoneCalls::new(1, 16).unwrap();
    let source = calls.admit(io, 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    assert!(matches!(
        calls.prepare_media(&source, &reply, None),
        Err(Error::Admission)
    ));
    calls
        .authenticate(&source, &reply, &records(), "100", "1234", 0)
        .unwrap();
    assert!(matches!(
        calls.prepare_media(&source, &reply, None),
        Err(Error::Admission)
    ));
    assert_eq!(calls.retire_media(&source, &reply), Err(Error::Admission));
    let io = calls.calls[0].io.take().unwrap();
    let (mut session, control) = PrivatePhoneSession::prepare(io, reply.clone()).unwrap();
    calls.calls[0].owner = Some(control);
    session.set_source_clock(
        crate::services::SourceTimeClock::new(std::time::Instant::now()).unwrap(),
    );
    let foreign = PeerSignals::for_phone(source.clone()).reply_target();
    assert!(matches!(
        calls.prepare_media(&source, &foreign, None),
        Err(Error::Admission)
    ));
    assert_eq!(calls.retire_media(&source, &foreign), Err(Error::Admission));
    let input = calls.prepare_media(&source, &reply, None).unwrap();
    assert!(!input.active());
    session.step(0).unwrap();
    assert!(input.active());
    assert_eq!(calls.retire_media(&source, &reply), Ok(()));
    assert!(!input.active());
    assert!(!reply.ended());
    let replacement = calls.prepare_media(&source, &reply, None).unwrap();
    session.step(1).unwrap();
    assert!(replacement.active());
    calls.retire(&source).unwrap();
    assert!(!replacement.active());
    assert!(matches!(
        calls.prepare_media(&source, &reply, None),
        Err(Error::Admission)
    ));
    let (io, _) = crate::link::session::tests::phone_source_peer();
    let newer = calls.admit(io, 2).unwrap();
    assert_ne!(source, newer);
    let current = calls.reply(&newer).unwrap();
    assert!(matches!(
        calls.prepare_media(&source, &current, None),
        Err(Error::Admission)
    ));
}
