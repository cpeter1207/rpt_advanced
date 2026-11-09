//! Private calls own bounded media replies before any radio attachment exists.
use super::*;
use crate::{abi, services::HostServices};
use rpt_advanced_core::{
    audio::{PcmRead, PcmStreamReader, PlaybackOutcome},
    link::MailboxId,
};
use std::{ffi::c_void, mem::size_of, ptr};

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
fn private_call_reply_queue_is_exact_bounded_and_revoked_on_retirement() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(PhoneCalls::new(0, 1).is_err());
    assert!(PhoneCalls::new(1, 0).is_err());
    let mut calls = PhoneCalls::new(2, 2).unwrap();
    let first = calls.admit(peer(false)).unwrap();
    assert!(
        calls.admit(peer(false)).is_err(),
        "a duplicate cannot replace its call"
    );
    let second = calls.admit(peer(true)).unwrap();
    assert!(
        calls.admit(peer(true)).is_err(),
        "transport capacity is bounded"
    );
    let reply = calls.reply(&first).unwrap();
    let other = calls.reply(&second).unwrap();
    assert!(!reply.phone_authenticated());
    assert!(
        calls
            .authenticate(&first, &other, MailboxId::new("100").unwrap())
            .is_err()
    );
    calls
        .authenticate(&first, &reply, MailboxId::new("100").unwrap())
        .unwrap();
    assert!(reply.phone_authenticated());
    assert!(
        calls
            .authenticate(&first, &reply, MailboxId::new("101").unwrap())
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
            .authenticate(&first, &reply, MailboxId::new("100").unwrap())
            .is_err()
    );
    assert!(calls.retire(&first).is_none());
    let replaced = calls.admit(peer(false)).unwrap();
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
    let source = calls.admit(peer(false)).unwrap();
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
    let source = calls.admit(peer(false)).unwrap();
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
