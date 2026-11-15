//! Selected released codec contexts through the existing neutral client.
use super::media::{AudioParameters, AudioProfile, MediaError};
use crate::abi;
use rptadv_codec_client::{
    CodecCatalog, Decoder, EncodeResult, Encoder, Error, ProviderSelection, Status,
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

/// One negotiated profile with independent encoder and decoder contexts.
pub struct AudioCodec {
    profile: AudioParameters,
    encoder: Encoder,
    decoder: Decoder,
    retained: u32,
    finishing: bool,
    silence: Box<[f32]>,
    decode_native: Box<[u8]>,
}

impl AudioCodec {
    /// Prepare the selected profile using the same verified catalog as composition.
    pub fn load(codec: AudioProfile) -> Result<Self, MediaError> {
        Self::from_catalog(&load_catalog()?, codec)
    }
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
            retained: 0,
            finishing: false,
            silence: vec![0.0; profile.packet_frames as usize].into_boxed_slice(),
            decode_native: vec![
                0;
                if codec == AudioProfile::L16Wideband {
                    profile.packet_bytes
                } else {
                    0
                }
            ]
            .into_boxed_slice(),
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
            .encode_packet(pcm, packet, false)
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
    /// Retire transmit codec history; the product retains source timing ownership.
    pub(crate) fn reset_encoder(&mut self) -> Result<(), MediaError> {
        self.encoder.reset().map_err(|_| MediaError::Codec)?;
        self.retained = 0;
        self.finishing = false;
        Ok(())
    }
    /// Inspect and decode the complete payload before any PCM overlap trimming.
    pub fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<(), MediaError> {
        if packet.len() != self.profile.packet_bytes {
            return Err(MediaError::Codec);
        }
        let packet = if self.decode_native.is_empty() {
            packet
        } else {
            self.decode_native.copy_from_slice(packet);
            for sample in self.decode_native.chunks_exact_mut(2) {
                sample.swap(0, 1);
            }
            &self.decode_native
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    fn prepare(
        codec: &mut AudioCodec,
        input: &[f32],
        capacity: usize,
        flush: bool,
    ) -> (i32, abi::rptadv_codec_encode_result_v1, Vec<u8>) {
        let mut result: abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = std::mem::size_of_val(&result) as u32;
        let mut bytes = vec![0; capacity];
        let status = codec.prepare(input, &mut bytes, u32::from(flush), &mut result);
        (status, result, bytes)
    }
    #[test]
    fn finite_packets_preserve_real_source_span_and_pad_pcm_before_flush() {
        for profile in AudioProfile::ALL {
            let frames = profile.parameters().packet_frames as usize;
            let capacity = profile.parameters().packet_bytes;
            for count in [1, frames / 2, frames - 1, frames] {
                for retained in [false, true] {
                    let mut codec = AudioCodec::load(profile).unwrap();
                    let mut reference = AudioCodec::load(profile).unwrap();
                    let pcm = vec![0.125; count];
                    let mut padded = vec![0.0; frames];
                    padded[..count].copy_from_slice(&pcm);
                    let mut expected = vec![0; capacity];
                    reference.encode(&padded, &mut expected).unwrap();
                    if retained && count != frames {
                        let (code, result, _) = prepare(&mut codec, &pcm, capacity, false);
                        assert_eq!(code, 1);
                        assert_eq!(result.consumed_frames, count);
                        assert_eq!(result.encoded_bytes, 0);
                    }
                    let input = if retained && count != frames {
                        &[][..]
                    } else {
                        &pcm[..]
                    };
                    let (code, result, bytes) = prepare(&mut codec, input, capacity, true);
                    assert_eq!(code, 0, "{profile:?}/{count}/{retained}");
                    assert_eq!(result.consumed_frames, input.len());
                    assert_eq!(result.source_start_frames, 0);
                    assert_eq!(result.source_frames, count as u32);
                    assert_eq!(result.packet_frames, frames as u32);
                    assert_eq!(result.encoded_bytes, capacity);
                    assert_eq!(bytes, expected);
                    assert!(
                        prepare(&mut codec, &[0.25], capacity, false).0 > 2,
                        "every complete finite tail rejects fresh input until reset"
                    );
                    for _ in 0..2 {
                        let (code, empty, _) = prepare(&mut codec, &[], capacity, true);
                        assert_eq!(code, 0);
                        assert_eq!(
                            (
                                empty.consumed_frames,
                                empty.encoded_bytes,
                                empty.source_frames,
                                empty.packet_frames
                            ),
                            (0, 0, 0, 0)
                        );
                    }
                    codec.reset_encoder().unwrap();
                    let (code, reset, _) = prepare(&mut codec, &padded, capacity, false);
                    assert_eq!(code, 0);
                    assert_eq!(reset.source_start_frames, 0);
                    assert_eq!(reset.source_frames, frames as u32);
                }
            }
        }
    }
    #[test]
    fn finite_multi_packet_input_preserves_unconsumed_source_until_final_packet() {
        for profile in AudioProfile::ALL {
            let frames = profile.parameters().packet_frames as usize;
            let capacity = profile.parameters().packet_bytes;
            let mut codec = AudioCodec::load(profile).unwrap();
            let input = vec![0.125; frames * 2];
            let (code, first, _) = prepare(&mut codec, &input, capacity, true);
            assert_eq!(code, 0);
            assert_eq!(first.consumed_frames, frames);
            let (code, last, _) = prepare(&mut codec, &input[frames..], capacity, true);
            assert_eq!(code, 0);
            assert_eq!(
                (
                    last.source_start_frames,
                    last.source_frames,
                    last.packet_frames
                ),
                (frames as u64, frames as u32, frames as u32)
            );
            assert!(prepare(&mut codec, &[0.25], capacity, false).0 > 2);
            assert_eq!(prepare(&mut codec, &[], capacity, true).0, 0);
        }
    }
    #[test]
    fn finite_packet_capacity_failure_and_reset_never_consume_source() {
        for profile in AudioProfile::ALL {
            let frames = profile.parameters().packet_frames as usize;
            let capacity = profile.parameters().packet_bytes;
            let mut codec = AudioCodec::load(profile).unwrap();
            let (code, full, _) = prepare(&mut codec, &[0.25], capacity - 1, true);
            assert_eq!(code, 2);
            assert_eq!(full.required_capacity, capacity);
            assert_eq!((full.consumed_frames, full.encoded_bytes), (0, 0));
            let (code, tail, _) = prepare(&mut codec, &[0.25], capacity, true);
            assert_eq!(code, 0);
            assert_eq!(
                (
                    tail.source_start_frames,
                    tail.source_frames,
                    tail.packet_frames
                ),
                (0, 1, frames as u32)
            );
            assert!(prepare(&mut codec, &[0.25], capacity, false).0 > 2);
            codec.reset_encoder().unwrap();
            assert_eq!(prepare(&mut codec, &[0.25], capacity, false).0, 1);
            codec.reset_encoder().unwrap();
            let (code, fresh, _) = prepare(&mut codec, &vec![0.125; frames], capacity, false);
            assert_eq!(code, 0);
            assert_eq!(
                (fresh.source_start_frames, fresh.source_frames),
                (0, frames as u32)
            );
        }
    }
}
