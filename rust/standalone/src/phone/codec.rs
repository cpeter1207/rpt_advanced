//! Selected released codec contexts through the existing neutral client.
use super::media::{AudioParameters, AudioProfile, MediaError};
use crate::abi;
use rptadv_codec_client::{CodecCatalog, Decoder, Encoder, ProviderSelection, Status};

/// One negotiated profile with independent encoder and decoder contexts.
pub struct AudioCodec {
    profile: AudioParameters,
    encoder: Encoder,
    decoder: Decoder,
}

impl AudioCodec {
    /// Validate provider metadata and prepare only the negotiated profile.
    pub fn load(codec: AudioProfile) -> Result<Self, MediaError> {
        let profile = codec.parameters();
        let catalog = CodecCatalog::load(&[ProviderSelection {
            path: "librptadv_codec_g711.so.1".into(),
            profiles: vec![profile.provider_profile],
        }])
        .map_err(|_| MediaError::Codec)?;
        let metadata = catalog
            .profile(profile.provider_profile)
            .ok_or(MediaError::Codec)?;
        let mode = metadata
            .modes
            .iter()
            .find(|mode| mode.id == profile.mode_id)
            .ok_or(MediaError::Codec)?;
        if metadata.sample_rate_hz != profile.pcm_rate_hz
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
        let decoder = catalog
            .decoder(
                profile.provider_profile,
                profile.mode_id,
                profile.packet_frames,
                profile.packet_bytes,
            )
            .map_err(|_| MediaError::Codec)?;
        Ok(Self {
            profile,
            encoder,
            decoder,
        })
    }
    /// Encode exactly one complete negotiated PCM packet.
    pub fn encode(&mut self, pcm: &[f32], packet: &mut [u8]) -> Result<(), MediaError> {
        if pcm.len() != self.profile.packet_frames as usize
            || packet.len() != self.profile.packet_bytes
        {
            return Err(MediaError::Codec);
        }
        let result = self
            .encoder
            .encode(pcm, packet, false)
            .map_err(|_| MediaError::Codec)?;
        if result.status != Status::Complete
            || result.consumed_frames != pcm.len()
            || result.encoded_bytes != packet.len()
            || result.source_frames != self.profile.packet_frames
            || result.packet_frames != self.profile.packet_frames
        {
            return Err(MediaError::Codec);
        }
        Ok(())
    }
    /// Adapt the common result into the host's existing sized ABI, without a second codec owner.
    pub(crate) fn prepare(
        &mut self,
        pcm: &[f32],
        packet: &mut [u8],
        flush: u32,
        result: &mut abi::rptadv_codec_encode_result_v1,
    ) -> i32 {
        if flush > 1 {
            return -1;
        }
        match self.encoder.encode(pcm, packet, flush != 0) {
            Ok(encoded) => {
                result.status = encoded.status.as_raw();
                result.consumed_frames = encoded.consumed_frames;
                result.encoded_bytes = encoded.encoded_bytes;
                result.source_start_frames = encoded.source_start_frames;
                result.source_frames = encoded.source_frames;
                result.packet_frames = encoded.packet_frames;
                result.required_capacity = 0;
                result.status as i32
            }
            Err(error) => {
                result.status = error.status.as_raw();
                result.required_capacity = error.required_capacity;
                result.status as i32
            }
        }
    }
    /// Retire transmit codec history; the product retains source timing ownership.
    pub(crate) fn reset_encoder(&mut self) -> Result<(), MediaError> {
        self.encoder.reset().map_err(|_| MediaError::Codec)
    }
    /// Inspect and decode the complete payload before any PCM overlap trimming.
    pub fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<(), MediaError> {
        let info = self
            .decoder
            .inspect(packet)
            .map_err(|_| MediaError::Codec)?;
        if packet.len() != self.profile.packet_bytes
            || info.frames != self.profile.packet_frames
            || info.mode != self.profile.mode_id
            || pcm.len() != self.profile.packet_frames as usize
        {
            return Err(MediaError::Codec);
        }
        let count = self
            .decoder
            .decode_recovering(packet, pcm)
            .map_err(|_| MediaError::Codec)?;
        if count != self.profile.packet_frames as usize {
            return Err(MediaError::Codec);
        }
        Ok(())
    }
}
