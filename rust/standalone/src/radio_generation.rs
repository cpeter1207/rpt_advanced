//! Owned FFmpeg graph and native radio-core setup for one standalone generation.

use crate::{ResolvedRadioNode, abi, processing::FfmpegPorts, radio_session::NativeRadioSession};
use std::{pin::Pin, ptr::NonNull};

/// A standalone radio generation could not be fully prepared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioGenerationError {
    /// Node settings do not fit the radio-core session ABI.
    Configuration(crate::radio_config::RadioConfigError),
    /// An FFmpeg graph could not be prepared or warmed.
    Processing(crate::processing::ProcessingError),
    /// The radio core rejected session creation or warmup.
    Session(crate::radio_session::RadioSessionError),
}

impl std::fmt::Display for RadioGenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(error) => error.fmt(formatter),
            Self::Processing(error) => error.fmt(formatter),
            Self::Session(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RadioGenerationError {}

/// One warmed radio core with its configured FFmpeg graph contexts retained.
pub struct PreparedRadioGeneration<'a> {
    session: Option<NativeRadioSession<'a>>,
    ports: NonNull<abi::rptadv_radio_session_ports>,
    _graphs: FfmpegPorts<'a>,
    _program: Option<Pin<Box<crate::program_audio::ProgramAudio>>>,
}

impl<'a> PreparedRadioGeneration<'a> {
    /// Create and warm the configured native session before opening audio hardware.
    pub fn prepare(
        radio_api: &'a abi::rptadv_radio_descriptor,
        ffmpeg_api: &'a abi::rptadv_ffmpeg_adapter_descriptor,
        radio: &ResolvedRadioNode,
        generation_id: u64,
        maximum_frames: u32,
        publication_interval_ms: u32,
    ) -> Result<Self, RadioGenerationError> {
        Self::prepare_inner(
            radio_api,
            ffmpeg_api,
            radio,
            generation_id,
            maximum_frames,
            publication_interval_ms,
            None,
        )
    }

    /// Prepare a generation with an optional external exact-frame program source.
    ///
    /// The generation owns the pinned program source through session destruction.
    pub fn prepare_with_program(
        radio_api: &'a abi::rptadv_radio_descriptor,
        ffmpeg_api: &'a abi::rptadv_ffmpeg_adapter_descriptor,
        radio: &ResolvedRadioNode,
        generation_id: u64,
        maximum_frames: u32,
        publication_interval_ms: u32,
        program: Pin<Box<crate::program_audio::ProgramAudio>>,
    ) -> Result<Self, RadioGenerationError> {
        Self::prepare_inner(
            radio_api,
            ffmpeg_api,
            radio,
            generation_id,
            maximum_frames,
            publication_interval_ms,
            Some(program),
        )
    }

    fn prepare_inner(
        radio_api: &'a abi::rptadv_radio_descriptor,
        ffmpeg_api: &'a abi::rptadv_ffmpeg_adapter_descriptor,
        radio: &ResolvedRadioNode,
        generation_id: u64,
        maximum_frames: u32,
        publication_interval_ms: u32,
        mut program: Option<Pin<Box<crate::program_audio::ProgramAudio>>>,
    ) -> Result<Self, RadioGenerationError> {
        let config = crate::radio_config::radio_session_config(
            radio,
            generation_id,
            maximum_frames,
            publication_interval_ms,
        )
        .map_err(RadioGenerationError::Configuration)?;
        let mut graphs = FfmpegPorts::create(ffmpeg_api, radio, maximum_frames)
            .map_err(RadioGenerationError::Processing)?;
        let program_port = program.as_mut().map(|source| source.as_mut().port());
        let ports = NonNull::from(Box::leak(Box::new(graphs.ports_with_program(program_port))));
        // SAFETY: the boxed port table and graph owners are retained by this generation. Drop
        // destroys the session first, then reclaims the table; graph contexts drop afterward.
        let ports_ref: &'a abi::rptadv_radio_session_ports = unsafe { ports.as_ref() };
        let session = match NativeRadioSession::prepare(radio_api, &config, ports_ref) {
            Ok(session) => session,
            Err(error) => {
                // SAFETY: preparation failed before a session could retain any port context.
                unsafe { drop(Box::from_raw(ports.as_ptr())) };
                return Err(RadioGenerationError::Session(error));
            }
        };
        Ok(Self {
            session: Some(session),
            ports,
            _graphs: graphs,
            _program: program,
        })
    }

    /// Split the native generation into callback owners and borrow its optional program source.
    pub fn split(
        &mut self,
    ) -> Result<
        (
            crate::radio_session::ReceiveEndpoint<'_, 'a>,
            crate::radio_session::TransmitEndpoint<'_, 'a>,
            Option<Pin<&mut crate::program_audio::ProgramAudio>>,
        ),
        crate::radio_session::RadioSessionError,
    > {
        let Self {
            session, _program, ..
        } = self;
        let (receive, transmit) = session.as_mut().expect("live prepared session").split()?;
        let program = _program.as_mut().map(Pin::as_mut);
        Ok((receive, transmit, program))
    }
}

impl Drop for PreparedRadioGeneration<'_> {
    fn drop(&mut self) {
        self.session.take();
        // SAFETY: session destruction completed before reclaiming the port table it borrowed.
        unsafe { drop(Box::from_raw(self.ports.as_ptr())) };
    }
}
