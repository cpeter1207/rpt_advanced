//! Adaptive opaque-packet jitter capability; SpeexDSP stays behind this descriptor.
#![deny(warnings, missing_docs)]

use std::{
    ffi::c_void,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
};

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    missing_docs
)]
mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod native_calls;
#[cfg(not(test))]
use abi as native_calls;

/// Stable storage transferred to SpeexDSP; the native destroy callback only retires a slot.
struct Slot {
    active: bool,
    retired: bool,
    timestamp: u64,
    span: u32,
    payload: Box<[u8]>,
}

/// Packet-release clock, deliberately independent of the downstream PCM ring's reserve.
struct Timeline {
    origin_timestamp: u64,
    origin_ns: u64,
    cursor: u64,
    deadline_ns: u64,
    started: bool,
}

/// One fetched decision must be committed exactly once before another operation.
struct Pending {
    output_span: u32,
    next_timestamp: u64,
}

/// Exclusive non-audio owner of native state and bounded packet storage.
struct Context {
    native: NonNull<abi::JitterBuffer>,
    config: abi::rptadv_jitter_config_v1,
    slots: Box<[Slot]>,
    timeline: Option<Timeline>,
    pending: Option<Pending>,
    last_now: u64,
    downstream_span: u32,
    unsuccessful_gets: u32,
    deferred_insertion: bool,
    observation: abi::rptadv_jitter_observation_v1,
}

/// The allocation remains owned by Context; native removal never frees Rust memory.
unsafe extern "C" fn retire(data: *mut c_void) {
    // SAFETY: native data always points to one stable Slot in the live Context.
    let slot = unsafe { &mut *data.cast::<Slot>() };
    slot.active = false;
    slot.retired = true;
}

impl Context {
    fn create(config: abi::rptadv_jitter_config_v1) -> Option<Self> {
        if config.abi_version != 1
            || config.struct_size < size_of::<abi::rptadv_jitter_config_v1>() as u32
            || !(1..=48000).contains(&config.clock_rate_hz)
            || config.minimum_delay == 0
            || config.minimum_delay > config.initial_delay
            || config.initial_delay > config.maximum_delay
            || config.maximum_delay > 32767
            || config.adjustment_step == 0
            || config.adjustment_step > config.maximum_delay
            || config.concealment_span == 0
            || config.concealment_span > config.maximum_delay
            || !(1..=200).contains(&config.maximum_packets)
            || !(1..=65527).contains(&config.maximum_payload_bytes)
        {
            return None;
        }
        let native = Self::native(&config)?;
        Some(Self {
            native,
            config,
            slots: (0..=config.maximum_packets)
                .map(|_| Slot {
                    active: false,
                    retired: false,
                    timestamp: 0,
                    span: 0,
                    payload: Box::default(),
                })
                .collect(),
            timeline: None,
            pending: None,
            last_now: 0,
            downstream_span: 0,
            unsuccessful_gets: 0,
            deferred_insertion: false,
            // SAFETY: the neutral diagnostics structure contains only integers.
            observation: unsafe { std::mem::zeroed() },
        })
    }

    fn native(config: &abi::rptadv_jitter_config_v1) -> Option<NonNull<abi::JitterBuffer>> {
        // SAFETY: validated positive step fits the native signed argument.
        let native = NonNull::new(unsafe {
            native_calls::jitter_buffer_init(config.adjustment_step as i32)
        })?;
        // Every command is a documented SpeexDSP 1.x operation; no native opcode escapes.
        for (request, value) in [
            (abi::JITTER_BUFFER_SET_MARGIN, config.minimum_delay),
            (
                abi::JITTER_BUFFER_SET_CONCEALMENT_SIZE,
                config.concealment_span,
            ),
            (abi::JITTER_BUFFER_SET_LATE_COST, 0),
        ] {
            let mut value = value as i32;
            unsafe {
                abi::jitter_buffer_ctl(
                    native.as_ptr(),
                    request as i32,
                    ptr::from_mut(&mut value).cast(),
                )
            };
        }
        unsafe {
            abi::jitter_buffer_ctl(
                native.as_ptr(),
                abi::JITTER_BUFFER_SET_DESTROY_CALLBACK as i32,
                retire as *const () as *mut c_void,
            )
        };
        // Empty-state update disables implicit updates in remaining_span/tick.
        unsafe {
            abi::jitter_buffer_update_delay(native.as_ptr(), ptr::null_mut(), ptr::null_mut())
        };
        Some(native)
    }

