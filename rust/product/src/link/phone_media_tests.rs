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
    super::prepare(reply, rate, NonZeroU32::MIN, processor)
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
    for time in [
        None,
        Some(birth(9)),
        Some(SourceBirth {
            host_clock_epoch: 8,
            born_ns: 10,
        }),
    ] {
        let (mut ingress, input, _) = prepare(reply(), 8000, None).unwrap();
        adopt(&mut ingress, 0, reference());
        assert_eq!(ingress.audio(0, &[0.25; 160], time), Ok(160));
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
        assert_eq!(input.input.receive_policy(), ReceivePolicy::Phone);
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
    assert_eq!(private.receive_policy(), ReceivePolicy::Phone);
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
fn prepared_phone_media_revision_adds_only_scalar_storage_to_existing_ingress() {
    type Unstamped = (
        Arc<Shared>,
        InboundProducer,
        u64,
        SourceTimeReference,
        Option<u64>,
    );
    let previous = std::mem::size_of::<Unstamped>();
    let actual = std::mem::size_of::<Ingress>();
    eprintln!("prepared_ingress_bytes unstamped={previous} stamped={actual}");
    assert!(actual <= previous + std::mem::align_of::<Ingress>());
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
                ReceivePolicy::Phone
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
