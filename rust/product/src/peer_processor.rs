//! One receive-path DSP graph with a preallocated buffer for signed-PCM-only providers.

use rpt_advanced_core::audio::{PeakGuard, signed_pcm16};

pub(super) struct Processor {
    pub(super) graph: crate::configuration_radio::PreparedPeer,
    pub(super) samples: Box<[i16]>,
    pub(super) signed_input: PeakGuard,
    pub(super) float_output: PeakGuard,
}

impl Processor {
    pub(super) fn process(&mut self, pcm: &mut [f32]) -> bool {
        let Some(block) = self.samples.get_mut(..pcm.len()) else {
            return false;
        };
        if let Some(result) = self.graph.process_f32(pcm) {
            if result.is_err() {
                return false;
            }
            self.float_output.apply(pcm);
            return true;
        }
        self.signed_input.apply(pcm);
        for (target, source) in block.iter_mut().zip(pcm.iter()) {
            *target = signed_pcm16(*source);
        }
        if self.graph.process(block).is_err() {
            return false;
        }
        for (target, sample) in pcm.iter_mut().zip(block.iter()) {
            *target = f32::from(*sample) / 32768.0;
        }
        true
    }
}