    fn reset(&mut self) -> bool {
        // SpeexDSP 1.2.1 reset does not clear deferred insertion; recreate the native state.
        let Some(native) = Self::native(&self.config) else {
            return false;
        };
        let old = std::mem::replace(&mut self.native, native);
        unsafe { abi::jitter_buffer_destroy(old.as_ptr()) };
        for slot in &mut self.slots {
            slot.active = false;
            slot.retired = false;
            slot.payload = Box::default();
        }
        self.timeline = None;
        self.pending = None;
        self.last_now = 0;
        self.downstream_span = 0;
        self.unsuccessful_gets = 0;
        self.deferred_insertion = false;
        self.observation = unsafe { std::mem::zeroed() };
        true
    }

    fn overload(&mut self) -> bool {
        let mut observation = self.observation;
        observation.limit_resets = observation.limit_resets.saturating_add(1);
        for slot in self.slots.iter().filter(|slot| slot.active) {
            observation.overload_drops = observation.overload_drops.saturating_add(1);
            observation.overload_span = observation
                .overload_span
                .saturating_add(u64::from(slot.span));
        }
        if !self.reset() {
            return false;
        }
        self.observation = observation;
        true
    }

    fn units_ns(&self, units: u32) -> u64 {
        u64::from(units) * 1_000_000_000 / u64::from(self.config.clock_rate_hz)
    }

    fn ideal_ns(&self, timestamp: u64) -> i128 {
        let timeline = self.timeline.as_ref().expect("established timeline");
        i128::from(timeline.origin_ns)
            + (i128::from(timestamp) - i128::from(timeline.origin_timestamp)) * 1_000_000_000
                / i128::from(self.config.clock_rate_hz)
    }

    fn deadline(&self) -> u64 {
        self.timeline
            .as_ref()
            .unwrap()
            .deadline_ns
            .saturating_add(self.units_ns(self.downstream_span))
    }

    fn collect_retired(&mut self) {
        for slot in &mut self.slots {
            if std::mem::take(&mut slot.retired) {
                slot.payload = Box::default();
                self.observation.late_drops = self.observation.late_drops.saturating_add(1);
            }
        }
    }

