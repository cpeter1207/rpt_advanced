//! Checked private IAX account handles. The provider owns authentication, sockets,
//! codec selection and packet clocks. Numeric radio dialing is a different admission path.
use crate::{abi, accounts::Route};
use std::{ffi::{c_void, CString}, mem::size_of, net::SocketAddr,
    path::Path, ptr::{self,NonNull}, sync::Arc, time::{Duration,Instant,SystemTime,UNIX_EPOCH}};
use records::{CallReceipt, DialRequest, ProviderAccount, ProviderCall, RegistrationReceipt, RegistrationRequest};
mod records;
mod timed;
mod listener;
mod worker;
pub(crate) use worker::Bank;
mod call;
pub(crate) use call::{Call, Registration};
pub(crate) use listener::Listener;
mod executor;
#[cfg(test)]
mod account_tests;
const ABI_VERSION:u32=2;
type ClientDescriptor=abi::rptadv_iax2_client_descriptor_v2;
type ServerDescriptor=abi::rptadv_iax2_server_descriptor_v2;
type ServerOptions=abi::rptadv_iax2_server_options_v2;
type ServerAuthorize=abi::rptadv_iax2_authorize_v1;
type ServerAccept=abi::rptadv_iax2_accept_v1;
#[repr(C)]
struct DescriptorHeader {
    struct_size: u32,
    abi_version: u32,
    capability: [u8; 16],
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum IaxError {
    /// The library could not be loaded.
    Load,
    /// The required descriptor symbol is missing.
    MissingSymbol,
    /// The descriptor does not match ABI 2.
    IncompatibleAdapter,
    /// The adapter rejected invalid dial parameters.
    InvalidParameters,
    /// The network adapter could not exchange datagrams.
    Network,
    /// The remote did not complete setup within the dial deadline.
    Timeout,
    /// The remote rejected the call.
    Rejected,
    /// The peer could not negotiate a supported codec.
    Codec,
    /// Call setup encountered an invalid protocol exchange.
    Protocol,
    /// The inbound UDP listener could not bind or returned no handle.
    Bind,
    /// A peer operation failed or returned malformed output.
    Operation,
}

#[derive(Clone)]
pub(crate) struct IaxClient(Arc<Owner>);
struct Owner {
    // The executor, catalog and verifier all call into the retained library.
    executor: executor::Executor,
    api: ClientDescriptor,
    server: ServerDescriptor,
    catalog: NonNull<c_void>,
    _library: libloading::Library,
}
unsafe impl Send for Owner {}
unsafe impl Sync for Owner {}
impl Drop for Owner {
    fn drop(&mut self) { unsafe { self.api.close_catalog.unwrap()(self.catalog.as_ptr()) }; }
}

impl IaxClient {
    pub(crate) fn supports_live_policy(&self)->bool { self.0.executor.supports_live_policy() }
    pub(crate) fn supports(&self,delays:[u32;3])->bool{self.0.executor.supports_private(delays)}
    pub(crate) fn open(path:&Path, codec:&Path)->Result<Self,IaxError>{
        let library=unsafe{libloading::Library::new(path)}.map_err(|_|IaxError::Load)?;
        unsafe fn table<T>(lib:&libloading::Library, name:&[u8])->Result<*const T,IaxError>{
            let entry=unsafe{lib.get::<unsafe extern "C" fn()->*const T>(name)}.map_err(|_|IaxError::MissingSymbol)?;
            Ok(unsafe{entry()})
        }
        let api=unsafe{descriptor_prefix::<ClientDescriptor>(table(&library,b"rptadv_iax2_client_descriptor_v2\0")?,
            std::mem::offset_of!(ClientDescriptor,retire_egress)+size_of::<*const c_void>(),*b"rptadv.iax2.v2\0\0")}?;
        let server=unsafe{descriptor_prefix::<ServerDescriptor>(table(&library,b"rptadv_iax2_server_descriptor_v2\0")?,
            std::mem::offset_of!(ServerDescriptor,dial_provider_account)+size_of::<*const c_void>(),*b"rptadv.iaxsrv2\0\0")}?;
        if api.open_catalog.is_none() || api.close_catalog.is_none() || api.destroy.is_none()
            || api.hangup.is_none() || api.send_digit.is_none() || api.sample_rate_hz.is_none()
            || api.prepare_audio.is_none() || api.elapsed_ms.is_none() || api.reset_encoder.is_none()
            || api.poll_timed.is_none() || api.send_prepared_audio_timed.is_none() || api.retire_egress.is_none()
            || server.bind.is_none() || server.destroy.is_none() || server.configure_accounts.is_none()
            || server.register_account.is_none() || server.poll_accounts.is_none() || server.dial_provider_account.is_none() {
            return Err(IaxError::IncompatibleAdapter);
        }
        let executor = unsafe {executor::Executor::new(table(&library,b"rptadv_iax2_async_descriptor_v1\0")?)}
            .map_err(|_|IaxError::IncompatibleAdapter)?;
        let path=CString::new(codec.as_os_str().as_encoded_bytes()).map_err(|_|IaxError::Codec)?;
        let selections=[1,2].map(|profile_id|abi::rptadv_iax2_codec_selection_v2{
            struct_size:size_of::<abi::rptadv_iax2_codec_selection_v2>() as u32,profile_id,provider_path:path.as_ptr(),
        });
        let mut raw=ptr::null_mut();
        if unsafe{api.open_catalog.unwrap()(selections.as_ptr(),selections.len(),&mut raw)}!=0{return Err(IaxError::Codec);}
        let catalog=NonNull::new(raw).ok_or(IaxError::Codec)?;
        Ok(Self(Arc::new(Owner{executor,api,server,catalog,_library:library})))
    }
    /// Adopt one peer handle returned by the matching inbound server descriptor.
    ///
    /// # Safety
    /// `handle` must be a unique live IaxPeer handle from the same library ABI and is transferred
    /// to this wrapper exactly once.
    pub unsafe fn adopt_inbound(self, handle: *mut c_void) -> Result<IaxPeer, IaxError> {
        let handle = NonNull::new(handle).ok_or(IaxError::Operation)?;
        Ok(IaxPeer {
            api: self.0.api,
            _owner: self,
            handle,
        })
    }
}

pub struct IaxServer {
    _owner: IaxClient,
    api: ServerDescriptor,
    handle: NonNull<c_void>,
}

impl Drop for IaxServer {
    fn drop(&mut self) {
        unsafe { self.api.destroy.unwrap()(self.handle.as_ptr()) };
    }
}

fn load_server_descriptor(client:&IaxClient)->Result<ServerDescriptor,IaxError>{Ok(client.0.server)}
pub struct IaxPeer {
    _owner: IaxClient,
    api: ClientDescriptor,
    handle: NonNull<c_void>,
}

impl IaxPeer {
    /// Feed the peer's retained encoder without committing a network packet.
    pub(crate) fn prepare_audio(
        &mut self,
        pcm: &[f32],
        output: &mut [u8],
        flush: u32,
        result: &mut abi::rptadv_codec_encode_result_v1,
    ) -> i32 {
        unsafe {
            self.api.prepare_audio.unwrap()(
                self.handle.as_ptr(),
                pcm.as_ptr(),
                pcm.len(),
                output.as_mut_ptr(),
                output.len(),
                flush,
                result,
            )
        }
    }
    /// Monotonic call age used to anchor the radio source timeline off the callback.
    pub(crate) fn elapsed_ms(&self) -> u32 {
        unsafe { self.api.elapsed_ms.unwrap()(self.handle.as_ptr()) }
    }
    /// Opaque peer identity for the retained executor from the same protocol DSO.
    pub(crate) fn raw_handle(&self) -> *mut c_void {
        self.handle.as_ptr()
    }

