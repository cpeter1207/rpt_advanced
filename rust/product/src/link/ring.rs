//! Checked binding to the released F32 ring; no conversion implementation lives here.
use crate::abi as ffi;
use rpt_advanced_core::link::{PeerInput, PeerSignals};
use std::{
    ffi::CStr,
    mem::size_of,
    ptr::{self, NonNull},
    sync::Arc,
};

/// Invalid or unavailable ring capability, construction, or operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingError;

/// Individually current released-ring diagnostics; fields are not transactional.
pub type Observation = ffi::rpcr2_observation;

struct Shared {
    handle: NonNull<ffi::rpcr2_ring>,
    api: &'static ffi::rpcr2_descriptor,
    input_rate: u32,
    signals: PeerSignals,
}
// SAFETY: exactly one non-cloneable producer and one consumer call the released SPSC API.
// Descriptor storage is process-lifetime immutable; Arc keeps the ring alive for both owners.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}
impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: both endpoints have gone; there can be no outstanding call.
        unsafe {
            (self.api.ring_destroy.unwrap())(self.handle.as_ptr());
        }
    }
}
/// Ring constructor assigning exactly one owner to each endpoint.
pub struct InboundRing;
/// Unique network producer; its owner serializes every publish operation.
pub struct InboundProducer(Arc<Shared>);
/// Unique native 48 kHz consumer, independent of producer progress.
pub struct InboundConsumer(Arc<Shared>);
/// Read-only diagnostics handle; it cannot consume or publish PCM.
#[derive(Clone)]
pub struct InboundObserver(Arc<Shared>);

