//! Typed create and update requests retain the callback/provider environment of the original native station.
use super::{PreparationError, PreparedStation, Product, Result, abi, owned, succeeded};
use rpt_advanced_core::config::resources::candidate::ResourceId;
use std::{
    any::Any,
    ffi::c_void,
    mem::size_of,
    ptr::{self, NonNull},
    sync::Arc,
};

/// Native descriptors and callbacks plus all owners to which their pointers refer.
pub struct NativeBindings {
    manifest: abi::UrpAstProviderManifest,
    callbacks: abi::UrpAstDirectCallbacks,
    _keep_alive: Arc<dyn Any + Send + Sync>,
}
impl NativeBindings {
    /// Retain a complete native callback/provider environment during the station lifetime.
    ///
    /// # Safety
    /// The guard must own every DSO, object and callback context reachable from both C records.
    /// Those pointers must remain valid on the threads and under the call ordering required by product1.
    pub unsafe fn from_owned<T: Any + Send + Sync>(
        guard: Arc<T>,
        manifest: abi::UrpAstProviderManifest,
        callbacks: abi::UrpAstDirectCallbacks,
    ) -> Self {
        Self {
            manifest,
            callbacks,
            _keep_alive: guard,
        }
    }
}
fn args(generation: u64, frames: u32) -> Result<abi::UrpNativeCreateArgs> {
    if generation == 0 || !(1..=4096).contains(&frames) {
        return Err(PreparationError::Limit);
    }
    Ok(abi::UrpNativeCreateArgs {
        struct_size: size_of::<abi::UrpNativeCreateArgs>() as u32,
        abi_version: 1,
        config: ptr::null(),
        providers: ptr::null(),
        callbacks: ptr::null(),
        generation_id: generation,
        maximum_frames: frames,
    })
}
/// Retains a stopped native result or one borrowed by an active runtime admission owner.
pub struct NativeStation {
    product: Arc<Product>,
    handle: NonNull<c_void>,
    id: ResourceId,
    updating: bool,
    _bindings: Box<NativeBindings>,
}
impl PreparedStation {
    /// Create a stopped native object while retaining the exact providers and callback contexts.
    /// This hardware admission belongs to activation, after the side-effect-free prepare call.
    pub fn create_stopped(
        &self,
        bindings: NativeBindings,
        generation: u64,
        frames: u32,
    ) -> Result<NativeStation> {
        let bindings = Box::new(bindings);
        let mut args = args(generation, frames)?;
        args.providers = &bindings.manifest;
        args.callbacks = &bindings.callbacks;
        let mut raw = ptr::null_mut();
        // SAFETY: the prepared owner, null config, retained callback environment and writable output satisfy product1.
        let status = unsafe {
            (self.product.stations().create.expect("validated"))(
                self.resolved.raw().as_ptr(),
                &args,
                &mut raw,
            )
        };
        Ok(NativeStation {
            product: self.product.clone(),
            handle: owned(status, raw, None)?,
            id: self.port_id(),
            updating: false,
            _bindings: bindings,
        })
    }
}
/// Acknowledgment state returned by the serialized native update control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Both callback owners have acknowledged the operation.
    Complete,
    /// Keep the physical station and its reverse state; poll again on the control owner.
    Pending,
}
fn progress(status: i32) -> Result<UpdateStatus> {
    if status == abi::URP_AST_RELOAD_PENDING as i32 {
        Ok(UpdateStatus::Pending)
    } else {
        succeeded(status).map(|()| UpdateStatus::Complete)
    }
}
/// An unconsumed token borrowed from the native station it can update.
pub struct PreparedUpdate<'a> {
    station: &'a mut NativeStation,
    token: NonNull<c_void>,
}
impl NativeStation {
    /// Stable UUID of the retained station.
    pub fn port_id(&self) -> ResourceId {
        self.id
    }
    /// Borrow the physical handle for the runtime's existing start, stop, routing and quiesce owner.
    ///
    /// # Safety
    /// The caller must use the same loaded product1 descriptor, serialize control calls, keep
    /// this guard alive, and successfully quiesce after starting the station before dropping it.
    /// The borrowed handle must never be separately destroyed.
    pub unsafe fn borrowed_handle(&mut self) -> NonNull<c_void> {
        self.handle
    }
    /// Create the reversible provider update without replacing the live station or its callbacks.
    pub fn prepare_update(
        &mut self,
        candidate: &PreparedStation,
        generation: u64,
        frames: u32,
    ) -> Result<PreparedUpdate<'_>> {
        self.product.same(&candidate.product)?;
        if self.id != candidate.port_id() {
            return Err(PreparationError::DifferentPort);
        }
        if self.updating {
            return Err(PreparationError::UpdateInProgress);
        }
        let args = args(generation, frames)?;
        let mut raw = ptr::null_mut();
        // SAFETY: a native update reuses its retained providers and callbacks. Both owners belong to the same product.
        let status = unsafe {
            (self.product.stations().prepare_update.expect("validated"))(
                self.handle.as_ptr(),
                candidate.resolved.raw().as_ptr(),
                &args,
                &mut raw,
            )
        };
        Ok(PreparedUpdate {
            station: self,
            token: owned(status, raw, None)?,
        })
    }
    /// Poll the ongoing update. Pending/error leaves its rollback state with the live station.
    pub fn accept_update(&mut self) -> Result<UpdateStatus> {
        if !self.updating {
            return Err(PreparationError::NoUpdate);
        }
        // SAFETY: this mutable borrow serializes access to the live station.
        progress(unsafe {
            (self
                .product
                .ordinary()
                .native_accept_update
                .expect("validated"))(self.handle.as_ptr())
        })
    }
    /// Commit the accepted generation or restore the retained generation. Retry while pending.
    pub fn finish_update(&mut self, commit: bool) -> Result<UpdateStatus> {
        if !self.updating {
            return Err(PreparationError::NoUpdate);
        }
        // SAFETY: the retained owner holds both generations until product1 acknowledges completion.
        let result = progress(unsafe {
            (self
                .product
                .ordinary()
                .native_finish_update
                .expect("validated"))(self.handle.as_ptr(), u32::from(commit))
        })?;
        if result == UpdateStatus::Complete {
            self.updating = false
        }
        Ok(result)
    }
}
impl PreparedUpdate<'_> {
    /// Transfer the token's pending generation to its original station.
    pub fn begin(self) -> Result<()> {
        // SAFETY: the exclusive native borrow pins the intended owner throughout this transfer.
        succeeded(unsafe {
            (self
                .station
                .product
                .ordinary()
                .native_begin_update
                .expect("validated"))(self.station.handle.as_ptr(), self.token.as_ptr())
        })?;
        self.station.updating = true;
        Ok(())
    }
}
impl Drop for PreparedUpdate<'_> {
    fn drop(&mut self) {
        // SAFETY: product1 destroys both unconsumed tokens and tokens whose contents have transferred.
        unsafe {
            (self
                .station
                .product
                .ordinary()
                .native_destroy_update
                .expect("validated"))(self.token.as_ptr())
        }
    }
}
impl Drop for NativeStation {
    fn drop(&mut self) {
        // SAFETY: the object was created stopped. Any external activation obeys borrowed_handle's
        // quiesce contract; the environment and product are retained until destruction returns.
        unsafe {
            (self.product.ordinary().native_destroy.expect("validated"))(self.handle.as_ptr())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Stub {
        pending_accept: bool,
        finish_calls: u32,
        reverted: bool,
        token_destroyed: bool,
        native_destroyed: bool,
    }
    unsafe extern "C" fn begin(handle: *mut c_void, _: *mut c_void) -> i32 {
        // SAFETY: this fixture supplies its live Stub as the only native handle.
        unsafe { (*handle.cast::<Stub>()).pending_accept = true };
        0
    }
    unsafe extern "C" fn accept(handle: *mut c_void) -> i32 {
        // SAFETY: serial native calls borrow the same fixture.
        let state = unsafe { &mut *handle.cast::<Stub>() };
        if std::mem::take(&mut state.pending_accept) {
            1
        } else {
            0
        }
    }
    unsafe extern "C" fn finish(handle: *mut c_void, commit: u32) -> i32 {
        // SAFETY: serial native calls borrow the same fixture.
        let state = unsafe { &mut *handle.cast::<Stub>() };
        state.finish_calls += 1;
        match state.finish_calls {
            1 => -6,
            2 => 1,
            _ => {
                state.reverted = commit == 0;
                0
            }
        }
    }
    unsafe extern "C" fn token_destroy(handle: *mut c_void) {
        // SAFETY: the fixture uses the same live Stub as its unconsumed/consumed token.
        unsafe { (*handle.cast::<Stub>()).token_destroyed = true }
    }
    unsafe extern "C" fn native_destroy(handle: *mut c_void) {
        // SAFETY: the fixture outlives every native borrow.
        unsafe { (*handle.cast::<Stub>()).native_destroyed = true }
    }
    #[test]
    fn pending_and_failed_rollback_retain_native_owner_until_acknowledged() {
        // SAFETY: the generated descriptor has nullable function slots, integer fields and raw pointers.
        let mut table: abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
        table.native_begin_update = Some(begin);
        table.native_accept_update = Some(accept);
        table.native_finish_update = Some(finish);
        table.native_destroy = Some(native_destroy);
        table.native_destroy_update = Some(token_destroy);
        let product = Arc::new(Product {
            product: NonNull::from(&mut table),
            // This control-only test never reads station or independent-role tables.
            station: NonNull::dangling(),
            link: NonNull::dangling(),
            _library: libloading::os::unix::Library::this().into(),
        });
        let mut stub = Stub::default();
        let ptr = NonNull::from(&mut stub).cast();
        let guard = Arc::new(());
        // SAFETY: no foreign callback or provider is present in these zero-filled test records.
        let bindings = unsafe {
            NativeBindings::from_owned(guard.clone(), std::mem::zeroed(), std::mem::zeroed())
        };
        let mut station = NativeStation {
            product,
            handle: ptr,
            id: "00000000-0000-4000-8000-000000000007".parse().unwrap(),
            updating: false,
            _bindings: Box::new(bindings),
        };
        PreparedUpdate {
            station: &mut station,
            token: ptr,
        }
        .begin()
        .unwrap();
        assert!(stub.token_destroyed);
        assert!(station.updating);
        assert_eq!(station.accept_update().unwrap(), UpdateStatus::Pending);
        assert_eq!(station.accept_update().unwrap(), UpdateStatus::Complete);
        assert!(station.finish_update(false).is_err());
        assert!(station.updating);
        assert_eq!(station.finish_update(false).unwrap(), UpdateStatus::Pending);
        assert!(!stub.native_destroyed);
        assert_eq!(Arc::strong_count(&guard), 2);
        assert_eq!(
            station.finish_update(false).unwrap(),
            UpdateStatus::Complete
        );
        assert!(stub.reverted);
        assert!(!station.updating);
        drop(station);
        assert!(stub.native_destroyed);
        assert_eq!(Arc::strong_count(&guard), 1);
    }
}
