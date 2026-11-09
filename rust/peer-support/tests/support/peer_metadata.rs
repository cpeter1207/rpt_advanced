//! Check the sized peer-info write contract through each actual host callback.

/// Exercise valid prefixes, foreign headers, and bytes beyond the caller's capacity.
pub fn check(prefix: usize, whole: usize, mut callback: impl FnMut(*mut u8) -> i32) {
    #[repr(align(16))]
    struct Buffer([u8; 256]);
    assert!(whole + 16 <= 256);
    for (capacity, abi, accepted) in [
        (0, 1, false),
        (prefix - 1, 1, false),
        (whole, 2, false),
        (prefix, 1, true),
        (whole, 1, true),
        (whole + 16, 1, true),
    ] {
        let mut buffer = Buffer([0xa5; 256]);
        let output = buffer.0.as_mut_ptr();
        // The ABI header is borrowed from a full aligned test buffer even when
        // the advertised output capacity deliberately rejects the request.
        unsafe {
            output.cast::<u32>().write(capacity as u32);
            output.add(4).cast::<u32>().write(abi);
        }
        let before = buffer.0;
        assert_eq!(callback(output), if accepted { 0 } else { -1 });
        if accepted {
            let written = capacity.min(whole);
            assert_eq!(unsafe { output.cast::<u32>().read() } as usize, written);
            assert_eq!(&buffer.0[written..], &before[written..]);
        } else {
            assert_eq!(buffer.0, before);
        }
    }
}
