//! The released 48 kHz mono Opus provider for the WebRTC media owner.

use std::path::Path;

use rptadv_codec_client::Decoder;
use rptadv_codec_client::{CodecCatalog, Error, ProfileId, ProviderSelection, Status};
pub use rptadv_codec_client::{DecodeError, DecodeSpan, Encoder, PacketInfo};

/// Opus RTP and this provider's normalized PCM use the same 48 kHz clock.
pub const CLOCK_HZ: u32 = 48_000;
/// The codec's selected outgoing 20 ms mode.
pub const PACKET_FRAMES: u32 = 960;
/// Receive storage covers the complete legal Opus packet duration of 120 ms.
pub const RECEIVE_FRAMES: usize = 5_760;

/// The checked codec selection, shared across independent media contexts.
pub struct OpusProvider {
    catalog: CodecCatalog,
    mode: u32,
    encoder_bytes: usize,
}

impl OpusProvider {
    /// Select the independent, versioned codec installed by the composition.
    pub fn load() -> Result<Self, Error> {
        Self::load_from(Path::new("librptadv_codec_opus.so.1"))
    }

    /// Select an explicit provider path from the trusted composition.
    pub fn load_from(path: &Path) -> Result<Self, Error> {
        let catalog = CodecCatalog::load(&[ProviderSelection {
            path: path.to_owned(),
            profiles: vec![ProfileId::Opus],
        }])?;
        let mismatch = || Error {
            status: Status::Unsupported,
            required_capacity: 0,
            operation: "Opus media profile",
        };
        let profile = catalog.profile(ProfileId::Opus).ok_or_else(mismatch)?;
        if profile.sample_rate_hz != CLOCK_HZ
            || profile.maximum_frame_samples < RECEIVE_FRAMES as u32
        {
            return Err(mismatch());
        }
        let mode = profile
            .modes
            .iter()
            .find(|mode| mode.frame_samples == PACKET_FRAMES && mode.frame_bytes == 0)
            .ok_or_else(mismatch)?
            .id;
        let encoder_bytes = profile.maximum_frame_bytes as usize;
        Ok(Self {
            catalog,
            mode,
            encoder_bytes,
        })
    }

    /// Caller allocation for the selected outgoing encoded packet.
    pub fn encoder_bytes(&self) -> usize {
        self.encoder_bytes
    }

    /// Open an exclusive encoder for this connection's outbound lane.
    ///
    /// Retain its reported real source range and packet duration separately;
    /// the codec may cover fewer source samples than the packet clock advances.
    pub fn encoder(&self) -> Result<Encoder, Error> {
        self.catalog.encoder(
            ProfileId::Opus,
            self.mode,
            PACKET_FRAMES,
            self.encoder_bytes,
        )
    }

    /// Open an exclusive decoder after admission of the inbound lane.
    ///
    /// The owner supplies its bound on RTP payload bytes and inspects actual
    /// packet duration before a jitter handoff. Conceal only a known missing
    /// interval. Origin spans are available when the provider can attest them.
    pub fn decoder(&self, maximum_packet_bytes: usize) -> Result<OpusDecoder, Error> {
        let mut decoder = self.catalog.decoder(
            ProfileId::Opus,
            self.mode,
            PACKET_FRAMES,
            maximum_packet_bytes,
        )?;
        let provenance = match decoder.require_provenance() {
            Ok(()) => true,
            Err(error) if error.status == Status::Unsupported => false,
            Err(error) => {
                return Err(Error {
                    status: error.status,
                    required_capacity: error.required_pcm_capacity,
                    operation: "Opus decode provenance",
                });
            }
        };
        Ok(OpusDecoder {
            decoder,
            provenance,
        })
    }
}

/// One inbound PCM owner per admitted WebRTC media lane.
pub struct OpusDecoder {
    decoder: Decoder,
    provenance: bool,
}

/// A complete packet decode with optional codec-attested PCM origin.
pub struct OpusDecode {
    /// The actual count of normalized PCM frames.
    pub frames: usize,
    /// Number of populated spans, or absent if the provider reports no origin.
    pub span_count: Option<usize>,
}

impl OpusDecoder {
    /// Whether this exact checked provider can attest decoded sample origin.
    pub fn supports_provenance(&self) -> bool {
        self.provenance
    }

    /// Inspect the incoming duration without advancing the decoder history.
    pub fn inspect(&self, packet: &[u8]) -> Result<PacketInfo, Error> {
        self.decoder.inspect(packet)
    }

