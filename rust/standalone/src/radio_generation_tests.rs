use crate::{
    processing::ProcessingError, radio_config::RadioConfigError,
    radio_generation::RadioGenerationError, radio_session::RadioSessionError,
};

#[test]
fn displays_each_nested_generation_error() {
    assert_eq!(
        RadioGenerationError::Configuration(RadioConfigError::UnsupportedTone).to_string(),
        "configured CTCSS tone is not supported by the radio core"
    );
    assert_eq!(
        RadioGenerationError::Processing(ProcessingError::MissingGraph).to_string(),
        "FFmpeg returned no graph"
    );
    assert_eq!(
        RadioGenerationError::Session(RadioSessionError::MissingHandle).to_string(),
        "radio session returned no handle"
    );
}

#[test]
fn displays_each_native_session_error() {
    for (error, message) in [
        (
            RadioSessionError::IncompleteDescriptor,
            "radio session ABI is incomplete",
        ),
        (
            RadioSessionError::Create(-3),
            "radio session creation failed (-3)",
        ),
        (
            RadioSessionError::MissingHandle,
            "radio session returned no handle",
        ),
        (RadioSessionError::Warm(-4), "radio DSP warmup failed (-4)"),
        (
            RadioSessionError::IncompleteCallbacks,
            "radio session callbacks are incomplete",
        ),
        (
            RadioSessionError::InvalidFrame,
            "radio session received an invalid stereo frame",
        ),
        (
            RadioSessionError::Process(-5),
            "radio session callback failed (-5)",
        ),
    ] {
        assert_eq!(error.to_string(), message);
    }
}
