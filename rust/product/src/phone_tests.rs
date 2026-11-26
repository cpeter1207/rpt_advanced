//! Private calls own bounded media replies before any radio attachment exists.
use super::*;
use crate::{abi, fixture::station, services::HostServices};
use rpt_advanced_core::{
    audio::{PcmRead, PcmStreamReader, PlaybackOutcome},
    config::resources::{
        candidate::{ResourceKind, ResourceScope},
        profiles::mailbox::ScopedMailboxes,
    },
};
use rptadv_peer_support::sealed_records::SecretBytes;
use serde_json::json;
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
        authenticate(&mut calls, &source, &reply, &records(), "100", "1234", 0),
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

pub(crate) const FIXTURE_PIN_PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2l4dGVlbi1ieXRlLXNhbHQ$JJvcFomwgumB44bmQdOCEbN00saqrIis8c6S0WG6Kzw";

fn records() -> ScopedMailboxes {
    let configuration = station::prepare(
        &[(station::LOCAL, "1000", json!({}))],
        &[(
            station::LOCAL,
            ResourceKind::Mailbox,
            "100",
            json!({
                "first_name":"Chris", "callsign":"KG0BP"
            }),
        )],
    );
    let scope = ResourceScope::Port(station::LOCAL.parse().unwrap());
    let profile = &configuration.runtime().scopes[&scope].mailboxes;
    ScopedMailboxes {
        effective: profile.effective.clone(),
        warnings: Vec::new(),
    }
}

fn authenticate(
    calls: &mut PhoneCalls,
    source: &PhoneSource,
    reply: &PeerReply,
    records: &ScopedMailboxes,
    mailbox: &str,
    pin: &str,
    now_ms: u64,
) -> Result<PhoneLogin, Error> {
    let selected = SecretBytes::new(FIXTURE_PIN_PHC.as_bytes().to_vec());
    calls.authenticate_scoped(
        source,
        reply,
        records,
        |_| Some(&selected),
        (mailbox, pin),
        now_ms,
    )
}

fn peer(second: bool) -> PeerIo {
    peer_with_generation(usize::from(second))
}

fn peer_with_generation(mode: usize) -> PeerIo {
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
        let (call_id, generation) = match context.addr() {
            0 => (b"aabbccddeeff00112233445566778899", 9),
            1 => (b"112233445566778899aabbccddeeff00", 10),
            3 => (b"00112233445566778899aabbccddeeff", 11),
            _ => (b"112233445566778899aabbccddeeff00", 9),
        };
        output.qso_generation = generation;
        output.remote = abi::rptadv_peer_bytes_v1 {
            data: call_id.as_ptr(),
            length: call_id.len(),
        };
        output.provider_account = abi::rptadv_peer_bytes_v1 {
            data: b"provider".as_ptr(),
            length: 8,
        };
        0
    }
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    table.context = ptr::without_provenance_mut(mode);
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
    let profiles = ScopedMailboxes {
        effective: Default::default(),
        warnings: Vec::new(),
    };
    let result =
        calls.authenticate_scoped(&source, &reply, &profiles, |_| None, ("999", "1234"), 0);
    assert_eq!(
        result.unwrap(),
        PhoneLogin::Retry,
        "provider admission is not a mailbox/PIN verifier"
    );
    assert!(!reply.phone_authenticated());
}