    fn put(&mut self, packet: &abi::rptadv_jitter_packet_v1, payload: &[u8]) -> i32 {
        if self.pending.is_some()
            || packet.span == 0
            || packet.span > i32::MAX as u32 - self.config.maximum_delay
            || payload.is_empty()
            || payload.len() > self.config.maximum_payload_bytes as usize
            || packet
                .timestamp
                .checked_add(u64::from(packet.span))
                .is_none()
        {
            return -1;
        }
        // Match native resynchronization on receive after more than twenty failed fetches.
        if self.unsuccessful_gets > 20 {
            let mut observation = self.observation;
            observation.late_drops = observation
                .late_drops
                .saturating_add(self.slots.iter().filter(|slot| slot.active).count() as u64);
            if !self.reset() {
                return -1;
            }
            self.observation = observation;
        }
        let end = packet.timestamp + u64::from(packet.span);
        if self.timeline.is_none() {
            let Some(deadline_ns) = packet
                .arrival_ns
                .checked_add(self.units_ns(self.config.initial_delay))
            else {
                return -1;
            };
            self.timeline = Some(Timeline {
                origin_timestamp: packet.timestamp,
                origin_ns: packet.arrival_ns,
                cursor: packet.timestamp,
                deadline_ns,
                started: false,
            });
        }
        let timeline = self.timeline.as_ref().unwrap();
        if (i128::from(packet.timestamp) - i128::from(timeline.cursor)).abs() > i128::from(i32::MAX)
        {
            return -1;
        }
        if self.slots.iter().any(|slot| {
            slot.active && slot.timestamp == packet.timestamp && slot.span == packet.span
        }) {
            self.observation.duplicate_drops = self.observation.duplicate_drops.saturating_add(1);
            return abi::RPTADV_JITTER_DUPLICATE as i32;
        }
        let useful = !timeline.started
            || end.saturating_add(u64::from(self.config.adjustment_step)) >= timeline.cursor;
        let occupied = self
            .slots
            .iter()
            .filter(|slot| {
                slot.active
                    && (!timeline.started
                        || slot.timestamp + u64::from(slot.span) > timeline.cursor)
            })
            .count();
        if occupied >= self.config.maximum_packets as usize {
            self.observation.full_drops = self.observation.full_drops.saturating_add(1);
            return abi::RPTADV_JITTER_FULL as i32;
        }
        if timeline.started {
            let offset = (i128::from(packet.arrival_ns) - i128::from(timeline.deadline_ns))
                * i128::from(self.config.clock_rate_hz)
                / 1_000_000_000
                - i128::from(self.downstream_span);
            let at_arrival = (i128::from(timeline.cursor) + offset) as u32;
            let native_cursor =
                unsafe { abi::jitter_buffer_get_pointer_timestamp(self.native.as_ptr()) } as u32;
            // Reconstruct receive time even when the async task handles several old arrivals together.
            unsafe {
                abi::jitter_buffer_remaining_span(
                    self.native.as_ptr(),
                    native_cursor.wrapping_sub(at_arrival),
                )
            };
        }
        // Native get/update may retire a slot between admissions. Reclaim it
        // before reuse so an old retirement cannot clear the new payload.
        self.collect_retired();
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| !slot.active)
            .expect("one spare native insertion slot");
        slot.timestamp = packet.timestamp;
        slot.span = packet.span;
        slot.payload = Box::from(payload);
        slot.active = useful;
        let raw = abi::JitterBufferPacket {
            data: ptr::from_mut(slot).cast(),
            len: payload.len() as u32,
            timestamp: packet.timestamp as u32,
            span: packet.span,
            sequence: 0,
            user_data: 0,
        };
        unsafe { abi::jitter_buffer_put(self.native.as_ptr(), &raw) };
        self.collect_retired();
        if useful {
            0
        } else {
            self.observation.late_drops = self.observation.late_drops.saturating_add(1);
            abi::RPTADV_JITTER_LATE as i32
        }
    }

    fn get(
        &mut self,
        now: u64,
        desired: u32,
        payload: &mut [u8],
        result: &mut abi::rptadv_jitter_result_v1,
    ) -> i32 {
        if self.pending.is_some()
            || now < self.last_now
            || desired == 0
            || desired > self.config.maximum_delay
            || desired % self.config.concealment_span != 0
            || payload.len() < self.config.maximum_payload_bytes as usize
        {
            return -1;
        }
        self.last_now = now;
        *result = abi::rptadv_jitter_result_v1 {
            struct_size: size_of::<abi::rptadv_jitter_result_v1>() as u32,
            kind: abi::RPTADV_JITTER_NOT_DUE,
            timestamp: 0,
            span: 0,
            start_offset: 0,
            length: 0,
            deadline_ns: 0,
        };
        let Some(timeline) = self.timeline.as_ref() else {
            return 0;
        };
        result.deadline_ns = self.deadline();
        if now < result.deadline_ns {
            return 0;
        }
        if !timeline.started {
            let first = self
                .slots
                .iter()
                .filter(|slot| slot.active)
                .map(|slot| slot.timestamp)
                .min()
                .unwrap();
            self.timeline.as_mut().unwrap().cursor = first;
        }
        let cursor = self.timeline.as_ref().unwrap().cursor;
        let delay = i128::from(self.deadline()) - self.ideal_ns(cursor);
        if delay > i128::from(self.units_ns(self.config.maximum_delay)) {
            if !self.overload() {
                return -1;
            }
            result.deadline_ns = 0;
            return 0;
        }
        // The public API warns if get returns less than its desired native span. Keep
        // that quantum within both concealment and every eligible packet's remaining
        // extent. Native selection, lateness statistics and adaptation stay in Speex.
        let quantum = self
            .slots
            .iter()
            .filter(|slot| slot.active)
            .map(|slot| slot.timestamp + u64::from(slot.span))
            .filter(|end| *end > cursor)
            .map(|end| (end - cursor).min(u64::from(u32::MAX)) as u32)
            .fold(desired.min(self.config.concealment_span), u32::min);
        let mut native: abi::JitterBufferPacket = unsafe { std::mem::zeroed() };
        let mut offset = 0;
        let kind = unsafe {
            native_calls::jitter_buffer_get(
                self.native.as_ptr(),
                &mut native,
                quantum as i32,
                &mut offset,
            )
        };
        if !(0..=2).contains(&kind) || native.span == 0 {
            return -1;
        }
        let deferred = std::mem::take(&mut self.deferred_insertion);
        if kind == abi::JITTER_BUFFER_INSERTION as i32
            && delay + i128::from(self.units_ns(native.span))
                > i128::from(self.units_ns(self.config.maximum_delay))
        {
            if !self.overload() {
                return -1;
            }
            result.deadline_ns = 0;
            return 0;
        }
        result.kind = kind as u32;
        result.timestamp = cursor;
        result.span = native.span;
        let (output_span, next_timestamp) = if kind == abi::JITTER_BUFFER_OK as i32 {
            // With the native destroy callback, get transfers our exact slot pointer back.
            let slot = unsafe { &mut *native.data.cast::<Slot>() };
            result.timestamp = slot.timestamp;
            result.start_offset = (i128::from(slot.timestamp) - i128::from(cursor)) as i32;
            result.length = slot.payload.len();
            payload[..result.length].copy_from_slice(&slot.payload);
            slot.payload = Box::default();
            slot.active = false;
            self.unsuccessful_gets = 0;
            (
                (i64::from(slot.span) + i64::from(result.start_offset)) as u32,
                slot.timestamp + u64::from(slot.span),
            )
        } else if kind == abi::JITTER_BUFFER_MISSING as i32 {
            self.unsuccessful_gets = self.unsuccessful_gets.saturating_add(1);
            self.observation.missing_span = self
                .observation
                .missing_span
                .saturating_add(u64::from(native.span));
            (native.span, cursor.saturating_add(u64::from(native.span)))
        } else {
            if !deferred {
                self.unsuccessful_gets = self.unsuccessful_gets.saturating_add(1);
            }
            self.observation.insertion_span = self
                .observation
                .insertion_span
                .saturating_add(u64::from(native.span));
            (native.span, cursor)
        };
        self.timeline.as_mut().unwrap().started = true;
        self.pending = Some(Pending {
            output_span,
            next_timestamp,
        });
        0
    }

    fn advance(&mut self, now: u64, downstream_span: u32) -> i32 {
        if now < self.last_now
            || downstream_span > i32::MAX as u32
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| downstream_span > pending.output_span + self.downstream_span)
        {
            return -1;
        }
        let Some(pending) = self.pending.take() else {
            return -1;
        };
        self.last_now = now;
        // Previously retained lookahead is released before fetching the next decision;
        // count it once, and withhold only the newly reported tail.
        let released = pending.output_span + self.downstream_span - downstream_span;
        self.downstream_span = downstream_span;
        let duration = self.units_ns(released);
        let timeline = self.timeline.as_mut().unwrap();
        timeline.cursor = pending.next_timestamp;
        timeline.deadline_ns = timeline.deadline_ns.saturating_add(duration);
        let adjustment = unsafe {
            abi::jitter_buffer_update_delay(self.native.as_ptr(), ptr::null_mut(), ptr::null_mut())
        };
        self.deferred_insertion = adjustment < 0;
        if adjustment > 0 {
            timeline.cursor = timeline.cursor.saturating_add(adjustment as u64);
            self.observation.skipped_span = self
                .observation
                .skipped_span
                .saturating_add(adjustment as u64);
        }
        let minimum = (self.ideal_ns(self.timeline.as_ref().unwrap().cursor)
            + i128::from(self.units_ns(self.config.minimum_delay))
            - i128::from(self.units_ns(downstream_span)))
        .clamp(0, i128::from(u64::MAX)) as u64;
        let timeline = self.timeline.as_mut().unwrap();
        timeline.deadline_ns = timeline.deadline_ns.max(minimum);
        0
    }

    fn observe(&self, output: &mut abi::rptadv_jitter_observation_v1) {
        *output = self.observation;
        output.struct_size = size_of::<abi::rptadv_jitter_observation_v1>() as u32;
        output.queued_packets = self.slots.iter().filter(|slot| slot.active).count() as u32;
        output.buffered_span = self
            .slots
            .iter()
            .filter(|slot| slot.active)
            .map(|slot| u64::from(slot.span))
            .sum();
        output.delay = self
            .timeline
            .as_ref()
            .map_or(self.config.initial_delay, |timeline| {
                ((i128::from(self.deadline()) - self.ideal_ns(timeline.cursor))
                    * i128::from(self.config.clock_rate_hz)
                    / 1_000_000_000)
                    .clamp(0, i128::from(u32::MAX)) as u32
            });
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // Native destruction retires slots before Rust releases their backing allocation.
        unsafe { abi::jitter_buffer_destroy(self.native.as_ptr()) };
    }
}

