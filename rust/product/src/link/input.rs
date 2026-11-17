//! Unique product mixer input; each variant retains its existing ring and graph owner.
use super::{private_phone::media::PhoneMediaInput, ring::InboundConsumer};
use crate::services::ProcessedPeer;
use rpt_advanced_core::link::{
    PeerInput, PeerReply, PeerSignals, ReceivePolicy, RenderedPeerBlock, TimedPeerSample,
};

/// Direct owned variants avoid another allocation or per-sample virtual dispatch.
/// Private identity, source timing and retirement remain with the original input.
pub enum AudioInput {
    /// An ordinary protocol peer or existing regeneration source.
    Peer(ProcessedPeer<InboundConsumer>),
    /// One exact call's independently revocable prepared media generation.
    Phone(PhoneMediaInput),
}

impl PeerInput for AudioInput {
    fn phone_reply(&self) -> Option<&PeerReply> {
        match self {
            Self::Peer(input) => input.phone_reply(),
            Self::Phone(input) => input.phone_reply(),
        }
    }
    fn source_rate(&self) -> u32 {
        match self {
            Self::Peer(input) => input.source_rate(),
            Self::Phone(input) => input.source_rate(),
        }
    }
    fn receive_policy(&self) -> ReceivePolicy {
        match self {
            Self::Peer(input) => input.receive_policy(),
            Self::Phone(input) => input.receive_policy(),
        }
    }
    fn source_timing_available(&self) -> bool {
        match self {
            Self::Peer(input) => input.source_timing_available(),
            Self::Phone(input) => input.source_timing_available(),
        }
    }
    fn render_timed_sample(&mut self) -> Option<TimedPeerSample> {
        match self {
            Self::Peer(input) => input.render_timed_sample(),
            Self::Phone(input) => input.render_timed_sample(),
        }
    }
    fn render_timed_block(&mut self, output: &mut [f32]) -> Option<RenderedPeerBlock> {
        match self {
            Self::Peer(input) => input.render_timed_block(output),
            Self::Phone(input) => input.render_timed_block(output),
        }
    }
    fn signals(&self) -> &PeerSignals {
        match self {
            Self::Peer(input) => input.signals(),
            Self::Phone(input) => input.signals(),
        }
    }
    fn available(&self) -> u64 {
        match self {
            Self::Peer(input) => input.available(),
            Self::Phone(input) => input.available(),
        }
    }
    fn finite_playout(&self) -> Option<bool> {
        match self {
            Self::Peer(input) => input.finite_playout(),
            Self::Phone(input) => input.finite_playout(),
        }
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        match self {
            Self::Peer(input) => input.render(output),
            Self::Phone(input) => input.render(output),
        }
    }
    fn silence(&mut self, output: &mut [f32]) {
        match self {
            Self::Peer(input) => input.silence(output),
            Self::Phone(input) => input.silence(output),
        }
    }
    fn render_sample(&mut self) -> Option<f32> {
        match self {
            Self::Peer(input) => input.render_sample(),
            Self::Phone(input) => input.render_sample(),
        }
    }
}
