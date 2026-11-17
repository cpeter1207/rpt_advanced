use super::*;
use crate::{abi, services::HostServices};
use rpt_advanced_core::link::{PhoneAccountId, PhoneCallId};
use std::{
    ffi::{c_char, c_void},
    ptr,
};

fn reply() -> PeerReply {
    let source = PhoneSource::new(
        PhoneAccountId::new("provider").unwrap(),
        7,
        PhoneCallId::new([1; 16]).unwrap(),
        9,
        13,
    )
    .unwrap();
    let signals = PeerSignals::for_phone(source);
    assert!(signals.authenticate_phone());
    signals.reply_target()
}

fn prepare(
    reply: PeerReply,
    rate: u32,
    processor: Option<PcmProcessor>,
) -> Result<(Box<Ingress>, PhoneMediaInput, Arc<Shared>), Error> {
    super::prepare(reply, rate, origin(NonZeroU32::MIN), processor)
}

fn origin(revision: NonZeroU32) -> MediaOrigin {
    MediaOrigin {
        revision,
        epoch: NonZeroU32::MIN,
        source: Arc::new(super::super::SourceReady::new()),
    }
}
fn prepare_voiced(
    reply: PeerReply,
    rate: u32,
    revision: NonZeroU32,
    processor: Option<PcmProcessor>,
    settings: PhoneSpeechSettings,
) -> Result<(Box<Ingress>, PhoneMediaInput, Arc<Shared>), Error> {
    super::prepare_voiced(reply, rate, origin(revision), processor, settings)
}

fn adopt(ingress: &mut Ingress, minimum_frame: u64, reference: SourceTimeReference) {
    let owner = ingress.shared.reply.clone();
    ingress.adopt(&owner, minimum_frame, reference);
}

fn reference() -> SourceTimeReference {
    SourceTimeReference {
        host_clock_epoch: 7,
        now_ns: 10,
    }
}
fn birth(time: u64) -> SourceBirth {
    SourceBirth {
        host_clock_epoch: 7,
        born_ns: time,
    }
}
fn position(shared: &Shared, frame: u64, fraction: u32) -> SourceCoordinate {
    SourceCoordinate {
        generation: shared.generation,
        frame,
        fraction_billionths: fraction,
        negative: false,
    }
}

#[test]
fn prepared_phone_media_bounds_are_earliest_partition_times_and_never_cross_live_slots() {
    let (_ingress, _input, shared) = prepare(reply(), 8000, None).unwrap();
    assert!(shared.record(0, 160, birth(30)));
    assert!(shared.record(1, 2, birth(20)));
    assert!(shared.record(2, 3, birth(40)));
    assert_eq!(
        shared
            .locate(position(&shared, 159, 0))
            .unwrap()
            .not_after_ns,
        20
    );
    assert!(shared.locate(position(&shared, 159, 1)).is_none());
    assert!(shared.record(160, 320, birth(10)));
    assert_eq!(
        shared
            .locate(position(&shared, 159, 1))
            .unwrap()
            .not_after_ns,
        10
    );
    for index in 2..64 {
        assert!(shared.record(index * 160, (index + 1) * 160, birth(50)));
    }
    assert!(!shared.record(64 * 160, 65 * 160, birth(60)));
    assert_eq!(shared.bound(0).unwrap().not_after_ns, 20);
    shared.locate(position(&shared, 320, 0)).unwrap();
    assert!(shared.record(64 * 160, 65 * 160, birth(60)));
    assert!(
        shared.bound(0).is_none(),
        "reused slots cannot relabel old source"
    );
    assert_eq!(shared.bound(64 * 160).unwrap().not_after_ns, 60);
    let mut foreign = position(&shared, 320, 0);
    foreign.generation += 1;
    assert!(shared.locate(foreign).is_none());
    foreign.generation -= 1;
    foreign.negative = true;
    assert!(shared.locate(foreign).is_none());
    shared.slot(2).revision.fetch_add(1, Ordering::SeqCst);
    assert!(
        shared.bound(320).is_none(),
        "in-progress writes remain unknown"
    );
}

#[test]
fn prepared_phone_media_interpolation_never_combines_clock_epochs() {
    let (_ingress, _input, shared) = prepare(reply(), 8000, None).unwrap();
    assert!(shared.record(0, 160, birth(20)));
    assert!(shared.record(
        160,
        320,
        SourceBirth {
            host_clock_epoch: 8,
            born_ns: 10,
        },
    ));
    for (frame, fraction, expected) in [
        (159, 0, Some((7, 20))),
        (160, 0, Some((8, 10))),
        (159, 1, None),
    ] {
        assert_eq!(
            shared
                .locate(position(&shared, frame, fraction))
                .map(|bound| (bound.host_clock_epoch, bound.not_after_ns)),
            expected,
        );
    }
}

#[test]
fn prepared_phone_media_sidecar_refuses_before_pcm_and_credit_is_actual_ring_acceptance() {
    let (mut ingress, input, shared) = prepare(reply(), 8000, None).unwrap();
    adopt(&mut ingress, 0, reference());
    assert!(shared.record(0, 160, birth(10)));
    assert_eq!(
        ingress.audio(64 * 160, &[0.25; 160], Some(birth(10))),
        Ok(0)
    );
    assert_eq!(input.available(), 0);
    assert_eq!(ingress.audio(0, &[0.25; 1600], Some(birth(10))), Ok(160));
    assert_eq!(input.available(), 160);
    assert_eq!(ingress.cursor, Some(160));
    assert_eq!(
        ingress.audio(170, &[0.25; 160], Some(birth(10))),
        Err(Error::InvalidFrame)
    );
    assert_eq!(input.available(), 160);
}

#[test]
fn prepared_phone_media_adoption_cannot_resurrect_retired_or_untimed_generations() {
    for mode in 0..4 {
        let current = reply();
        let (mut ingress, mut input, shared) = prepare(current.clone(), 8000, None).unwrap();
        assert_eq!(ingress.audio(0, &[0.5; 160], Some(birth(10))), Ok(160));
        assert_eq!(input.available(), 0, "pre-adoption data is never admitted");
        match mode {
            0 => shared.retire(),
            1 => current.retire(),
            2 => {
                adopt(&mut ingress, 0, SourceTimeReference::default());
            }
            _ => {
                drop(input);
                adopt(&mut ingress, 0, reference());
                assert!(!shared.active());
                continue;
            }
        }
        adopt(&mut ingress, 0, reference());
        assert!(!input.active());
        assert_eq!(ingress.audio(160, &[0.5; 160], Some(birth(10))), Ok(160));
        assert_eq!(input.render_timed_sample(), None);
        assert_eq!(input.render_sample(), None);
        let mut output = [1.0; 4];
        assert_eq!(input.render(&mut output), None);
        assert_eq!(output, [0.0; 4]);
        input.silence(&mut output);
        assert_eq!(output, [0.0; 4]);
    }
}

#[test]
fn prepared_phone_media_rejects_old_source_prefixes_unknown_age_and_wrong_epoch() {
    let (mut ingress, input, _) = prepare(reply(), 8000, None).unwrap();
    adopt(&mut ingress, 80, reference());
    assert_eq!(ingress.audio(0, &[0.25; 160], Some(birth(10))), Ok(80));
    assert_eq!(input.available(), 0);
    assert_eq!(ingress.audio(80, &[0.25; 160], Some(birth(10))), Ok(160));
    assert_eq!(input.available(), 160);
    assert_eq!(ingress.audio(240, &[], Some(birth(10))), Ok(0));
    assert_eq!(ingress.gap(240, 80), Ok(80));
    assert!(!input.active());
    assert_eq!(input.available(), 0);
    assert!(!input.shared.reply.ended());
    for (time, start) in [
        (None, 0),
        (Some(birth(9)), 160),
        (
            Some(SourceBirth {
                host_clock_epoch: 8,
                born_ns: 10,
            }),
            0,
        ),
    ] {
        let (mut ingress, input, _) = prepare(reply(), 8000, None).unwrap();
        adopt(&mut ingress, 0, reference());
        if start != 0 {
            assert_eq!(ingress.audio(0, &[0.25; 160], Some(birth(10))), Ok(160));
        }
        assert_eq!(ingress.audio(start, &[0.25; 160], time), Ok(160));
        assert_eq!(input.available(), 0);
        assert!(!input.active());
        assert!(!input.shared.reply.ended());
    }
}

struct ProcessorState {
    target: Option<Arc<Shared>>,
    retire: bool,
    destroyed: bool,
}
unsafe extern "C" fn process(context: *mut c_void, samples: *mut f32, count: usize) -> i32 {
    let state = unsafe { &mut *context.cast::<ProcessorState>() };
    unsafe { std::slice::from_raw_parts_mut(samples, count) }.fill(0.75);
    if state.retire {
        state.target.as_ref().unwrap().retire();
    }
    0
}
unsafe extern "C" fn destroy(context: *mut c_void) {
    unsafe { &mut *context.cast::<ProcessorState>() }.destroyed = true;
}
unsafe extern "C" fn processor(
    context: *mut c_void,
    _: *const c_char,
    _: usize,
    _: *const c_char,
    _: usize,
    _: usize,
    output: *mut abi::rptadv_pcm_processor_v1,
) -> i32 {
    let output = unsafe { &mut *output };
    output.context = context;
    output.process = Some(process);
    output.destroy = Some(destroy);
    0
}

