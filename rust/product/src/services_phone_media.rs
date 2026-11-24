//! Ordered, source-timed media and digits for one genuine authenticated transport.
use super::{Error, HostServices, PeerIo, ProtocolId, abi};
use std::{
    ffi::c_void, mem::size_of, panic::{catch_unwind, AssertUnwindSafe},
    ptr, sync::atomic::{AtomicU64, Ordering},
};

/// This epoch names one actual frontend clock reader for the lifetime of its
/// serial call. Observed failure/backwards time is sticky for that call.
pub(crate) struct PhoneClock {
    epoch: u64,
    read: unsafe extern "C" fn() -> u64,
    last: u64,
}
impl PhoneClock {
    pub(crate) fn prepare(services: HostServices) -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let group = services.phone_provider()?;
        let mut out = abi::rptadv_media_clock_reader_v1 {
            struct_size: size_of::<abi::rptadv_media_clock_reader_v1>() as u32,
            abi_version: 1, clock_kind: 0, reserved: 0, read_ns: None,
        };
        if unsafe { group.media_clock_reader.unwrap()(services.context(), &mut out) } != 0
            || out.struct_size < size_of::<abi::rptadv_media_clock_reader_v1>() as u32
            || out.abi_version != 1 || out.clock_kind != 1 || out.reserved != 0 {
            return Err(Error::Admission);
        }
        let read = out.read_ns.ok_or(Error::Admission)?;
        let origin = unsafe { read() };
        if origin == 0 { return Err(Error::Admission); }
        let epoch = NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| next.checked_add(1))
            .map_err(|_| Error::Admission)?;
        Ok(Self { epoch, read, last: origin })
    }
    pub(crate) fn now(&mut self) -> Result<u64, Error> {
        if self.epoch == 0 { return Err(Error::Operation); }
        let now = unsafe { (self.read)() };
        if now < self.last { self.epoch = 0; return Err(Error::Operation); }
        self.last = now;
        Ok(now)
    }
}

pub(crate) enum Input<'a> {
    Audio { start: u64, pcm: &'a [f32], born_ns: Option<u64>, concealed: bool },
    Gap { start: u64, count: u32 },
    Discontinuity,
    Adaptation { start: u64, count: u32 },
    Digit(char),
}

