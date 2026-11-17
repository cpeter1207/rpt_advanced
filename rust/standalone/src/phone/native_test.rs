//! Test-only native source fixture. Packet inputs are real loopback UDP. The
//! only source reader is the downstream owner's actual versioned ABI callback.
use super::{
    call_media::Media,
    codec::{AudioCodec, load_catalog},
    media::{AudioProfile, MediaDescription, MediaError},
};
use crate::{abi, providers::ProviderSet};
use std::{ffi::c_void, net::UdpSocket, ptr, time::Duration};

/// Owns the codec, jitter and socket for one serial private call. Field order
/// drops the media handle before unloading its borrowed jitter descriptor.
pub struct NativeRtp {
    media: Media,
    encoder: AudioCodec,
    socket: UdpSocket,
    runtime: tokio::runtime::Runtime,
    _providers: ProviderSet,
}
impl NativeRtp {
    /// Open a negotiated PCMU call, optionally with RFC4733 receive support.
    pub fn new(events: bool) -> Result<Self, MediaError> {
        let providers = ProviderSet::load().map_err(|_| MediaError::Jitter)?;
        // SAFETY: stable DSO data, explicitly retained until Media is dropped.
        let descriptor = unsafe {
            &*ptr::from_ref(
                providers
                    .jitter_descriptor()
                    .map_err(|_| MediaError::Jitter)?,
            )
        };
        let socket = UdpSocket::bind("127.0.0.1:0").map_err(|_| MediaError::Network)?;
        let port = socket.local_addr().map_err(|_| MediaError::Network)?.port();
        let formats = if events { "0 101" } else { "0" };
        let attrs = if events {
            "a=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\n"
        } else {
            ""
        };
        let offer = format!(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {port} RTP/AVP {formats}\r\na=rtpmap:0 PCMU/8000\r\n{attrs}a=ptime:20\r\na=sendrecv\r\n"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| MediaError::Network)?;
        let media = runtime.block_on(Media::new(
            offer.as_bytes(),
            1,
            descriptor,
            &load_catalog()?,
        ))?;
        let remote = MediaDescription::parse(media.answer())?.remote;
        socket.connect(remote).map_err(|_| MediaError::Network)?;
        Ok(Self {
            media,
            encoder: AudioCodec::load(AudioProfile::Mulaw)?,
            socket,
            _providers: providers,
            runtime: runtime,
        })
    }
    fn packet(&self, seq: u16, ts: u32, pt: u8, payload: &[u8]) -> Result<(), MediaError> {
        let mut packet = vec![0x80, pt];
        packet.extend(seq.to_be_bytes());
        packet.extend(ts.to_be_bytes());
        packet.extend(19_u32.to_be_bytes());
        packet.extend(payload);
        self.socket.send(&packet).map_err(|_| MediaError::Network)?;
        self.runtime
            .block_on(async { tokio::time::sleep(Duration::from_millis(2)).await });
        Ok(())
    }
    /// Encode one 20ms PCM block with the released codec and send its RTP packet.
    pub fn audio(&mut self, seq: u16, ts: u32, samples: &[f32; 160]) -> Result<(), MediaError> {
        let mut buf = [0; 160];
        self.encoder.encode(samples, &mut buf)?;
        self.packet(seq, ts, 0, &buf)
    }
    /// Send one ended RFC4733 event with a fresh event identity.
    pub fn digit(&self, seq: u16, ts: u32, event: u8) -> Result<(), MediaError> {
        self.packet(seq, ts, 101, &[event, 0x8a, 0, 160])
    }
    /// Released native jitter counters and all codec/fallback synthetic frames.
    pub fn observation(&self) -> Result<(rptadv_jitter_client::Observation, u64), MediaError> {
        let o = self.media.observe()?;
        Ok((
            o.jitter,
            o.native_plc_frames + o.fallback_frames + o.decode_recovery_frames,
        ))
    }
    /// Forward one actual contract read. No hook can mutate a retained decision.
    ///
    /// # Safety
    /// Both pointers must be live, aligned, nonaliasing full objects generated
    /// from the same C sink/result v1 definitions, with the sizes supplied.
    /// The sink's callback contracts and context must remain valid until return.
    pub unsafe fn read(
        &mut self,
        completion: u32,
        sink: *const c_void,
        sink_size: usize,
        result: *mut c_void,
        result_size: usize,
    ) -> i32 {
        if sink.is_null()
            || result.is_null()
            || sink_size != size_of::<abi::rptadv_peer_source_sink_v1>()
            || result_size != size_of::<abi::rptadv_peer_source_result_v1>()
        {
            return -1;
        }
        match unsafe {
            self.media
                .read(completion, sink.cast(), &mut *result.cast())
        } {
            Ok(()) => 0,
            Err(_) => -1,
        }
    }
}