impl InboundObserver {
    /// Whether both observations belong to the same prepared ring allocation.
    /// Control owners use this identity to acknowledge only the intended redirection.
    pub fn same_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Immutable decoded input sample rate.
    pub fn rate(&self) -> u32 {
        self.0.input_rate
    }
    /// Activity/source/terminal atomics belonging to this exact ring generation.
    pub fn signals(&self) -> &PeerSignals {
        &self.0.signals
    }
    /// Read the released ring's individually current counters without blocking audio.
    pub fn observe(&self) -> Result<Observation, RingError> {
        self.0.observe()
    }
}
impl InboundRing {
    /// Create one released ring per peer at its negotiated decoded input rate.
    pub fn open(input_rate: u32) -> Result<(InboundProducer, InboundConsumer), RingError> {
        // SAFETY: the linked provider retains its immutable descriptor for process lifetime.
        unsafe { Self::from_descriptor(input_rate, ffi::rpcr2_descriptor()) }
    }
    // The provider retains the descriptor and callback code until all endpoints are dropped.
    unsafe fn from_descriptor(
        input_rate: u32,
        pointer: *const ffi::rpcr2_descriptor,
    ) -> Result<(InboundProducer, InboundConsumer), RingError> {
        if input_rate == 0 || input_rate > 48000 {
            return Err(RingError);
        }
        // SAFETY: linked released library guarantees a process-lifetime descriptor header.
        unsafe {
            if pointer.is_null() {
                return Err(RingError);
            }
            // Inspect the header before borrowing the complete current function table.
            if ptr::addr_of!((*pointer).struct_size).read()
                < size_of::<ffi::rpcr2_descriptor>() as u32
                || ptr::addr_of!((*pointer).abi_version).read() != 2
            {
                return Err(RingError);
            }
            let api = &*pointer;
            if api.capability_name.is_null()
                || CStr::from_ptr(api.capability_name) != c"rptadv.rate-adjusting-pcm-ring.f32"
                || api.ring_create.is_none()
                || api.ring_destroy.is_none()
                || api.ring_producer_push.is_none()
                || api.ring_consumer_render.is_none()
                || api.ring_consumer_reset.is_none()
                || api.ring_observe.is_none()
            {
                return Err(RingError);
            }
            let config = ffi::rpcr2_config {
                struct_size: size_of::<ffi::rpcr2_config>() as u32,
                abi_version: 2,
                capacity_samples: (u64::from(input_rate) * 300 / 1000).max(512),
                input_rate_hz: input_rate,
                output_rate_hz: 48000,
                quality: 2,
            };
            let mut handle = ptr::null_mut();
            if api.ring_create.unwrap()(&config, &mut handle) != 0 {
                return Err(RingError);
            }
            let shared = Arc::new(Shared {
                handle: NonNull::new(handle).ok_or(RingError)?,
                api,
                input_rate,
                signals: PeerSignals::new(),
            });
            Ok((InboundProducer(shared.clone()), InboundConsumer(shared)))
        }
    }
}
impl InboundProducer {
    /// Obtain a diagnostics-only control handle outside audio work.
    pub fn observer(&self) -> InboundObserver {
        InboundObserver(self.0.clone())
    }
    /// Decoded source rate established when this endpoint was prepared.
    pub fn rate(&self) -> u32 {
        self.0.input_rate
    }
    /// Shared publication and activity state for the serial channel owner.
    pub fn signals(&self) -> &PeerSignals {
        &self.0.signals
    }
    /// Publish a decoded F32 block once; return the accepted leading sample count.
    pub fn write(&mut self, samples: &[f32]) -> Result<usize, RingError> {
        let mut accepted = 0;
        // SAFETY: this noncloneable endpoint is the only producer; the slice is live.
        let code = unsafe {
            (self.0.api.ring_producer_push.unwrap())(
                self.0.handle.as_ptr(),
                samples.as_ptr(),
                samples.len() as u64,
                &mut accepted,
            )
        };
        if code == 0 {
            if accepted != 0 {
                self.0.signals.publish_pcm();
            }
            Ok(accepted as usize)
        } else {
            Err(RingError)
        }
    }
}
impl Drop for InboundProducer {
    fn drop(&mut self) {
        self.0.signals.end();
    }
}
impl PeerInput for InboundConsumer {
    fn source_rate(&self) -> u32 {
        self.0.input_rate
    }
    fn signals(&self) -> &PeerSignals {
        &self.0.signals
    }
    fn available(&self) -> u64 {
        self.observe()
            .map_or(0, |observation| observation.available_samples)
    }
    fn render(&mut self, output: &mut [f32]) -> bool {
        InboundConsumer::render(self, output).is_ok()
    }
}
impl InboundConsumer {
    /// Render the requested native block; the library counts concealment exactly once.
    pub fn render(&mut self, samples: &mut [f32]) -> Result<usize, RingError> {
        self.render_with_timing(samples, 60, 260)
    }
    /// Render with a caller-owned native delay policy. Local RF uses this same
    /// released ring as its squelch-delay line rather than a second PCM queue.
    pub fn render_with_timing(
        &mut self,
        samples: &mut [f32],
        reserve_ms: u64,
        target_ms: u64,
    ) -> Result<usize, RingError> {
        let mut real = 0;
        let rate = u64::from(self.0.input_rate);
        // SAFETY: the unique consumer owns conversion state and this output slice.
        let code = unsafe {
            (self.0.api.ring_consumer_render.unwrap())(
                self.0.handle.as_ptr(),
                samples.as_mut_ptr(),
                samples.len() as u64,
                rate.saturating_mul(reserve_ms) / 1000,
                rate.saturating_mul(target_ms.max(reserve_ms)) / 1000,
                &mut real,
            )
        };
        if code == 0 {
            Ok(real as usize)
        } else {
            samples.fill(0.0);
            Err(RingError)
        }
    }
    /// Copy current diagnostics without locking either endpoint.
    pub fn observe(&self) -> Result<Observation, RingError> {
        self.0.observe()
    }
    /// End a receive burst so the next one must refill its reserve.
    pub fn reset(&mut self) -> Result<(), RingError> {
        let code = unsafe { (self.0.api.ring_consumer_reset.unwrap())(self.0.handle.as_ptr()) };
        (code == 0).then_some(()).ok_or(RingError)
    }
}
impl Shared {
    fn observe(&self) -> Result<Observation, RingError> {
        // SAFETY: observation is plain integer ABI storage initialized with caller size.
        let mut observation: Observation = unsafe { std::mem::zeroed() };
        observation.struct_size = size_of::<Observation>() as u32;
        let code =
            unsafe { (self.api.ring_observe.unwrap())(self.handle.as_ptr(), &mut observation) };
        if code == 0 {
            Ok(observation)
        } else {
            Err(RingError)
        }
    }
}

#[cfg(test)]
#[path = "ring_tests.rs"]
mod tests;