    /// Return the selected codec's linear PCM rate.
    pub fn sample_rate_hz(&self) -> u32 {
        // SAFETY: the uniquely owned peer is live and the validated ABI owns the call.
        unsafe { self.api.sample_rate_hz.unwrap()(self.handle.as_ptr()) }
    }

    /// Send one completed DTMF digit.
    pub fn send_digit(&mut self, digit: u8) -> Result<(), IaxError> {
        // SAFETY: the peer has one mutable owner and the validated function table remains loaded.
        (unsafe { self.api.send_digit.unwrap()(self.handle.as_ptr(), digit) } == 0)
            .then_some(())
            .ok_or(IaxError::Operation)
    }

}
impl Drop for IaxPeer {
    fn drop(&mut self) {
        // SAFETY: this uniquely owned peer is destroyed once before its client is released.
        unsafe { self.api.destroy.unwrap()(self.handle.as_ptr()) };
    }
}

// SAFETY: T is one of the generated C descriptor tables below. Its common header
// is readable, and its remaining fields are nullable C function pointers. Never form
// a reference to the full table supplied by an older, shorter DSO.
unsafe fn descriptor_prefix<T>(pointer: *const T, required: usize, capability: [u8; 16]) -> Result<T, IaxError> {
    if pointer.is_null() {
        return Err(IaxError::IncompatibleAdapter);
    }
    let header = unsafe { pointer.cast::<DescriptorHeader>().read_unaligned() };
    let extent = (header.struct_size as usize).min(size_of::<T>());
    if header.abi_version != ABI_VERSION || header.capability != capability || extent < required
        || !(extent - required).is_multiple_of(size_of::<*const c_void>())
    {
        return Err(IaxError::IncompatibleAdapter);
    }
    let mut table = std::mem::MaybeUninit::<T>::zeroed();
    unsafe {
        std::ptr::copy_nonoverlapping(pointer.cast::<u8>(), table.as_mut_ptr().cast::<u8>(), extent);
        Ok(table.assume_init())
    }
}
