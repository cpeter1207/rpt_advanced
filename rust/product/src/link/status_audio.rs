//! Preallocated metering of admitted peer audio on the serial media owner.
use crate::status_meter::{Measurement, NATIVE_RATE, Quality, Reader, Span, Tap};
use std::cell::RefCell;

/// A named peer graph stage's negotiated format and last exact measurement.
pub(crate) struct StageReading {
    pub(crate) rate: u32,
    pub(crate) available: bool,
    pub(crate) measurement: Option<Measurement>,
}

/// The Product-owned media boundaries available for a direct peer or attached call.
pub(crate) struct PeerMeasurements {
    /// Identity of the installed decoded graph, available before its first measurement.
    pub(crate) ingress_graph_generation: Option<u64>,
    pub(crate) decoded: StageReading,
    pub(crate) mix_minus: StageReading,
    pub(crate) codec_input: StageReading,
    pub(crate) native_ingress: Option<NativeIngressMeasurements>,
    #[cfg(target_os = "linux")]
    pub(crate) provider_output: Option<crate::configuration_radio::ProviderOutput>,
}

/// Actual native mono playout after the ring and before the peer receive/mix gates.
pub(crate) struct NativeIngressMeasurements {
    pub(crate) graph_generation: Option<u64>,
    pub(crate) ring_output: StageReading,
    pub(crate) dsp_output: StageReading,
}

/// Owning callback's writers, allocated alongside the exact peer ring.
pub(crate) struct NativeIngressWriter {
    ring: Tap,
    dsp: Option<Tap>,
}

/// Off-audio reader for the corresponding peer ring allocation.
pub(super) struct NativeIngressReaders {
    ring: Reader,
    dsp: Option<Reader>,
}

impl NativeIngressWriter {
    pub(super) fn new(has_dsp: bool) -> (Self, NativeIngressReaders) {
        let (ring, ring_reader) = Tap::new();
        let (dsp, dsp_reader) = has_dsp.then(Tap::new).unzip();
        (
            Self { ring, dsp },
            NativeIngressReaders {
                ring: ring_reader,
                dsp: dsp_reader,
            },
        )
    }

    fn observe(tap: &mut Tap, span: Option<Span>, samples: &[f32]) {
        if let Some(span) = span {
            tap.observe(span, samples);
        } else {
            tap.invalidate(Quality::Unavailable);
        }
    }

    pub(crate) fn ring(&mut self, span: Option<Span>, samples: &[f32]) {
        Self::observe(&mut self.ring, span, samples);
    }

    pub(crate) fn dsp(&mut self, span: Option<Span>, samples: &[f32]) {
        if let Some(tap) = &mut self.dsp {
            Self::observe(tap, span, samples);
        }
    }

    pub(crate) fn dsp_failed(&mut self) {
        if let Some(tap) = &mut self.dsp {
            tap.invalidate(Quality::Fault);
        }
    }

    pub(crate) fn unavailable(&mut self) {
        self.ring(None, &[]);
        self.dsp(None, &[]);
    }
}

impl NativeIngressReaders {
    pub(super) fn readings(&mut self) -> NativeIngressMeasurements {
        let ring = self.ring.latest();
        let dsp = self.dsp.as_mut().and_then(Reader::latest);
        let graph_generation = ring
            .iter()
            .chain(dsp.iter())
            .max_by_key(|value| (value.monotonic_ms, value.graph_generation))
            .map(|value| value.graph_generation);
        NativeIngressMeasurements {
            graph_generation,
            ring_output: StageReading {
                rate: NATIVE_RATE,
                available: true,
                measurement: ring.filter(|value| Some(value.graph_generation) == graph_generation),
            },
            dsp_output: StageReading {
                rate: NATIVE_RATE,
                available: self.dsp.is_some(),
                measurement: dsp.filter(|value| Some(value.graph_generation) == graph_generation),
            },
        }
    }
}

pub(super) struct PeerReaders {
    rate: u32,
    decoded: RefCell<Option<Reader>>,
    mixed: RefCell<Option<Reader>>,
    codec: RefCell<Option<Reader>>,
}

impl PeerReaders {
    pub(super) fn new(
        rate: u32,
        decoded: Option<Reader>,
        egress: (Option<Reader>, Option<Reader>),
    ) -> Self {
        Self {
            rate,
            decoded: RefCell::new(decoded),
            mixed: RefCell::new(egress.0),
            codec: RefCell::new(egress.1),
        }
    }

