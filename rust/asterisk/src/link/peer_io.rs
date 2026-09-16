//! One serial owner for an answered IAX channel and its codec/frame resources.
use crate::{
    Error, bindings as ffi,
    codec::{Format, Object},
    connection::Channel,
    pcm,
    radio::Frame,
};
use std::{
    ffi::{CStr, c_void},
    ptr::{self, NonNull},
};

/// Borrowed input is dispatched before the Asterisk frame is freed.
pub enum Input<'a> {
    /// Complete text payload for the core's bounded protocol parser.
    Text(&'a [u8]),
    /// Conventional completed DTMF event.
    Digit(char),
    /// PCM is now visible in the peer's inbound ring.
    Audio(&'a [f32]),
}

struct Decoder {
    path: NonNull<ffi::ast_trans_pvt>,
    source: Object<ffi::ast_format>,
}
impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the sole channel owner has stopped using this translation path.
        unsafe {
            ffi::ast_translator_free_path(self.path.as_ptr());
        }
    }
}

/// Channel, translation state and bounded PCM storage operated by one media owner.
/// No method is called from a radio worker; inbound ring consumption is independent.
pub struct PeerIo {
    channel: Channel,
    linear: Format,
    decoder: Option<Decoder>,
    input: Vec<f32>,
    output: Vec<i16>,
}
impl PeerIo {
    /// Try ordered single-codec offers under one shared 20-second dial deadline.
    /// Recheck the runtime token before and after each blocking dial; final admission
    /// remains the serialized core owner's responsibility after reader preparation.
    pub fn dial(
        destination: &CStr,
        local: &CStr,
        maximum: usize,
        mut current: impl FnMut() -> bool,
    ) -> Result<Self, Error> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        for candidate in crate::codec::candidates()? {
            if !current() {
                return Err(Error::Reservation);
            }
            let remaining = deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_millis();
            if remaining == 0 {
                break;
            }
            let offer = candidate.offer()?;
            let mut reason = 0;
            // SAFETY: offer/strings remain owned through the public blocking call.
            let pointer = unsafe {
                ffi::ast_request_and_dial(
                    c"IAX2".as_ptr(),
                    offer.pointer(),
                    ptr::null(),
                    ptr::null(),
                    destination.as_ptr(),
                    remaining.min(20000) as i32,
                    &mut reason,
                    local.as_ptr(),
                    local.as_ptr(),
                )
            };
            let Some(pointer) = NonNull::new(pointer) else {
                continue;
            };
            let channel = Channel {
                pointer,
                keyed: false,
            };
            // SAFETY: this call owns the returned channel, including unanswered cases.
            if !current()
                || unsafe { ffi::ast_channel_state(pointer.as_ptr()) } != ffi::AST_STATE_UP
            {
                continue;
            }
            // SAFETY: the cached linear format is borrowed; take one owned reference.
            let linear = unsafe {
                let linear = ffi::ast_format_cache_get_slin_by_rate(candidate.rate());
                if linear.is_null() {
                    return Err(Error::UnsupportedFormat);
                }
                ffi::__ao2_ref(
                    linear.cast(),
                    1,
                    ptr::null(),
                    c"rust/asterisk/link".as_ptr(),
                    0,
                    c"dial".as_ptr(),
                );
                Format(Object::owned(linear).unwrap())
            };
            std::mem::forget(channel);
            // SAFETY: ownership transfers exactly once, including failed preparation.
            return unsafe { Self::from_owned_channel(pointer.as_ptr().cast(), linear, maximum) };
        }
        Err(Error::Reservation)
    }
    /// Take ownership of an answered peer and create its one released inbound ring.
    ///
    /// # Safety
    /// `channel` must be one exclusively owned live Asterisk channel reference.
    /// The caller stops every other reader/writer before transferring it here.
    /// Ownership transfers even on failure. `linear` must be a signed-linear format.
    pub unsafe fn from_owned_channel(
        channel: *mut c_void,
        linear: Format,
        maximum: usize,
    ) -> Result<Self, Error> {
        let channel = Channel {
            pointer: NonNull::new(channel.cast()).ok_or(Error::Reservation)?,
            keyed: false,
        };
        if maximum == 0 || maximum > i32::MAX as usize / 2 {
            return Err(Error::InvalidFrame);
        }
        // SAFETY: linear is held throughout this object's lifetime.
        unsafe {
            let cached = ffi::ast_format_cache_get_slin_by_rate(linear.rate());
            if cached.is_null()
                || ffi::ast_format_cmp(cached, linear.0.pointer()) != ffi::AST_FORMAT_CMP_EQUAL
            {
                return Err(Error::UnsupportedFormat);
            }
            if ffi::ast_set_read_format(channel.pointer.as_ptr(), linear.0.pointer()) != 0
                || ffi::ast_set_write_format(channel.pointer.as_ptr(), linear.0.pointer()) != 0
            {
                return Err(Error::ChannelFormat);
            }
        }
        let mut input = Vec::new();
        let mut output = Vec::new();
        input
            .try_reserve_exact(maximum)
            .map_err(|_| Error::Allocation)?;
        output
            .try_reserve_exact(maximum)
            .map_err(|_| Error::Allocation)?;
        input.resize(maximum, 0.0);
        output.resize(maximum, 0);
        Ok(Self {
            channel,
            linear,
            decoder: None,
            input,
            output,
        })
    }
    /// Negotiated decoded rate for persistent outbound conversion.
    pub fn rate(&self) -> u32 {
        self.linear.rate()
    }
    /// Wait at most one millisecond on the sole channel owner.
    pub fn ready(&mut self) -> Result<bool, Error> {
        // SAFETY: channel ownership is exclusive and retained throughout the wait.
        match unsafe { ffi::ast_waitfor(self.channel.pointer.as_ptr(), 1) } {
            value if value < 0 => Err(Error::Hangup),
            value => Ok(value > 0),
        }
    }
    /// Read and dispatch one owned frame, preserving codec buffering as a live session.
    pub fn read(&mut self, mut dispatch: impl FnMut(Input<'_>)) -> Result<(), Error> {
        // SAFETY: this object is the channel's exclusive I/O owner.
        unsafe {
            let frame = Frame(
                NonNull::new(ffi::ast_read(self.channel.pointer.as_ptr())).ok_or(Error::Hangup)?,
            );
            let raw = frame.0.as_ref();
            match raw.frametype {
                ffi::AST_FRAME_TEXT => {
                    if raw.datalen > 0 && !raw.data.ptr.is_null() {
                        dispatch(Input::Text(std::slice::from_raw_parts(
                            raw.data.ptr.cast(),
                            raw.datalen as usize,
                        )));
                    }
                }
                ffi::AST_FRAME_DTMF_END => {
                    if let Ok(value) = u8::try_from(raw.subclass.integer) {
                        if b"0123456789ABCD*#".contains(&value) {
                            dispatch(Input::Digit(value as char));
                        }
                    }
                }
                ffi::AST_FRAME_CONTROL
                    if raw.subclass.integer == ffi::AST_CONTROL_HANGUP as i32 =>
                {
                    return Err(Error::Hangup);
                }
                ffi::AST_FRAME_VOICE => {
                    if raw.samples == 0 && raw.datalen == 0 {
                        return Ok(());
                    }
                    let source = raw.subclass.__bindgen_anon_1.format;
                    if source.is_null() {
                        return Err(Error::InvalidFrame);
                    }
                    let audio = if ffi::ast_format_cmp(source, self.linear.0.pointer())
                        == ffi::AST_FORMAT_CMP_EQUAL
                    {
                        frame
                    } else {
                        let rebuild = self.decoder.as_ref().is_none_or(|decoder| {
                            ffi::ast_format_cmp(source, decoder.source.pointer())
                                != ffi::AST_FORMAT_CMP_EQUAL
                        });
                        if rebuild {
                            self.decoder = None;
                            let path = NonNull::new(ffi::ast_translator_build_path(
                                self.linear.0.pointer(),
                                source,
                            ))
                            .ok_or(Error::Translation)?;
                            ffi::__ao2_ref(
                                source.cast(),
                                1,
                                ptr::null(),
                                c"rust/asterisk/link".as_ptr(),
                                0,
                                c"decode".as_ptr(),
                            );
                            self.decoder = Some(Decoder {
                                path,
                                source: Object::owned(source).unwrap(),
                            });
                        }
                        let translated = ffi::ast_translate(
                            self.decoder.as_ref().unwrap().path.as_ptr(),
                            frame.0.as_ptr(),
                            1,
                        );
                        std::mem::forget(frame);
                        let Some(translated) = NonNull::new(translated) else {
                            return Ok(());
                        };
                        Frame(translated)
                    };
                    let raw = audio.0.as_ref();
                    let count = usize::try_from(raw.samples).map_err(|_| Error::InvalidFrame)?;
                    if count == 0 && raw.datalen == 0 {
                        return Ok(());
                    }
                    if raw.frametype != ffi::AST_FRAME_VOICE
                        || raw.data.ptr.is_null()
                        || count > self.input.len()
                        || raw.datalen <= 0
                        || raw.datalen as usize != count * 2
                        || ffi::ast_format_cmp(
                            raw.subclass.__bindgen_anon_1.format,
                            self.linear.0.pointer(),
                        ) != ffi::AST_FORMAT_CMP_EQUAL
                    {
                        return Err(Error::InvalidFrame);
                    }
                    for (index, sample) in self.input[..count].iter_mut().enumerate() {
                        *sample =
                            pcm::decode(raw.data.ptr.cast::<i16>().add(index).read_unaligned());
                    }
                    dispatch(Input::Audio(&self.input[..count]));
                }
                _ => {}
            }
        }
        Ok(())
    }
    /// Send control text from the sole channel owner; core classifies advisory failures.
    pub fn send_text(&mut self, text: &CStr) -> Result<(), Error> {
        // SAFETY: the owned channel and NUL-terminated text remain live for this call.
        if unsafe { ffi::ast_sendtext(self.channel.pointer.as_ptr(), text.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(Error::Write)
        }
    }
    /// Send a completed conventional digit from the sole channel owner.
    pub fn send_digit(&mut self, digit: char) -> Result<(), Error> {
        if !"0123456789ABCD*#".contains(digit) {
            return Err(Error::InvalidFrame);
        }
        // SAFETY: the serial channel owner invokes the public Asterisk DTMF operation.
        if unsafe {
            ffi::ast_senddigit(self.channel.pointer.as_ptr(), digit as std::ffi::c_char, 0)
        } == 0
        {
            Ok(())
        } else {
            Err(Error::Write)
        }
    }
    /// Send already converted codec-rate F32; the released egress adapter owns resampling.
    pub fn write(&mut self, audio: &[f32]) -> Result<(), Error> {
        if audio.len() > self.output.len() {
            return Err(Error::InvalidFrame);
        }
        for (output, input) in self.output.iter_mut().zip(audio) {
            *output = pcm::encode(*input);
        }
        // SAFETY: ast_write borrows the frame and bounded samples for this call only.
        unsafe {
            let mut frame: ffi::ast_frame = std::mem::zeroed();
            frame.frametype = ffi::AST_FRAME_VOICE;
            frame.subclass.__bindgen_anon_1.format = self.linear.0.pointer();
            frame.data.ptr = self.output.as_mut_ptr().cast();
            frame.samples = audio.len() as i32;
            frame.datalen = frame.samples * 2;
            if ffi::ast_write(self.channel.pointer.as_ptr(), &mut frame) == 0 {
                Ok(())
            } else {
                Err(Error::Write)
            }
        }
    }
}
// SAFETY: PeerIo uniquely owns channel, formats, translation state and workspaces;
// moving it to the one channel thread does not permit concurrent access.
unsafe impl Send for PeerIo {}
