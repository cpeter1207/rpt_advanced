//! One pending dial's local-only station endpoint, independent of admitted phone peers.
use super::{MAXIMUM_FRAMES, RuntimeError};
use crate::lifecycle::{Engine, MediaAdmission};
use crate::link::ring::{InboundConsumer, InboundPolicy, InboundProducer, InboundRing};
use rpt_advanced_core::{audio::ToneSequence, runtime::dtmf::ActionPermit};
use std::{
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

// The private producer keeps each decoded prefix for at most 50 ms before this poll.
const PRESENTATION_LIMIT: Duration = Duration::from_millis(150);
const RING: u32 = 1;
const ORIGINAL: u32 = 2;
const ENDED: u32 = 3;

pub(super) struct Registry {
    ring: Option<&'static [f32]>,
    attempts: Arc<Mutex<std::collections::BTreeMap<u64, Weak<State>>>>,
    dirty: Arc<AtomicBool>,
}

impl Registry {
    pub(super) fn new(phone: bool) -> Result<Self, RuntimeError> {
        static WAVE: OnceLock<Option<Box<[f32]>>> = OnceLock::new();
        let ring = if phone {
            Some(
                WAVE.get_or_init(|| {
                    // Both component periods divide this 50 ms tile exactly.
                    let mut tone = ToneSequence::new("440+480/50", -16).ok()?;
                    let mut tile = vec![0.0; 2400].into_boxed_slice();
                    (tone.render(&mut tile) == tile.len()).then_some(tile)
                })
                .as_deref()
                .ok_or(RuntimeError::Preparation)?,
            )
        } else {
            None
        };
        Ok(Self {
            ring,
            attempts: Default::default(),
            dirty: Default::default(),
        })
    }

    pub(super) fn begin(
        &self,
        permit: ActionPermit,
        engine: &Engine,
        revision: u64,
        confirmed: Arc<AtomicBool>,
    ) -> Result<Arc<State>, RuntimeError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| RuntimeError::Busy)?;
        let state = Arc::new(State {
            id,
            attempts: Arc::clone(&self.attempts),
            dirty: Arc::clone(&self.dirty),
            permit,
            confirmed,
            admission: engine.media_admission(revision),
            clock: Instant::now(),
            base_ms: engine.elapsed_ms(),
            mode: AtomicU32::new(0),
            rate: AtomicU32::new(0),
            installed_rate: AtomicU32::new(u32::MAX),
            reader: AtomicU64::new(0),
            start: OnceLock::new(),
            input: Mutex::default(),
        });
        let mut slots = self
            .attempts
            .lock()
            .map_err(|_| RuntimeError::Preparation)?;
        slots.insert(id, Arc::downgrade(&state));
        Ok(state)
    }

    pub(super) fn needs_cleanup(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }
    pub(super) fn cleaned(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    pub(super) fn prepare(&self) -> Result<(Vec<Reader>, Vec<Redirect>), RuntimeError> {
        let Some(wave) = self.ring else {
            return Ok(Default::default());
        };
        let slots = self
            .attempts
            .lock()
            .map_err(|_| RuntimeError::Preparation)?;
        let mut readers = Vec::new();
        let mut redirects = Vec::new();
        for state in slots.values().filter_map(Weak::upgrade) {
            if !matches!(state.mode.load(Ordering::Acquire), RING | ORIGINAL) || !state.current() {
                continue;
            }
            let rate = state.rate.load(Ordering::Acquire);
            let (writer, input) = if rate == 0 {
                (None, None)
            } else {
                let (writer, input) = InboundRing::open_receiver(rate, InboundPolicy::Media)
                    .map_err(|_| RuntimeError::Preparation)?;
                let (stamps, incoming) = rtrb::RingBuffer::new(32);
                (
                    Some(Writer {
                        pcm: writer,
                        stamps,
                        last: None,
                        frame: 0,
                    }),
                    Some(TimedInput {
                        pcm: input,
                        incoming,
                        stamp: None,
                    }),
                )
            };
            // A candidate is unusable until activate_dispatchers publishes its unique token.
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let token = NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
                .map_err(|_| RuntimeError::Preparation)?;
            readers.push(Reader {
                state: Arc::clone(&state),
                token,
                wave,
                input,
                scratch: vec![0.; MAXIMUM_FRAMES].into_boxed_slice(),
                ring_frame: None,
            });
            redirects.push(Redirect {
                state,
                token,
                rate,
                writer,
            });
        }
        Ok((readers, redirects))
    }
}