struct Sink<'a> {
    dispatch: &'a mut dyn FnMut(Input<'_>) -> usize,
    epoch: u64,
    now: u64,
    failed: bool,
}
impl Sink<'_> {
    fn accept(&mut self, input: Input<'_>, maximum: usize) -> usize {
        if self.failed { return 0; }
        match catch_unwind(AssertUnwindSafe(|| (self.dispatch)(input))) {
            Ok(accepted) if accepted <= maximum => accepted,
            _ => { self.failed = true; 0 }
        }
    }
}
unsafe extern "C" fn timed(context: *mut c_void, start: u64, data: *const f32,
    count: usize, concealed: u32, birth: *const abi::rptadv_source_birth_v1) -> usize {
    let state = unsafe { &mut *context.cast::<Sink<'_>>() };
    if count > 4096 || data.is_null() || concealed > 1 || start.checked_add(count as u64).is_none() {
        state.failed = true; return 0;
    }
    let born_ns = if birth.is_null() {
        None
    } else {
        if unsafe { ptr::addr_of!((*birth).struct_size).read() } < size_of::<abi::rptadv_source_birth_v1>() as u32 {
            state.failed = true; return 0;
        }
        let birth = unsafe { &*birth };
        if birth.abi_version != 1 || birth.host_clock_epoch != state.epoch || birth.born_ns > state.now {
            state.failed = true; return 0;
        }
        Some(birth.born_ns)
    };
    let pcm = unsafe { std::slice::from_raw_parts(data, count) };
    state.accept(Input::Audio { start, pcm, born_ns, concealed: concealed != 0 }, count)
}
unsafe extern "C" fn gap(context: *mut c_void, start: u64, count: u32) -> u32 {
    let state = unsafe { &mut *context.cast::<Sink<'_>>() };
    if start.checked_add(u64::from(count)).is_none() { state.failed = true; return 0; }
    state.accept(Input::Gap { start, count }, count as usize) as u32
}
unsafe extern "C" fn discontinuity(context: *mut c_void) -> i32 {
    let state = unsafe { &mut *context.cast::<Sink<'_>>() };
    i32::from(state.accept(Input::Discontinuity, 1) == 0)
}
unsafe extern "C" fn adaptation(context: *mut c_void, start: u64, count: u32) -> i32 {
    let state = unsafe { &mut *context.cast::<Sink<'_>>() };
    if count == 0 || start.checked_add(u64::from(count)).is_none() { state.failed = true; return -1; }
    i32::from(state.accept(Input::Adaptation { start, count }, 1) == 0)
}
unsafe extern "C" fn digit(context: *mut c_void, symbol: u32) -> i32 {
    let state = unsafe { &mut *context.cast::<Sink<'_>>() };
    let Some(symbol) = char::from_u32(symbol).filter(|d| rpt_advanced_core::link::valid_digit(*d)) else {
        state.failed = true; return -1;
    };
    i32::from(state.accept(Input::Digit(symbol), 1) == 0)
}

impl PeerIo {
    /// Return true on an observed native hangup. A telephone transport cannot
    /// contribute raw selected keys, finite Echo acknowledgements or ASL text.
    pub(crate) fn read_private(&mut self, dispatch: &mut dyn FnMut(Input<'_>) -> usize) -> Result<bool, Error> {
        if self.identity.protocol() != ProtocolId::Phone || !self.source_media() {
            return Err(Error::Admission);
        }
        let clock = self.private_clock.as_mut().ok_or(Error::Admission)?;
        let now = clock.now()?;
        let mut state = Sink { dispatch, epoch: clock.epoch, now, failed: false };
        let sink = abi::rptadv_peer_source_sink_v1 {
            struct_size: size_of::<abi::rptadv_peer_source_sink_v1>() as u32,
            abi_version: 1, context: ptr::from_mut(&mut state).cast(), audio: None,
            gap: Some(gap), host_clock_epoch: clock.epoch, reference_now_ns: now,
            audio_timed: Some(timed), source_discontinuity: Some(discontinuity),
            digit: Some(digit), source_adaptation: Some(adaptation),
        };
        let mut result: abi::rptadv_peer_source_result_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = size_of::<abi::rptadv_peer_source_result_v1>() as u32;
        result.abi_version = 1;
        let status = unsafe { self.services.0.peer_read_source.unwrap()(self.services.context(),
            self.handle.as_ptr(), 0, &sink, None, ptr::null_mut(), &mut result) };
        if status != 0 || state.failed || result.struct_size < size_of::<abi::rptadv_peer_source_result_v1>() as u32
            || result.abi_version != 1 || result.progress > 3 || result.activity != 0 || result.finish_pending != 0
            || result.represented_node_number != 0 || result.state > 3 {
            return Err(Error::InvalidFrame);
        }
        Ok(result.state == 3)
    }
}

impl PeerIo {
    /// Host/provider work records the birth once and retains it through all retries.
    pub(crate) fn private_now(&mut self) -> Result<u64, Error> {
        self.private_clock.as_mut().ok_or(Error::Admission)?.now()
    }
    /// Convert an already selected host-clock expiry into the provider clock
    /// once. Sample the provider first so a scheduling pause can only shorten it.
    pub(crate) fn private_deadline(&mut self, expires: u64) -> Result<Option<u32>, Error> {
        let elapsed = self.elapsed_ms();
        let now = self.private_now()?;
        let remaining = expires.checked_sub(now).and_then(|ns| u32::try_from(ns / 1_000_000).ok());
        Ok(remaining.filter(|&ms| ms != 0).and_then(|ms| elapsed.checked_add(ms)))
    }
    pub(crate) fn send_private(&mut self, encoded: &[u8], frames: u32, timestamp: u32, deadline: u32)
        -> Result<PrivateSend, Error> {
        if self.private_clock.is_none() { return Err(Error::Admission); }
        let group = self.services.phone_provider()?;
        match unsafe { group.send_prepared_timed.unwrap()(self.services.context(), self.handle.as_ptr(),
            encoded.as_ptr(), encoded.len(), frames, timestamp, deadline) } {
            0 => Ok(PrivateSend::Accepted),
            1 => Ok(PrivateSend::WouldBlock),
            2 => Ok(PrivateSend::Expired),
            _ => Err(Error::Write),
        }
    }
}
pub(crate) enum PrivateSend { Accepted, WouldBlock, Expired }


#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::PhoneClock;

    unsafe extern "C" fn monotonic_ns() -> u64 {
        let mut timestamp = std::mem::MaybeUninit::<libc::timespec>::uninit();
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, timestamp.as_mut_ptr()) } != 0 {
            return 0;
        }
        let timestamp = unsafe { timestamp.assume_init() };
        u64::try_from(timestamp.tv_sec).unwrap() * 1_000_000_000
            + u64::try_from(timestamp.tv_nsec).unwrap()
    }

    #[test]
    fn native_source_reference_covers_packets_received_before_peer_adoption() {
        let received = unsafe { monotonic_ns() };
        assert_ne!(received, 0);
        let admitted = unsafe { monotonic_ns() };
        let mut clock = PhoneClock { epoch: 1, read: monotonic_ns, last: admitted };
        let reference = clock.now().expect("the actual native clock remains available");
        let after = unsafe { monotonic_ns() };
        assert!((admitted..=after).contains(&reference),
            "private source reference must remain in the original native monotonic domain");
        assert!(reference.checked_sub(received).is_some());
    }
}