    /// Decode a received packet once. A legacy provider gives timing only:
    /// normal Opus decoding can conceal short coding units inside real packets.
    pub fn decode(
        &mut self,
        packet: &[u8],
        pcm: &mut [f32],
        spans: &mut [DecodeSpan],
    ) -> Result<OpusDecode, DecodeError> {
        let shape = self.inspect(packet).map_err(|error| DecodeError {
            status: error.status,
            required_pcm_capacity: error.required_capacity,
            required_span_capacity: 0,
        })?;
        let expected = shape.frames as usize;
        if expected == 0 || expected > RECEIVE_FRAMES {
            return Err(DecodeError {
                status: Status::Malformed,
                required_pcm_capacity: 0,
                required_span_capacity: 0,
            });
        }
        let (frames, span_count) = if self.provenance {
            let decoded = self.decoder.decode_with_provenance(packet, pcm, spans)?;
            (decoded.frames, Some(decoded.span_count))
        } else {
            let frames = self
                .decoder
                .decode(packet, pcm)
                .map_err(|error| DecodeError {
                    status: error.status,
                    required_pcm_capacity: error.required_capacity,
                    required_span_capacity: 0,
                })?;
            (frames, None)
        };
        if frames != expected {
            return Err(DecodeError {
                status: Status::Failed,
                required_pcm_capacity: expected,
                required_span_capacity: 0,
            });
        }
        Ok(OpusDecode { frames, span_count })
    }

    /// Request native PLC for a known missing duration, with known provenance.
    pub fn conceal(&mut self, requested: u32, pcm: &mut [f32]) -> Result<usize, Error> {
        self.decoder.conceal(requested, pcm)
    }

    /// Clear decoder history when the current media generation ends.
    pub fn reset(&mut self) -> Result<(), Error> {
        self.decoder.reset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use str0m::media::{Direction, Frequency, MediaKind};

    #[test]
    #[ignore = "requires the released Opus provider DSO"]
    fn released_opus_negotiates_and_exports_frames_and_provenance() {
        let now = Instant::now();
        let mut sender = super::super::transport(now);
        let mut change = sender.sdp_api();
        change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().unwrap();
        assert!(
            offer
                .to_sdp_string()
                .to_ascii_lowercase()
                .contains(" opus/48000/2")
        );
        let (receiver, answer) = super::super::accept_offer(now, offer).unwrap();
        let description = answer.to_sdp_string().to_ascii_lowercase();
        assert!(description.contains(" opus/48000/2"));
        assert!(
            description
                .lines()
                .any(|line| line.trim_end() == "a=recvonly")
        );
        sender.sdp_api().accept_answer(pending, answer).unwrap();
        let [negotiated] = receiver.codec_config().params() else {
            panic!("expect one accepted audio payload");
        };
        assert_eq!(negotiated.spec().clock_rate, Frequency::FORTY_EIGHT_KHZ);

        let provider = if let Some(path) = std::env::var_os("RPTADV_TEST_WEBRTC_OPUS") {
            OpusProvider::load_from(Path::new(&path))
        } else {
            OpusProvider::load()
        }
        .unwrap();
        let mut encoder = provider.encoder().unwrap();
        let mut decoder = provider.decoder(provider.encoder_bytes()).unwrap();
        let mut encoded = vec![0_u8; provider.encoder_bytes()];
        let samples: [f32; PACKET_FRAMES as usize] =
            std::array::from_fn(|index| (index as f32 * 0.058).sin() * 0.1);
        let frame = encoder.encode(&samples, &mut encoded, false).unwrap();
        assert!(frame.encoded_bytes > 0);
        assert_eq!(frame.consumed_frames, samples.len());
        assert_eq!(frame.packet_frames, PACKET_FRAMES);
        assert!(frame.source_frames <= frame.packet_frames);
        assert_eq!(frame.source_start_frames, 0);
        let packet = &encoded[..frame.encoded_bytes];
        assert_eq!(decoder.inspect(packet).unwrap().frames, PACKET_FRAMES);

        let mut pcm = vec![0.0; RECEIVE_FRAMES];
        let mut origins = [DecodeSpan::default(); 48];
        let decoded = decoder.decode(packet, &mut pcm, &mut origins).unwrap();
        assert_eq!(decoded.frames, PACKET_FRAMES as usize);
        if decoder.provenance {
            assert_eq!(decoded.span_count, Some(1));
            assert_eq!(origins[0].offset_frames, 0);
            assert_eq!(origins[0].frame_count, PACKET_FRAMES);
            assert_eq!(origins[0].origin, 0);
        } else {
            assert_eq!(decoded.span_count, None);
        }
        assert!(
            pcm[..decoded.frames]
                .iter()
                .any(|value| value.abs() > 0.0001)
        );

        // A shorter Opus packet still has the same RTP clock and uses its
        // inspected duration. This is a 10 ms fullband silence packet.
        let short = [0xf0, 0xff, 0xfe];
        assert_eq!(decoder.inspect(&short).unwrap().frames, 480);
        let decoded = decoder.decode(&short, &mut pcm, &mut origins).unwrap();
        assert_eq!(decoded.frames, 480);
        // A nonempty normal packet can contain a zero-length coding unit.
        // Verify this packet's duration while never inferring its PCM source.
        let toc = [0xf8];
        assert_eq!(decoder.inspect(&toc).unwrap().frames, PACKET_FRAMES);
        let decoded = decoder.decode(&toc, &mut pcm, &mut origins).unwrap();
        assert_eq!(decoded.frames, PACKET_FRAMES as usize);
        if !decoder.provenance {
            assert_eq!(decoded.span_count, None);
        }
        assert_eq!(decoder.conceal(480, &mut pcm).unwrap(), 480);
        decoder.reset().unwrap();
    }
}
