//! Released G.711 ABI ownership; no duplicate implementation or resampler.
use super::media::{G711, MediaError};
use crate::abi;
use std::{ffi::c_void, mem::size_of, ptr};

type Descriptor = abi::rptadv_codec_descriptor_v1_table;

/// One selected G.711 profile with independent encoder and decoder contexts.
pub struct G711Codec {
    api: Descriptor,
    encoder: *mut c_void,
    decoder: *mut c_void,
    _library: libloading::Library,
}
// SAFETY: both contexts move together to one serial media owner, never shared.
unsafe impl Send for G711Codec {}

impl G711Codec {
    /// Load and validate the versioned provider, preparing only this profile.
    pub fn load(codec: G711) -> Result<Self, MediaError> {
        // SAFETY: only the project's released versioned provider is loaded.
        let library = unsafe { libloading::Library::new("librptadv_codec_g711.so.1") }
            .map_err(|_| MediaError::Codec)?;
        let api = unsafe {
            let entry = library
                .get::<unsafe extern "C" fn() -> *const Descriptor>(b"rptadv_codec_descriptor_v1\0")
                .map_err(|_| MediaError::Codec)?;
            let table = entry();
            // The mandatory readable size prefix is checked before copying.
            if table.is_null() || ptr::read(table.cast::<u32>()) < size_of::<Descriptor>() as u32 {
                return Err(MediaError::Codec);
            }
            table.read()
        };
        if api.abi_version != 1
            || api.capability != *b"rptadv.codec.v1\0"
            || api.open_encoder.is_none()
            || api.open_decoder.is_none()
            || api.encode.is_none()
            || api.decode.is_none()
            || api.destroy.is_none()
        {
            return Err(MediaError::Codec);
        }
        let config = abi::rptadv_codec_config_v1 {
            struct_size: size_of::<abi::rptadv_codec_config_v1>() as u32,
            profile_id: match codec {
                G711::Mulaw => 1,
                G711::Alaw => 2,
            },
            mode_id: 0,
            packet_frames: 160,
            max_packet_bytes: 160,
        };
        let mut owner = Self {
            api,
            encoder: ptr::null_mut(),
            decoder: ptr::null_mut(),
            _library: library,
        };
        // SAFETY: complete config and writable owned pointer slots; destroy also
        // handles partial setup before this function returns an error.
        if unsafe { owner.api.open_encoder.unwrap()(&config, &mut owner.encoder) } != 0
            || owner.encoder.is_null()
            || unsafe { owner.api.open_decoder.unwrap()(&config, &mut owner.decoder) } != 0
            || owner.decoder.is_null()
        {
            return Err(MediaError::Codec);
        }
        Ok(owner)
    }
    /// Encode exactly 160 source-rate normalized mono samples.
    pub fn encode(&mut self, pcm: &[f32; 160], packet: &mut [u8; 160]) -> Result<(), MediaError> {
        let mut result: abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = size_of::<abi::rptadv_codec_encode_result_v1>() as u32;
        // SAFETY: the unique encoder and nonoverlapping complete arrays live
        // through this synchronous off-audio call.
        let status = unsafe {
            self.api.encode.unwrap()(
                self.encoder,
                pcm.as_ptr(),
                pcm.len(),
                packet.as_mut_ptr(),
                packet.len(),
                0,
                &mut result,
            )
        };
        if status != 0
            || result.consumed_frames != 160
            || result.encoded_bytes != 160
            || result.source_frames != 160
            || result.packet_frames != 160
        {
            return Err(MediaError::Codec);
        }
        Ok(())
    }
    /// Decode one complete 20 ms G.711 payload.
    pub fn decode(&mut self, packet: &[u8; 160], pcm: &mut [f32; 160]) -> Result<(), MediaError> {
        let mut count = 0;
        // SAFETY: the unique decoder and borrowed complete arrays remain live.
        let status = unsafe {
            self.api.decode.unwrap()(
                self.decoder,
                packet.as_ptr(),
                packet.len(),
                pcm.as_mut_ptr(),
                pcm.len(),
                &mut count,
            )
        };
        if status != 0 || count != 160 {
            return Err(MediaError::Codec);
        }
        Ok(())
    }
}
impl Drop for G711Codec {
    fn drop(&mut self) {
        // SAFETY: no callback retains these handles; the DSO is still retained.
        unsafe {
            self.api.destroy.unwrap()(self.encoder);
            self.api.destroy.unwrap()(self.decoder);
        }
    }
}
