//! Hardware-paced radio frame exchange.
use crate::{Error, bindings as ffi, connection::Connection, pcm};
use std::{ffi::CStr, ptr::NonNull};

/// Owns a reservation, carrier state and bounded float workspace.
pub struct Radio {
    connection: Connection,
    receiving: bool,
    scratch: Vec<f32>,
}
// SAFETY: the sole owner transfers its reserved channel, immutable retained format,
// and scratch together. Asterisk's public channel operations support this owner thread;
// no method or destructor can run concurrently because Radio is not Sync or cloneable.
unsafe impl Send for Radio {}

/// One owned Asterisk frame; native representation never crosses inward.
pub struct Frame(pub(crate) NonNull<ffi::ast_frame>);

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: ast_read/ast_translate supplied this uniquely owned frame.
        unsafe {
            ffi::ast_frame_free(self.0.as_ptr(), 1);
        }
    }
}

impl Radio {
    /// Synchronously attach this radio's link profile before the peer starts media.
    /// Peer construction has already installed its negotiated decoded read format.
    pub(crate) fn bind_peer(&self, peer: &mut crate::link::peer_io::PeerIo) -> Result<(), Error> {
        let mut attachment = ffi::urp_ast_link_attach {
            struct_size: std::mem::size_of::<ffi::urp_ast_link_attach>() as u32,
            abi_version: ffi::URP_AST_LINK_ATTACH_ABI_VERSION,
            peer_channel: peer.channel.pointer.as_ptr().cast(),
            accepted_abi_version: 0,
        };
        // SAFETY: both owned channels and the exclusive option payload remain live.
        let result = unsafe {
            ffi::ast_channel_setoption(
                self.connection.channel.pointer.as_ptr(),
                ffi::URP_AST_OPTION_LINK_ATTACH as i32,
                std::ptr::from_mut(&mut attachment).cast(),
                std::mem::size_of_val(&attachment) as i32,
                0,
            )
        };
        if result == 0 && attachment.accepted_abi_version == ffi::URP_AST_LINK_ATTACH_ABI_VERSION {
            Ok(())
        } else {
            Err(Error::Call)
        }
    }

    /// Attach both direct callbacks and require acknowledgment before channel start.
    ///
    /// # Safety
    /// Endpoint code and contexts must remain valid through synchronous channel hangup.
    pub unsafe fn attach_direct(
        &mut self,
        direct: &mut ffi::urp_ast_direct_callbacks,
    ) -> Result<(), Error> {
        direct.accepted_abi_version = 0;
        // SAFETY: the uniquely owned channel synchronously copies the callbacks
        // and writes acknowledgment into this exclusively borrowed descriptor.
        let result = unsafe {
            ffi::ast_channel_setoption(
                self.connection.channel.pointer.as_ptr(),
                ffi::URP_AST_OPTION_DIRECT_CALLBACKS as i32,
                std::ptr::from_mut(direct).cast(),
                std::mem::size_of_val(direct) as i32,
                0,
            )
        };
        if result == 0 && direct.accepted_abi_version == ffi::URP_AST_DIRECT_CALLBACKS_ABI_VERSION {
            Ok(())
        } else {
            Err(Error::Call)
        }
    }
    /// Start the already reserved radio only after all candidate media is prepared.
    pub fn start(&mut self, destination: &CStr) -> Result<(), Error> {
        if unsafe {
            ffi::ast_call(
                self.connection.channel.pointer.as_ptr(),
                destination.as_ptr(),
                0,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(Error::Call)
        }
    }
    /// Wait up to 100 ms for hardware readiness; the timeout never generates audio.
    pub fn ready(&mut self) -> Result<bool, Error> {
        match unsafe { ffi::ast_waitfor(self.connection.channel.pointer.as_ptr(), 100) } {
            value if value < 0 => Err(Error::Hangup),
            0 => Ok(false),
            _ => Ok(true),
        }
    }
    pub(crate) fn new(connection: Connection, maximum: usize) -> Result<Self, Error> {
        if maximum == 0 || maximum > i32::MAX as usize / 2 {
            return Err(Error::InvalidFrame);
        }
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(maximum)
            .map_err(|_| Error::Allocation)?;
        scratch.resize(maximum, 0.0);
        Ok(Self {
            connection,
            receiving: false,
            scratch,
        })
    }

    /// Read one frame, invoke canonical PCM processing and emit matching audio.
    pub fn exchange(
        &mut self,
        mut render: impl FnMut(bool, &mut [f32]) -> bool,
    ) -> Result<(), Error> {
        // SAFETY: only this owner reads/writes the live channel. Asterisk frame
        // storage remains owned until Frame drops, including every error path.
        unsafe {
            let channel = &mut self.connection.channel;
            let frame =
                Frame(NonNull::new(ffi::ast_read(channel.pointer.as_ptr())).ok_or(Error::Hangup)?);
            let raw = frame.0.as_ref();
            if raw.frametype == ffi::AST_FRAME_VOICE {
                let count = usize::try_from(raw.samples).map_err(|_| Error::InvalidFrame)?;
                if raw.data.ptr.is_null()
                    || count == 0
                    || count > self.scratch.len()
                    || raw.datalen <= 0
                    || raw.datalen as usize != count * 2
                    || ffi::ast_format_cmp(
                        raw.subclass.__bindgen_anon_1.format,
                        self.connection.linear.0.pointer(),
                    ) != ffi::AST_FORMAT_CMP_EQUAL
                {
                    return Err(Error::InvalidFrame);
                }
                let data = raw.data.ptr.cast::<i16>();
                let pcm = &mut self.scratch[..count];
                // Unaligned access also accepts byte-aligned host payload storage.
                for (index, sample) in pcm.iter_mut().enumerate() {
                    *sample = pcm::decode(data.add(index).read_unaligned());
                }
                let keyed = render(self.receiving, pcm);
                channel.indicate(keyed)?;
                for (index, sample) in pcm.iter().enumerate() {
                    data.add(index).write_unaligned(pcm::encode(*sample));
                }
                if ffi::ast_write(channel.pointer.as_ptr(), frame.0.as_ptr()) != 0 {
                    return Err(Error::Write);
                }
            } else if raw.frametype == ffi::AST_FRAME_CONTROL
                && [
                    ffi::AST_CONTROL_RADIO_KEY as i32,
                    ffi::AST_CONTROL_RADIO_UNKEY as i32,
                ]
                .contains(&raw.subclass.integer)
            {
                self.receiving = raw.subclass.integer == ffi::AST_CONTROL_RADIO_KEY as i32;
                channel.indicate(render(self.receiving, &mut []))?;
            }
            Ok(())
        }
    }
}
