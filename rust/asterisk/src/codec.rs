//! Ordered live-registry codec candidates and Asterisk object ownership.
use crate::{Error, bindings as ffi};
use std::{
    cmp::Reverse,
    ptr::{self, NonNull},
};

pub(crate) struct Object<T>(NonNull<T>);

impl<T> Object<T> {
    // The input must carry one owned Asterisk reference when nonnull.
    pub(crate) unsafe fn owned(pointer: *mut T) -> Option<Self> {
        NonNull::new(pointer).map(Self)
    }

    pub(crate) fn pointer(&self) -> *mut T {
        self.0.as_ptr()
    }
}

impl<T> Drop for Object<T> {
    fn drop(&mut self) {
        // SAFETY: each owner releases exactly its acquired ao2 reference.
        unsafe {
            ffi::__ao2_ref(
                self.pointer().cast(),
                -1,
                ptr::null(),
                c"rust/asterisk".as_ptr(),
                0,
                c"drop".as_ptr(),
            );
        }
    }
}

/// One owned concrete Asterisk wire format.
pub struct Format(pub(crate) Object<ffi::ast_format>);

impl Format {
    /// Negotiated samples per second.
    pub fn rate(&self) -> u32 {
        // SAFETY: self retains the immutable format reference.
        unsafe { ffi::ast_format_get_sample_rate(self.0.pointer()) }
    }
    /// Create an offer containing only this candidate.
    pub fn offer(&self) -> Result<Offer, Error> {
        // SAFETY: the new capability and format remain owned throughout append.
        unsafe {
            let cap = Object::owned(ffi::__ast_format_cap_alloc(
                ffi::AST_FORMAT_CAP_FLAG_DEFAULT,
                ptr::null(),
                c"rust/asterisk".as_ptr(),
                0,
                c"offer".as_ptr(),
            ))
            .ok_or(Error::Allocation)?;
            if ffi::__ast_format_cap_append(
                cap.pointer(),
                self.0.pointer(),
                0,
                ptr::null(),
                c"rust/asterisk".as_ptr(),
                0,
                c"offer".as_ptr(),
            ) != 0
            {
                return Err(Error::Offer);
            }
            Ok(Offer { _cap: cap })
        }
    }
}

/// Owned single-format channel capability.
pub struct Offer {
    _cap: Object<ffi::ast_format_cap>,
}

impl Offer {
    pub(crate) fn pointer(&self) -> *mut ffi::ast_format_cap {
        self._cap.pointer()
    }
}

/// An owned translation path borrowing its source and destination formats.
pub struct Translator<'a> {
    path: Option<NonNull<ffi::ast_trans_pvt>>,
    _formats: std::marker::PhantomData<&'a Format>,
}

impl<'a> Translator<'a> {
    /// Construct a path, or use identity PCM without allocating a translator.
    pub fn new(destination: &'a Format, source: &'a Format) -> Result<Self, Error> {
        // SAFETY: the borrowed formats outlive the translator owner.
        let path = unsafe {
            if ffi::ast_format_cmp(destination.0.pointer(), source.0.pointer())
                == ffi::AST_FORMAT_CMP_EQUAL
            {
                None
            } else {
                Some(
                    NonNull::new(ffi::ast_translator_build_path(
                        destination.0.pointer(),
                        source.0.pointer(),
                    ))
                    .ok_or(Error::Translation)?,
                )
            }
        };
        Ok(Self {
            path,
            _formats: std::marker::PhantomData,
        })
    }
    /// Consume an input frame; `None` means the codec buffered this packet.
    pub fn translate(&mut self, frame: crate::radio::Frame) -> Option<crate::radio::Frame> {
        let Some(path) = self.path else {
            return Some(frame);
        };
        // SAFETY: consume=1 transfers the input even when buffering returns null.
        let output = unsafe { ffi::ast_translate(path.as_ptr(), frame.0.as_ptr(), 1) };
        std::mem::forget(frame);
        NonNull::new(output).map(crate::radio::Frame)
    }
}

impl Drop for Translator<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.path {
            // SAFETY: the translator is uniquely owned and quiescent.
            unsafe {
                ffi::ast_translator_free_path(path.as_ptr());
            }
        }
    }
}

/// Collect supported codecs, descending by rate and then preferring linear PCM.
pub fn candidates() -> Result<Vec<Format>, Error> {
    // SAFETY: registry lookups return owned references, linear cache values are borrowed.
    unsafe {
        let maximum = ffi::ast_codec_get_max();
        if maximum <= 0 {
            return Err(Error::NoCandidates);
        }
        let mut candidates = Vec::new();
        candidates
            .try_reserve_exact(maximum as usize)
            .map_err(|_| Error::Allocation)?;
        for id in 1..=maximum {
            let Some(codec) = Object::owned(ffi::ast_codec_get_by_id(id)) else {
                continue;
            };
            let codec_info = &*codec.pointer();
            let rate = codec_info.sample_rate;
            if codec_info.type_ != ffi::AST_MEDIA_TYPE_AUDIO || rate == 0 || rate > 48000 {
                continue;
            }
            let Some(format) = Object::owned(ffi::ast_format_cache_get_by_codec(codec.pointer()))
            else {
                continue;
            };
            let linear = ffi::ast_format_cache_get_slin_by_rate(rate);
            let format = Format(format);
            if linear.is_null()
                || ffi::ast_format_get_sample_rate(linear) != rate
                || format.rate() != rate
            {
                continue;
            }
            let direct =
                ffi::ast_format_cmp(format.0.pointer(), linear) == ffi::AST_FORMAT_CMP_EQUAL;
            if !direct
                && (ffi::ast_translate_path_steps(format.0.pointer(), linear) == u32::MAX
                    || ffi::ast_translate_path_steps(linear, format.0.pointer()) == u32::MAX)
            {
                continue;
            }
            candidates.push((rate, direct, format));
        }
        if candidates.is_empty() {
            return Err(Error::NoCandidates);
        }
        // Stable ordering preserves registry ties and duplicate owned references.
        candidates.sort_by_key(|(rate, direct, _)| Reverse((*rate, *direct)));
        let mut result = Vec::new();
        result
            .try_reserve_exact(candidates.len())
            .map_err(|_| Error::Allocation)?;
        result.extend(candidates.into_iter().map(|(_, _, format)| format));
        Ok(result)
    }
}