#[test]
fn prepared_phone_media_keeps_opaque_dsp_unknown_and_rechecks_retirement_after_processing() {
    for mode in 0..4 {
        let mut state = Box::new(ProcessorState {
            target: None,
            retire: false,
            destroyed: false,
        });
        let mut table = unsafe { crate::fixture::host_descriptor().read() };
        table.context = ptr::from_mut(state.as_mut()).cast();
        table.peer_processing_prepare = Some(processor);
        let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
        let graph = services.prepare_peer_processing("", "1000", 960).unwrap();
        let (mut ingress, mut input, shared) = prepare(reply(), 8000, graph).unwrap();
        state.target = Some(shared);
        adopt(&mut ingress, 0, reference());
        for index in 0..10 {
            assert_eq!(
                ingress.audio(index * 160, &[0.25; 160], Some(birth(10))),
                Ok(160)
            );
        }
        assert!(!input.source_timing_available());
        let sample = input.render_timed_sample().unwrap();
        assert_eq!(sample.sample, 0.75);
        assert!(
            sample.provenance.is_none(),
            "opaque graph cannot relabel retained PCM"
        );
        state.retire = true;
        match mode {
            0 => assert!(input.render_timed_sample().is_none()),
            1 => {
                for _ in 0..9600 {
                    assert!(input.render_sample().is_none());
                    if !input.active() {
                        break;
                    }
                }
            }
            _ => {
                let mut output = [1.0; 960];
                if mode == 2 {
                    assert!(input.render(&mut output).is_none());
                } else {
                    assert!(input.render_timed_block(&mut output).is_none());
                }
                assert_eq!(output, [0.0; 960]);
            }
        }
        assert!(!input.active());
        assert!(!input.shared.reply.ended());
        drop(input);
        assert!(state.destroyed);
    }
}

#[test]
fn prepared_phone_media_retirement_ends_ring_generation_but_not_private_call() {
    for retire in 0..3 {
        let current = reply();
        let (mut ingress, input, shared) = prepare(current.clone(), 8000, None).unwrap();
        adopt(&mut ingress, 0, reference());
        let generation = input.signals().reply_target();
        assert!(!generation.ended());
        match retire {
            0 => shared.retire(),
            1 => drop(input),
            _ => ingress.retire(),
        }
        assert!(generation.ended());
        assert!(!current.ended());
    }
}

