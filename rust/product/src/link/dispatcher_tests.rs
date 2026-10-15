use super::*;
use rpt_advanced_core::{
    audio::{LinkAudioConsumer, LinkAudioQueue},
    controller::{ControllerSettings, CourtesySettings, NodeController},
    link::{AudioPeer, LinkAudio, Mode, PeerInput, PeerSignals, SourcePosition},
};
use std::sync::{Barrier, atomic::AtomicUsize};

struct Silent(PeerSignals);
impl PeerInput for Silent {
    fn source_rate(&self) -> u32 {
        48_000
    }
    fn signals(&self) -> &PeerSignals {
        &self.0
    }
    fn available(&self) -> u64 {
        0
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        output.fill(0.0);
        Some(0)
    }
}
fn fixture(count: usize) -> (LinkAudio<Silent>, LinkDispatcher, Vec<LinkAudioConsumer>) {
    let mut outputs = Vec::new();
    let peers = (0..count)
        .map(|index| {
            let (producer, consumer) = LinkAudioQueue::new(9600, 64).unwrap().into_endpoints();
            outputs.push(consumer);
            AudioPeer::new(
                &(index + 1000).to_string(),
                Mode::TRANSCEIVE,
                Silent(PeerSignals::new()),
                producer,
                960,
                0,
            )
            .unwrap()
        })
        .collect();
    let (audio, dispatcher) = LinkAudio::new(peers, 960).unwrap();
    (audio, dispatcher, outputs)
}
fn controller() -> NodeController {
    NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap()
    .0
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            std::time::Instant::now() < until,
            "media owner made no progress"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn egress_dispatch_survives_control_pause() {
    let mut worker = MediaWorker::start().unwrap();
    let (mut audio, dispatcher, mut outputs) = fixture(130);
    let registration = worker
        .prepare(dispatcher, vec![PeerWake::default(); 130])
        .unwrap();
    registration.activate();
    let mut controller = controller();
    for block in 0..8 {
        audio
            .process(
                &mut controller,
                true,
                &mut [0.25; 960],
                SourcePosition {
                    generation: 1,
                    sample: block * 960,
                    ..SourcePosition::default()
                },
            )
            .unwrap();
        // No Host::pump/control work runs while all destinations receive this block.
        for output in &mut outputs {
            let mut samples = [0.0; 960];
            let mut received = None;
            wait_for(|| {
                received = output.read_span(&mut samples);
                received.is_some()
            });
            assert_eq!(received.unwrap().span.source_start, block * 960);
            assert_eq!(samples, [0.25; 960]);
        }
    }
    assert_eq!(audio.status().dropped_blocks(), 0);
    assert!(registration.timing().0 >= 24);
    registration.request_stop();
    wait_for(|| registration.is_quiescent());
}

#[test]
fn egress_stopped_media_worker_rejects_new_dispatch() {
    let mut worker = MediaWorker::start().unwrap();
    worker.stop();
    let (_, dispatcher, _) = fixture(1);
    assert!(matches!(
        worker.prepare(dispatcher, vec![PeerWake::default()]),
        Err(Error::Reservation)
    ));
}

struct BlockedWake {
    enter: Barrier,
    leave: Barrier,
    calls: AtomicUsize,
}
unsafe extern "C" fn blocked_wake(context: *mut std::ffi::c_void, _: *mut std::ffi::c_void) {
    let target = unsafe { &*context.cast::<BlockedWake>() };
    target.calls.fetch_add(1, Ordering::SeqCst);
    target.enter.wait();
    target.leave.wait();
}

#[test]
fn egress_wake_close_race() {
    let target = Arc::new(BlockedWake {
        enter: Barrier::new(2),
        leave: Barrier::new(2),
        calls: AtomicUsize::new(0),
    });
    let wake = PeerWake::default();
    // Test keeps the borrowed callback context alive through the close barrier.
    unsafe {
        wake.attach(
            blocked_wake,
            Arc::as_ptr(&target).cast_mut().cast(),
            std::ptr::null_mut(),
        )
    };
    let notifier = wake.clone();
    let thread = thread::spawn(move || notifier.notify());
    target.enter.wait();
    wake.close();
    assert!(!wake.is_quiescent());
    wake.notify();
    assert_eq!(target.calls.load(Ordering::SeqCst), 1);
    target.leave.wait();
    thread.join().unwrap();
    assert!(wake.is_quiescent());
    wake.notify();
    assert_eq!(target.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn egress_retirement_waits_for_partial_dispatch() {
    let mut worker = MediaWorker::start().unwrap();
    let (mut audio, dispatcher, _) = fixture(130);
    let target = Arc::new(BlockedWake {
        enter: Barrier::new(2),
        leave: Barrier::new(2),
        calls: AtomicUsize::new(0),
    });
    let wake = PeerWake::default();
    unsafe {
        wake.attach(
            blocked_wake,
            Arc::as_ptr(&target).cast_mut().cast(),
            std::ptr::null_mut(),
        )
    };
    let mut wakes = vec![PeerWake::default(); 130];
    wakes[0] = wake.clone();
    let registration = worker.prepare(dispatcher, wakes).unwrap();
    registration.activate();
    audio
        .process(
            &mut controller(),
            true,
            &mut [0.25; 960],
            SourcePosition {
                generation: 1,
                sample: 0,
                ..SourcePosition::default()
            },
        )
        .unwrap();
    target.enter.wait();
    registration.request_stop();
    assert!(
        !registration.is_quiescent(),
        "a partial batch still owns the endpoints"
    );
    wake.close();
    target.leave.wait();
    wait_for(|| registration.is_quiescent());
    // A failed candidate is dropped before activation; it must release its slot.
    let (_, dispatcher, _) = fixture(1);
    drop(
        worker
            .prepare(dispatcher, vec![PeerWake::default()])
            .unwrap(),
    );
    let (mut next, dispatcher, mut outputs) = fixture(1);
    let replacement = worker
        .prepare(dispatcher, vec![PeerWake::default()])
        .unwrap();
    replacement.activate();
    next.process(
        &mut controller(),
        true,
        &mut [0.5; 960],
        SourcePosition {
            generation: 2,
            sample: 960,
            ..SourcePosition::default()
        },
    )
    .unwrap();
    let mut received = [0.0; 960];
    wait_for(|| outputs[0].read_span(&mut received).is_some());
    assert_eq!(received, [0.5; 960]);
}

#[test]
fn egress_shutdown_releases_dispatches_still_in_the_publication_queue() {
    let mut worker = MediaWorker::start().unwrap();
    let (mut audio, dispatcher, _) = fixture(1);
    let target = Arc::new(BlockedWake {
        enter: Barrier::new(2),
        leave: Barrier::new(2),
        calls: AtomicUsize::new(0),
    });
    let wake = PeerWake::default();
    unsafe {
        wake.attach(
            blocked_wake,
            Arc::as_ptr(&target).cast_mut().cast(),
            std::ptr::null_mut(),
        )
    };
    let active = worker.prepare(dispatcher, vec![wake]).unwrap();
    active.activate();
    audio
        .process(
            &mut controller(),
            true,
            &mut [0.25; 960],
            SourcePosition::default(),
        )
        .unwrap();
    target.enter.wait();
    let mut pending = Vec::new();
    for _ in 0..2 {
        let (_, dispatcher, _) = fixture(1);
        pending.push(
            worker
                .prepare(dispatcher, vec![PeerWake::default()])
                .unwrap(),
        );
    }
    // Slot zero is held in its callback. At most one publication can enter
    // slot one before the stop check; the other must be drained on shutdown.
    worker.stop.store(true, Ordering::Release);
    target.leave.wait();
    worker.stop();
    assert!(active.is_quiescent());
    assert!(pending.iter().all(PreparedDispatch::is_quiescent));
    assert_eq!(target.calls.load(Ordering::SeqCst), 1);
}
