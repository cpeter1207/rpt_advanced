//! One independently retained code lease for all private-call packet jitter contexts.
use libloading::Library;
use rptadv_jitter_client::Jitter;
use std::{ffi::c_void, path::Path, ptr::NonNull, sync::Arc};

/// The immutable descriptor belongs to the library until every native context has retired.
pub struct JitterProvider {
    descriptor: NonNull<c_void>,
    _library: Library,
    pub(crate) live_update: bool,
}

// The released descriptor is immutable, and each created jitter context has one serial caller.
unsafe impl Send for JitterProvider {}
unsafe impl Sync for JitterProvider {}

impl JitterProvider {
    /// Load the product's configured, trusted jitter adapter.
    ///
    /// # Safety
    /// Loading a dynamic library executes code from the supplied path.
    pub unsafe fn load(path: &Path) -> Result<Arc<Self>, crate::Error> {
        let library = unsafe { Library::new(path) }.map_err(|_| crate::Error::Provider)?;
        let get = unsafe {
            library.get::<unsafe extern "C" fn() -> *const c_void>(
                b"rptadv_jitter_adapter_descriptor_v1\0",
            )
        }
        .map_err(|_| crate::Error::Provider)?;
        let descriptor = NonNull::new(unsafe { get() }.cast_mut()).ok_or(crate::Error::Provider)?;
        let probe =
            unsafe { Jitter::phone(descriptor.as_ptr()) }.map_err(|_| crate::Error::Provider)?;
        let live_update = probe.supports_delay_update();
        drop(probe);
        Ok(Arc::new(Self {
            descriptor,
            _library: library,
            live_update,
        }))
    }

    pub(crate) fn descriptor(&self) -> *const c_void {
        self.descriptor.as_ptr()
    }
    pub(crate) fn create(
        &self,
        rate: u32,
        frames: u32,
        bytes: usize,
        delays: [u32; 3],
    ) -> Result<Jitter, rptadv_jitter_client::Error> {
        unsafe { Jitter::with_delays(self.descriptor.as_ptr(), rate, frames, bytes, delays) }
    }
}