#[test]
fn prepared_phone_media_publishes_typed_mixer_and_revokes_live_destination_contribution() {
    use rpt_advanced_core::{
        audio::LinkAudioQueue,
        controller::{ControllerSettings, CourtesySettings, NodeController},
        link::{AudioPeer, LinkAudio, Mode, SourcePosition},
    };
    let mut peers = Vec::new();
    let mut outputs = Vec::new();
    let mut generations = Vec::new();
    for index in 0..2 {
        let source = PhoneSource::new(
            PhoneAccountId::new("provider").unwrap(),
            7,
            PhoneCallId::new([index + 1; 16]).unwrap(),
            9,
            u64::from(index) + 13,
        )
        .unwrap();
        let signals = PeerSignals::for_phone(source.clone());
        assert!(signals.authenticate_phone());
        let call = signals.reply_target();
        let (mut ingress, input, shared) = prepare(call.clone(), 8000, None).unwrap();
        shared.origin.source.attached.store(true, Ordering::Release);
        adopt(&mut ingress, 0, reference());
        for index in 0..12 {
            assert_eq!(
                ingress.audio(index * 160, &[0.25; 160], Some(birth(10))),
                Ok(160)
            );
        }
        let (writer, reader) = LinkAudioQueue::new(1920, 480).unwrap().into_endpoints();
        peers.push(
            AudioPeer::for_phone(&source, &call, Mode::TRANSCEIVE, input, writer, 480).unwrap(),
        );
        outputs.push(reader);
        generations.push((ingress, shared, call));
    }
    let (mut mixer, mut dispatcher) = LinkAudio::new(peers, 480).unwrap();
    let status = mixer.status();
    let (mut controller, _) = NodeController::new(
        ControllerSettings::default(),
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    mixer
        .process(
            &mut controller,
            false,
            &mut [0.0; 480],
            SourcePosition::default(),
        )
        .unwrap();
    assert_eq!(status.last_keyed(), None);
    assert_eq!(dispatcher.dispatch(1), 1);
    let mut pcm = [0.0; 480];
    assert_eq!(outputs[1].read(&mut pcm), 0);
    assert!(pcm.iter().any(|sample| *sample != 0.0));
    mixer
        .process(
            &mut controller,
            false,
            &mut [0.0; 480],
            SourcePosition::default(),
        )
        .unwrap();
    generations[0].1.retire();
    assert!(
        !generations[0].2.ended(),
        "media retirement keeps private control alive"
    );
    assert_eq!(dispatcher.dispatch(1), 1);
    assert_eq!(outputs[1].read(&mut pcm), 480);
    assert_eq!(
        pcm, [0.0; 480],
        "retired source cannot leak into another live call"
    );
}

#[test]
fn prepared_phone_media_qualifies_original_age_through_the_actual_processor() {
    // Measure a native two-variant representation before choosing the mixed owner.
    eprintln!(
        "input_owner_bytes ordinary={} phone={} two_variant={}",
        std::mem::size_of::<ProcessedPeer<InboundConsumer>>(),
        std::mem::size_of::<PhoneMediaInput>(),
        std::mem::size_of::<Result<ProcessedPeer<InboundConsumer>, PhoneMediaInput>>(),
    );
    for rate in [8000, 48000] {
        let (_state, graph) = crate::services::history_tests::processor_fixture(1, 1);
        let call = reply();
        let (mut ingress, mut input, shared) = prepare(call.clone(), rate, Some(graph)).unwrap();
        assert_eq!(input.phone_reply(), Some(&call));
        assert_eq!(
            input.input.receive_policy(),
            ReceivePolicy::Phone { reserve_ms: 160 }
        );
        assert!(
            input.source_timing_available(),
            "the graph needs the inner exact call token at preparation"
        );
        adopt(&mut ingress, 0, reference());
        let count = rate as usize / 50;
        let samples = vec![0.25; count];
        for index in 0..10 {
            assert_eq!(
                ingress.audio(index * count as u64, &samples, Some(birth(10))),
                Ok(count)
            );
        }
        let qualified = (0..4800)
            .filter_map(|_| input.render_timed_sample())
            .find(|sample| sample.age_bound.is_some())
            .expect("real prepared phone history must qualify the original receipt");
        assert_eq!(
            qualified.provenance, None,
            "a conservative DSP bound is not exact provenance"
        );
        assert_eq!(
            qualified.age_bound,
            Some(SourceTimeBound {
                host_clock_epoch: 7,
                not_after_ns: 10,
            })
        );
        shared.retire();
        assert!(!call.ended());
        assert_eq!(input.render_timed_sample(), None);
    }
}

#[test]
fn prepared_phone_media_joins_the_product_mixer_with_an_ordinary_peer() {
    use crate::link::input::AudioInput;
    assert_eq!(
        std::mem::size_of::<AudioInput>(),
        std::mem::size_of::<Result<ProcessedPeer<InboundConsumer>, PhoneMediaInput>>(),
        "the mixed owner must not add allocation or storage beyond its native variants",
    );
    eprintln!(
        "input_owner_bytes ordinary={} phone={} enum={}",
        std::mem::size_of::<ProcessedPeer<InboundConsumer>>(),
        std::mem::size_of::<PhoneMediaInput>(),
        std::mem::size_of::<AudioInput>(),
    );
    use rpt_advanced_core::{
        audio::LinkAudioQueue,
        controller::{ControllerSettings, CourtesySettings, NodeController},
        link::{AudioPeer, LinkAudio, Mode, PeerIdentity, SourcePosition},
    };
    let call = reply();
    let source = call.phone_source().unwrap().clone();
    let (mut ingress, private, shared) = prepare(call.clone(), 8000, None).unwrap();
    adopt(&mut ingress, 0, reference());
    for index in 0..12 {
        assert_eq!(
            ingress.audio(index * 160, &[0.25; 160], Some(birth(10))),
            Ok(160)
        );
    }
    let private = AudioInput::Phone(private);
    assert_eq!(private.source_rate(), 8000);
    assert_eq!(private.phone_reply(), Some(&call));
    assert_eq!(
        private.receive_policy(),
        ReceivePolicy::Phone { reserve_ms: 160 }
    );
    assert!(private.source_timing_available());
    assert_eq!(private.finite_playout(), None);
    let (private_out, _) = LinkAudioQueue::new(1920, 480).unwrap().into_endpoints();
    let phone =
        AudioPeer::for_phone(&source, &call, Mode::TRANSCEIVE, private, private_out, 480).unwrap();

    let (mut ordinary_writer, ordinary) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
    // Preserve the ordinary 260ms target and the released 4096-sample offer limit.
    for _ in 0..4 {
        assert_eq!(ordinary_writer.write(&[0.5; 4096]).unwrap(), 4096);
    }
    let ordinary = AudioInput::Peer(ProcessedPeer::new(ordinary, None));
    assert_eq!(ordinary.source_rate(), 48000);
    assert_eq!(ordinary.phone_reply(), None);
    assert_eq!(ordinary.receive_policy(), ReceivePolicy::Peer);
    let (public_out, mut public_reader) = LinkAudioQueue::new(1920, 480).unwrap().into_endpoints();
    let public = AudioPeer::new(
        PeerIdentity::from_operator("2000").unwrap(),
        Mode::TRANSCEIVE,
        ordinary,
        public_out,
        480,
        0,
    )
    .unwrap();
    // This is the actual fixed product worker type, not a separate test mixer.
    let domain = rpt_advanced_core::link::RetirementDomain::default();
    let (mut mixer, mut dispatcher): (crate::worker::Audio, _) =
        LinkAudio::new_in_domain(vec![phone, public], 480, domain.clone()).unwrap();
    let status = mixer.status();
    let (mut controller, mut control) = NodeController::new(
        ControllerSettings {
            parrot_enabled: true,
            ..ControllerSettings::default()
        },
        vec![],
        vec![],
        CourtesySettings::default(),
    )
    .unwrap();
    controller.bind_phone_retirement(&mut control, domain);
    let (_, allocations) = crate::fixture::allocations(|| {
        mixer
            .process(
                &mut controller,
                false,
                &mut [0.0; 480],
                SourcePosition::default(),
            )
            .unwrap();
        assert_eq!(dispatcher.dispatch(1), 1);
    });
    assert_eq!(
        allocations, 0,
        "mixed ownership adds no callback allocations"
    );
    assert_eq!(status.last_keyed(), Some("2000"));
    let mut pcm = [0.0; 480];
    assert_eq!(public_reader.read(&mut pcm), 0);
    assert!(pcm.iter().any(|sample| *sample != 0.0));
    mixer
        .process(
            &mut controller,
            false,
            &mut [0.0; 480],
            SourcePosition::default(),
        )
        .unwrap();
    shared.retire();
    assert!(!call.ended());
    assert_eq!(dispatcher.dispatch(1), 1);
    assert_eq!(public_reader.read(&mut pcm), 480);
    assert_eq!(pcm, [0.0; 480]);
}

#[test]
fn prepared_phone_media_timed_blocks_keep_real_receipt_without_callback_allocations() {
    for rate in [8000, 48000] {
        let (_state, graph) = crate::services::history_tests::processor_fixture(1, 1);
        let call = reply();
        let (mut ingress, mut input, shared) = prepare(call.clone(), rate, Some(graph)).unwrap();
        adopt(&mut ingress, 0, reference());
        let count = rate as usize / 50;
        let samples = vec![0.25; count];
        for index in 0..10 {
            assert_eq!(
                ingress.audio(index * count as u64, &samples, Some(birth(10))),
                Ok(count)
            );
        }
        let mut known = None;
        let (_, allocations) = crate::fixture::allocations(|| {
            for _ in 0..10 {
                let block = input.render_timed_block(&mut [0.0; 480]).unwrap();
                if block.age_bound.is_some() {
                    known = block.age_bound;
                    break;
                }
            }
        });
        assert_eq!(allocations, 0);
        assert_eq!(
            known,
            Some(SourceTimeBound {
                host_clock_epoch: 7,
                not_after_ns: 10
            })
        );
        shared.retire();
        let mut pcm = [9.0; 480];
        assert_eq!(input.render_timed_block(&mut pcm), None);
        assert_eq!(pcm, [0.0; 480]);
        assert!(!call.ended());
    }
}

#[test]
fn prepared_phone_media_adoption_does_not_rewrite_active_or_retired_anchors() {
    let owner = reply();
    let (mut ingress, input, _) = prepare(owner.clone(), 8000, None).unwrap();
    ingress.adopt(&owner, 12, reference());
    assert!(input.active());
    ingress.adopt(
        &owner,
        500,
        SourceTimeReference {
            host_clock_epoch: 7,
            now_ns: 999,
        },
    );
    assert_eq!(ingress.minimum_frame, 12);
    assert_eq!(
        (ingress.admission.host_clock_epoch, ingress.admission.now_ns),
        (7, 10)
    );
    ingress.retire();
    ingress.adopt(
        &owner,
        600,
        SourceTimeReference {
            host_clock_epoch: 7,
            now_ns: 1000,
        },
    );
    assert!(!input.active());
    assert_eq!(ingress.minimum_frame, 12);
    assert_eq!(
        (ingress.admission.host_clock_epoch, ingress.admission.now_ns),
        (7, 10)
    );
}

#[test]
fn prepared_phone_media_revision_and_timing_add_only_scalar_storage_to_existing_ingress() {
    type Unstamped = (
        Arc<Shared>,
        InboundProducer,
        u64,
        SourceTimeReference,
        Option<u64>,
        Option<LinkAudioConsumer>,
        Option<SpeechAnalysis>,
        Option<u64>,
        u64,
    );
    let previous = std::mem::size_of::<Unstamped>();
    let actual = std::mem::size_of::<Ingress>();
    eprintln!("prepared_ingress_bytes unstamped={previous} stamped={actual}");
    // The revision retains its original alignment allowance; configurable
    // lookback and hold add exactly two source-clock u64 scalars, not buffers.
    assert!(actual <= previous + 2 * std::mem::size_of::<u64>() + std::mem::align_of::<Ingress>());
}

#[test]
fn prepared_phone_media_requires_a_live_authenticated_call_before_allocation() {
    for authenticated in [false, true] {
        let source = reply().phone_source().unwrap().clone();
        let signals = PeerSignals::for_phone(source);
        let owner = signals.reply_target();
        if authenticated {
            assert!(signals.authenticate_phone());
            owner.retire();
        }
        assert!(matches!(prepare(owner, 8000, None), Err(Error::Admission)));
    }
}

#[test]
fn mixed_audio_inputs_delegate_live_timed_and_untimed_output_and_silence() {
    use crate::link::input::AudioInput;
    for phone in [false, true] {
        let (mut ingress, private, shared) = prepare(reply(), 48000, None).unwrap();
        adopt(&mut ingress, 0, reference());
        let (mut writer, ordinary) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
        let mut input = if phone {
            for index in 0..12 {
                assert_eq!(
                    ingress.audio(index * 960, &[0.25; 960], Some(birth(10))),
                    Ok(960)
                );
            }
            AudioInput::Phone(private)
        } else {
            for _ in 0..4 {
                assert_eq!(writer.write(&[0.25; 4096]), Ok(4096));
            }
            AudioInput::Peer(ProcessedPeer::new(ordinary, None))
        };
        assert!(input.source_timing_available());
        assert_eq!(input.source_rate(), 48000);
        assert_eq!(
            input.receive_policy(),
            if phone {
                ReceivePolicy::Phone { reserve_ms: 160 }
            } else {
                ReceivePolicy::Peer
            }
        );
        let sample = input.render_timed_sample().unwrap();
        assert!(sample.provenance.is_some());
        // Both producers exceed their real reserve. A 10 ms block also drains
        // the released ring's 3.75 ms PLC lookahead before single-sample checks.
        let mut pcm = [9.0; 480];
        assert!(input.render_timed_block(&mut pcm).unwrap().real > 0);
        assert!(pcm.iter().all(|sample| sample.is_finite()));
        assert!(pcm.iter().any(|sample| (0.2..0.3).contains(sample)));
        assert!(input.render(&mut pcm).unwrap() > 0);
        assert!((0.2..0.3).contains(&input.render_sample().unwrap()));
        let timed = input.render_timed_sample().unwrap();
        assert!(timed.provenance.unwrap().real);
        if phone {
            assert_eq!(
                timed.age_bound,
                Some(SourceTimeBound {
                    host_clock_epoch: 7,
                    not_after_ns: 10
                })
            );
        }
        input.silence(&mut pcm);
        assert_eq!(pcm, [0.0; 480]);
        if phone {
            shared.retire();
            assert_eq!(input.render_timed_sample(), None);
            assert_eq!(input.render_sample(), None);
            assert_eq!(input.render(&mut pcm), None);
            assert_eq!(pcm, [0.0; 480]);
        }
    }
}

#[test]
fn voiced_phone_silence_does_not_create_receive_or_transmit_activity() {
    let (mut ingress, mut input, _) = prepare_voiced(
        reply(),
        8000,
        NonZeroU32::MIN,
        None,
        PhoneSpeechSettings::default(),
    )
    .unwrap();
    adopt(&mut ingress, 0, reference());
    for block in 0..10 {
        assert_eq!(
            ingress.audio(block * 160, &[0.0; 160], Some(birth(10))),
            Ok(160)
        );
    }
    for _ in 0..12 {
        let mut output = [1.0; 480];
        assert!(
            input.render_timed_block(&mut output).is_none(),
            "unqualified real silence is not voice"
        );
        assert!(output.iter().all(|sample| *sample == 0.0));
    }
}

#[test]
fn voiced_phone_delayed_decision_preserves_short_onset_in_the_existing_ring() {
    use rpt_advanced_core::link::ReceiveState;
    for chunk in [160, 7] {
        let (mut ingress, mut input, _) = prepare_voiced(
            reply(),
            8000,
            NonZeroU32::MIN,
            None,
            PhoneSpeechSettings::default(),
        )
        .unwrap();
        adopt(&mut ingress, 0, reference());
        let source: Vec<_> = (0..1600)
            .map(|frame| if frame < 640 { 0.25 } else { 0.0 })
            .collect();
        let mut start = 0;
        while start < 800 {
            let end = (start + chunk).min(800);
            let accepted = ingress
                .audio(start as u64, &source[start..end], Some(birth(10)))
                .unwrap();
            assert!(accepted > 0);
            start += accepted;
        }
        let mut receive =
            ReceiveState::with_policy(8000, ReceivePolicy::Phone { reserve_ms: 160 }).unwrap();
        assert!(
            !receive.should_render(input.signals().pcm_epoch(), input.available(), false, 480),
            "the existing 160-ms reserve still retains the onset"
        );
        // The positive frame ends after this short utterance. No PCM is copied,
        // rewound or queued in the classifier; only source admission changes.
        ingress.voice_decision(Some(799));
        while start < source.len() {
            let end = (start + chunk).min(source.len());
            start += ingress
                .audio(start as u64, &source[start..end], Some(birth(10)))
                .unwrap();
        }
        assert!(
            receive.should_render(input.signals().pcm_epoch(), input.available(), false, 480),
            "there is no second programme delay"
        );
        let mut onset = false;
        for _ in 0..9600 {
            let Some(sample) = input.render_timed_sample() else {
                continue;
            };
            if sample.provenance.is_some_and(|p| {
                p.real
                    && !p.muted
                    && p.position
                        .is_some_and(|position| !position.negative && position.frame < 640)
            }) && sample.sample > 0.1
            {
                onset = true;
                assert_eq!(sample.age_bound.unwrap().not_after_ns, 10);
            }
        }
        assert!(
            onset,
            "retained onset survives delayed qualification and partial credit {chunk}"
        );
    }
}

#[test]
fn voiced_phone_released_speech_composes_with_the_existing_product_mixer() {
    use crate::link::input::AudioInput;
    use rpt_advanced_core::{
        audio::{LinkAudioQueue, PcmRead, PcmStreamReader},
        controller::{ControllerSettings, CourtesySettings, NodeController, PreparedMedia},
        link::{AudioPeer, LinkAudio, Mode, PeerIdentity, SourcePosition},
    };
    // Deliberate constant routing/scheduling stand-in, NOT Morse P. The separate
    // courtesy_positive fixture checks configured native Morse and nonzero hold.
    struct Courtesy(usize);
    impl PcmStreamReader for Courtesy {
        fn start(&mut self) {
            self.0 = 960;
        }
        fn render(&mut self, output: &mut [f32]) -> PcmRead {
            let count = output.len().min(self.0);
            output[..count].fill(0.4);
            self.0 -= count;
            if self.0 == 0 {
                PcmRead::FinalSamples(count)
            } else {
                PcmRead::Samples(count)
            }
        }
    }
    let speech: Vec<f32> = include_bytes!("../tests/fixtures/phone-speech/yes-48000-s16le.pcm")
        .chunks_exact(12)
        .map(|p| f32::from(i16::from_le_bytes([p[0], p[1]])) / 32768.0)
        .chain(std::iter::repeat_n(0.0, 12_000))
        .collect();
    let guard = crate::link::phone_speech::loss_reset_contract_tests::NativeGuard::new();
    for (credit, processed, loss) in [(160, false, false), (160, true, false), (7, true, true)] {
        let call = reply();
        let source = call.phone_source().unwrap().clone();
        let (_graph_state, graph) = crate::services::history_tests::processor_fixture(1, 1);
        let (mut ingress, mut private, shared) = prepare_voiced(
            call.clone(),
            8000,
            NonZeroU32::MIN,
            processed.then_some(graph),
            PhoneSpeechSettings::default(),
        )
        .unwrap();
        adopt(&mut ingress, 0, reference());
        let private_out = private.take_output().unwrap();
        let phone = AudioPeer::for_phone(
            &source,
            &call,
            Mode::TRANSCEIVE,
            AudioInput::Phone(private),
            private_out,
            480,
        )
        .unwrap();
        let (_writer, ordinary) = InboundRing::open(48000, InboundPolicy::Peer).unwrap();
        let (public_out, mut public_reader) =
            LinkAudioQueue::new(1920, 64).unwrap().into_endpoints();
        let public = AudioPeer::new(
            PeerIdentity::from_operator("2000").unwrap(),
            Mode::TRANSCEIVE,
            AudioInput::Peer(ProcessedPeer::new(ordinary, None)),
            public_out,
            480,
            0,
        )
        .unwrap();
        let domain = rpt_advanced_core::link::RetirementDomain::default();
        let (mut mixer, mut dispatcher): (crate::worker::Audio, _) =
            LinkAudio::new_in_domain(vec![phone, public], 480, domain.clone()).unwrap();
        let (mut controller, mut control) = NodeController::new(
            ControllerSettings {
                courtesy_delay_ms: 20,
                ..Default::default()
            },
            vec![],
            vec![],
            CourtesySettings {
                phone: Some(PreparedMedia::new_stream(Box::new(Courtesy(960))).unwrap()),
                ..Default::default()
            },
        )
        .unwrap();
        controller.bind_phone_retirement(&mut control, domain);
        mixer = mixer.with_phone_clock(7);
        let mut native = 0;
        let mut voice = false;
        let mut remote_audio = false;
        let mut final_voice = false;
        let mut courtesy_frames = 0;
        let mut lost = false;
        for (block_index, block) in speech.chunks(160).enumerate() {
            let conceal = loss && voice && !lost;
            let mut offset = 0;
            while offset < block.len() {
                let end = (offset + credit).min(block.len());
                let start = block_index * 160 + offset;
                let (accepted, allocations) = guard.measure(|| {
                    if conceal {
                        ingress.concealed(start as u64, &block[offset..end], Some(birth(10)))
                    } else {
                        ingress.audio(
                            start as u64,
                            &block[offset..end],
                            Some(birth(10 + start as u64 * 125_000)),
                        )
                    }
                });
                assert_eq!(allocations, 0);
                let accepted = accepted.unwrap();
                assert!(accepted > 0);
                offset += accepted;
            }
            lost |= conceal;
            for _ in 0..2 {
                let mut rf = [0.0; 480];
                let (_, allocations) = guard.measure(|| {
                    mixer
                        .process(
                            &mut controller,
                            false,
                            &mut rf,
                            SourcePosition {
                                generation: 1,
                                sample: native,
                                stream: 1,
                                born_ns: Some(10 + native * 1_000_000_000 / 48_000),
                            },
                        )
                        .unwrap();
                    dispatcher.dispatch(2);
                });
                assert_eq!(allocations, 0);
                let metadata = mixer.rf_programme();
                if metadata.private && metadata.active {
                    voice = true;
                    assert_eq!(metadata.age_bound.unwrap().host_clock_epoch, 7);
                    assert!(
                        metadata.age_bound.unwrap().not_after_ns
                            <= 10 + block_index as u64 * 20_000_000
                    );
                }
                final_voice = metadata.active;
                if !metadata.active && rf.iter().any(|sample| *sample != 0.0) {
                    assert_eq!(
                        rf, [0.4; 480],
                        "only the prepared local P follows genuine close"
                    );
                    courtesy_frames += 480;
                }
                let mut output = [0.0; 480];
                while let Some(read) = public_reader.read_span(&mut output) {
                    assert_ne!(read.span.phone_expires_ns, 0);
                    assert!(
                        read.span.phone_expires_ns <= 500_000_010 + block_index as u64 * 20_000_000
                    );
                    remote_audio |= output.iter().any(|value| value.abs() > 0.01);
                    if courtesy_frames != 0 {
                        assert!(
                            output.iter().all(|sample| sample.abs() < 0.01),
                            "local courtesy cannot reach ordinary peer PCM"
                        );
                    }
                }
                native += 480;
            }
        }
        assert!(
            voice && remote_audio,
            "actual released analysis opens existing typed fanout ({credit})"
        );
        assert!(
            !final_voice,
            "the existing source-coordinate hang closes after silence"
        );
        assert_eq!(
            courtesy_frames, 960,
            "one complete prepared P, no silence-triggered repeats ({credit}, {processed})"
        );
        assert_eq!(
            lost, loss,
            "loss case must traverse the real concealed source seam after rendered open"
        );
        shared.retire();
        assert!(!call.ended(), "retiring voice preserves private control");
    }
}

#[test]
fn voiced_phone_preserves_the_qualified_dsp_tail_after_source_gate_closes() {
    let (state, graph) = crate::services::history_tests::processor_fixture(1, 1);
    let (mut ingress, mut input, shared) = prepare_voiced(
        reply(),
        48000,
        NonZeroU32::MIN,
        Some(graph),
        PhoneSpeechSettings::default(),
    )
    .unwrap();
    adopt(&mut ingress, 0, reference());
    // Exercise an exact qualification boundary independently of classifier hang.
    // The real ring and the qualified one-frame processor retain the sole PCM.
    ingress.speech = None;
    for block in 0..10 {
        let mut source = [0.0; 960];
        if block == 1 {
            source[959] = 0.75;
        }
        assert_eq!(
            ingress.audio(block * 960, &source, Some(birth(10))),
            Ok(960)
        );
    }
    shared.mark_voice(0, 1920);
    let mut previous_qualified = false;
    let mut observed_tail = false;
    for _ in 0..4000 {
        let before = input.input.source().qualified;
        let retained = state.retained_sample();
        let mut output = [0.0];
        let result = input.render_timed_block(&mut output);
        let qualified = input.input.source().qualified != before;
        if previous_qualified && !qualified {
            assert!(retained.abs() > 0.001, "the real ring retained the marker");
            assert_eq!(
                output,
                [retained],
                "the final admitted marker is still inside the DSP"
            );
            assert!(
                result.is_some(),
                "delayed output must retain voice activity"
            );
            assert_eq!(result.unwrap().age_bound.unwrap().not_after_ns, 10);
            observed_tail = true;
            break;
        }
        previous_qualified = qualified;
    }
    assert!(observed_tail);
}

#[test]
fn prepared_phone_media_drains_pre_cut_jitter_prefix_without_chasing_a_new_admission() {
    let (mut ingress, input, shared) = prepare(reply(), 8000, None).unwrap();
    adopt(&mut ingress, 160, reference());
    for start in [160, 320, 480] {
        assert_eq!(ingress.audio(start, &[0.75; 160], Some(birth(9))), Ok(160));
        assert!(
            input.active(),
            "a known pre-cut prefix must not move or retire the fresh cut"
        );
        assert_eq!(input.available(), 0);
        assert_eq!(ingress.cursor, None);
        assert_eq!(ingress.admission.now_ns, 10);
    }
    assert_eq!(ingress.audio(640, &[0.25; 160], Some(birth(11))), Ok(160));
    assert_eq!(input.available(), 160);
    assert_eq!(
        shared.bound(640).unwrap().not_after_ns,
        11,
        "retain actual birth"
    );
    assert_eq!(ingress.cursor, Some(800));
    assert_eq!(ingress.audio(800, &[0.75; 160], Some(birth(9))), Ok(160));
    assert!(
        !input.active(),
        "stale input after admitted PCM is still a discontinuity"
    );
    assert!(!shared.reply.ended());
}

#[test]
fn configured_phone_lookback_and_hold_are_independent_of_programme_reserve() {
    for reserve_ms in [86, 160, 280] {
        for (preroll_ms, hold_ms, first_voice, until) in [
            (0, 0, None, 1600),
            (0, 1000, None, 9600),
            (50, 10, Some(1200), 1680),
            (300, 0, Some(0), 1600),
        ] {
            let settings = PhoneSpeechSettings {
                preprocessing_enabled: true,
                threshold: 1.0,
                onset_frames: u32::MAX,
                preroll_ms,
                hold_ms,
                reserve_ms,
                ..Default::default()
            };
            let (mut ingress, input, shared) =
                prepare_voiced(reply(), 8000, NonZeroU32::MIN, None, settings).unwrap();
            adopt(&mut ingress, 0, reference());
            for start in (0..1600).step_by(160) {
                assert_eq!(ingress.audio(start, &[0.0; 160], Some(birth(10))), Ok(160));
            }
            ingress.voice_decision(Some(1599));
            assert_eq!(ingress.voice_until, until);
            for frame in 0..1600 {
                assert_eq!(
                    shared.voiced_frame(frame),
                    first_voice.is_some_and(|first| frame >= first)
                );
            }
            assert_eq!(input.receive_policy(), ReceivePolicy::Phone { reserve_ms });
            assert_eq!(ingress.audio(1600, &[0.0; 160], Some(birth(10))), Ok(160));
            assert_eq!(shared.voiced_frame(1600), hold_ms != 0);
            assert_eq!(shared.voiced_frame(1680), hold_ms > 10);
            ingress.voice_decision(Some(1599));
            assert_eq!(
                ingress.voice_until, until,
                "duplicate decisions cannot extend hold"
            );
        }
    }
}

#[test]
fn configured_phone_analysis_transmits_original_accepted_pcm_and_retries_only_unaccepted_suffix() {
    for rate in [8000, 16000, 48000] {
        let settings = PhoneSpeechSettings {
            preprocessing_enabled: true,
            threshold: 0.0,
            onset_frames: 1,
            ..Default::default()
        };
        let (mut ingress, mut input, _) =
            prepare_voiced(reply(), rate, NonZeroU32::MIN, None, settings).unwrap();
        adopt(&mut ingress, 0, reference());
        let width = rate as usize / 50;
        let offered = vec![0.25; width + 1];
        let mut start = 0;
        for _ in 0..15 {
            assert_eq!(ingress.audio(start, &offered, Some(birth(10))), Ok(width));
            start += width as u64;
        }
        assert_eq!(ingress.audio(start, &offered, Some(birth(10))), Ok(0));
        let mut original = 0;
        for _ in 0..9600 {
            if let Some(sample) = input.render_timed_sample() {
                if sample.provenance.is_some_and(|p| {
                    p.real
                        && !p.muted
                        && p.position.is_some_and(|position| {
                            !position.negative && position.frame > (width * 2) as u64
                        })
                }) {
                    assert!(
                        (sample.sample - 0.25).abs() < 0.01,
                        "filtered analysis PCM leaked at {rate}: {}",
                        sample.sample
                    );
                    original += 1;
                }
            }
        }
        assert!(
            original > 0,
            "selected analysis must qualify original programme at {rate}"
        );
        assert_eq!(ingress.audio(start, &offered, Some(birth(10))), Ok(width));
        assert_eq!(ingress.cursor, Some(start + width as u64));
    }
}

#[test]
fn configured_phone_media_rejects_unbounded_history_and_reserve_before_publication() {
    for (preroll_ms, reserve_ms) in [(301, 160), (160, 85), (160, 281)] {
        assert!(
            prepare_voiced(
                reply(),
                8000,
                NonZeroU32::MIN,
                None,
                PhoneSpeechSettings {
                    preroll_ms,
                    reserve_ms,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}

#[test]
fn timed_loss_continues_open_original_hold_without_refreshing_it() {
    for rate in [8000, 16000] {
        let (mut ingress, mut input, shared) = prepare_voiced(
            reply(),
            rate,
            NonZeroU32::MIN,
            None,
            PhoneSpeechSettings::default(),
        )
        .unwrap();
        adopt(&mut ingress, 0, reference());
        ingress.speech = None;
        let width = u64::from(rate) / 50;
        for block in 0..10 {
            assert_eq!(
                ingress.audio(block * width, &vec![0.25; width as usize], Some(birth(10))),
                Ok(width as usize)
            );
        }
        ingress.voice_decision(Some(10 * width - 1));
        let mut output = [0.0; 480];
        assert!((0..10).any(|_| input.render_timed_block(&mut output).is_some()));
        let hold = (ingress.last_voice, ingress.voice_until);
        assert_eq!(ingress.gap(10 * width, width as usize), Ok(width as usize));
        assert!(
            shared.active(),
            "known timed loss is not a programme discontinuity at {rate}"
        );
        assert_eq!((ingress.last_voice, ingress.voice_until), hold);
    }
}

#[test]
fn mixed_single_sample_credit_preserves_plc_origin() {
    for rate in [8000, 16000] {
        let (mut ingress, mut input, shared) = prepare_voiced(
            reply(),
            rate,
            NonZeroU32::MIN,
            None,
            PhoneSpeechSettings::default(),
        )
        .unwrap();
        adopt(&mut ingress, 0, reference());
        ingress.speech = None;
        let width = u64::from(rate) / 50;
        for frame in 0..width {
            let accepted = if frame % 2 == 0 {
                ingress.audio(frame, &[0.25], Some(birth(10)))
            } else {
                ingress.concealed(frame, &[0.5], Some(birth(20)))
            };
            assert_eq!(accepted, Ok(1));
        }
        ingress.voice_decision(Some(width - 1));
        for frame in 0..width {
            assert_eq!(
                shared.synthetic_at(position(&shared, frame, 0)),
                Some(frame % 2 == 1)
            );
            assert_eq!(shared.voiced_frame(frame), frame % 2 == 0);
        }
        assert_eq!(shared.synthetic_at(position(&shared, 0, 1)), Some(true));
        assert_eq!(shared.synthetic_at(position(&shared, 1, 1)), Some(true));

        let capacity = u64::from(rate) * 300 / 1000;
        for frame in width..capacity {
            assert_eq!(
                if frame % 2 == 0 {
                    ingress.audio(frame, &[0.25], Some(birth(10)))
                } else {
                    ingress.concealed(frame, &[0.5], Some(birth(20)))
                },
                Ok(1)
            );
        }
        assert_eq!(ingress.concealed(capacity, &[0.5], Some(birth(20))), Ok(0));
        assert_eq!(ingress.cursor, Some(capacity));
        for _ in 0..960 {
            input.render_timed_sample();
        }
        assert_eq!(ingress.concealed(capacity, &[0.5], Some(birth(20))), Ok(1));
        assert_eq!(ingress.audio(capacity + 1, &[0.25], Some(birth(30))), Ok(1));
        assert_eq!(
            shared.synthetic_at(position(&shared, capacity, 0)),
            Some(true)
        );
        assert_eq!(
            shared.synthetic_at(position(&shared, capacity + 1, 0)),
            Some(false)
        );
        assert_eq!(
            shared.synthetic_at(position(&shared, capacity, 1)),
            Some(true)
        );
    }
}

#[test]
fn later_voice_does_not_promote_synthetic_preroll_to_rf_onset() {
    let (mut ingress, mut input, shared) = prepare_voiced(
        reply(),
        8000,
        NonZeroU32::MIN,
        None,
        PhoneSpeechSettings::default(),
    )
    .unwrap();
    adopt(&mut ingress, 0, reference());
    ingress.speech = None;
    for frame in 0..1600 {
        assert_eq!(
            if frame < 800 {
                ingress.concealed(frame, &[0.5], Some(birth(10)))
            } else {
                ingress.audio(frame, &[0.25], Some(birth(20)))
            },
            Ok(1)
        );
    }
    ingress.voice_decision(Some(1599));
    let mut original_reached_output = false;
    for _ in 0..9600 {
        let Some(sample) = input.render_timed_sample() else {
            continue;
        };
        let Some(provenance) = sample.provenance else {
            continue;
        };
        if provenance.real && !provenance.muted {
            original_reached_output = true;
        }
        if provenance
            .position
            .is_some_and(|p| !p.negative && p.frame < 799)
        {
            assert!(provenance.plc && !provenance.real);
            assert!(
                provenance.muted,
                "later genuine VAD cannot promote synthetic onset"
            );
            assert_eq!(sample.sample, 0.0);
        }
    }
    assert!(original_reached_output);
    assert!(shared.active());
}

#[test]
fn phone_speech_closes_only_after_original_hold_and_processed_tail_drain() {
    // Controlled phase seam (zero hold and injected decision), not native VAD.
    // courtesy_positive covers real analysis with a nonzero original hold.
    use rpt_advanced_core::link::PhoneSpeechPhase;
    for processed in [false, true] {
        let (_state, graph) = crate::services::history_tests::processor_fixture(1, 1);
        let graph = processed.then_some(graph);
        let (mut ingress, mut input, shared) = prepare_voiced(
            reply(),
            48000,
            NonZeroU32::MIN,
            graph,
            PhoneSpeechSettings {
                hold_ms: 0,
                ..Default::default()
            },
        )
        .unwrap();
        adopt(&mut ingress, 0, reference());
        ingress.speech = None;
        assert_eq!(
            input.phone_speech_state().unwrap().phase,
            PhoneSpeechPhase::Unavailable
        );
        for block in 0..10 {
            assert_eq!(
                ingress.audio(block * 960, &[0.25; 960], Some(birth(10))),
                Ok(960)
            );
        }
        ingress.voice_decision(Some(1919));
        let mut open = false;
        let mut draining = false;
        let mut closed = false;
        for _ in 0..4000 {
            let mut output = [0.0];
            let result = input.render_timed_block(&mut output);
            match input.phone_speech_state().unwrap().phase {
                PhoneSpeechPhase::Open => open = true,
                PhoneSpeechPhase::Draining => {
                    draining = true;
                    assert!(result.is_some());
                    assert!(input.render_timed_block(&mut []).is_none());
                    assert_eq!(
                        input.phone_speech_state().unwrap().phase,
                        PhoneSpeechPhase::Draining
                    );
                }
                PhoneSpeechPhase::Closed => {
                    closed = true;
                    assert!(result.is_none());
                    break;
                }
                PhoneSpeechPhase::Unavailable => {}
            }
        }
        assert!(
            open && closed,
            "processed={processed} open={open} closed={closed} phase={:?} past_hold={}",
            input.speech_phase,
            input.input.source().past_hold
        );
        assert_eq!(
            draining, processed,
            "one-sample DSP retains one exact tail sample"
        );
        shared.retire();
        assert_eq!(
            input.phone_speech_state().unwrap().phase,
            PhoneSpeechPhase::Unavailable
        );
    }
}

#[test]
fn native_and_fallback_loss_render_only_inside_the_original_hold() {
    for rate in [8000, 16000] {
        for (native, queued) in [(false, false), (false, true), (true, false), (true, true)] {
            let (mut ingress, mut input, shared) = prepare_voiced(
                reply(),
                rate,
                NonZeroU32::MIN,
                None,
                PhoneSpeechSettings {
                    hold_ms: 20,
                    ..Default::default()
                },
            )
            .unwrap();
            adopt(&mut ingress, 0, reference());
            ingress.speech = None;
            let width = u64::from(rate) / 50;
            for block in 0..10 {
                assert_eq!(
                    ingress.audio(block * width, &vec![0.25; width as usize], Some(birth(10))),
                    Ok(width as usize)
                );
            }
            ingress.voice_decision(Some(10 * width - 1));
            if !queued {
                assert!((0..10).any(|_| input.render_timed_block(&mut [0.0; 480]).is_some()));
            }
            let hold = (ingress.last_voice, ingress.voice_until);
            let concealed_pcm = vec![0.5; width as usize];
            for block in 10..12 {
                let (accepted, allocations) = crate::fixture::allocations(|| {
                    if native {
                        ingress.concealed(block * width, &concealed_pcm, Some(birth(20)))
                    } else {
                        ingress.gap(block * width, width as usize)
                    }
                });
                assert_eq!(allocations, 0);
                assert_eq!(accepted, Ok(width as usize));
            }
            assert_eq!((ingress.last_voice, ingress.voice_until), hold);
            let mut rendered = 0;
            for _ in 0..18000 {
                let (value, allocations) =
                    crate::fixture::allocations(|| input.render_timed_sample());
                assert_eq!(allocations, 0);
                let Some(value) = value else {
                    continue;
                };
                let Some(provenance) = value.provenance else {
                    continue;
                };
                let Some(position) = provenance.position else {
                    continue;
                };
                if position.negative || position.frame < 10 * width || position.frame >= 12 * width
                {
                    continue;
                }
                assert!(!provenance.real && provenance.plc);
                assert_eq!(
                    value.age_bound.unwrap().not_after_ns,
                    10,
                    "loss cannot renew receipt age"
                );
                if position.frame < 11 * width - 1 && !provenance.zero_extension {
                    assert!(
                        !provenance.muted,
                        "held {rate} native={native} pos={position:?}"
                    );
                    rendered += 1;
                } else if position.frame >= 11 * width {
                    assert!(provenance.muted, "loss cannot renew source hold");
                }
            }
            assert!(
                rendered > 0,
                "mapped fallback/native must reach output {rate} native={native}"
            );
            assert!(shared.active());
        }
    }
}

#[test]
fn synthetic_age_does_not_poison_fresh_original_receipts_in_the_same_bucket() {
    for rate in [8000, 16000] {
        let (mut ingress, _input, shared) = prepare_voiced(
            reply(),
            rate,
            NonZeroU32::MIN,
            None,
            PhoneSpeechSettings::default(),
        )
        .unwrap();
        adopt(&mut ingress, 0, reference());
        ingress.speech = None;
        let width = u64::from(rate) / 50;
        assert_eq!(
            ingress.audio(0, &vec![0.25; width as usize], Some(birth(10))),
            Ok(width as usize)
        );
        assert_eq!(
            ingress.concealed(width, &vec![0.5; width as usize / 2], Some(birth(20))),
            Ok(width as usize / 2)
        );
        assert_eq!(
            ingress.audio(
                width + width / 2,
                &vec![0.25; width as usize / 2],
                Some(birth(30))
            ),
            Ok(width as usize / 2)
        );
        assert_eq!(shared.bound(width).unwrap().not_after_ns, 10);
        assert_eq!(shared.bound(width + width / 2).unwrap().not_after_ns, 30);
        assert_eq!(
            shared
                .locate(position(&shared, width + width / 2 - 1, 1))
                .unwrap()
                .not_after_ns,
            10
        );
        assert_eq!(
            ingress.gap(2 * width, width as usize / 2),
            Ok(width as usize / 2)
        );
        assert_eq!(shared.bound(2 * width).unwrap().not_after_ns, 30);
    }
}

#[test]
fn provider_shortfall_cannot_borrow_an_original_slot_as_synthetic_authority() {
    use rpt_advanced_core::link::SampleProvenance;
    let (_ingress, _input, shared) = prepare(reply(), 8000, None).unwrap();
    assert!(shared.record(0, 2, birth(10)));
    for (real, plc, gap) in [
        (false, true, false),
        (true, true, false),
        (false, false, true),
    ] {
        let mut sample = TimedPeerSample {
            sample: 0.5,
            age_bound: None,
            provenance: Some(SampleProvenance {
                real,
                plc,
                gap,
                position: Some(position(&shared, 0, 0)),
                ..Default::default()
            }),
        };
        shared.stamp(&mut sample);
        assert_eq!(
            sample.age_bound, None,
            "only exact synthetic ledger slots may inherit PLC age"
        );
    }
}

#[test]
fn delayed_dsp_cannot_open_from_new_input_before_genuine_output_sample_or_block() {
    for block in [false, true] {
        for native in [false, true] {
            for genuine_frames in [16, 32] {
                let (state, graph) = crate::services::history_tests::processor_fixture(8, 1);
                let (mut ingress, mut input, shared) = prepare_voiced(
                    reply(),
                    48000,
                    NonZeroU32::MIN,
                    Some(graph),
                    PhoneSpeechSettings {
                        preroll_ms: 0,
                        hold_ms: 20,
                        ..Default::default()
                    },
                )
                .unwrap();
                adopt(&mut ingress, 0, reference());
                ingress.speech = None;
                for start in [0, 960] {
                    assert_eq!(ingress.audio(start, &[0.25; 960], Some(birth(10))), Ok(960));
                }
                let end = 1920 + genuine_frames as u64;
                assert_eq!(
                    ingress.audio(1920, &vec![0.25; genuine_frames], Some(birth(10))),
                    Ok(genuine_frames)
                );
                ingress.voice_decision(Some(end - 1));
                shared.mark_voice(1920, end);
                assert_eq!(
                    if native {
                        ingress.concealed(end, &[0.5; 960], Some(birth(20)))
                    } else {
                        ingress.gap(end, 960)
                    },
                    Ok(960)
                );
                for start in (end + 960..end + 8640).step_by(960) {
                    assert_eq!(ingress.audio(start, &[0.25; 960], Some(birth(30))), Ok(960));
                }
                let mut first = false;
                let mut delivered = false;
                let mut opened = false;
                for _ in 0..4000 {
                    let mut pcm = [0.0];
                    let result = if block {
                        input.render_timed_block(&mut pcm).map(|_| ())
                    } else {
                        input.render_sample().map(|sample| {
                            pcm[0] = sample;
                        })
                    };
                    if !first && state.retained_sample() != 0.0 {
                        first = true;
                        assert_eq!(pcm[0], 0.0, "fixture output is still preceding muted PCM");
                        assert!(
                            !shared.output_open.load(Ordering::Acquire),
                            "new genuine input is not yet output: block={block} native={native}"
                        );
                        assert_eq!(
                            input.phone_speech_state().unwrap().phase,
                            PhoneSpeechPhase::Unavailable
                        );
                    } else if first && result.is_some() && pcm[0] != 0.0 {
                        delivered = true;
                    }
                    opened |= shared.output_open.load(Ordering::Acquire);
                }
                assert!(
                    first && delivered,
                    "genuine programme/tail must survive: block={block} native={native} frames={genuine_frames}"
                );
                assert_eq!(
                    opened,
                    genuine_frames > 1,
                    "long genuine delivery must authorize queued continuity"
                );
            }
        }
    }
}

#[test]
fn native_and_fallback_inherit_expired_age_even_with_fresh_egress_birth() {
    use rpt_advanced_core::audio::AudioSpan;
    for rate in [8000, 16000] {
        for native in [false, true] {
            let width = rate as usize / 50;
            let (mut ingress, mut input, shared) = prepare_voiced(
                reply(),
                rate,
                NonZeroU32::MIN,
                None,
                PhoneSpeechSettings {
                    hold_ms: 80,
                    ..Default::default()
                },
            )
            .unwrap();
            adopt(&mut ingress, 0, reference());
            ingress.speech = None;
            for block in 0..10 {
                assert_eq!(
                    ingress.audio((block * width) as u64, &vec![0.25; width], Some(birth(10))),
                    Ok(width)
                );
            }
            ingress.voice_decision(Some((10 * width - 1) as u64));
            let held = (ingress.last_voice, ingress.voice_until);
            for block in 10..13 {
                assert_eq!(
                    if native {
                        ingress.concealed(
                            (block * width) as u64,
                            &vec![0.5; width],
                            Some(birth(600_000_000)),
                        )
                    } else {
                        ingress.gap((block * width) as u64, width)
                    },
                    Ok(width)
                );
            }
            let mut pcm = Vec::new();
            let mut inherited = None;
            for _ in 0..18000 {
                let Some(value) = input.render_timed_sample() else {
                    continue;
                };
                if value.provenance.is_some_and(|p| {
                    p.plc
                        && !p.real
                        && !p.muted
                        && p.position
                            .is_some_and(|p| !p.negative && p.frame >= (10 * width + 2) as u64)
                }) {
                    inherited = value.age_bound;
                    pcm.push(value.sample);
                    if pcm.len() == 480 {
                        break;
                    }
                }
            }
            assert_eq!(pcm.len(), 480);
            let inherited = inherited.unwrap();
            assert_eq!(
                inherited.not_after_ns, 10,
                "new synthetic receipt cannot refresh original age"
            );
            assert_eq!((ingress.last_voice, ingress.voice_until), held);
            assert!(shared.active());
            let (mut io, state) = crate::link::session::tests::phone_source_peer_at_rate(rate);
            let (mut writer, reader) = LinkAudioQueue::new(960, 4).unwrap().into_endpoints();
            let mut outbound = crate::link::outbound::Outbound::new(rate, reader).unwrap();
            writer.write(
                AudioSpan {
                    born_ns: Some(599_000_000),
                    phone_expires_ns: inherited.not_after_ns + 500_000_000,
                    ..Default::default()
                },
                &pcm,
            );
            outbound.step(&mut io, 600).unwrap();
            assert_eq!(outbound.statistics.snapshot().expired_native_samples, 480);
            assert!(state.lock().unwrap().audio.is_empty());
            assert!(state.lock().unwrap().prepared.is_empty());
        }
    }
}

#[test]
fn configured_phone_media_reset_preserves_programme_hold_birth_and_requalifies_real_speech() {
    let speech: Vec<f32> = include_bytes!("../tests/fixtures/phone-speech/yes-48000-s16le.pcm")
        .chunks_exact(2)
        .step_by(6)
        .map(|p| f32::from(i16::from_le_bytes([p[0], p[1]])) / 32768.0)
        .collect();
    for native in [false, true] {
        let (mut ingress, mut input, shared) = prepare_voiced(
            reply(),
            8000,
            NonZeroU32::MIN,
            None,
            PhoneSpeechSettings {
                preprocessing_enabled: true,
                threshold: 0.75,
                onset_frames: 2,
                ..Default::default()
            },
        )
        .unwrap();
        adopt(&mut ingress, 1000, reference());
        let mut cursor = 1000;
        for loss in 0..3 {
            let origin = cursor;
            for block in speech.chunks(127) {
                assert_eq!(
                    ingress.audio(cursor, block, Some(birth(10))),
                    Ok(block.len())
                );
                cursor += block.len() as u64;
                for _ in 0..block.len() * 6 {
                    let _ = input.render_timed_sample();
                }
            }
            assert!(
                ingress
                    .last_voice
                    .is_some_and(|last| (origin..cursor).contains(&last)),
                "successor speech requalifies on its genuine coordinates: loss {loss}"
            );
            let hold = (ingress.last_voice, ingress.voice_until);
            let admission = ingress.admission;
            let retained = input.available();
            let prior = shared.bound(cursor - 1).unwrap();
            let (result, allocations) = crate::fixture::allocations(|| {
                if native {
                    ingress.concealed(cursor, &[0.9; 80], Some(birth(99)))
                } else {
                    ingress.gap(cursor, 80)
                }
            });
            assert_eq!(result, Ok(80));
            assert_eq!(allocations, 0);
            assert!(ingress.analysis_gap);
            assert_eq!((ingress.last_voice, ingress.voice_until), hold);
            assert_eq!(
                shared.bound(cursor).unwrap(),
                prior,
                "loss cannot refresh source age"
            );
            cursor += 80;
            // A one-sample genuine successor triggers reset but cannot complete a
            // new model frame or manufacture a new hold. Programme is not flushed.
            let (result, allocations) =
                crate::fixture::allocations(|| ingress.audio(cursor, &[0.25], Some(birth(20))));
            assert_eq!(result, Ok(1));
            assert_eq!(allocations, 0);
            assert!(!ingress.analysis_gap);
            assert!(shared.active());
            assert_eq!((ingress.last_voice, ingress.voice_until), hold);
            assert_eq!(
                ingress.admission.host_clock_epoch,
                admission.host_clock_epoch
            );
            assert_eq!(ingress.admission.now_ns, admission.now_ns);
            assert_eq!(input.available(), retained + 81);
            assert_eq!(shared.bound(cursor - 1).unwrap(), prior);
            cursor += 1;
            for _ in 0..81 * 6 {
                let _ = input.render_timed_sample();
            }
        }
    }
}

#[test]
fn configured_phone_media_defers_loss_reset_until_a_genuine_retry_is_accepted() {
    for rate in [8000, 16000] {
        let width = rate as usize / 50;
        let (mut ingress, mut input, shared) = prepare_voiced(
            reply(),
            rate,
            NonZeroU32::MIN,
            None,
            PhoneSpeechSettings {
                preprocessing_enabled: true,
                threshold: 0.0,
                onset_frames: 1,
                ..Default::default()
            },
        )
        .unwrap();
        adopt(&mut ingress, 0, reference());
        let original = vec![0.25; width];
        for block in 0..14 {
            assert_eq!(
                ingress.audio((block * width) as u64, &original, Some(birth(10))),
                Ok(width)
            );
        }
        assert!(ingress.last_voice.is_some());
        assert_eq!(ingress.gap((14 * width) as u64, width), Ok(width));
        let cursor = (15 * width) as u64;
        let hold = (ingress.last_voice, ingress.voice_until);
        for _ in 0..2 {
            assert_eq!(ingress.audio(cursor, &original, Some(birth(20))), Ok(0));
            assert!(
                ingress.analysis_gap,
                "zero credit must not reset or consume a prefix"
            );
            assert_eq!(ingress.cursor, Some(cursor));
            assert_eq!((ingress.last_voice, ingress.voice_until), hold);
            assert_eq!(input.available(), cursor);
        }
        for _ in 0..4800 {
            let _ = input.render_timed_sample();
        }
        let retained = input.available();
        assert!(retained > 0);
        let (result, allocations) =
            crate::fixture::allocations(|| ingress.audio(cursor, &original[..1], Some(birth(20))));
        assert_eq!(result, Ok(1));
        assert_eq!(allocations, 0);
        assert!(!ingress.analysis_gap);
        assert_eq!(
            input.available(),
            retained + 1,
            "successful reset retains older programme"
        );
        assert_eq!((ingress.last_voice, ingress.voice_until), hold);
        assert_eq!(shared.bound(cursor - 1).unwrap().not_after_ns, 10);
        assert_eq!(shared.bound(cursor).unwrap().not_after_ns, 20);
        assert!(shared.active());
    }
}

#[test]
fn native_ilbc_units_preserve_product_authority_hold_age_and_normal_requalification() {
    use rpt_advanced_standalone::phone::media::{AudioCodec, AudioProfile, DecodeProvenance};

    fn deliver(
        ingress: &mut Ingress,
        input: &mut PhoneMediaInput,
        cursor: &mut u64,
        pcm: &[f32],
        origin: DecodeProvenance,
        receipt: u64,
        recovered_only: bool,
    ) {
        let mut offset = 0;
        while offset != pcm.len() {
            let (result, allocations) = crate::fixture::allocations(|| {
                if origin == DecodeProvenance::Concealed {
                    ingress.concealed(*cursor, &pcm[offset..], Some(birth(receipt)))
                } else {
                    ingress.audio(*cursor, &pcm[offset..], Some(birth(receipt)))
                }
            });
            assert_eq!(allocations, 0);
            let accepted = result.unwrap();
            assert!(accepted > 0);
            *cursor += accepted as u64;
            offset += accepted;
            for _ in 0..accepted * 6 {
                if let Some(sample) = input.render_timed_sample() {
                    if recovered_only {
                        assert!(sample.provenance.is_none_or(|p| p.muted));
                        assert_eq!(
                            sample.sample, 0.0,
                            "recovered-only audio cannot key programme output"
                        );
                    }
                }
            }
        }
    }

    let speech: Vec<f32> = include_bytes!("../tests/fixtures/phone-speech/yes-48000-s16le.pcm")
        .chunks_exact(2)
        .step_by(6)
        .map(|p| f32::from(i16::from_le_bytes([p[0], p[1]])) / 32768.0)
        .collect();
    let mut codec = AudioCodec::load(AudioProfile::Ilbc30).unwrap();
    let mut packet = [0; 50];
    let mut pcm = [0.0; 240];
    // Preserve real native speech/enhancer history before the product sees only
    // damaged units. This is intentionally not a fresh silent decoder.
    for samples in speech.chunks(240) {
        let mut padded = [0.0; 240];
        padded[..samples.len()].copy_from_slice(samples);
        codec.encode(&padded, &mut packet).unwrap();
        codec.decode(&packet, &mut pcm).unwrap();
    }
    packet[49] |= 1;
    let (mut ingress, mut input, shared) = prepare_voiced(
        reply(),
        8000,
        NonZeroU32::MIN,
        None,
        PhoneSpeechSettings {
            preprocessing_enabled: true,
            threshold: 0.75,
            onset_frames: 2,
            ..Default::default()
        },
    )
    .unwrap();
    adopt(&mut ingress, 1000, reference());
    let mut cursor = 1000;
    for _ in 0..8 {
        let origin = codec.decode(&packet, &mut pcm).unwrap();
        assert_eq!(origin, DecodeProvenance::Concealed);
        deliver(
            &mut ingress,
            &mut input,
            &mut cursor,
            &pcm,
            origin,
            10,
            true,
        );
        assert_eq!(
            ingress.last_voice, None,
            "native recovered units never enter speech analysis"
        );
        assert_eq!(ingress.voice_until, 0);
    }
    for repetition in 0..2 {
        let original_start = cursor;
        for samples in speech.chunks(240) {
            let mut padded = [0.0; 240];
            padded[..samples.len()].copy_from_slice(samples);
            codec.encode(&padded, &mut packet).unwrap();
            let origin = codec.decode(&packet, &mut pcm).unwrap();
            assert_eq!(origin, DecodeProvenance::Original);
            deliver(
                &mut ingress,
                &mut input,
                &mut cursor,
                &pcm,
                origin,
                20,
                false,
            );
        }
        assert!(
            ingress
                .last_voice
                .is_some_and(|last| (original_start..cursor).contains(&last)),
            "valid decoded speech requalifies at normal thresholds after native recovery: {repetition}"
        );
        let hold = (ingress.last_voice, ingress.voice_until);
        let original_birth = shared.bound(cursor - 1).unwrap();
        let loss_start = cursor;
        packet[49] |= 1;
        let origin = codec.decode(&packet, &mut pcm).unwrap();
        assert_eq!(origin, DecodeProvenance::Concealed);
        deliver(
            &mut ingress,
            &mut input,
            &mut cursor,
            &pcm,
            origin,
            99,
            false,
        );
        assert_eq!((ingress.last_voice, ingress.voice_until), hold);
        assert_eq!(
            shared.bound(loss_start).unwrap(),
            original_birth,
            "damaged packet receipt cannot renew admitted original age"
        );
        assert!(shared.active());
    }
}

#[test]
fn phone_speech_invalidation_starvation_and_temporary_processor_pending_are_not_close() {
    use rpt_advanced_core::link::PhoneSpeechPhase;
    for cut in 0..6 {
        let call = reply();
        let (state, graph) = crate::services::history_tests::processor_fixture(1, 1);
        let (mut ingress, mut input, shared) = prepare_voiced(
            call.clone(),
            48000,
            NonZeroU32::MIN,
            Some(graph),
            PhoneSpeechSettings::default(),
        )
        .unwrap();
        adopt(&mut ingress, 0, reference());
        ingress.speech = None;
        for block in 0..10 {
            assert_eq!(
                ingress.audio(block * 960, &[0.25; 960], Some(birth(10))),
                Ok(960)
            );
        }
        ingress.voice_decision(Some(1919));
        for _ in 0..9600 {
            input.render_timed_block(&mut [0.0]);
            if input.phone_speech_state().unwrap().phase == PhoneSpeechPhase::Open {
                break;
            }
        }
        assert_eq!(
            input.phone_speech_state().unwrap().phase,
            PhoneSpeechPhase::Open
        );
        match cut {
            0 => shared.retire(),
            1 => call.retire(),
            2 => input.signals().set_dtmf_muted(true),
            3 => {
                ingress
                    .audio(
                        9600,
                        &[0.25],
                        Some(SourceBirth {
                            host_clock_epoch: 8,
                            born_ns: 10,
                        }),
                    )
                    .unwrap();
            }
            4 => input.silence(&mut [0.0]),
            _ => {
                state.set_output_pending(true);
                input.render_timed_block(&mut [0.0]);
            }
        }
        assert_eq!(
            input.phone_speech_state().unwrap().phase,
            PhoneSpeechPhase::Unavailable,
            "cut {cut} cannot fabricate Closed"
        );
        state.set_output_pending(false);
        for _ in 0..16 {
            input.render_timed_block(&mut [0.0]);
            assert_ne!(
                input.phone_speech_state().unwrap().phase,
                PhoneSpeechPhase::Closed,
                "neither no output nor its recovery proves original hold expiry"
            );
        }
        if cut == 5 {
            assert_eq!(
                input.phone_speech_state().unwrap().phase,
                PhoneSpeechPhase::Open
            );
        }
    }
}

#[path = "phone_courtesy_positive_tests.rs"]
mod courtesy_positive;

pub(in crate::link::private_phone) fn recovery_snapshot(shared: &Shared) -> (u64, bool) {
    (shared.generation, shared.active())
}

pub(in crate::link::private_phone) fn input_snapshot(input: &PhoneMediaInput) -> (u64, bool) {
    (input.shared.generation, input.active())
}
pub(in crate::link::private_phone) fn fresh_analysis(ingress: &Ingress) -> bool {
    ingress.speech.is_some() && ingress.last_voice.is_none() && ingress.voice_until == 0
}

#[test]
fn recovery_shared_and_ingress_state_sizes_are_bounded() {
    assert!(std::mem::size_of::<Shared>() <= 4304);
    eprintln!(
        "recovery_media_bytes shared={} ingress={}",
        std::mem::size_of::<Shared>(),
        std::mem::size_of::<Ingress>()
    );
}