// One native conversion generation. The SPSC queue maps accepted source positions to
// the poll that copied them; this age never restarts when a reader is published.
struct Writer {
    pcm: InboundProducer,
    stamps: rtrb::Producer<(u64, Instant)>,
    last: Option<(u64, Instant)>,
    frame: u64,
}

impl Writer {
    fn write(&mut self, samples: &[f32], polled_at: Instant) -> Result<usize, ()> {
        // Publish before PCM. A full PCM ring can refuse many polls at the
        // same frame. Keep the first (oldest) stamp until PCM advances.
        if self
            .last
            .is_none_or(|(frame, stamp)| frame != self.frame && stamp != polled_at)
        {
            if self.stamps.push((self.frame, polled_at)).is_err() {
                return Ok(0);
            }
            self.last = Some((self.frame, polled_at));
        }
        let count = self
            .pcm
            .write_tagged(samples, 1, self.frame, &[])
            .map_err(|_| ())?;
        self.frame += count as u64;
        Ok(count)
    }
}

struct TimedInput {
    pcm: InboundConsumer,
    incoming: rtrb::Consumer<(u64, Instant)>,
    stamp: Option<Instant>,
}

impl TimedInput {
    fn render(&mut self, output: &mut [f32]) -> bool {
        // Discard a stale/unlocated prefix. Native source positions are FIFO within this
        // generation. Once the oldest real sample is fresh, later originals cannot be older.
        for i in 0..output.len() {
            output[i] = 0.0;
            let Ok(sample) = self.pcm.render_timed_sample() else {
                return false;
            };
            let Some(provenance) = sample.provenance else {
                continue;
            };
            let Some(pos) = provenance.position else {
                continue;
            };
            if !provenance.real
                || provenance.plc
                || provenance.muted
                || provenance.zero_extension
                || provenance.gap
                || pos.negative
                || pos.generation != 1
            {
                continue;
            }
            while self
                .incoming
                .peek()
                .is_ok_and(|(start, _)| *start <= pos.frame)
            {
                self.stamp = self.incoming.pop().ok().map(|(_, at)| at);
            }
            let Some(at) = self.stamp else { continue };
            if at.elapsed() > PRESENTATION_LIMIT {
                continue;
            }
            output[i] = sample.sample;
            if i + 1 < output.len() && self.pcm.render(&mut output[i + 1..]).is_err() {
                return false;
            }
            return at.elapsed() <= PRESENTATION_LIMIT;
        }
        false
    }
}

pub(super) struct State {
    id: u64,
    attempts: Arc<Mutex<std::collections::BTreeMap<u64, Weak<State>>>>,
    dirty: Arc<AtomicBool>,
    permit: ActionPermit,
    admission: MediaAdmission,
    confirmed: Arc<AtomicBool>,
    clock: Instant,
    base_ms: u64,
    mode: AtomicU32,
    rate: AtomicU32,
    installed_rate: AtomicU32,
    reader: AtomicU64,
    start: OnceLock<Instant>,
    // Only serialized control-side writers take this lock.
    input: Mutex<Input>,
}
struct Samples {
    at: Instant,
    pcm: Box<[f32]>,
    used: usize,
}

#[derive(Default)]
struct Input {
    writer: Option<Writer>,
    last_input: Option<Instant>,
    waiting: std::collections::VecDeque<Samples>,
    held: usize,
}

impl Input {
    fn remove_front(&mut self) {
        if let Some(block) = self.waiting.pop_front() {
            self.held -= block.pcm.len() - block.used;
        }
    }

    fn append(&mut self, pcm: &[f32], rate: u32, polled_at: Instant) {
        let limit = rate as usize / 5;
        if pcm.is_empty() || pcm.len() > limit || polled_at.elapsed() > PRESENTATION_LIMIT {
            return;
        }
        while self.held + pcm.len() > limit {
            self.remove_front();
        }
        self.waiting.push_back(Samples {
            at: polled_at,
            pcm: pcm.into(),
            used: 0,
        });
        self.held += pcm.len();
    }

