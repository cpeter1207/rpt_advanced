//! Shared non-audio host ownership. Accounts retain their context and provider DSO.
use super::{Api, abi};
use libloading::Library;
use std::{path::Path, ptr::NonNull, sync::Arc};

/// Safe, non-secret provider setup failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Shared object or its descriptor could not be loaded.
    Load,
    /// Descriptor, operation group, or returned handle is incomplete.
    Contract,
    /// Provider rejected the operation.
    Status(abi::Status),
}

fn completed(status: abi::Status) -> Result<(), Error> {
    if status == abi::Status::Ok {
        Ok(())
    } else {
        Err(Error::Status(status))
    }
}

/// One dynamically loaded provider and worker context shared by both host adapters.
pub struct Provider {
    api: Api,
    context: NonNull<abi::Context>,
    // Drop only after context_destroy has returned.
    _library: Library,
}
// The ABI context is thread-safe; stateful QSO work has its own serial owner.
unsafe impl Send for Provider {}
unsafe impl Sync for Provider {}

impl Provider {
    /// Load and validate all advertised operations before creating any context.
    /// This does not bind sockets, log in, or open radio hardware.
    ///
    /// # Safety
    /// The path names a trusted native provider. Configuration spans remain
    /// readable throughout context creation; the provider copies retained data.
    pub unsafe fn load(path: &Path, config: &abi::RuntimeConfig) -> Result<Arc<Self>, Error> {
        let library = unsafe { Library::new(path) }.map_err(|_| Error::Load)?;
        let descriptor = unsafe {
            library.get::<unsafe extern "C" fn() -> *const abi::Descriptor>(
                b"rptadv_echolink_descriptor_v1\0",
            )
        }
        .map_err(|_| Error::Load)?;
        let api = unsafe { Api::open(descriptor()) }.map_err(|_| Error::Contract)?;
        let mut context = std::ptr::null_mut();
        completed(unsafe { api.table().context_create.unwrap()(config, &mut context) })?;
        Ok(Arc::new(Self {
            api,
            context: NonNull::new(context).ok_or(Error::Contract)?,
            _library: library,
        }))
    }

    /// Copy a resolved account without activating network I/O. Enabled accounts
    /// require the validated network and source-media groups before preparation.
    ///
    /// # Safety
    /// All foreign spans/lists in both records remain readable throughout this
    /// synchronous call. Preparation copies them before returning.
    pub unsafe fn prepare(
        self: &Arc<Self>,
        config: &abi::AccountConfig,
        secrets: &abi::AccountSecrets,
    ) -> Result<Account, Error> {
        if config.enabled != 0 && !self.api.prepared_media_available() {
            return Err(Error::Status(abi::Status::Unsupported));
        }
        let mut handle = std::ptr::null_mut();
        completed(unsafe {
            self.api.table().account_prepare.unwrap()(
                self.context.as_ptr(),
                config,
                secrets,
                &mut handle,
            )
        })?;
        Ok(Account {
            handle: NonNull::new(handle).ok_or(Error::Contract)?,
            provider: Arc::clone(self),
        })
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        unsafe { self.api.table().context_destroy.unwrap()(self.context.as_ptr()) };
    }
}

/// An off-audio account owner; drop only after its host callbacks are quiescent.
pub struct Account {
    handle: NonNull<abi::Account>,
    provider: Arc<Provider>,
}
// Account operations are serialized internally and never called by audio callbacks.
unsafe impl Send for Account {}
unsafe impl Sync for Account {}
impl Account {
    /// Gate incoming work and join account notifications before releasing host state.
    /// Repeated stop is harmless. Never call from a provider executor or audio callback.
    pub fn stop(&self) -> Result<(), Error> {
        completed(unsafe { self.provider.api.table().account_stop.unwrap()(self.handle.as_ptr()) })
    }
}
impl Drop for Account {
    fn drop(&mut self) {
        if self.stop().is_ok() {
            unsafe { self.provider.api.table().account_destroy.unwrap()(self.handle.as_ptr()) };
        } else {
            // A failed join may still execute provider code. Retain its context/DSO
            // along with the foreign account rather than unload beneath that work.
            std::mem::forget(Arc::clone(&self.provider));
        }
    }
}
