//! Selected released codec contexts through the existing neutral client.
use super::media::{AudioParameters, AudioProfile, MAX_RECEIVE_MS, MediaError};
use crate::abi;
use rptadv_codec_client::{
    CodecCatalog, DecodeSpan, Decoder, EncodeResult, Encoder, Error, ProviderSelection, Status,
};

fn codec_error(status: Status) -> Error {
    Error {
        status,
        required_capacity: 0,
        operation: "SIP packet contract",
    }
}

/// Startup-only verification; calls and reloads reuse this immutable provider catalog.
pub(crate) fn load_catalog() -> Result<CodecCatalog, MediaError> {
    let catalog = CodecCatalog::load(&[
        ProviderSelection {
            path: "librptadv_codec_g711.so.1".into(),
            profiles: vec![
                rptadv_codec_client::ProfileId::G711Mu,
                rptadv_codec_client::ProfileId::G711A,
            ],
        },
        ProviderSelection {
            path: "librptadv_codec_spandsp.so.1".into(),
            profiles: vec![
                rptadv_codec_client::ProfileId::G722,
                rptadv_codec_client::ProfileId::G726,
                rptadv_codec_client::ProfileId::G726Aal2,
            ],
        },
        ProviderSelection {
            path: "librptadv_codec_gsm.so.1".into(),
            profiles: vec![rptadv_codec_client::ProfileId::GsmFr],
        },
        ProviderSelection {
            path: "librptadv_codec_ilbc.so.1".into(),
            profiles: vec![rptadv_codec_client::ProfileId::Ilbc],
        },
        ProviderSelection {
            path: "librptadv_codec_pcm.so.1".into(),
            profiles: vec![
                rptadv_codec_client::ProfileId::Pcm8,
                rptadv_codec_client::ProfileId::Pcm16,
            ],
        },
    ])
    .map_err(|_| MediaError::Codec)?;
    for profile in AudioProfile::ALL {
        // Validate the actual negotiated packet configuration in both directions.
        let _ = AudioCodec::from_catalog(&catalog, profile)?;
    }
    Ok(catalog)
}

/// Origin of decoded PCM at the provider boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeProvenance {
    /// PacketDecoded coding unit; ordinary prior decoder/filter history remains.
    Original,
    /// NativeRecovered coding unit or explicit PLC; neither initiates speech.
    Concealed,
    /// Native recovery supplied no PCM; use the existing fallback instead.
    Missing,
}

/// One negotiated profile with independent encoder and decoder contexts.
pub struct AudioCodec {
    profile: AudioParameters,
    encoder: Encoder,
    decoder: Decoder,
    receive_frames: u32,
    receive_bytes: usize,
    retained: u32,
    finishing: bool,
    silence: Box<[f32]>,
    decode_native: Box<[u8]>,
    decode_spans: Box<[DecodeSpan]>,
    decode_span_count: usize,
}

