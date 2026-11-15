//! Failure before ACK must still close the retained private worker exactly once.
use super::*;
use std::{cell::RefCell, rc::Rc, sync::atomic::AtomicUsize};
unsafe extern "C" fn step(context: *mut c_void) -> i32 {
    unsafe {
        (*context.cast::<AtomicUsize>()).fetch_add(1, Ordering::Relaxed);
    }
    1
}
#[tokio::test(flavor = "current_thread")]
async fn failed_native_close_before_ack_still_retires_the_product_step() {
    let (mut entry, directory) = super::super::tests::native_fixture(false, "pre-ack");
    let provider = unsafe {
        rptadv_peer_support::sip::registration::Provider::load(&directory.join("provider.so"))
    }
    .unwrap();
    let providers = crate::providers::ProviderSet::load().unwrap();
    // All media and callback leases are dropped before the descriptor provider.
    let jitter = unsafe { &*std::ptr::from_ref(providers.jitter_descriptor().unwrap()) };
    let offer = b"v=0\r\no=fixture 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 9999 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=ptime:20\r\na=sendrecv\r\n";
    let catalog = Arc::new(crate::phone::codec::load_catalog().unwrap());
    let media = Media::new(offer, 1, jitter, &catalog).await.unwrap();
    let (sender, _receiver) = mpsc::channel(1);
    let client = Client::new(sender);
    let (incoming, _receipts) = std::sync::mpsc::sync_channel(8);
    let mut reactor = Reactor::new(provider, client.clone(), incoming, catalog);
    let closed = Arc::new(AtomicBool::new(true));
    let shared = Arc::new(Shared {
        key: Key {
            account: "pre-ack".into(),
            generation: entry.settings.generation(),
            call: 1,
        },
        account: "pre-ack".into(),
        local: "desk".into(),
        remote: "opaque".into(),
        source: "127.0.0.1:9999".parse().unwrap(),
        media: Mutex::new(media),
        digit_capabilities: 64,
        pcm_rate: 8000,
        direction: Direction::Incoming,
        state: AtomicU32::new(2),
        close: Arc::clone(&closed),
        stopped: AtomicBool::new(false),
        client,
    });
    let steps = AtomicUsize::new(0);
    let mut observation = Observation::default();
    observation.state = 2;
    observation.call = 1;
    entry.calls.insert(
        1,
        Slot {
            outgoing: None,
            observation,
            close: closed,
            dispatched: true,
            shared: Some(Arc::clone(&shared)),
            step: Some(Step {
                callback: step,
                context: std::ptr::from_ref(&steps) as usize,
            }),
        },
    );
    let entry = Rc::new(RefCell::new(entry));
    reactor.active.insert("pre-ack".into(), Rc::clone(&entry));
    // Synthetic provider refuses native hangup and emits no call event.
    reactor.service_calls();
    reactor.service_calls();
    assert_eq!(steps.load(Ordering::Relaxed), 1);
    assert!(!shared.live());
    assert!(shared.stopped.load(Ordering::Acquire));
    entry.borrow_mut().begin_shutdown(std::time::Instant::now());
    entry.borrow_mut().service(std::time::Instant::now());
    drop(reactor);
    drop(entry);
    drop(shared);
    std::fs::remove_dir_all(directory).unwrap();
}
