//! Receive-only progress retained by one originating frontend request and its SIP call.
use crate::abi;
use std::{
    collections::VecDeque,
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

// The Product allows the remaining 150 ms after poll entry, including conversion and playback.
const PRIVATE_RESIDENCE_NS: u64 = 50_000_000;
const MAX_FRAMES: usize = 9600; // 200 ms at the largest negotiated rate (48 kHz).

struct Block {
    born: u64,
    available: u64,
    pcm: Box<[f32]>,
    used: usize,
}

#[derive(Default)]
pub(crate) struct Progress {
    ringing: AtomicBool,
    original: AtomicBool,
    ended: AtomicBool,
    status: AtomicU32,
    rate: AtomicU32,
    frames: Mutex<VecDeque<Block>>,
}

impl Progress {
    pub(crate) fn observe(&self, ringing: bool, terminal: bool, status: u32) {
        if ringing {
            self.ringing.store(true, Ordering::Release);
        }
        if status != 0 {
            self.status.store(status, Ordering::Release);
        }
        if terminal {
            self.ended.store(true, Ordering::Release);
        }
    }

    pub(crate) fn sink(&self, rate: u32, now: u64) -> abi::rptadv_peer_source_sink_v1 {
        self.rate.store(rate, Ordering::Release);
        abi::rptadv_peer_source_sink_v1 {
            struct_size: size_of::<abi::rptadv_peer_source_sink_v1>() as u32,
            abi_version: 1,
            context: std::ptr::from_ref(self).cast_mut().cast(),
            host_clock_epoch: 1,
            reference_now_ns: now,
            audio_timed: Some(capture),
            gap: Some(gap),
            digit: Some(digit),
            source_discontinuity: Some(cut),
            ..unsafe { std::mem::zeroed() }
        }
    }

    fn admit(&self, born: u64, samples: &[f32]) {
        if samples.is_empty() || self.ended.load(Ordering::Acquire) {
            return;
        }
        let now = crate::frontend::monotonic_ns();
        if now == 0 || born > now || samples.len() > MAX_FRAMES {
            return;
        }
        self.original.store(true, Ordering::Release);
        let mut queue = match self.frames.try_lock() {
            Ok(q) => q,
            Err(_) => return,
        };
        let allowed = self.rate.load(Ordering::Acquire) as usize / 5;
        let mut held: usize = queue.iter().map(|b| b.pcm.len() - b.used).sum();
        while queue.front().is_some_and(|b| {
            now.saturating_sub(b.available) > PRIVATE_RESIDENCE_NS || held + samples.len() > allowed
        }) {
            if let Some(old) = queue.pop_front() {
                held -= old.pcm.len() - old.used;
            }
        }
        if samples.len() <= allowed {
            queue.push_back(Block {
                born,
                available: now,
                pcm: samples.into(),
                used: 0,
            });
        }
    }

    /// Only raw writes to the validated, separate caller-provided PCM output.
    pub(crate) unsafe fn copy(&self, pcm: *mut f32, capacity: usize) -> (u32, u32, usize, u32) {
        let status = self.status.load(Ordering::Acquire);
        if self.ended.load(Ordering::Acquire) {
            return (0, status, 0, 0);
        }
        let ring = u32::from(
            self.ringing.load(Ordering::Acquire) && !self.original.load(Ordering::Acquire),
        );
        let mut copied = 0;
        if capacity != 0
            && let Ok(mut q) = self.frames.try_lock()
        {
            let now = crate::frontend::monotonic_ns();
            while copied < capacity {
                let Some(block) = q.front_mut() else { break };
                if block.born <= block.available
                    && now >= block.available
                    && now - block.available <= PRIVATE_RESIDENCE_NS
                {
                    let count = (block.pcm.len() - block.used).min(capacity - copied);
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            block.pcm.as_ptr().add(block.used),
                            pcm.add(copied),
                            count,
                        )
                    };
                    copied += count;
                    block.used += count;
                    if block.used < block.pcm.len() {
                        break;
                    }
                }
                q.pop_front();
            }
        }
        if self.ended.load(Ordering::Acquire) {
            return (0, self.status.load(Ordering::Acquire), 0, 0);
        }
        (
            ring,
            status,
            copied,
            if copied == 0 {
                0
            } else {
                self.rate.load(Ordering::Acquire)
            },
        )
    }
}

