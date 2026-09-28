use super::*;
use std::cell::Cell;
thread_local! { static MODE: Cell<u8> = const { Cell::new(0) }; }

#[test]
fn old_unbounded_samplerate_descriptor_is_rejected() {
    // SAFETY: the installed descriptor is copied into process-lifetime storage.
    unsafe {
        let mut api = *ffi::rptadv_samplerate_adapter_descriptor();
        api.abi_version = 1;
        assert!(Egress::from_descriptor(8000, 960, Box::leak(Box::new(api))).is_err());
    }
}

#[test]
fn bandlimited_egress_retains_fir_input_without_duplication_across_partitions() {
    let source: Vec<f32> = (0..4096)
        .map(|index| (index as f32 * 0.07).sin() * 0.5)
        .collect();
    let mut whole = Egress::new(8000, 4096).unwrap();
    let expected = whole.process(&source).unwrap().to_vec();
    let mut split = Egress::new(8000, 4096).unwrap();
    let mut actual = split.process(&source[..1]).unwrap().to_vec();
    for chunk in source[1..].chunks(17) {
        actual.extend_from_slice(split.process(chunk).unwrap());
    }
    actual.extend_from_slice(split.process(&[]).unwrap());
    assert!(actual.len() > 600);
    assert_eq!(actual.len(), expected.len());
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(actual, expected)| (actual - expected).abs() < 0.00001)
    );
}
unsafe extern "C" fn failed_create(
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: *mut *mut ffi::rptadv_samplerate_converter,
) -> i32 {
    -1
}
unsafe extern "C" fn empty_create(
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: *mut *mut ffi::rptadv_samplerate_converter,
) -> i32 {
    0
}
unsafe extern "C" fn process(
    _: *mut ffi::rptadv_samplerate_converter,
    _: *const f32,
    input: u32,
    _: *mut f32,
    output: u32,
    _: f64,
    consumed: *mut u32,
    generated: *mut u32,
) -> i32 {
    // SAFETY: the converter wrapper supplies these two live output counters.
    unsafe {
        *consumed = if MODE.get() == 1 { input + 1 } else { 0 };
        *generated = if MODE.get() == 2 { output + 1 } else { 0 };
    }
    if MODE.get() == 0 { -1 } else { 0 }
}
#[test]
fn malformed_converter_tables_and_impossible_output_counts_fail_closed() {
    // SAFETY: released descriptor is immutable; copied tables live for process lifetime.
    unsafe {
        assert!(Egress::from_descriptor(8000, 16, ptr::null()).is_err());
        for (rate, maximum) in [(0, 16), (48001, 16), (8000, 0), (8000, 48001)] {
            assert!(Egress::new(rate, maximum).is_err());
        }
        let original = *ffi::rptadv_samplerate_adapter_descriptor();
        for case in 0..12 {
            let mut api = original;
            match case {
                0 => api.struct_size = 8,
                1 => api.abi_version = 99,
                2 => api.capability_name = ptr::null(),
                3 => api.capability_name = c"other".as_ptr(),
                4 => api.create = None,
                5 => api.process = None,
                6 => api.destroy = None,
                7 => api.create = Some(failed_create),
                8 => api.create = Some(empty_create),
                9 => api.reset = None,
                10 => api.queued_input = None,
                _ => api.converter_output_delay = None,
            }
            assert!(Egress::from_descriptor(8000, 16, Box::leak(Box::new(api))).is_err());
        }
        for mode in 0..4 {
            MODE.set(mode);
            let mut api = original;
            api.process = Some(process);
            let mut converter =
                Egress::from_descriptor(8000, 16, Box::leak(Box::new(api))).unwrap();
            if mode < 3 {
                assert!(converter.process(&[0.5; 16]).is_err());
            } else {
                assert!(converter.process(&[0.5; 16]).unwrap().is_empty());
                assert!(converter.process(&[0.5; 16]).unwrap().is_empty());
                assert!(converter.process(&[0.5]).is_err());
            }
        }
    }
}

#[test]
fn native_rate_is_copied_without_conversion_and_oversize_input_is_rejected() {
    let mut converter = Egress::new(48000, 4).unwrap();
    assert_eq!(converter.process(&[0.25, -0.25]).unwrap(), &[0.25, -0.25]);
    assert!(converter.process(&[0.0; 5]).is_err());
}
