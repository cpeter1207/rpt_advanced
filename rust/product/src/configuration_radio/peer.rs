//! Distinct hardware-free roles and negotiated-rate peer graphs.
use super::{
    PreparationError, Product, RadioPreparationApi, Result, abi, diagnostic, equivalent, named,
    owned,
    peer_observation::{ProviderOutput, ProviderOutputMeasurement, ProviderOutputState},
    succeeded,
};
use libloading::Library;
use rpt_advanced_core::config::resources::profiles::radio::ProcessingProfile;
use std::{
    ffi::c_void,
    ptr::{self, NonNull},
    sync::Arc,
};

/// One prepared independent link role. The product owns its settings.
pub struct PreparedRole {
    product: Arc<Product>,
    role: NonNull<abi::UrpPreparedLinkRole>,
}
impl RadioPreparationApi {
    /// Prepare an absent neutral role or one resolved explicit processing profile.
    pub fn prepare_link(&self, profile: Option<&ProcessingProfile>) -> Result<PreparedRole> {
        let body = match profile {
            Some(profile) => serde_json::json!({"link":profile}),
            None => serde_json::json!({}),
        };
        let bytes = serde_json::to_vec(&body).map_err(|_| PreparationError::Limit)?;
        if bytes.len() > 1_048_576 {
            return Err(PreparationError::Limit);
        }
        let mut out = ptr::null_mut();
        let mut field = None;
        // SAFETY: the checked operation synchronously copies the bounded bytes and lends diagnostic tokens.
        let status = unsafe {
            (self.product.roles().prepare.expect("validated"))(
                bytes.as_ptr(),
                bytes.len() as u32,
                Some(diagnostic),
                ptr::from_mut(&mut field).cast(),
                &mut out,
            )
        };
        Ok(PreparedRole {
            product: self.product.clone(),
            role: owned(status, out, field)?,
        })
    }
}
impl PreparedRole {
    /// Compare effective DSP settings and presence through the same provider instance.
    pub fn equivalent(&self, other: &Self) -> Result<bool> {
        self.product.same(&other.product)?;
        let mut flag = 2;
        // SAFETY: both immutable roles belong to this same retained provider.
        let status = unsafe {
            (self.product.roles().equal.expect("validated"))(
                self.role.as_ptr(),
                other.role.as_ptr(),
                &mut flag,
            )
        };
        equivalent(status, flag)
    }
    /// Create an independently metered graph at the actual negotiated peer dimensions.
    pub fn prepare_peer(&self, rate: u32, frames: u32) -> Result<PreparedPeer> {
        PreparedPeer::create(self.product.clone(), rate, |descriptor, out| {
            // SAFETY: both provider libraries, the prepared role, and the output slot are live.
            unsafe {
                (self.product.roles().link_prepare.expect("validated"))(
                    self.role.as_ptr(),
                    descriptor,
                    rate,
                    frames,
                    out,
                )
            }
        })
    }
}
impl Drop for PreparedRole {
    fn drop(&mut self) {
        // SAFETY: this owner releases its unique role before dropping the product DSO.
        unsafe { (self.product.roles().destroy.expect("validated"))(self.role.as_ptr()) }
    }
}
/// One independently owned graph. Both product and FFmpeg code outlive its destruction.
pub struct PreparedPeer {
    product: Arc<Product>,
    peer: NonNull<c_void>,
    rate: u32,
    _ffmpeg: Library,
}
// SAFETY: a peer transfers to one processing owner, with mutable borrowing serializing calls.
// Its FFmpeg and product libraries remain loaded until synchronous destruction.
unsafe impl Send for PreparedPeer {}
impl PreparedPeer {
    pub(super) fn create(
        product: Arc<Product>,
        rate: u32,
        prepare: impl FnOnce(*const c_void, *mut *mut c_void) -> i32,
    ) -> Result<Self> {
        // SAFETY: fixed release SONAME and published descriptor getter.
        let ffmpeg = unsafe { Library::new("librptadv_ffmpeg_adapter.so.1") }
            .map_err(|_| PreparationError::Unavailable)?;
        let table = unsafe {
            named::<abi::rptadv_ffmpeg_adapter_descriptor>(
                &ffmpeg,
                b"rptadv_ffmpeg_adapter_descriptor\0",
                b"rptadv.ffmpeg",
            )?
        };
        let mut out = ptr::null_mut();
        let status = prepare(table.as_ptr().cast(), &mut out);
        Ok(Self {
            product,
            peer: owned(status, out, None)?,
            rate,
            _ffmpeg: ffmpeg,
        })
    }
    /// Acquire the released observer before moving the graph to its PCM owner.
    pub(crate) fn output_observer(&self) -> PeerOutputObserver {
        let observer = if self.rate == crate::status_meter::NATIVE_RATE {
            self.link_observer()
        } else {
            None
        };
        PeerOutputObserver {
            observer,
            rate: self.rate,
        }
    }

