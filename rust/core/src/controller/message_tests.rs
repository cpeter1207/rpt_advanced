use super::*;
use crate::audio::{MorseSwitch, PcmRead, PcmStreamReader};
use crate::controller::MessagePreparation;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct TestStream {
    value: f32,
    morse: bool,
    drops: Arc<AtomicUsize>,
}

impl Drop for TestStream {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl PcmStreamReader for TestStream {
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        output.fill(if self.morse { -self.value } else { self.value });
        PcmRead::Samples(output.len())
    }
    fn select_morse_fallback(&mut self) -> MorseSwitch {
        self.morse = true;
        MorseSwitch::Selected
    }
}

fn prepared(value: f32, drops: &Arc<AtomicUsize>) -> PreparedMedia {
    PreparedMedia::new_stream(Box::new(TestStream {
        value,
        morse: false,
        drops: drops.clone(),
    }))
    .unwrap()
}

#[test]
fn composition_handoff_refreshes_attempt_and_keeps_receive_latched() {
    let mut control = MessagePreparation::new(7);
    let mut media = control.register(12).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut audio = [0.0];
    media.0.restart(false);
    assert_eq!(media.0.render(false, &mut audio), 0);
    let first = control.take_request().unwrap();
    assert_eq!(first.content_index, 12);
    assert_eq!(first.attempt.generation, 7);
    assert!(control.take_request().is_none());
    // Receive can end before control finishes; this attempt must still use Morse.
    assert_eq!(media.0.render(true, &mut audio), 0);
    assert!(
        control
            .complete(first.attempt, Ok(prepared(0.25, &drops)))
            .is_ok()
    );
    assert_eq!(media.0.render(false, &mut audio), 1);
    assert_eq!(audio, [-0.25]);
    media.0.restart(false);
    assert_eq!(media.0.render(false, &mut audio), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    let second = control.take_request().unwrap();
    assert_ne!(first.attempt, second.attempt);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(
        control
            .complete(second.attempt, Ok(prepared(0.5, &drops)))
            .is_ok()
    );
    assert_eq!(media.0.render(false, &mut audio), 1);
    assert_eq!(audio, [0.5]);
}

#[test]
fn composition_handoff_reclaims_stale_results_and_completes_failures() {
    let mut control = MessagePreparation::new(9);
    let mut media = control.register(0).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut audio = [0.0];
    media.0.render(false, &mut audio);
    let first = control.take_request().unwrap();
    media.0.restart(false);
    media.0.render(false, &mut audio);
    assert!(control.take_request().is_none());
    assert!(
        control
            .complete(first.attempt, Ok(prepared(0.5, &drops)))
            .is_ok()
    );
    assert_eq!(media.0.render(false, &mut audio), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    let second = control.take_request().unwrap();
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(
        control
            .complete(second.attempt, Err(ControllerError))
            .is_ok()
    );
    assert_eq!(media.0.render(false, &mut audio), 0);
    assert_eq!(media.0.outcome(), crate::audio::PlaybackOutcome::Failed);
    let rejected = control.complete(first.attempt, Ok(prepared(1.0, &drops)));
    assert!(rejected.is_err());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    drop(rejected);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn composition_handoff_cancellation_never_reclaims_on_audio() {
    let mut control = MessagePreparation::new(4);
    let mut media = control.register(0).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut audio = [0.0];
    media.0.render(false, &mut audio);
    let request = control.take_request().unwrap();
    assert!(
        control
            .complete(request.attempt, Ok(prepared(0.5, &drops)))
            .is_ok()
    );
    media.0.render(false, &mut audio);
    let mut stream = media.0.take_stream().unwrap();
    stream.cancel();
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(control.take_request().is_none());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn composition_handoff_one_shot_ports_are_reclaimed_with_their_reader() {
    let mut control = MessagePreparation::new(1);
    for _ in 0..8 {
        let media = control.register(0).unwrap();
        drop(media);
        assert!(control.take_request().is_none());
        assert!(control.ports.is_empty());
    }
}

#[test]
fn composition_tail_rewind_retains_attempt_values_and_morse() {
    let mut control = MessagePreparation::new(2);
    let mut media = control.register(0).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut audio = [0.0];
    media.0.render(false, &mut audio);
    let request = control.take_request().unwrap();
    assert!(
        control
            .complete(request.attempt, Ok(prepared(0.25, &drops)))
            .is_ok()
    );
    assert_eq!(media.0.render(true, &mut audio), 1);
    media.0.rewind();
    assert_eq!(media.0.render(false, &mut audio), 1);
    assert_eq!(audio, [-0.25]);
    assert!(
        control.take_request().is_none(),
        "ID preemption is not a new value snapshot"
    );
    assert_eq!(drops.load(Ordering::Relaxed), 0);
}
