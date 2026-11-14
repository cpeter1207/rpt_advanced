//! Original phone deadlines beside the existing converter/codec, never PCM.
//! Twenty-ms buckets cover the unchanged 200-ms egress retention allowance plus
//! boundary/filter overlap. Reused or missing metadata fails closed.
use super::{AGE_NS, BLOCK, BLOCK_NS};

const CAPACITY: usize = (AGE_NS / BLOCK_NS) as usize + 2;
#[derive(Clone, Copy)]
struct Bucket {
    index: u64,
    expires: u64,
}
impl Default for Bucket {
    fn default() -> Self {
        Self {
            index: u64::MAX,
            expires: u64::MAX,
        }
    }
}
pub(super) struct History {
    buckets: [Bucket; CAPACITY],
    phone: bool,
}
impl Default for History {
    fn default() -> Self {
        Self {
            buckets: [Bucket::default(); CAPACITY],
            phone: false,
        }
    }
}
impl History {
    pub(super) fn record(&mut self, start: u64, count: usize, expires: u64) {
        self.phone |= expires != u64::MAX;
        let Some(end) = start.checked_add(count as u64) else {
            self.buckets.fill(Bucket::default());
            return;
        };
        if start == end {
            return;
        }
        for index in start / BLOCK as u64..=(end - 1) / BLOCK as u64 {
            let bucket = &mut self.buckets[(index % CAPACITY as u64) as usize];
            if bucket.index == index {
                bucket.expires = bucket.expires.min(expires);
            } else {
                *bucket = Bucket { index, expires };
            }
        }
    }
    pub(super) fn packet(
        &self,
        origin_ns: u128,
        start: u64,
        frames: u64,
        rate: u32,
        admitted_start: u64,
        admitted_end: u64,
    ) -> u64 {
        if !self.phone || frames == 0 {
            return u64::MAX;
        }
        let Some(end) = start.checked_add(frames) else {
            return 0;
        };
        let position = |frame, upper| {
            origin_ns
                .checked_add(u128::from(frame) * 1_000_000_000 / u128::from(rate))
                .and_then(|time| time.checked_mul(48_000))
                .and_then(|time| {
                    u64::try_from(if upper {
                        time.div_ceil(1_000_000_000)
                    } else {
                        time / 1_000_000_000
                    })
                    .ok()
                })
        };
        let (Some(first), Some(last)) = (position(start, false), position(end, true)) else {
            return 0;
        };
        // Released adapter ABI2 create fixes SWR filter_size 16/cutoff 1. Its
        // conservative support bound is ceil(16/min(ratio,ratio*.985,1))+4:
        // at worst 102 native input frames for supported 8–48 kHz egress. Include
        // another codec-output frame for rounded reset/trim phase. Native
        // 48 kHz is the existing exact-copy path with no filter history.
        let support = if rate == 48_000 {
            0
        } else {
            102 + 48_000u64.div_ceil(u64::from(rate))
        };
        let first = first.saturating_sub(support).max(admitted_start);
        let last = last.saturating_add(support).min(admitted_end);
        if first >= last {
            return 0;
        }
        let first = first / BLOCK as u64;
        let last = (last - 1) / BLOCK as u64;
        if last - first >= CAPACITY as u64 {
            return 0;
        }
        let mut expires = u64::MAX;
        for index in first..=last {
            let bucket = self.buckets[(index % CAPACITY as u64) as usize];
            if bucket.index != index {
                return 0;
            }
            expires = expires.min(bucket.expires);
        }
        expires
    }
}