#[cfg(target_os = "linux")]
#[test]
fn authenticated_call_retains_only_the_public_identity_selected_for_its_scope() {
    use rpt_advanced_core::config::resources::{
        PortKey,
        candidate::{ResourceBinding, ResourceId, ResourceKind, ResourceScope},
        profiles::mailbox::{PublicMailbox, ScopedMailboxes},
    };
    use rptadv_peer_support::sealed_records::SecretBytes;
    use std::{collections::BTreeMap, sync::Arc};

    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let port: ResourceId = "adcbbaff-de64-41a5-94dd-80a3ab259e9c".parse().unwrap();
    let declaration: ResourceId = "b862047f-77d8-4707-9a2a-e464555ab9ef".parse().unwrap();
    let other: ResourceId = "e9fa54ed-753a-4145-adfa-3df479e1843c".parse().unwrap();
    let mut profiles = ScopedMailboxes {
        effective: BTreeMap::new(),
        warnings: Vec::new(),
    };
    profiles.effective.insert(
        PortKey::new("0100"),
        PublicMailbox {
            binding: ResourceBinding {
                id: declaration,
                scope: ResourceScope::Port(port),
                kind: ResourceKind::Mailbox,
            },
            number: rpt_advanced_core::link::MailboxId::new("0100").unwrap(),
            first_name: "Chris".into(),
            callsign: "KG0BP".into(),
        },
    );
    let selected = Arc::new(SecretBytes::new(FIXTURE_PIN_PHC.as_bytes().to_vec()));
    let retained = Arc::downgrade(&selected);
    let mut calls = PhoneCalls::new(1, 1).unwrap();
    let source = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    let mut verify = |owner, mailbox, now| {
        calls
            .authenticate_scoped(
                &source,
                &reply,
                &profiles,
                |record| (record.binding.id == owner).then_some(selected.as_ref()),
                (mailbox, "1234"),
                now,
            )
            .unwrap()
    };
    assert_eq!(verify(declaration, "100", 1), PhoneLogin::Retry);
    assert_eq!(verify(other, "0100", 2), PhoneLogin::Retry);
    assert_eq!(verify(declaration, "0100", 3), PhoneLogin::Authenticated);
    drop(selected);
    drop(profiles);
    assert!(retained.upgrade().is_none());
    let profile: &super::CallerMailbox = calls.mailbox(&source, &reply).unwrap();
    assert_eq!(
        (
            profile.id.as_str(),
            profile.first_name.as_str(),
            profile.callsign.as_str()
        ),
        ("0100", "Chris", "KG0BP")
    );
    assert!(reply.phone_authenticated());
    calls.retire(&source).unwrap();
    assert!(calls.mailbox(&source, &reply).is_none());
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
    assert!(authenticate(&mut calls, &first, &other, &records, "100", "1234", 100).is_err());
    for (mailbox, pin, expected) in [
        ("100", "9999", PhoneLogin::Retry),
        ("unknown", "1234", PhoneLogin::Retry),
        ("100", "1234#", PhoneLogin::Closed),
    ] {
        assert_eq!(
            authenticate(&mut calls, &first, &reply, &records, mailbox, pin, 101).unwrap(),
            expected
        );
    }
    assert!(reply.ended());
    assert!(authenticate(&mut calls, &first, &reply, &records, "100", "1234", 102).is_err());
    assert!(!other.phone_authenticated());
    assert_eq!(
        authenticate(&mut calls, &second, &other, &records, "100", "1234", 59_999).unwrap(),
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
        authenticate(&mut calls, &source, &reply, &records, "100", "1234", 60_100).unwrap(),
        PhoneLogin::Closed
    );
    calls.retire(&source).unwrap();
    let fresh = calls.admit(peer(false), 60_100).unwrap();
    assert_ne!(source.ingress(), fresh.ingress());
    let new_reply = calls.reply(&fresh).unwrap();
    assert!(authenticate(&mut calls, &fresh, &reply, &records, "100", "1234", 60_101).is_err());
    assert_eq!(
        authenticate(
            &mut calls, &fresh, &new_reply, &records, "100", "1234", 60_101
        )
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
        "a duplicate provider call must be rejected"
    );
    let reply = calls.reply(&first).unwrap();
    let other = calls.reply(&second).unwrap();
    assert!(!reply.phone_authenticated());
    assert!(authenticate(&mut calls, &first, &other, &records(), "100", "1234", 0).is_err());
    authenticate(&mut calls, &first, &reply, &records(), "100", "1234", 0).unwrap();
    assert!(reply.phone_authenticated());
    assert!(
        authenticate(&mut calls, &first, &reply, &records(), "101", "1234", 0).is_err(),
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
    assert!(authenticate(&mut calls, &first, &reply, &records(), "100", "1234", 0).is_err());
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
fn private_call_reservation_can_grow_while_preserving_per_call_reply_limits() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut calls = PhoneCalls::new(2, 1).unwrap();
    let first = calls.admit(peer(false), 0).unwrap();
    let second = calls.admit(peer(true), 1).unwrap();
    let first_reply = calls.reply(&first).unwrap();
    let second_reply = calls.reply(&second).unwrap();
    assert_eq!(
        authenticate(
            &mut calls,
            &first,
            &first_reply,
            &records(),
            "100",
            "1234",
            2
        ),
        Ok(PhoneLogin::Authenticated)
    );
    for (source, reply) in [(&first, &first_reply), (&second, &second_reply)] {
        calls.queue_reply(source, reply, media()).unwrap();
        assert_eq!(
            calls.queue_reply(source, reply, media()),
            Err(Error::Allocation)
        );
    }

    let candidate = peer_with_generation(3);
    let new_call = candidate.identity().call_id().unwrap();
    for existing in [&first, &second] {
        assert_ne!(new_call, existing.call_id());
        assert_eq!(candidate.phone_account(), Some(existing.account()));
        let (account_generation, call_generation) = candidate.generations();
        assert_eq!(account_generation, existing.account_generation());
        assert_ne!(call_generation, existing.call_generation());
    }
    let third = calls.admit(candidate, 3).ok();
    assert_eq!(calls.reply(&first), Some(first_reply.clone()));
    assert_eq!(calls.reply(&second), Some(second_reply.clone()));
    assert!(first_reply.phone_authenticated());
    assert!(!second_reply.phone_authenticated());
    assert!(calls.take_reply(&first, &second_reply).is_err());

    if let Some(third) = &third {
        let reply = calls.reply(third).unwrap();
        assert_eq!(reply.phone_source(), Some(third));
        assert!(!reply.phone_authenticated());
        assert!(calls.queue_reply(third, &first_reply, media()).is_err());
        calls.queue_reply(third, &reply, media()).unwrap();
        assert_eq!(
            calls.queue_reply(third, &reply, media()),
            Err(Error::Allocation)
        );
        let prepared = calls.take_reply(third, &reply).unwrap().unwrap();
        assert_eq!(prepared.source(), third);
        assert!(prepared.current());
        assert_eq!(calls.retire(third), Some(reply.clone()));
        assert!(reply.ended() && !prepared.current());
    }

    let first_prompt = calls.take_reply(&first, &first_reply).unwrap().unwrap();
    let second_prompt = calls.take_reply(&second, &second_reply).unwrap().unwrap();
    assert_eq!(first_prompt.source(), &first);
    assert_eq!(second_prompt.source(), &second);
    assert!(first_prompt.current() && second_prompt.current());
    assert_eq!(calls.retire_all().len(), 2);
    assert!(calls.is_empty() && first_reply.ended() && second_reply.ended());
    assert!(!first_prompt.current() && !second_prompt.current());
    let third_admitted = third.is_some();
    eprintln!(
        "PRIVATE_CALL_CAPACITY third_admitted={third_admitted} existing_owned=true reply_isolation=true all_retired=true"
    );
    assert!(
        third_admitted,
        "private call admission must grow beyond its initial reservation"
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
fn an_existing_native_call_cannot_be_reoffered_under_another_display_id() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut calls = PhoneCalls::new(3, 1).unwrap();
    let incumbent = calls.admit(peer_with_generation(0), 5).unwrap();
    let reply = calls.reply(&incumbent).unwrap();
    assert!(calls.admit(peer_with_generation(2), 7).is_err());
    assert_eq!(calls.reply(&incumbent), Some(reply));
    assert!(calls.admit(peer_with_generation(1), 9).is_ok());
}

#[test]
fn a_new_digit_retires_queued_authentication_success_without_retiring_generic_replies() {
    use rpt_advanced_core::messages::PhoneAuthenticationPrompt::Authenticated;

    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut calls = PhoneCalls::new(1, 2).unwrap();
    let source = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    assert_eq!(
        authenticate(&mut calls, &source, &reply, &records(), "100", "1234", 1),
        Ok(PhoneLogin::Authenticated)
    );
    calls
        .queue_login_prompt(&source, &reply, media(), Authenticated)
        .unwrap();
    calls.queue_reply(&source, &reply, media()).unwrap();
    let confirmation = calls.take_reply(&source, &reply).unwrap().unwrap();
    let generic = calls.take_reply(&source, &reply).unwrap().unwrap();
    assert!(!confirmation.obsolete_before_output(Some(0)));
    assert!(confirmation.obsolete_before_output(Some(1)));
    assert!(!generic.obsolete_before_output(Some(1)));
    assert!(reply.phone_authenticated());
    assert_eq!(calls.retire(&source), Some(reply));
    assert!(!confirmation.current() && !generic.current());
}

#[test]
fn login_prompt_queue_failure_and_stale_completion_leave_the_current_field_clock_intact() {
    use rpt_advanced_core::messages::PhoneAuthenticationPrompt::{Password, Retry};

    struct Sample;
    impl PcmStreamReader for Sample {
        fn render(&mut self, output: &mut [f32]) -> PcmRead {
            output[0] = 0.25;
            PcmRead::FinalSamples(1)
        }
    }

    let mut calls = PhoneCalls::new(1, 1).unwrap();
    let source = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    let digit = |symbol, received, revision| Digit {
        symbol,
        received,
        revision,
    };
    let timeout = 100;
    calls
        .login_digit(&source, &reply, digit('4', 10, 1), timeout)
        .unwrap();
    assert!(matches!(
        calls.login_digit(&source, &reply, digit('#', 15, 2), timeout),
        Ok(Some(LoginField::Password))
    ));
    calls.queue_reply(&source, &reply, media()).unwrap();
    assert_eq!(
        calls.queue_login_prompt(&source, &reply, Box::new(Sample), Password),
        Err(Error::Allocation)
    );
    calls.take_reply(&source, &reply).unwrap().unwrap();
    assert!(
        calls
            .expired_login_fields(114, |_| Some(timeout))
            .is_empty()
    );
    assert_eq!(calls.expired_login_fields(115, |_| Some(timeout)).len(), 1);

    // An untouched retry cannot overtake the subsequent mailbox terminator.
    calls
        .queue_login_prompt(&source, &reply, Box::new(Sample), Retry)
        .unwrap();
    let stale = calls.take_reply(&source, &reply).unwrap().unwrap();
    calls
        .login_digit(&source, &reply, digit('4', 124, 3), timeout)
        .unwrap();
    assert!(matches!(
        calls.login_digit(&source, &reply, digit('#', 129, 4), timeout),
        Ok(Some(LoginField::Password))
    ));
    assert!(stale.obsolete_before_output(Some(4)));

    calls
        .queue_login_prompt(&source, &reply, Box::new(Sample), Password)
        .unwrap();
    let mut password = calls.take_reply(&source, &reply).unwrap().unwrap();
    assert_eq!(password.render(false, &mut [0.0]), 1);
    assert_eq!(password.outcome(), PlaybackOutcome::Succeeded);
    // The next native digit arrived but its control turn has not happened.
    password.played_login_prompt(170, Some(5));
    assert!(
        calls
            .expired_login_fields(270, |_| Some(timeout))
            .is_empty()
    );
    calls
        .login_digit(&source, &reply, digit('1', 160, 5), timeout)
        .unwrap();
    assert!(
        calls
            .expired_login_fields(259, |_| Some(timeout))
            .is_empty()
    );
    assert_eq!(calls.expired_login_fields(260, |_| Some(timeout)).len(), 1);

    calls
        .queue_login_prompt(&source, &reply, Box::new(Sample), Retry)
        .unwrap();
    let mut retry = calls.take_reply(&source, &reply).unwrap().unwrap();
    assert_eq!(retry.render(false, &mut [0.0]), 1);
    retry.played_login_prompt(280, Some(5));
    assert!(
        calls
            .expired_login_fields(379, |_| Some(timeout))
            .is_empty()
    );
    assert_eq!(calls.expired_login_fields(380, |_| Some(timeout)).len(), 1);
}

#[test]
fn half_entered_private_pin_expires_without_a_later_digit_and_cannot_complete_the_next_login() {
    let mut calls = PhoneCalls::new(1, 2).unwrap();
    let source = calls.admit(peer(false), 0).unwrap();
    let reply = calls.reply(&source).unwrap();
    assert!(
        calls
            .login_digit(
                &source,
                &reply,
                Digit {
                    symbol: '4',
                    received: 10,
                    revision: 10
                },
                100
            )
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        calls
            .login_digit(
                &source,
                &reply,
                Digit {
                    symbol: '#',
                    received: 15,
                    revision: 15
                },
                100
            )
            .unwrap(),
        Some(LoginField::Password)
    ));
    assert!(calls.expired_login_fields(114, |_| Some(100)).is_empty());
    assert_eq!(
        calls.expired_login_fields(115, |_| Some(100)),
        vec![(source.clone(), reply.clone())]
    );
    assert!(calls.expired_login_fields(116, |_| Some(100)).is_empty());
    for (i, digit) in "8#12".chars().enumerate() {
        let field = calls
            .login_digit(
                &source,
                &reply,
                Digit {
                    symbol: digit,
                    received: 120 + i as u64,
                    revision: 120 + i as u64,
                },
                100,
            )
            .unwrap();
        if digit == '#' {
            assert!(matches!(field, Some(LoginField::Password)));
        } else {
            assert!(field.is_none());
        }
    }
    let Some(LoginField::Submitted(number, pin)) = calls
        .login_digit(
            &source,
            &reply,
            Digit {
                symbol: '#',
                received: 125,
                revision: 125,
            },
            100,
        )
        .unwrap()
    else {
        panic!("a fresh mailbox and PIN must complete on their own terminators")
    };
    assert_eq!(number, "8");
    assert_eq!(pin.as_str(), "12");
    assert!(calls.expired_login_fields(400, |_| Some(100)).is_empty());
}
