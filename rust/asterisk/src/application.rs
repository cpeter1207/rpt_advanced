//! Copied incoming identity and exactly-once public Asterisk channel handoff.
use crate::{
    Error, bindings as ffi,
    codec::{Format, Object},
    connection::Channel,
    link::peer_io::PeerIo,
};
use std::{
    ffi::{CStr, CString, c_void},
    ptr::{self, NonNull},
};

pub(crate) unsafe extern "C" fn callback(
    channel: *mut ffi::ast_channel,
    data: *const std::ffi::c_char,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), Error> {
        if data.is_null() {
            return Err(Error::Admission);
        }
        let identity = unsafe { IncomingIdentity::inspect(channel.cast(), CStr::from_ptr(data)) }
            .map_err(|_| Error::Admission)?;
        let product = crate::lifecycle::product().ok_or(Error::Admission)?;
        if !product.authorize_incoming(&identity.local, &identity.remote, &identity.address) {
            return Err(Error::Admission);
        }
        let peer = unsafe { identity.handoff(channel.cast(), 4096) }?;
        let peer = crate::services::into_raw(peer);
        match unsafe {
            product.incoming(&identity.local, &identity.remote, &identity.address, peer)
        } {
            0 => Ok(()),
            1 => {
                unsafe { crate::services::destroy_raw(peer) };
                Err(Error::Admission)
            }
            _ => Err(Error::Admission),
        }
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(-1, |()| 0)
}

/// Copied input used for directory verification and current core admission.
pub struct IncomingIdentity {
    /// Requested configured local node.
    pub local: String,
    /// Valid caller-number identity.
    pub remote: String,
    /// Numeric source returned by CHANNEL(peerip).
    pub address: String,
}

impl IncomingIdentity {
    /// Read public channel metadata without retaining any Asterisk string pointer.
    ///
    /// # Safety
    /// `channel` is live and owned by this application's dialplan caller throughout the call.
    pub unsafe fn inspect(channel: *mut c_void, local: &CStr) -> Result<Self, Error> {
        let channel = NonNull::new(channel.cast::<ffi::ast_channel>()).ok_or(Error::Admission)?;
        let local = local.to_str().map_err(|_| Error::Admission)?;
        if local.is_empty() {
            return Err(Error::Admission);
        }
        let technology =
            unsafe { ffi::ast_channel_tech(channel.as_ptr()).as_ref() }.ok_or(Error::Admission)?;
        if technology.type_.is_null() || unsafe { CStr::from_ptr(technology.type_) } != c"IAX2" {
            return Err(Error::Admission);
        }
        let caller = unsafe { ffi::ast_channel_caller(channel.as_ptr()).as_ref() }
            .ok_or(Error::Admission)?;
        if caller.id.number.valid == 0 || caller.id.number.str_.is_null() {
            return Err(Error::Admission);
        }
        let remote = unsafe { CStr::from_ptr(caller.id.number.str_) }
            .to_str()
            .map_err(|_| Error::Admission)?
            .to_owned();
        let mut address = [0_u8; 256];
        if unsafe {
            ffi::ast_func_read(
                channel.as_ptr(),
                c"CHANNEL(peerip)".as_ptr(),
                address.as_mut_ptr().cast(),
                address.len(),
            )
        } != 0
        {
            return Err(Error::Admission);
        }
        let address = CStr::from_bytes_until_nul(&address)
            .map_err(|_| Error::Admission)?
            .to_str()
            .map_err(|_| Error::Admission)?
            .to_owned();
        Ok(Self {
            local: local.into(),
            remote,
            address,
        })
    }

    /// After current-policy authorization, move the dialplan channel into one queued DOWN owner,
    /// answer it, and transfer that owner to the IAX media boundary. Every failed stage hangs up
    /// only the newly owned channel; the original dialplan channel is never ours to hang up.
    ///
    /// # Safety
    /// `channel` must remain the same live dialplan-owned channel inspected above, with no
    /// concurrent move/hangup. The caller must revalidate policy before runtime attachment.
    pub unsafe fn handoff(&self, channel: *mut c_void, maximum: usize) -> Result<PeerIo, Error> {
        let source = NonNull::new(channel.cast::<ffi::ast_channel>()).ok_or(Error::Admission)?;
        let remote = CString::new(self.remote.as_str()).map_err(|_| Error::Admission)?;
        let unique = unsafe { ffi::ast_channel_uniqueid(source.as_ptr()) };
        if unique.is_null() {
            return Err(Error::Admission);
        }
        let pointer = NonNull::new(unsafe {
            ffi::__ast_channel_alloc(
                1,
                ffi::AST_STATE_DOWN as i32,
                remote.as_ptr(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                source.as_ptr(),
                ffi::AST_AMA_NONE,
                ptr::null_mut(),
                c"rust/asterisk/application.rs".as_ptr(),
                0,
                c"handoff".as_ptr(),
                c"RptAdvanced/%s-%s".as_ptr(),
                remote.as_ptr(),
                unique,
            )
        })
        .ok_or(Error::Allocation)?;
        let owned = Channel {
            pointer,
            keyed: false,
        };
        unsafe {
            ffi::__ao2_unlock(
                pointer.as_ptr().cast(),
                c"rust/asterisk/application.rs".as_ptr(),
                c"handoff".as_ptr(),
                0,
                c"owned".as_ptr(),
            );
        }
        if unsafe { ffi::ast_channel_move(pointer.as_ptr(), source.as_ptr()) } != 0
            || unsafe { ffi::ast_answer(pointer.as_ptr()) } != 0
        {
            return Err(Error::Admission);
        }
        let capabilities = unsafe { ffi::ast_channel_nativeformats(pointer.as_ptr()) };
        if capabilities.is_null() {
            return Err(Error::MissingFormat);
        }
        let native = Format(
            unsafe { Object::owned(ffi::ast_format_cap_get_format(capabilities, 0)) }
                .ok_or(Error::MissingFormat)?,
        );
        let linear = unsafe { ffi::ast_format_cache_get_slin_by_rate(native.rate()) };
        if linear.is_null() {
            return Err(Error::MissingFormat);
        }
        unsafe {
            ffi::__ao2_ref(
                linear.cast(),
                1,
                ptr::null(),
                c"rust/asterisk/application.rs".as_ptr(),
                0,
                c"handoff".as_ptr(),
            );
        }
        let format =
            Format(unsafe { Object::owned(linear) }.expect("checked cached linear format"));
        // from_owned_channel assumes ownership even on error; disarm this guard first.
        std::mem::forget(owned);
        unsafe { PeerIo::from_owned_channel(pointer.as_ptr().cast(), format, maximum) }
    }
}

#[cfg(test)]
#[path = "application_tests.rs"]
mod tests;
