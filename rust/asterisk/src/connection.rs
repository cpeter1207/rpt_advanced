//! Reservation of the fixed 48 kHz RadioPlusAdvanced channel.
use crate::{
    Error, bindings as ffi,
    codec::{Format, Object},
    radio::Radio,
};
use std::{
    ffi::CStr,
    ptr::{self, NonNull},
};

/// Reserved radio channel and its native format.
pub struct Connection {
    pub(crate) channel: Channel,
    pub(crate) linear: Format,
}

pub(crate) struct Channel {
    pub(crate) pointer: NonNull<ffi::ast_channel>,
    pub(crate) keyed: bool,
}

impl Channel {
    pub(crate) fn indicate(&mut self, keyed: bool) -> Result<(), Error> {
        if self.keyed != keyed {
            let condition = if keyed {
                ffi::AST_CONTROL_RADIO_KEY
            } else {
                ffi::AST_CONTROL_RADIO_UNKEY
            };
            // SAFETY: this owner exclusively operates its reserved channel.
            if unsafe { ffi::ast_indicate(self.pointer.as_ptr(), condition as i32) } != 0 {
                return Err(Error::Indication);
            }
            self.keyed = keyed;
        }
        Ok(())
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        let _ = self.indicate(false);
        // SAFETY: channel ownership is unique, and hangup ends it exactly once.
        unsafe {
            ffi::ast_hangup(self.pointer.as_ptr());
        }
    }
}

impl Connection {
    /// Reserve without calling or keying the radio.
    ///
    /// A technology lookup is used only as an availability check. The request
    /// capability owns its cached slin48 format and never borrows provider state.
    pub fn open(name: &CStr) -> Result<Self, Error> {
        // SAFETY: the cached format is retained before it is used to create the
        // owned request offer; ast_request establishes the channel's own lifetime.
        unsafe {
            if ffi::ast_get_channel_tech(c"RadioPlusAdvanced".as_ptr()).is_null() {
                return Err(Error::MissingTechnology);
            }
            let cached = ffi::ast_format_cache_get_slin_by_rate(48000);
            if cached.is_null() {
                return Err(Error::MissingFormat);
            }
            if ffi::ast_format_get_sample_rate(cached) != 48000 {
                return Err(Error::UnsupportedFormat);
            }
            // The live cache owns this valid AO2 object. Retaining it does not allocate
            // or fail; the return value is the old reference count, not a status.
            ffi::__ao2_ref(
                cached.cast(),
                1,
                ptr::null(),
                c"rust/asterisk".as_ptr(),
                0,
                c"connection".as_ptr(),
            );
            let linear = Format(Object::owned(cached).expect("checked cached format"));
            let offer = linear.offer()?;
            let mut cause = 0;
            let pointer = NonNull::new(ffi::ast_request(
                c"RadioPlusAdvanced".as_ptr(),
                offer.pointer(),
                ptr::null(),
                ptr::null(),
                name.as_ptr(),
                &mut cause,
            ))
            .ok_or(Error::Reservation)?;
            let channel = Channel {
                pointer,
                keyed: false,
            };
            // The now-owned channel, not a borrowed technology object, determines
            // the reserved native transport. Never accept an implicit rate/codec path.
            let native_cap = ffi::ast_channel_nativeformats(pointer.as_ptr());
            if native_cap.is_null() {
                return Err(Error::MissingFormat);
            }
            if ffi::ast_format_cap_count(native_cap) != 1 {
                return Err(Error::UnsupportedFormat);
            }
            let native = Object::owned(ffi::ast_format_cap_get_format(native_cap, 0))
                .ok_or(Error::MissingFormat)?;
            if ffi::ast_format_get_sample_rate(native.pointer()) != 48000
                || ffi::ast_format_cmp(native.pointer(), linear.0.pointer())
                    != ffi::AST_FORMAT_CMP_EQUAL
            {
                return Err(Error::UnsupportedFormat);
            }
            if ffi::ast_set_read_format(pointer.as_ptr(), linear.0.pointer()) != 0
                || ffi::ast_set_write_format(pointer.as_ptr(), linear.0.pointer()) != 0
            {
                return Err(Error::ChannelFormat);
            }
            Ok(Self { channel, linear })
        }
    }

    /// Transfer reservation into a bounded frame exchange owner.
    pub fn into_radio(self, maximum_frames: usize) -> Result<Radio, Error> {
        Radio::new(self, maximum_frames)
    }
}