    fn pump(&mut self) {
        while self
            .waiting
            .front()
            .is_some_and(|b| b.at.elapsed() > PRESENTATION_LIMIT)
        {
            self.remove_front();
        }
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        // Native writes can accept a prefix. Give each poll a bounded amount of work.
        for _ in 0..10 {
            let Some(block) = self.waiting.front_mut() else {
                break;
            };
            let frames = &block.pcm[block.used..];
            let Ok(count) = writer.write(frames, block.at) else {
                break;
            };
            if count == 0 || count > frames.len() {
                break;
            }
            block.used += count;
            self.held -= count;
            if block.used == block.pcm.len() {
                self.waiting.pop_front();
            }
        }
    }
}

impl State {
    /// Boot-scoped opaque identity of the accepted outgoing attempt.
    pub(super) fn id(&self) -> u64 {
        self.id
    }

    fn current(&self) -> bool {
        let now = self
            .base_ms
            .saturating_add(self.clock.elapsed().as_millis() as u64);
        self.admission.current()
            && self.confirmed.load(Ordering::Acquire)
            && self.permit.current(Some(now))
            && self.clock.elapsed() < Duration::from_secs(35)
    }
    pub(super) fn end(&self) {
        if self.mode.swap(ENDED, Ordering::AcqRel) == ENDED {
            return;
        }
        self.attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        if self.reader.load(Ordering::Acquire) != 0 {
            self.dirty.store(true, Ordering::Release);
        }
    }
    pub(super) fn update(&self, ring: bool, pcm: &[f32], rate: u32, polled_at: Instant) {
        if self.mode.load(Ordering::Acquire) == ENDED || !self.current() {
            return;
        }
        if !pcm.is_empty() {
            let mut input = self.input.lock().unwrap_or_else(|e| e.into_inner());
            if input
                .last_input
                .is_some_and(|at| polled_at.saturating_duration_since(at) > PRESENTATION_LIMIT)
            {
                // Retire the converter history at a real gap as well as rejecting aged queued blocks.
                self.reader.store(0, Ordering::Release);
                self.installed_rate.store(u32::MAX, Ordering::Release);
                self.dirty.store(true, Ordering::Release);
                *input = Input::default();
            }
            input.last_input = Some(polled_at);
            self.rate.store(rate, Ordering::Release);
            self.mode.store(ORIGINAL, Ordering::Release);
            input.append(pcm, rate, polled_at);
            input.pump();
        } else if self.mode.load(Ordering::Acquire) == ORIGINAL {
            self.input.lock().unwrap_or_else(|e| e.into_inner()).pump();
        } else if ring && self.mode.load(Ordering::Acquire) == 0 {
            self.start.get_or_init(Instant::now);
            self.mode.store(RING, Ordering::Release);
        } else if !ring && self.mode.load(Ordering::Acquire) == RING {
            self.mode.store(0, Ordering::Release);
        }
    }
    pub(super) fn needs_reader(&self) -> bool {
        matches!(self.mode.load(Ordering::Acquire), RING | ORIGINAL)
            && self.installed_rate.load(Ordering::Acquire) != self.rate.load(Ordering::Acquire)
    }
}

pub(super) struct Redirect {
    state: Arc<State>,
    token: u64,
    rate: u32,
    writer: Option<Writer>,
}
impl Redirect {
    pub(super) fn activate(self) {
        let mut input = self.state.input.lock().unwrap_or_else(|e| e.into_inner());
        input.writer = self.writer;
        input.pump();
        self.state.reader.store(self.token, Ordering::Release);
        self.state
            .installed_rate
            .store(self.rate, Ordering::Release);
    }
}

