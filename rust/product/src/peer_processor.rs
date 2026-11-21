//! One native graph and its preallocated signed-PCM buffer, owned by the receive path.
use super::PcmProcessor;

pub(super) enum Processor {
    Host(PcmProcessor),
    #[cfg(target_os = "linux")]
    Prepared {
        graph: crate::configuration_radio::PreparedPeer,
        samples: Box<[i16]>,
    },
}

impl Processor {
    pub(super) fn process(&mut self, pcm: &mut [f32]) -> bool {
        match self {
            Self::Host(processor) => processor.process(pcm),
            #[cfg(target_os = "linux")]
            Self::Prepared { graph, samples } => {
                let Some(block) = samples.get_mut(..pcm.len()) else {
                    return false;
                };
                for (target, source) in block.iter_mut().zip(pcm.iter()) {
                    *target = (*source * 32768.0).round() as i16;
                }
                if graph.process(block).is_err() {
                    return false;
                }
                for (target, sample) in pcm.iter_mut().zip(block.iter()) {
                    *target = f32::from(*sample) / 32768.0;
                }
                true
            }
        }
    }
}
