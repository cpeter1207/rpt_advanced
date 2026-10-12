use super::ToneSequence;

#[test]
fn released_descriptor_rejects_bad_prefix_identity_and_missing_operations() {
    let descriptor = unsafe { &*super::rptadv_tone_descriptor() };
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
    bad.capability_name = c"wrong.tone".as_ptr();
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
    missing!(sequence_create);
    missing!(sequence_rendered_samples);
    missing!(sequence_render);
    missing!(sequence_destroy);
    missing!(oscillator_create);
    missing!(oscillator_reset);
    missing!(oscillator_render);
    missing!(oscillator_destroy);
}

#[test]
fn released_sequence_preserves_original_bits_and_partition_counts() {
    // Original controller c63db09, native amd64 and arm64 captures; both
    // independently produced these exact f32 bits and written counts.
    // The amd64 fixture is in librptadvtone/.work/baseline-linux-bits.txt;
    // native CI capture: cpeter1207/librptadvmorse run 38287693814.
    let mut whole_hash = None;
    for (text, block, total, expected_hash) in [
        ("1100/1,1100/1", 256, 96, 0x45b7_1f71_55cd_4f3b_u64),
        ("1100/1,1100/1", 7, 96, 0x45b7_1f71_55cd_4f3b),
        ("1000+1100@-6/1", 256, 48, 0x01f8_88c9_afd6_2643),
        ("1000+1100@-6/1", 7, 48, 0x01f8_88c9_afd6_2643),
        ("silence/1,1000/1", 256, 96, 0x876e_8275_2b4c_6ebc),
        ("silence/1,1000/1", 7, 96, 0x876e_8275_2b4c_6ebc),
    ] {
        let mut sequence = ToneSequence::new(text, -20).unwrap();
        assert_eq!(sequence.rendered_samples(), total);
        let mut count = 0;
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        loop {
            let mut output = vec![99.0_f32; block];
            let written = sequence.render(&mut output);
            assert_eq!(written, (total - count).min(block));
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
        assert_eq!(count, total);
        if block == 256 {
            whole_hash = Some(hash);
        } else {
            assert_eq!(Some(hash), whole_hash, "partition changed {text}");
        }
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        assert_eq!(hash, expected_hash, "{text}, block {block}");
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let _ = expected_hash;
    }
}

#[test]
fn released_sequence_rejects_invalid_text_and_finishes_cleanly() {
    assert!(ToneSequence::provider_ready().is_ok());
    assert!(ToneSequence::new("1000/0", -6).is_err());
    assert!(ToneSequence::new("1000/1", -61).is_err());
    let mut sequence = ToneSequence::new("1000/1", -6).unwrap();
    assert_eq!(sequence.render(&mut []), 0);
    assert_eq!(sequence.render(&mut [0.0; 49]), 48);
    assert_eq!(sequence.render(&mut [0.0; 1]), 0);
}

#[test]
fn impossible_provider_render_error_returns_zero_without_changing_output() {
    unsafe extern "C" fn fail(
        _: *mut std::ffi::c_void,
        _: *mut f32,
        _: usize,
        _: *mut usize,
    ) -> i32 {
        1
    }
    unsafe extern "C" fn no_op_destroy(_: *mut std::ffi::c_void) {}
    let mut sequence = ToneSequence {
        handle: std::ptr::NonNull::dangling(),
        rendered_samples: 1,
        render: fail,
        destroy: no_op_destroy,
    };
    let mut output = [9.0_f32; 1];
    assert_eq!(sequence.render(&mut output), 0);
    assert_eq!(output, [9.0]);
}

#[test]
fn foreign_sequence_lifecycle_failures_release_owned_handles() {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DESTROYS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn create_handle(
        _: *const u8,
        _: usize,
        _: i8,
        out: *mut *mut c_void,
    ) -> i32 {
        unsafe { *out = std::ptr::NonNull::<c_void>::dangling().as_ptr() };
        0
    }
    unsafe extern "C" fn create_error(
        text: *const u8,
        len: usize,
        db: i8,
        out: *mut *mut c_void,
    ) -> i32 {
        unsafe { create_handle(text, len, db, out) };
        2
    }
    unsafe extern "C" fn create_null(_: *const u8, _: usize, _: i8, _: *mut *mut c_void) -> i32 {
        0
    }
    unsafe extern "C" fn samples_ok(_: *const c_void, out: *mut usize) -> i32 {
        unsafe { *out = 48 };
        0
    }
    unsafe extern "C" fn samples_error(_: *const c_void, _: *mut usize) -> i32 {
        1
    }
    unsafe extern "C" fn render(_: *mut c_void, _: *mut f32, _: usize, written: *mut usize) -> i32 {
        unsafe { *written = usize::MAX };
        0
    }
    unsafe extern "C" fn destroy(_: *mut c_void) {
        DESTROYS.fetch_add(1, Ordering::SeqCst);
    }
    let api = super::ToneApi {
        create: create_handle,
        rendered_samples: samples_ok,
        render,
        destroy,
    };
    DESTROYS.store(0, Ordering::SeqCst);
    assert!(
        ToneSequence::from_api(
            super::ToneApi {
                create: create_error,
                ..api
            },
            "a",
            -6
        )
        .is_err()
    );
    assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
    assert!(
        ToneSequence::from_api(
            super::ToneApi {
                create: create_null,
                ..api
            },
            "a",
            -6
        )
        .is_err()
    );
    assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
    assert!(
        ToneSequence::from_api(
            super::ToneApi {
                rendered_samples: samples_error,
                ..api
            },
            "a",
            -6
        )
        .is_err()
    );
    assert_eq!(DESTROYS.load(Ordering::SeqCst), 2);
    let mut sequence = ToneSequence::from_api(api, "a", -6).unwrap();
    assert_eq!(sequence.rendered_samples(), 48);
    assert_eq!(sequence.render(&mut [0.0; 1]), 1);
    drop(sequence);
    assert_eq!(DESTROYS.load(Ordering::SeqCst), 3);
}

#[test]
fn trait_rendering_obeys_exact_tone_duration() {
    let mut sequence = ToneSequence::new("1000/1", -6).unwrap();
    let mut samples = [0.0; 49];
    assert_eq!(
        crate::audio::AudioSource::render(&mut sequence, &mut samples),
        48
    );
    assert_eq!(
        crate::audio::AudioSource::render(&mut sequence, &mut samples),
        0
    );
}
