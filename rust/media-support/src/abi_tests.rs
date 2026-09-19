use super::*;

#[test]
fn rejects_incompatible_or_incomplete_tables_before_invocation() {
    assert!(matches!(
        unsafe { Descriptor::from_pointer(std::ptr::null()) },
        Err(crate::MediaError::IncompatibleAdapter)
    ));
    assert!(matches!(
        unsafe { RawConfig::from_pointer(std::ptr::null()) },
        Err(crate::MediaError::InvalidRequest)
    ));
    let valid = crate::provider::DESCRIPTOR;
    assert_eq!(valid.validate(), Ok(()));
    let mut incompatible = valid;
    incompatible.abi_version += 1;
    assert!(matches!(
        unsafe { Descriptor::from_pointer(&incompatible) },
        Err(crate::MediaError::IncompatibleAdapter)
    ));
    let incompatible_config = RawConfig {
        struct_size: size_of::<RawConfig>() as u32,
        abi_version: ABI_VERSION + 1,
        executable: std::ptr::null(),
        temporary_directory: std::ptr::null(),
        timeout_ms: 1,
        reaper_acquire: None,
        reaper_release: None,
    };
    assert!(matches!(
        unsafe { RawConfig::from_pointer(&incompatible_config) },
        Err(crate::MediaError::InvalidRequest)
    ));
    let mut candidates = [valid; 7];
    candidates[0].struct_size -= 1;
    candidates[1].abi_version += 1;
    candidates[2].capability[0] = b'x';
    candidates[3].create = None;
    candidates[4].destroy = None;
    #[cfg(file_adapter)]
    {
        candidates[5].prepare_file = None;
    }
    #[cfg(speech_adapter)]
    {
        candidates[5].prepare_speech = None;
    }
    candidates[6].release_audio = None;
    for table in candidates {
        assert_eq!(
            table.validate(),
            Err(crate::MediaError::IncompatibleAdapter)
        );
    }
}
