//! Checked code ownership for the shared jitter adapter.
use super::{Error, Jitter};
use libloading::Library;
use std::{ffi::c_void, path::Path, ptr::NonNull, sync::Arc};

/// Installed ABI-1 library name inside the selected composition directory.
pub const JITTER_LIBRARY_NAME: &str = "librptadv_jitter_adapter.so.1";

/// Immutable descriptor and its dynamic library, shared by exclusive jitter contexts.
pub struct JitterProvider {
    descriptor: NonNull<c_void>,
    _library: Library,
    live_update: bool,
}

// ABI 1 descriptors are immutable, and a separately created context has one serial caller.
unsafe impl Send for JitterProvider {}
unsafe impl Sync for JitterProvider {}

impl JitterProvider {
    /// Load the trusted composition's jitter adapter and verify its ABI-1 operations.
    ///
    /// # Safety
    /// The supplied path must name a trusted library that implements the jitter ABI.
    /// Loading the library and invoking its entry point execute native code.
    pub unsafe fn load(path: &Path) -> Result<Arc<Self>, Error> {
        let library = unsafe { Library::new(path) }.map_err(|_| Error::OperationFailed)?;
        let get = unsafe {
            library.get::<unsafe extern "C" fn() -> *const c_void>(
                b"rptadv_jitter_adapter_descriptor_v1\0",
            )
        }
        .map_err(|_| Error::InvalidDescriptor)?;
        let descriptor =
            NonNull::new(unsafe { get() }.cast_mut()).ok_or(Error::InvalidDescriptor)?;
        let probe = unsafe { Jitter::phone(descriptor.as_ptr()) }?;
        let live_update = probe.supports_delay_update();
        drop(probe);
        Ok(Arc::new(Self {
            descriptor,
            _library: library,
            live_update,
        }))
    }

    /// Give a native adapter the borrowed descriptor. Retain this owner while its
    /// native contexts may call any descriptor function.
    pub fn descriptor(&self) -> *const c_void {
        self.descriptor.as_ptr()
    }

    /// Whether the checked adapter can edit delay bounds on live contexts.
    pub fn supports_delay_update(&self) -> bool {
        self.live_update
    }

    /// Create one exclusive context and keep this library loaded through its destruction.
    pub fn create(
        self: &Arc<Self>,
        rate: u32,
        loss_unit: u32,
        maximum_payload_bytes: usize,
        delays_ms: [u32; 3],
    ) -> Result<Jitter, Error> {
        // The checked descriptor remains valid under self until the context retains its own lease.
        let mut jitter = unsafe {
            Jitter::with_delays(
                self.descriptor(),
                rate,
                loss_unit,
                maximum_payload_bytes,
                delays_ms,
            )
        }?;
        jitter._provider = Some(Arc::clone(self));
        Ok(jitter)
    }
}