impl AudioCodec {
    pub(crate) fn from_catalog(
        catalog: &CodecCatalog,
        codec: AudioProfile,
    ) -> Result<Self, MediaError> {
        let profile = codec.parameters();
        let metadata = catalog
            .profile(profile.provider_profile)
            .ok_or(MediaError::Codec)?;
        let mode = metadata
            .modes
            .iter()
            .find(|mode| mode.id == profile.mode_id)
            .ok_or(MediaError::Codec)?;
        if metadata.lookahead_frames != 0
            || metadata.sample_rate_hz != profile.pcm_rate_hz
            || profile.packet_frames % mode.frame_samples != 0
            || profile.packet_frames / mode.frame_samples * mode.frame_bytes
                != profile.packet_bytes as u32
        {
            return Err(MediaError::Codec);
        }
        let encoder = catalog
            .encoder(
                profile.provider_profile,
                profile.mode_id,
                profile.packet_frames,
                profile.packet_bytes,
            )
            .map_err(|_| MediaError::Codec)?;
        // Receive is independently bounded in complete native coding units.
        // GSM remains20ms under this30ms ceiling; TX retains its selected cadence.
        let units = (profile.pcm_rate_hz / 1000 * MAX_RECEIVE_MS) / mode.frame_samples;
        let receive_frames = units
            .checked_mul(mode.frame_samples)
            .filter(|frames| *frames >= profile.packet_frames)
            .ok_or(MediaError::Codec)?;
        let receive_bytes = usize::try_from(
            units
                .checked_mul(mode.frame_bytes)
                .ok_or(MediaError::Codec)?,
        )
        .map_err(|_| MediaError::Codec)?;
        let mut decoder = catalog
            .decoder(
                profile.provider_profile,
                profile.mode_id,
                receive_frames,
                receive_bytes,
            )
            .map_err(|_| MediaError::Codec)?;
        // Probe the actual opened configuration without invented media. Every
        // current SIP profile coalesces to one span under the unchanged 30 ms bound.
        decoder
            .require_provenance()
            .map_err(|_| MediaError::Codec)?;
        Ok(Self {
            decode_spans: vec![DecodeSpan::default(); 1].into_boxed_slice(),
            decode_span_count: 0,
            profile,
            encoder,
            decoder,
            receive_frames,
            receive_bytes,
            retained: 0,
            finishing: false,
            silence: vec![0.0; profile.packet_frames as usize].into_boxed_slice(),
            decode_native: vec![
                0;
                if codec == AudioProfile::L16Wideband {
                    receive_bytes
                } else {
                    0
                }
            ]
            .into_boxed_slice(),
        })
    }
    /// Adapt the common result into the host's existing sized ABI, without a second codec owner.
    pub(crate) fn prepare(
        &mut self,
        pcm: &[f32],
        packet: &mut [u8],
        flush: u32,
        result: &mut abi::rptadv_codec_encode_result_v1,
    ) -> i32 {
        // Every result is fresh even when the caller reuses its sized record.
        result.consumed_frames = 0;
        result.encoded_bytes = 0;
        result.source_start_frames = 0;
        result.source_frames = 0;
        result.packet_frames = 0;
        result.required_capacity = 0;
        let encoded = if flush > 1 {
            Err(codec_error(Status::Malformed))
        } else {
            self.encode_packet(pcm, packet, flush != 0)
        };
        match encoded {
            Ok(encoded) => {
                result.status = encoded.status.as_raw();
                result.consumed_frames = encoded.consumed_frames;
                result.encoded_bytes = encoded.encoded_bytes;
                result.source_start_frames = encoded.source_start_frames;
                result.source_frames = encoded.source_frames;
                result.packet_frames = encoded.packet_frames;
            }
            Err(error) => {
                result.status = error.status.as_raw();
                result.required_capacity = error.required_capacity;
            }
        }
        result.status as i32
    }
    // One packet per turn. PCM padding belongs here, before native finite flush,
    // because provider coding units need not equal the negotiated RTP packet.
    fn encode_packet(
        &mut self,
        pcm: &[f32],
        packet: &mut [u8],
        flush: bool,
    ) -> Result<EncodeResult, Error> {
        let encoded = self.encode_native_packet(pcm, packet, flush)?;
        if !self.decode_native.is_empty() {
            // The IAX-compatible PCM16 provider is little endian; RTP L16 is big endian.
            for sample in packet[..encoded.encoded_bytes].chunks_exact_mut(2) {
                sample.swap(0, 1);
            }
        }
        Ok(encoded)
    }
    fn encode_native_packet(
        &mut self,
        pcm: &[f32],
        packet: &mut [u8],
        flush: bool,
    ) -> Result<EncodeResult, Error> {
        if self.finishing && (!pcm.is_empty() || !flush) {
            return Err(codec_error(Status::Malformed));
        }
        if packet.len() < self.profile.packet_bytes {
            return Err(Error {
                required_capacity: self.profile.packet_bytes,
                ..codec_error(Status::OutputFull)
            });
        }
        let mut encoded = self.encoder.encode(pcm, packet, false)?;
        let retained = self
            .retained
            .checked_add(encoded.consumed_frames as u32)
            .filter(|count| *count <= self.profile.packet_frames)
            .ok_or_else(|| codec_error(Status::Failed))?;
        if encoded.encoded_bytes != 0 {
            if encoded.status != Status::Complete
                || encoded.encoded_bytes != self.profile.packet_bytes
                || encoded.packet_frames != self.profile.packet_frames
                || encoded.source_frames != self.profile.packet_frames
                || retained != self.profile.packet_frames
            {
                return Err(codec_error(Status::Failed));
            }
            self.retained = 0;
            self.finishing = flush && encoded.consumed_frames == pcm.len();
            return Ok(encoded);
        }
        if encoded.source_frames != 0
            || encoded.packet_frames != 0
            || retained >= self.profile.packet_frames
        {
            return Err(codec_error(Status::Failed));
        }
        self.retained = retained;
        if !flush {
            return Ok(encoded);
        }
        if encoded.consumed_frames != pcm.len() {
            return Err(codec_error(Status::Failed));
        }
        if retained == 0 {
            // Native flush can now only finish an empty, zero-lookahead encoder.
            self.finishing = true;
            let finished = self.encoder.encode(&[], packet, true)?;
            if finished.status != Status::Complete
                || finished.encoded_bytes != 0
                || finished.consumed_frames != 0
                || finished.source_frames != 0
                || finished.packet_frames != 0
            {
                return Err(codec_error(Status::Failed));
            }
            return Ok(finished);
        }
        let padding = (self.profile.packet_frames - retained) as usize;
        let padded = self
            .encoder
            .encode(&self.silence[..padding], packet, false)?;
        if padded.status != Status::Complete
            || padded.consumed_frames != padding
            || padded.encoded_bytes != self.profile.packet_bytes
            || padded.source_frames != self.profile.packet_frames
            || padded.packet_frames != self.profile.packet_frames
        {
            return Err(codec_error(Status::Failed));
        }
        // Neither consumed input nor source provenance may include local silence.
        encoded.status = padded.status;
        encoded.encoded_bytes = padded.encoded_bytes;
        encoded.source_start_frames = padded.source_start_frames;
        encoded.source_frames = retained;
        encoded.packet_frames = padded.packet_frames;
        self.retained = 0;
        self.finishing = true;
        Ok(encoded)
    }
    pub(super) fn reset_decoder(&mut self) -> Result<(), MediaError> {
        self.decoder.reset().map_err(|_| MediaError::Codec)?;
        self.decode_span_count = 0;
        Ok(())
    }
    /// Retire transmit codec history; the product retains source timing ownership.
    pub(crate) fn reset_encoder(&mut self) -> Result<(), MediaError> {
        self.encoder.reset().map_err(|_| MediaError::Codec)?;
        self.retained = 0;
        self.finishing = false;
        Ok(())
    }
    /// Complete-unit receive workspace bound, independent of fixed TX framing.
    pub(crate) fn receive_frames(&self) -> u32 {
        self.receive_frames
    }
    /// Encoded receive workspace bound for the selected native profile.
    pub(crate) fn receive_bytes(&self) -> usize {
        self.receive_bytes
    }
    /// Framing-only inspection before RTP source adoption; never advances decode/PLC.
    pub fn inspect(&self, packet: &[u8]) -> Result<u32, MediaError> {
        if packet.is_empty() || packet.len() > self.receive_bytes {
            return Err(MediaError::InvalidPacket);
        }
        let info = self.decoder.inspect(packet).map_err(|error| {
            if error.status == Status::Malformed {
                MediaError::InvalidPacket
            } else {
                MediaError::Codec
            }
        })?;
        // Every currently enabled profile has fixed native coding units. A
        // successful provider result must agree with that verified byte/PCM ratio.
        if info.frames == 0
            || info.frames > self.receive_frames
            || info.mode != self.profile.mode_id
            || u64::from(info.frames) * self.profile.packet_bytes as u64
                != packet.len() as u64 * u64::from(self.profile.packet_frames)
        {
            return Err(MediaError::Codec);
        }
        Ok(info.frames)
    }
    /// Decode once and retain exhaustive coding-unit origins before overlap trimming.
    /// Failure never grants successful PCM or permission to call native PLC again.
    pub fn decode(
        &mut self,
        packet: &[u8],
        pcm: &mut [f32],
    ) -> Result<DecodeProvenance, MediaError> {
        self.decode_span_count = 0;
        let frames = self.inspect(packet).map_err(|_| MediaError::Codec)? as usize;
        if pcm.len() != frames {
            return Err(MediaError::Codec);
        }
        let packet = if self.decode_native.is_empty() {
            packet
        } else {
            let native = &mut self.decode_native[..packet.len()];
            native.copy_from_slice(packet);
            for sample in native.chunks_exact_mut(2) {
                sample.swap(0, 1);
            }
            native
        };
        let decoded = self
            .decoder
            .decode_with_provenance(packet, pcm, &mut self.decode_spans)
            .map_err(|_| MediaError::Codec)?;
        if decoded.frames != frames || decoded.span_count != 1 {
            return Err(MediaError::Codec);
        }
        self.decode_span_count = decoded.span_count;
        Ok(if self.decode_spans[0].origin == 0 {
            DecodeProvenance::Original
        } else {
            DecodeProvenance::Concealed
        })
    }
    /// Validated immutable packet origins, retained until the next actual decode.
    pub(crate) fn decoded_spans(&self) -> &[DecodeSpan] {
        &self.decode_spans[..self.decode_span_count]
    }
    /// Request native PLC exactly once. Missing means the caller must use fallback.
    pub fn conceal(
        &mut self,
        frames: u32,
        pcm: &mut [f32],
    ) -> Result<DecodeProvenance, MediaError> {
        match self
            .decoder
            .conceal(frames, pcm)
            .map_err(|_| MediaError::Codec)?
        {
            0 => Ok(DecodeProvenance::Missing),
            count if count == frames as usize => Ok(DecodeProvenance::Concealed),
            _ => Err(MediaError::Codec),
        }
    }
}
