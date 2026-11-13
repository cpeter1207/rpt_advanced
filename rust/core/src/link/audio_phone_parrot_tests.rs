thread_local! {
    static BEFORE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
pub(crate) fn before_capture() {
    BEFORE_CAPTURE.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() {
            callback();
        }
    });
}

use super::*;
use crate::controller::ControllerControl;
use crate::link::{PhoneAccountId, PhoneCallId, PhoneSource};

struct CapturedPhone {
    signals: PeerSignals,
    reply: PeerReply,
    age: Option<SourceTimeBound>,
    finished: bool,
    value: f32,
    blocks: usize,
}
impl PeerInput for CapturedPhone {
    fn phone_reply(&self) -> Option<&PeerReply> {
        Some(&self.reply)
    }
    fn source_rate(&self) -> u32 {
        48000
    }
    fn receive_policy(&self) -> ReceivePolicy {
        ReceivePolicy::Phone
    }
    fn signals(&self) -> &PeerSignals {
        &self.signals
    }
    fn available(&self) -> u64 {
        16000
    }
    fn finite_playout(&self) -> Option<bool> {
        self.finished.then_some(false)
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        self.blocks += 1;
        output.fill(self.value);
        Some(output.len())
    }
    fn render_timed_block(&mut self, output: &mut [f32]) -> Option<RenderedPeerBlock> {
        self.render(output).map(|real| RenderedPeerBlock {
            real,
            age_bound: self.age,
        })
    }
}
fn bound(epoch: u64, time: u64) -> SourceTimeBound {
    SourceTimeBound {
        host_clock_epoch: epoch,
        not_after_ns: time,
    }
}
fn fixture() -> (
    LinkAudio<CapturedPhone>,
    LinkDispatcher,
    NodeController,
    ControllerControl,
    RetirementDomain,
) {
    let domain = RetirementDomain::default();
    let source = PhoneSource::new(
        PhoneAccountId::new("provider").unwrap(),
        7,
        PhoneCallId::new([1; 16]).unwrap(),
        9,
        13,
    )
    .unwrap();
    let call = PeerSignals::for_phone(source.clone());
    assert!(call.authenticate_phone());
    let reply = call.reply_target();
    let signals = PeerSignals::new();
    signals.publish_pcm();
    signals.set_radio_keyed(true);
    let (writer, _) = LinkAudioQueue::new(16, 4).unwrap().into_endpoints();
    let peer = AudioPeer::for_phone(
        &source,
        &reply,
        Mode::TRANSCEIVE,
        CapturedPhone {
            signals,
            reply: reply.clone(),
            age: Some(bound(7, 30)),
            finished: false,
            value: 0.0,
            blocks: 0,
        },
        writer,
        4,
    )
    .unwrap();
    let (links, dispatcher) = LinkAudio::new_in_domain(vec![peer], 4, domain.clone()).unwrap();
    let (mut node, mut control) = NodeController::new(
        ControllerSettings {
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    node.bind_phone_retirement(&mut control, domain.clone());
    (links, dispatcher, node, control, domain)
}
fn tick(links: &mut LinkAudio<CapturedPhone>, node: &mut NodeController) {
    links
        .process(node, false, &mut [0.0; 4], SourcePosition::default())
        .unwrap();
}
fn finish(links: &mut LinkAudio<CapturedPhone>, node: &mut NodeController) {
    links.peers[0].input.finished = true;
    links.peers[0].input.signals.set_radio_keyed(false);
    tick(links, node);
}

#[test]
fn phone_parrot_actual_mix_keeps_zero_valued_taint_and_original_oldest_age() {
    for second in [Some(bound(7, 10)), None, Some(bound(8, 10))] {
        let (mut links, _dispatcher, mut node, mut control, domain) = fixture();
        tick(&mut links, &mut node);
        links.peers[0].input.age = second;
        tick(&mut links, &mut node);
        finish(&mut links, &mut node);
        assert_eq!(links.peers[0].input.blocks, 2, "one render per block");
        let clip = control.take_parrot_capture().unwrap();
        assert_eq!(
            clip.samples(),
            &[0.0; 8],
            "zero PCM is still real private contribution"
        );
        let phone = clip
            .phone_source()
            .expect("taint is independent of amplitude or age qualification");
        assert_eq!(phone.retirement_revision, domain.revision());
        assert_eq!(
            phone.age_bound,
            if second.is_some_and(|time| time.host_clock_epoch == 7) {
                second
            } else {
                None
            }
        );
    }
}

#[test]
fn phone_parrot_capture_and_completed_handoff_reject_retired_generation_only() {
    for retire in 0..3 {
        let (mut links, _dispatcher, mut node, mut control, _domain) = fixture();
        tick(&mut links, &mut node);
        if retire == 0 {
            links.peers[0].input.signals.end();
        }
        finish(&mut links, &mut node);
        if retire == 1 {
            links.peers[0].input.reply.retire();
        } else if retire == 2 {
            let other = RetirementDomain::default();
            let source = PeerSignals::new().reply_target();
            source.bind_retirement_domain(&other);
            source.retire();
        }
        assert_eq!(control.take_parrot_capture().is_some(), retire == 2);
    }
}

#[test]
fn phone_parrot_retirement_after_render_before_capture_cannot_publish_stale_clip() {
    let (mut links, _dispatcher, mut node, mut control, _domain) = fixture();
    let source = links.peers[0].input.reply.clone();
    BEFORE_CAPTURE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || source.retire())));
    tick(&mut links, &mut node);
    finish(&mut links, &mut node);
    assert!(control.take_parrot_capture().is_none());
}

#[test]
fn phone_parrot_wrong_or_missing_node_domain_refuses_capture() {
    for bound_domain in [None, Some(RetirementDomain::default())] {
        let (mut links, _dispatcher, _, _, _domain) = fixture();
        let (mut node, mut control) = NodeController::new(
            ControllerSettings {
                parrot_enabled: true,
                ..Default::default()
            },
            vec![],
            vec![],
            CourtesySettings::default(),
        )
        .unwrap();
        if let Some(domain) = bound_domain {
            node.bind_phone_retirement(&mut control, domain);
        }
        tick(&mut links, &mut node);
        finish(&mut links, &mut node);
        assert!(control.take_parrot_capture().is_none());
    }
}

#[test]
fn phone_parrot_ordinary_clip_survives_node_retirement() {
    let (links, _dispatcher, mut node, mut control, _domain) = fixture();
    node.observe_parrot(true, &[0.5; 4]);
    links.peers[0].input.reply.retire();
    node.observe_parrot(false, &[]);
    let clip = control.take_parrot_capture().unwrap();
    assert_eq!(clip.samples(), &[0.5; 4]);
    assert_eq!(clip.phone_source(), None);
}