fn boundary(operation: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(-1)
}

unsafe extern "C" fn create(
    config: *const abi::rptadv_jitter_config_v1,
    output: *mut *mut c_void,
) -> i32 {
    boundary(|| {
        let Some(output) = (unsafe { output.as_mut() }) else {
            return -1;
        };
        *output = ptr::null_mut();
        let Some(config) = (unsafe { config.as_ref() }) else {
            return -1;
        };
        let Some(context) = Context::create(*config) else {
            return -1;
        };
        *output = Box::into_raw(Box::new(context)).cast();
        0
    })
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    if !context.is_null() {
        unsafe { drop(Box::from_raw(context.cast::<Context>())) };
    }
}

unsafe extern "C" fn reset(context: *mut c_void) -> i32 {
    boundary(|| {
        let Some(context) = (unsafe { context.cast::<Context>().as_mut() }) else {
            return -1;
        };
        if context.reset() { 0 } else { -1 }
    })
}

unsafe extern "C" fn put(context: *mut c_void, packet: *const abi::rptadv_jitter_packet_v1) -> i32 {
    boundary(|| {
        let (Some(context), Some(packet)) =
            (unsafe { context.cast::<Context>().as_mut() }, unsafe {
                packet.as_ref()
            })
        else {
            return -1;
        };
        if packet.struct_size < size_of::<abi::rptadv_jitter_packet_v1>() as u32
            || packet.payload.is_null()
            || packet.length > isize::MAX as usize
        {
            return -1;
        }
        context.put(packet, unsafe {
            std::slice::from_raw_parts(packet.payload, packet.length)
        })
    })
}

