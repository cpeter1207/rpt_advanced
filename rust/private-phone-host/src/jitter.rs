//! Keep existing private-call composition and error semantics at the shared loader.
use rptadv_jitter_client::{Jitter, JitterProvider as SharedProvider};
use std::{ffi::c_void, path::Path, sync::Arc};

/// Checked jitter code retained by the private-call owner.
pub struct JitterProvider {
    provider: Arc<SharedProvider>,
    pub(crate) live_update: bool,
}

impl JitterProvider {
    /// Load the product's configured, trusted jitter adapter.
    ///
    /// # Safety
    /// Loading a dynamic library executes code from the supplied path. The library
    /// must implement the jitter ABI.
    pub unsafe fn load(path: &Path) -> Result<Arc<Self>, crate::Error> {
        let provider = unsafe { SharedProvider::load(path) }.map_err(|_| crate::Error::Provider)?;
        Ok(Arc::new(Self {
            live_update: provider.supports_delay_update(),
            provider,
        }))
    }

    pub(crate) fn descriptor(&self) -> *const c_void {
        self.provider.descriptor()
    }

    pub(crate) fn create(
        &self,
        rate: u32,
        frames: u32,
        bytes: usize,
        delays: [u32; 3],
    ) -> Result<Jitter, rptadv_jitter_client::Error> {
        self.provider.create(rate, frames, bytes, delays)
    }
}