    fn link_observer(&self) -> Option<NativeLinkObserver> {
        let api = self.product.ordinary();
        let create = api.link_observer_create?;
        let read = api.link_observer_read?;
        let destroy = api.link_observer_destroy?;
        let mut observer = ptr::null_mut();
        // SAFETY: preparation exclusively holds this quiescent graph before PCM publication.
        if unsafe { create(self.peer.as_ptr(), &mut observer) } != 0 {
            return None;
        }
        Some(NativeLinkObserver {
            observer: NonNull::new(observer)?,
            read,
            destroy,
            _product: self.product.clone(),
        })
    }

    /// Prefer the float callback when the admitted provider exposes it.
    /// A missing callback allows the old signed-PCM path, but a failed call does not.
    pub(crate) fn process_f32(&mut self, samples: &mut [f32]) -> Option<Result<()>> {
        let process = self.product.ordinary().link_process_f32?;
        let Ok(count) = u32::try_from(samples.len()) else {
            return Some(Err(PreparationError::Limit));
        };
        // SAFETY: this exclusive graph owner retains the provider and writable PCM
        // span throughout the call.
        Some(succeeded(unsafe {
            process(
                self.peer.as_ptr(),
                abi::URP_AST_LINK_DIRECTION_READ,
                self.rate,
                samples.as_mut_ptr(),
                count,
            )
        }))
    }

    /// Process one incoming mono PCM block. Mutable access serializes graph calls.
    pub fn process(&mut self, samples: &mut [i16]) -> Result<()> {
        let count = u32::try_from(samples.len()).map_err(|_| PreparationError::Limit)?;
        // SAFETY: the exclusive graph owner and the entire PCM block remain live for the call.
        succeeded(unsafe {
            (self.product.ordinary().link_process.expect("validated"))(
                self.peer.as_ptr(),
                abi::URP_AST_LINK_DIRECTION_READ,
                self.rate,
                samples.as_mut_ptr(),
                count,
            )
        })
    }
}
impl Drop for PreparedPeer {
    fn drop(&mut self) {
        // SAFETY: destruction runs before either retained library is released.
        unsafe { (self.product.ordinary().link_destroy.expect("validated"))(self.peer.as_ptr()) }
    }
}

type NativeRead = unsafe extern "C" fn(*const c_void, *mut abi::UrpAstLinkObservation) -> i32;
struct NativeLinkObserver {
    observer: NonNull<c_void>,
    read: NativeRead,
    destroy: unsafe extern "C" fn(*mut c_void),
    _product: Arc<Product>,
}
// SAFETY: the SDK gives this independent lease immutable reads concurrent with processing,
// graph destruction and other reads. The owned lease and DSO outlive each read.
unsafe impl Send for NativeLinkObserver {}
unsafe impl Sync for NativeLinkObserver {}

impl Drop for NativeLinkObserver {
    fn drop(&mut self) {
        // SAFETY: releasing the sole SDK observer lease cannot race a surviving borrow.
        unsafe { (self.destroy)(self.observer.as_ptr()) };
    }
}

pub(crate) struct PeerOutputObserver {
    observer: Option<NativeLinkObserver>,
    rate: u32,
}

impl PeerOutputObserver {
    pub(crate) fn output(&self) -> ProviderOutput {
        let state = if self.rate != crate::status_meter::NATIVE_RATE {
            // The graph was prepared at decoder rate, but its input here is native 48 kHz.
            // Report the actual discrepancy, not a loudness value from the wrong weighting.
            ProviderOutputState::RateMismatch
        } else if let Some(observer) = &self.observer {
            // SAFETY: the SDK record is flat and caller-initialized with scalar fields.
            let mut wire: abi::UrpAstLinkObservation = unsafe { std::mem::zeroed() };
            wire.struct_size = std::mem::size_of_val(&wire) as u32;
            wire.abi_version = 1;
            // SAFETY: the independent lease outlives this synchronous, read-only call.
            if unsafe { (observer.read)(observer.observer.as_ptr(), &mut wire) } == 0 {
                ProviderOutputMeasurement::from_wire(&wire).map_or(
                    ProviderOutputState::ReadFailed,
                    ProviderOutputState::Observed,
                )
            } else {
                ProviderOutputState::ReadFailed
            }
        } else {
            ProviderOutputState::ObserverUnavailable
        };
        ProviderOutput {
            rate: self.rate,
            state,
        }
    }
}
