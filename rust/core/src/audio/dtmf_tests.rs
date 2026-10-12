use super::DtmfDetector;

const FRAME: usize = 612;

fn tone(row: f32, column: f32, first: usize) -> impl Iterator<Item = f32> {
    (0..FRAME).map(move |offset| {
        let time = (first + offset) as f32 / 48_000.0;
        0.031
            * ((std::f32::consts::TAU * row * time).sin()
                + (std::f32::consts::TAU * column * time).sin())
    })
}

fn fixture() -> Vec<f32> {
    let mut input = Vec::new();
    for _ in 0..4 {
        input.extend(tone(770.0, 1336.0, input.len())); // '5'
    }
    input.resize(input.len() + FRAME, 0.0);
    input.extend(tone(770.0, 1336.0, input.len()));
    input.resize(input.len() + 3 * FRAME, 0.0);
    for _ in 0..5 {
        let first = input.len();
        input.extend((0..FRAME).map(|offset| {
            let phase = std::f32::consts::TAU * (first + offset) as f32 / 48_000.0;
            0.05 * (phase * 697.0).sin() + 0.8 * (phase * 1209.0).sin()
        })); // excessive twist: no event, no muting
    }
    for _ in 0..2 {
        input.extend(tone(697.0, 1336.0, input.len())); // '2', completed at carrier loss
    }
    input
}

#[test]
fn released_descriptor_rejects_bad_prefix_identity_and_missing_operations() {
    assert!(DtmfDetector::provider_ready().is_ok());
    let descriptor = unsafe { &*super::rptadv_dtmf_descriptor() };
    assert!(unsafe { super::validate_descriptor(std::ptr::null()) }.is_err());
    let mut bad = *descriptor;
    bad.struct_size = 8;
    assert!(unsafe { super::validate_descriptor(&raw const bad) }.is_err());
    bad = *descriptor;
    bad.abi_version = 2;
    assert!(unsafe { super::validate_descriptor(&raw const bad) }.is_err());
    bad = *descriptor;
    bad.capability_name = std::ptr::null();
    assert!(unsafe { super::validate_descriptor(&raw const bad) }.is_err());
    bad = *descriptor;
    bad.capability_name = c"wrong.dtmf".as_ptr();
    assert!(unsafe { super::validate_descriptor(&raw const bad) }.is_err());
    macro_rules! missing {
        ($field:ident) => {{
            let mut bad = *descriptor;
            bad.$field = None;
            assert!(
                unsafe { super::validate_descriptor(&raw const bad) }.is_err(),
                stringify!($field)
            );
        }};
    }
    missing!(create);
    missing!(destroy);
    missing!(set_muting);
    missing!(suppressing);
    missing!(process);
}

#[test]
fn released_dtmf_matches_original_events_and_mutated_pcm_bits() {
    // Original controller c63db09 / beb8cef, native amd64 capture before replacement:
    // every partition emitted 5,2 and hashed all 9792 output f32 bit patterns identically.
    let input = fixture();
    assert_eq!(input.len(), 9792);
    for partition in [1, 37, FRAME, 960, input.len()] {
        let mut detector = DtmfDetector::new(true).unwrap();
        let mut output = input.clone();
        let mut events = Vec::new();
        for chunk in output.chunks_mut(partition) {
            detector.process(true, chunk, |digit| events.push(digit));
        }
        detector.process(false, &mut [], |digit| events.push(digit));
        assert!(!detector.suppressing());
        assert_eq!(events, ['5', '2'], "partition {partition}");
        let hash = output
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, sample| {
                sample
                    .to_bits()
                    .to_le_bytes()
                    .into_iter()
                    .fold(hash, |hash, byte| {
                        (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
                    })
            });
        assert_eq!(hash, 0x124d_1b80_5c93_10c5, "partition {partition}");
        assert!(
            output[2 * FRAME..4 * FRAME]
                .iter()
                .all(|sample| *sample == 0.0)
        );
        assert_eq!(output[9 * FRAME..14 * FRAME], input[9 * FRAME..14 * FRAME]);
    }
}

#[test]
fn muting_switch_and_carrier_loss_keep_the_original_contract() {
    let mut detector = DtmfDetector::new(false).unwrap();
    detector.set_muting(true);
    for block in 0..2 {
        let mut audio = tone(697.0, 1209.0, block * FRAME).collect::<Vec<_>>();
        detector.process(true, &mut audio, |_| panic!("digit completed early"));
    }
    assert!(detector.suppressing());
    let mut muted = tone(697.0, 1209.0, 2 * FRAME).collect::<Vec<_>>();
    detector.process(true, &mut muted, |_| panic!("digit completed early"));
    assert!(muted.iter().all(|sample| *sample == 0.0));
    detector.set_muting(false);
    assert!(!detector.suppressing());
    let mut events = Vec::new();
    detector.process(false, &mut [], |digit| events.push(digit));
    assert_eq!(events, ['1']);
}