pub(super) struct Reader {
    state: Arc<State>,
    token: u64,
    wave: &'static [f32],
    input: Option<TimedInput>,
    scratch: Box<[f32]>,
    ring_frame: Option<usize>,
}
impl Reader {
    pub(super) fn render(&mut self, pcm: &mut [f32]) -> bool {
        let mode = self.state.mode.load(Ordering::Acquire);
        let valid = |s: &State| {
            s.mode.load(Ordering::Acquire) == mode
                && s.reader.load(Ordering::Acquire) == self.token
                && s.current()
        };
        if pcm.len() > self.scratch.len() || !valid(&self.state) {
            return false;
        }
        let block = &mut self.scratch[..pcm.len()];
        let active = match mode {
            RING => {
                let Some(start) = self.state.start.get() else {
                    return false;
                };
                let position = *self
                    .ring_frame
                    .get_or_insert_with(|| (start.elapsed().as_micros() * 48 / 1000) as usize);
                self.ring_frame = Some(position + block.len());
                let mut on = false;
                for (i, sample) in block.iter_mut().enumerate() {
                    let n = (position + i) % 288000;
                    *sample = if n < 96000 {
                        on = true;
                        self.wave[n % self.wave.len()]
                    } else {
                        0.0
                    };
                }
                on
            }
            ORIGINAL => self.input.as_mut().is_some_and(|input| input.render(block)),
            _ => false,
        };
        if !active || !valid(&self.state) {
            return false;
        }
        for (dest, value) in pcm.iter_mut().zip(block) {
            *dest += *value;
        }
        true
    }
}

#[cfg(test)]
pub(crate) fn fixture_cut_progress(
    engine: &Engine,
    revision: u64,
    permit: ActionPermit,
    revoke: impl FnOnce(),
) -> Result<(bool, bool, bool, bool), RuntimeError> {
    let registry = Registry::new(true)?;
    let state = registry.begin(
        permit.clone(),
        engine,
        revision,
        Arc::new(AtomicBool::new(true)),
    )?;
    state.update(true, &[], 0, Instant::now());
    let (mut readers, redirects) = registry.prepare()?;
    for redirect in redirects {
        redirect.activate();
    }
    let mut reader = readers.pop().ok_or(RuntimeError::Preparation)?;
    let live = reader.render(&mut [0.0; 480]);
    revoke();
    let core_still_current = permit.current(Some(engine.elapsed_ms()));
    let mut pcm = [0.0; 480];
    let keyed = reader.render(&mut pcm);
    state.end();
    Ok((
        live,
        core_still_current,
        keyed,
        pcm.iter().any(|&v| v != 0.0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_pcm_ring_coalesces_unwritten_timestamps_and_resumes_fresh_audio() {
        let (pcm, reader) = InboundRing::open_receiver(48_000, InboundPolicy::Media).unwrap();
        let (stamps, incoming) = rtrb::RingBuffer::new(32);
        let mut writer = Writer {
            pcm,
            stamps,
            last: None,
            frame: 0,
        };
        let mut reader = TimedInput {
            pcm: reader,
            incoming,
            stamp: None,
        };
        let old = Instant::now() - Duration::from_secs(2);
        let old_pcm = [-0.5; 1024];
        for _ in 0..32 {
            if writer.write(&old_pcm, old).unwrap() == 0 {
                break;
            }
        }
        let full_at = writer.frame;
        assert!(full_at > 0);
        for n in 1..=40 {
            assert_eq!(
                writer
                    .write(&old_pcm, old + Duration::from_micros(n))
                    .unwrap(),
                0
            );
        }
        assert_eq!(writer.frame, full_at);
        let waiting_stamps = reader.incoming.slots();

        // Drain every old original. A pending marker for the first newly accepted
        // prefix may conservatively discard it, but cannot renew any old original.
        let mut block = [0.0; 480];
        let mut stale_audible = false;
        for _ in 0..(full_at / 480 + 8) {
            stale_audible |= reader.render(&mut block);
        }
        let mut resumed = false;
        for _ in 0..8 {
            let accepted = writer.write(&[0.5; 480], Instant::now()).unwrap();
            block.fill(0.0);
            resumed |= accepted > 0
                && reader.render(&mut block)
                && block.iter().filter(|&&sample| sample > 0.25).count() > 240;
        }
        assert!(
            !stale_audible,
            "full-ring retries must not renew expired source positions"
        );
        assert!(
            resumed,
            "fresh PCM must resume after the real PCM reader releases capacity"
        );
        assert_eq!(
            waiting_stamps, 2,
            "one accepted origin and one unwritten position"
        );
    }
}