unsafe extern "C" fn capture(
    ctx: *mut c_void,
    _: u64,
    pcm: *const f32,
    count: usize,
    concealed: u32,
    birth: *const abi::rptadv_source_birth_v1,
) -> usize {
    if concealed == 0 && !birth.is_null() && count != 0 {
        // This sink is used only by the selected serial Media owner during read_checked.
        let owner = unsafe { &*ctx.cast::<Progress>() };
        owner.admit(unsafe { (*birth).born_ns }, unsafe {
            std::slice::from_raw_parts(pcm, count)
        });
    }
    count
}
unsafe extern "C" fn digit(_: *mut c_void, _: u32) -> i32 {
    0
}
unsafe extern "C" fn gap(_: *mut c_void, _: u64, count: u32) -> u32 {
    count
}
unsafe extern "C" fn cut(_: *mut c_void) -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_original_keeps_source_birth_and_ages_from_decoded_availability() {
        let progress = Progress::default();
        let now = crate::frontend::monotonic_ns();
        let sink = progress.sink(8000, now);
        let born = now
            .checked_sub(300_000_000)
            .expect("300 ms configured jitter");
        let birth = abi::rptadv_source_birth_v1 {
            struct_size: size_of::<abi::rptadv_source_birth_v1>() as u32,
            abi_version: 1,
            host_clock_epoch: 1,
            born_ns: born,
        };
        let samples = [0.4, 0.2];
        unsafe { capture(sink.context, 0, samples.as_ptr(), samples.len(), 0, &birth) };
        assert_eq!(progress.frames.lock().unwrap().front().unwrap().born, born);
        let mut out = [0.0];
        assert_eq!(
            unsafe { progress.copy(out.as_mut_ptr(), 1) },
            (0, 0, 1, 8000)
        );
        assert_eq!(out, [0.4]);
        progress
            .frames
            .lock()
            .unwrap()
            .front_mut()
            .unwrap()
            .available = now - PRIVATE_RESIDENCE_NS - 1;
        assert_eq!(unsafe { progress.copy(out.as_mut_ptr(), 1) }, (0, 0, 0, 0));
    }

    #[test]
    fn only_original_sip_frames_replace_ring_and_terminal_cuts_stored_pcm() {
        let progress = Progress::default();
        let now = crate::frontend::monotonic_ns();
        let sink = progress.sink(8000, now);
        let birth = abi::rptadv_source_birth_v1 {
            struct_size: size_of::<abi::rptadv_source_birth_v1>() as u32,
            abi_version: 1,
            host_clock_epoch: 1,
            born_ns: now,
        };
        let mut output = [1.0; 4];
        let pcm = [0.0, 0.0, 0.5, -0.5];
        let take = |out: &mut [f32]| unsafe { progress.copy(out.as_mut_ptr(), out.len()) };
        assert_eq!(take(&mut output), (0, 0, 0, 0));
        progress.observe(true, false, 0);
        unsafe { capture(sink.context, 10, pcm.as_ptr(), pcm.len(), 1, &birth) };
        assert_eq!(take(&mut output), (1, 0, 0, 0));
        unsafe { capture(sink.context, 14, pcm.as_ptr(), pcm.len(), 0, &birth) };
        assert_eq!(take(&mut output[..2]), (0, 0, 2, 8000));
        assert_eq!(&output[..2], &[0.0, 0.0]);
        progress.observe(false, true, 486);
        assert_eq!(take(&mut output), (0, 486, 0, 0));
        progress.observe(false, true, 0);
        assert_eq!(take(&mut output), (0, 486, 0, 0));
    }
}
