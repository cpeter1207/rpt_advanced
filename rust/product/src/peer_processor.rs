//! One receive-path DSP graph with a preallocated buffer for signed-PCM-only providers.

pub(super) struct Processor {
    pub(super) graph: crate::configuration_radio::PreparedPeer,
    pub(super) samples: Box<[i16]>,
}

impl Processor {
    pub(super) fn process(&mut self, pcm: &mut [f32]) -> bool {
        let Some(block) = self.samples.get_mut(..pcm.len()) else {
            return false;
        };
        if let Some(result) = self.graph.process_f32(pcm) {
            return result.is_ok();
        }
        for (target, source) in block.iter_mut().zip(pcm.iter()) {
            *target = (*source * 32768.0).round() as i16;
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