unsafe extern "C" fn get(
    context: *mut c_void,
    now: u64,
    desired: u32,
    payload: *mut u8,
    capacity: usize,
    result: *mut abi::rptadv_jitter_result_v1,
) -> i32 {
    boundary(|| {
        let (Some(context), Some(result)) =
            (unsafe { context.cast::<Context>().as_mut() }, unsafe {
                result.as_mut()
            })
        else {
            return -1;
        };
        if result.struct_size < size_of::<abi::rptadv_jitter_result_v1>() as u32
            || payload.is_null()
            || capacity > isize::MAX as usize
        {
            return -1;
        }
        context.get(
            now,
            desired,
            unsafe { std::slice::from_raw_parts_mut(payload, capacity) },
            result,
        )
    })
}

unsafe extern "C" fn advance(context: *mut c_void, now: u64, downstream_span: u32) -> i32 {
    boundary(|| {
        unsafe { context.cast::<Context>().as_mut() }
            .map_or(-1, |context| context.advance(now, downstream_span))
    })
}

unsafe extern "C" fn observe(
    context: *const c_void,
    result: *mut abi::rptadv_jitter_observation_v1,
) -> i32 {
    boundary(|| {
        let (Some(context), Some(result)) =
            (unsafe { context.cast::<Context>().as_ref() }, unsafe {
                result.as_mut()
            })
        else {
            return -1;
        };
        if result.struct_size < size_of::<abi::rptadv_jitter_observation_v1>() as u32 {
            return -1;
        }
        context.observe(result);
        0
    })
}

static DESCRIPTOR: abi::rptadv_jitter_descriptor_v1 = abi::rptadv_jitter_descriptor_v1 {
    struct_size: size_of::<abi::rptadv_jitter_descriptor_v1>() as u32,
    abi_version: 1,
    capability: *b"rptadv.jitter\0\0\0",
    create: Some(create),
    destroy: Some(destroy),
    reset: Some(reset),
    put: Some(put),
    get: Some(get),
    advance: Some(advance),
    observe: Some(observe),
};

/// Return the immutable jitter ABI 1 descriptor while this shared object is loaded.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_jitter_adapter_descriptor_v1() -> *const abi::rptadv_jitter_descriptor_v1 {
    &DESCRIPTOR
}

#[cfg(test)]
mod tests;
