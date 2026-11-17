//! One immutable media origin, retained code scope, and per-serial-reader history.
use super::{HostServices, SourceTimeReference, abi};
use std::{
    mem::size_of,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

static NEXT_SOURCE_CLOCK: AtomicU64 = AtomicU64::new(1);

enum Axis {
    Legacy(Instant),
    Provider {
        kind: u32,
        read: Option<unsafe extern "C" fn() -> u64>,
        origin: u64,
    },
}
struct Origin {
    identity: u64,
    axis: Axis,
    qualified: AtomicBool,
}

/// Prepared off audio. Clones retain the same origin but have independent serial histories.
/// Provider code remains retained by the enclosing HostServices lifecycle until all users stop.
pub(crate) struct SourceTimeClock {
    origin: Arc<Origin>,
    last: AtomicU64,
}
impl Clone for SourceTimeClock {
    fn clone(&self) -> Self {
        Self {
            origin: Arc::clone(&self.origin),
            last: AtomicU64::new(self.last.load(Ordering::Relaxed)),
        }
    }
}
impl SourceTimeClock {
    fn create(axis: Axis, qualified: bool) -> Option<Self> {
        let identity = NEXT_SOURCE_CLOCK
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .ok()?;
        let last = match axis {
            Axis::Provider { origin, .. } => origin,
            Axis::Legacy(_) => 0,
        };
        Some(Self {
            origin: Arc::new(Origin {
                identity,
                axis,
                qualified: AtomicBool::new(qualified),
            }),
            last: AtomicU64::new(last),
        })
    }
    /// Legacy ordinary media remains on its original Instant axis; it has no raw-clock join.
    pub(crate) fn new(origin: Instant) -> Option<Self> {
        Self::create(Axis::Legacy(origin), true)
    }
    /// Query exactly once off audio. A malformed/present getter never selects legacy fallback.
    pub(crate) fn prepare(services: HostServices, legacy: Instant) -> Option<Self> {
        let Some(getter) = services.0.media_clock_reader else {
            return Self::new(legacy);
        };
        let mut record: abi::rptadv_media_clock_reader_v1 = unsafe { std::mem::zeroed() };
        record.struct_size = size_of::<abi::rptadv_media_clock_reader_v1>() as u32;
        record.abi_version = 1;
        let valid = unsafe { getter(services.0.context, &mut record) } == 0
            && record.struct_size as usize >= size_of::<abi::rptadv_media_clock_reader_v1>()
            && record.abi_version == 1
            && record.clock_kind == 1
            && record.reserved == 0;
        let read = valid.then_some(record.read_ns).flatten();
        let origin = read.map_or(0, |read| unsafe { read() });
        Self::create(
            Axis::Provider {
                kind: 1,
                read,
                origin,
            },
            origin != 0,
        )
    }
    /// Shared sticky qualification, safe to inspect on audio without reading the OS clock.
    pub(crate) fn available(&self) -> bool {
        self.origin.qualified.load(Ordering::Acquire)
    }
    pub(crate) fn identity(&self) -> u64 {
        if self.available() {
            self.origin.identity
        } else {
            0
        }
    }
    /// Invalidate before quiescence/replacement. Neither reload nor a radio restart resets this.
    pub(crate) fn invalidate(&self) {
        self.origin.qualified.store(false, Ordering::Release);
    }
    /// Serial owner only. No global watermark: unrelated readers may finish out of order.
    pub(crate) fn reference(&self) -> SourceTimeReference {
        if !self.available() {
            return SourceTimeReference::default();
        }
        let now = match self.origin.axis {
            Axis::Legacy(origin) => Instant::now()
                .checked_duration_since(origin)
                .and_then(|elapsed| u64::try_from(elapsed.as_nanos()).ok()),
            Axis::Provider { kind, read, origin } => {
                let raw = read.map_or(0, |read| unsafe { read() });
                if kind != 1 || raw == 0 || raw < self.last.load(Ordering::Relaxed) {
                    None
                } else {
                    self.last.store(raw, Ordering::Relaxed);
                    raw.checked_sub(origin)
                }
            }
        };
        let Some(now_ns) = now else {
            self.invalidate();
            return SourceTimeReference::default();
        };
        if !self.available() {
            return SourceTimeReference::default();
        }
        SourceTimeReference {
            host_clock_epoch: self.origin.identity,
            now_ns,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;
    thread_local! {
        static RAW: Cell<u64> = const { Cell::new(0) };
        static MODE: Cell<u32> = const { Cell::new(0) };
        static CALLS: Cell<usize> = const { Cell::new(0) };
    }
    pub(crate) fn set(raw: u64) {
        RAW.set(raw);
    }
    unsafe extern "C" fn read() -> u64 {
        CALLS.set(CALLS.get() + 1);
        RAW.get()
    }
    unsafe extern "C" fn get(
        _: *mut std::ffi::c_void,
        out: *mut abi::rptadv_media_clock_reader_v1,
    ) -> i32 {
        let out = unsafe { &mut *out };
        out.clock_kind = 1;
        out.read_ns = Some(read);
        match MODE.get() {
            1 => return -1,
            2 => out.struct_size = 23,
            3 => out.abi_version = 2,
            4 => out.clock_kind = 2,
            5 => out.reserved = 1,
            6 => out.read_ns = None,
            7 => {
                out.clock_kind = 0;
                out.reserved = 0;
                out.read_ns = None;
                return -1;
            }
            _ => {}
        }
        0
    }
    pub(crate) fn clock(raw: u64) -> SourceTimeClock {
        MODE.set(0);
        set(raw);
        let mut table = unsafe { crate::fixture::host_descriptor().read() };
        table.media_clock_reader = Some(get);
        let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
        SourceTimeClock::prepare(services, Instant::now() - std::time::Duration::from_secs(9))
            .unwrap()
    }
    #[test]
    fn stage_c_clock_origin_and_fault_serial_history_and_shared_sticky_epoch() {
        let first = clock(1_000_000_000_000);
        let second = first.clone();
        set(1_000_400_000_000);
        let reference = first.reference();
        assert_eq!(reference.now_ns, 400_000_000);
        assert_eq!(
            first.reference().now_ns,
            reference.now_ns,
            "equality is valid"
        );
        set(1_000_300_000_000);
        assert_eq!(
            second.reference().now_ns,
            300_000_000,
            "independent serial completion order"
        );
        assert_eq!(
            first.reference().host_clock_epoch,
            0,
            "own regression invalidates"
        );
        set(1_000_500_000_000);
        assert_eq!(second.reference().host_clock_epoch, 0);
        assert_eq!(
            first.clone().reference().host_clock_epoch,
            0,
            "a new reader cannot reopen"
        );
        let replacement = clock(1_000_500_000_000);
        assert_ne!(replacement.identity(), reference.host_clock_epoch);
        set(0);
        assert_eq!(replacement.reference().host_clock_epoch, 0);
        set(u64::MAX);
        assert_eq!(replacement.reference().host_clock_epoch, 0);
        let maximum = clock(u64::MAX);
        assert_eq!(
            maximum.reference().now_ns,
            0,
            "raw maximum is representable at its origin"
        );
    }
    #[test]
    fn stage_c_clock_origin_and_fault_malformed_getter_never_samples_or_falls_back() {
        for mode in 1..=7 {
            let _valid = clock(1_000_000_000_000);
            MODE.set(mode);
            CALLS.set(0);
            let mut table = unsafe { crate::fixture::host_descriptor().read() };
            table.media_clock_reader = Some(get);
            let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
            let bad = SourceTimeClock::prepare(services, Instant::now()).unwrap();
            assert_eq!(bad.reference().host_clock_epoch, 0, "mode {mode}");
            assert_eq!(CALLS.get(), 0);
            assert!(matches!(bad.origin.axis, Axis::Provider { .. }));
        }
        let initial_zero = clock(0);
        assert_eq!(initial_zero.reference().host_clock_epoch, 0);
        assert!(matches!(initial_zero.origin.axis, Axis::Provider { .. }));
        set(1_000_000_000_000);
        assert_eq!(
            initial_zero.reference().host_clock_epoch,
            0,
            "initial failure cannot reopen"
        );
        let legacy = SourceTimeClock::new(Instant::now()).unwrap();
        assert!(
            matches!(legacy.origin.axis, Axis::Legacy(_)),
            "legacy never fabricates a raw-clock join"
        );
        assert_ne!(legacy.reference().host_clock_epoch, 0);
    }
}