    pub(super) fn readings(&self, ingress_graph_generation: Option<u64>) -> PeerMeasurements {
        fn read(rate: u32, reader: &RefCell<Option<Reader>>) -> StageReading {
            let mut reader = reader.borrow_mut();
            StageReading {
                rate,
                available: reader.is_some(),
                measurement: reader.as_mut().and_then(Reader::latest),
            }
        }
        PeerMeasurements {
            ingress_graph_generation,
            decoded: read(self.rate, &self.decoded),
            mix_minus: read(NATIVE_RATE, &self.mixed),
            codec_input: read(self.rate, &self.codec),
            native_ingress: None,
            #[cfg(target_os = "linux")]
            provider_output: None,
        }
    }
}

/// The decoded boundary counts only accepted samples, including when the provider retries.
pub(super) struct DecodedMeter {
    tap: Option<Tap>,
    position: u64,
}

impl DecodedMeter {
    pub(super) fn new(rate: u32) -> (Self, Option<Reader>) {
        let (tap, reader) = Tap::at_rate(rate).unzip();
        (Self { tap, position: 0 }, reader)
    }

    pub(super) fn accept(
        &mut self,
        graph: u64,
        source: u64,
        start: Option<u64>,
        end_ms: u64,
        pcm: &[f32],
    ) {
        if let Some(tap) = &mut self.tap {
            let first_sample = start.unwrap_or(self.position);
            tap.observe(
                Span {
                    graph_generation: graph,
                    source_generation: source,
                    first_sample,
                    end_ms,
                },
                pcm,
            );
            self.position = first_sample.saturating_add(pcm.len() as u64);
        }
    }

    pub(super) fn discontinuity(&mut self) {
        if let Some(tap) = &mut self.tap {
            tap.invalidate(Quality::Unavailable);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_prefixes_warm_exactly_once_and_unavailable_stages_stay_explicit() {
        let (mut producer, reader) = DecodedMeter::new(8_000);
        let readers = PeerReaders::new(8_000, reader, (None, None));
        let speech = [0.25; 400];
        for index in 0..8 {
            let position = 400 * index;
            producer.accept(3, 92, Some(position), 50 * index, &[]);
            producer.accept(3, 92, Some(position), 50 * index + 25, &speech[..200]);
            producer.accept(
                3,
                92,
                Some(position + 200),
                50 * (index + 1),
                &speech[200..],
            );
            let view = readers.readings(Some(3));
            assert!(view.decoded.available);
            assert_eq!(view.decoded.rate, 8_000);
            assert_eq!(
                view.decoded.measurement.unwrap().valid_samples,
                400 * (index + 1)
            );
            assert!(!view.mix_minus.available);
            assert!(!view.codec_input.available);
            assert!(view.codec_input.measurement.is_none());
        }
        producer.discontinuity();
        let source = readers.readings(Some(3)).decoded.measurement.unwrap();
        assert_eq!(source.quality, Quality::Unavailable);
        assert_eq!(source.valid_samples, 0);
    }
}

#[cfg(test)]
mod native_ingress_tests {
    use super::*;

    #[test]
    fn native_meter_uses_rendered_pcm_and_native_sample_position() {
        let (mut writer, mut readers) = NativeIngressWriter::new(true);
        let startup = readers.readings();
        assert!(startup.ring_output.available && startup.dsp_output.available);
        assert_eq!(startup.graph_generation, None);

        let span = Span {
            graph_generation: 19,
            source_generation: 4,
            first_sample: 96_000,
            end_ms: 2_100,
        };
        writer.ring(Some(span), &[0.5; 4_800]);
        writer.dsp(Some(span), &[0.25; 4_800]);
        let measured = readers.readings();
        assert_eq!(measured.graph_generation, Some(19));
        let ring = measured.ring_output.measurement.unwrap();
        let dsp = measured.dsp_output.measurement.unwrap();
        assert_eq!((ring.source_generation, ring.end_sample), (4, 100_800));
        assert_eq!((dsp.source_generation, dsp.end_sample), (4, 100_800));
        assert_eq!((ring.monotonic_ms, dsp.monotonic_ms), (2_100, 2_100));
        assert!((ring.peak_dbfs.unwrap() + 6.0206).abs() < 0.001);
        assert!((dsp.peak_dbfs.unwrap() + 12.0412).abs() < 0.001);

        writer.ring(None, &[]);
        writer.dsp_failed();
        let ended = readers.readings();
        assert_eq!(
            ended.ring_output.measurement.unwrap().quality,
            Quality::Unavailable
        );
        assert_eq!(
            ended.dsp_output.measurement.unwrap().quality,
            Quality::Fault
        );
        assert!(
            !NativeIngressWriter::new(false)
                .1
                .readings()
                .dsp_output
                .available
        );
    }
}
