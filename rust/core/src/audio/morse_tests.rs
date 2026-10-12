use super::MorseRenderer;

#[test]
fn released_descriptor_rejects_bad_prefix_identity_and_missing_operations() {
    assert!(MorseRenderer::provider_ready().is_ok());
    let descriptor = unsafe { &*super::rptadv_morse_descriptor() };
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
    bad.capability_name = c"wrong.morse".as_ptr();
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
    missing!(render);
    missing!(restart);
    missing!(destroy);
}

#[test]
fn released_morse_matches_original_bits_across_partitions() {
    // Original controller source blobs at c63db09 and published 19a7e004 match.
    // Native amd64/arm64 original-source capture: librptadvmorse Actions 38287693814.
    let expected_hashes = match std::env::consts::ARCH {
        "x86_64" => [
            0x8b93_9bda_f931_e465_u64,
            0x68ab_9113_b68e_4c03,
            0x1d78_3fb1_fa36_6fc0,
        ],
        "aarch64" => [
            0x8b93_9bda_f931_e465_u64,
            0x68ab_9113_b68e_4c03,
            0x1d78_3fb1_fa36_6fc0,
        ],
        architecture => panic!("missing original-source baseline for {architecture}"),
    };
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/.,?-=+@()'!\":;_$&";
    for ((text, speed, frequency, level, total), expected_hash) in [
        (alphabet, 100, 1_000.0, -6, 461_376),
        ("EE E T I", 17, 1_000.0, -6, 111_811),
        ("E T I E", 37, 1_250.0, -11, 45_145),
    ]
    .into_iter()
    .zip(expected_hashes)
    {
        for block in [1, 7, 960] {
            let mut renderer = MorseRenderer::new(text, speed, frequency, level).unwrap();
            let mut count = 0;
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            loop {
                let mut output = vec![99.0_f32; block];
                let written = renderer.render(&mut output);
                assert!(output[written..].iter().all(|sample| *sample == 99.0));
                for sample in &output[..written] {
                    for byte in sample.to_bits().to_le_bytes() {
                        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
                    }
                }
                count += written;
                if written == 0 {
                    break;
                }
            }
            assert_eq!(count, total, "block {block}");
            assert_eq!(hash, expected_hash, "block {block}");
        }
    }

    let mut renderer = MorseRenderer::new("E T I E", 37, 1_250.0, -11).unwrap();
    let mut first = [0.0; 960];
    assert_eq!(renderer.render(&mut first), first.len());
    renderer.restart();
    let mut replay = [0.0; 960];
    assert_eq!(renderer.render(&mut replay), replay.len());
    assert!(
        first
            .iter()
            .zip(replay)
            .all(|(left, right)| left.to_bits() == right.to_bits())
    );
    assert_eq!(first[0].to_bits(), 0x0000_0000);
    assert_eq!(first[1].to_bits(), 0x3d3c_0c51);
}

#[test]
fn released_morse_rejects_invalid_input_and_finishes_cleanly() {
    for (text, speed, frequency, level) in [
        ("E*", 20, 1_000.0, -6),
        ("E", 0, 1_000.0, -6),
        ("E", 101, 1_000.0, -6),
        ("E", 20, 0.0, -6),
        ("E", 20, 24_000.0, -6),
        ("E", 20, f32::NAN, -6),
        ("E", 20, f32::INFINITY, -6),
        ("E", 20, 1_000.0, -61),
    ] {
        assert!(MorseRenderer::new(text, speed, frequency, level).is_err());
    }
    let mut empty = MorseRenderer::new("", 20, 1_000.0, -6).unwrap();
    assert_eq!(empty.render(&mut [0.0; 1]), 0);
    let mut dot = MorseRenderer::new("E", 20, 1_000.0, -6).unwrap();
    assert_eq!(dot.render(&mut []), 0);
    assert_eq!(dot.render(&mut [0.0; 2_881]), 2_880);
    assert_eq!(dot.render(&mut [0.0; 1]), 0);
}

#[test]
fn foreign_lifecycle_failures_release_handles_and_bound_written_count() {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DESTROYS: AtomicUsize = AtomicUsize::new(0);
    static RESTARTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn create_handle(
        _: *const u8,
        _: usize,
        _: u32,
        _: f32,
        _: i8,
        out: *mut *mut c_void,
    ) -> i32 {
        unsafe { *out = std::ptr::NonNull::<c_void>::dangling().as_ptr() };
        0
    }
    unsafe extern "C" fn create_error(
        text: *const u8,
        len: usize,
        speed: u32,
        frequency: f32,
        level: i8,
        out: *mut *mut c_void,
    ) -> i32 {
        unsafe { create_handle(text, len, speed, frequency, level, out) };
        1
    }
    unsafe extern "C" fn create_null(
        _: *const u8,
        _: usize,
        _: u32,
        _: f32,
        _: i8,
        _: *mut *mut c_void,
    ) -> i32 {
        0
    }
    unsafe extern "C" fn render(_: *mut c_void, _: *mut f32, _: usize, written: *mut usize) -> i32 {
        unsafe { *written = usize::MAX };
        0
    }
    unsafe extern "C" fn fail_render(_: *mut c_void, _: *mut f32, _: usize, _: *mut usize) -> i32 {
        1
    }
    unsafe extern "C" fn restart(_: *mut c_void) -> i32 {
        RESTARTS.fetch_add(1, Ordering::SeqCst);
        0
    }
    unsafe extern "C" fn destroy(_: *mut c_void) {
        DESTROYS.fetch_add(1, Ordering::SeqCst);
    }
    let api = super::MorseApi {
        create: create_handle,
        render,
        restart,
        destroy,
    };
    DESTROYS.store(0, Ordering::SeqCst);
    RESTARTS.store(0, Ordering::SeqCst);
    assert!(
        MorseRenderer::from_api(
            super::MorseApi {
                create: create_error,
                ..api
            },
            "E",
            20,
            1_000.0,
            -6
        )
        .is_err()
    );
    assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
    assert!(
        MorseRenderer::from_api(
            super::MorseApi {
                create: create_null,
                ..api
            },
            "E",
            20,
            1_000.0,
            -6
        )
        .is_err()
    );
    assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
    let mut renderer = MorseRenderer::from_api(api, "E", 20, 1_000.0, -6).unwrap();
    assert_eq!(renderer.render(&mut [0.0; 1]), 1);
    renderer.restart();
    assert_eq!(RESTARTS.load(Ordering::SeqCst), 1);
    renderer.render = fail_render;
    let mut output = [9.0_f32; 1];
    assert_eq!(renderer.render(&mut output), 0);
    assert_eq!(output, [9.0]);
    drop(renderer);
    assert_eq!(DESTROYS.load(Ordering::SeqCst), 2);
}

#[test]
fn trait_rendering_preserves_the_exact_dot_duration() {
    let mut renderer = MorseRenderer::new("E", 20, 1000.0, -6).unwrap();
    assert_eq!(
        crate::audio::AudioSource::render(&mut renderer, &mut [0.0; 2881]),
        2880
    );
}
