//! One station thread advances each active media source by a bounded turn.

use super::{
    Context, ProviderStream, StreamStep,
    finite::{FiniteWriter, SegmentConsumer, SegmentProgress},
};
use crate::{
    abi as ffi,
    link::ring::{InboundConsumer, InboundPolicy, InboundProducer, InboundRing},
};
use rpt_advanced_core::{
    audio::{MorseRenderer, MorseSwitch, PcmRead, PcmStreamReader, ToneSequence},
    media::{MediaError, MediaProgram, MediaSource, PreparedAudio, StationMediaSession},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    collections::VecDeque,
    ffi::{CString, c_void},
    future::{Future, poll_fn},
    pin::Pin,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::{Poll, Waker},
    thread::{self, JoinHandle},
    time::Duration,
};

#[path = "lifecycle.rs"]
mod lifecycle;
pub(super) use lifecycle::Lifecycle;

const STREAM_CHUNK: usize = 4096;

#[derive(Default)]
struct JobControl {
    requested: AtomicU64,
    cancelled: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    fallback_morse: AtomicU64,
    consumer_outstanding: AtomicUsize,
    abandoned: AtomicBool,
    registration_failed: AtomicBool,
}

impl JobControl {
    fn is_cancelled(&self, stopping: &AtomicBool, generation: u64) -> bool {
        stopping.load(Ordering::Acquire)
            || self.requested.load(Ordering::Acquire) != generation
            || self.cancelled.load(Ordering::Acquire) == generation
    }
}

struct WorkerJob {
    source: WorkerSource,
    control: Arc<JobControl>,
    ready: Producer<(u64, u64, SegmentConsumer)>,
    retired: Consumer<(u64, SegmentConsumer)>,
    handled: u64,
    one_shot: bool,
}

enum WorkerSource {
    Program(MediaProgram),
    Prepared(PreparedAudio),
}

/// Callback-side handle; endpoint transfer prevents ring destruction on audio.
struct StreamReader {
    control: Arc<JobControl>,
    ready: Consumer<(u64, u64, SegmentConsumer)>,
    retired: Producer<(u64, SegmentConsumer)>,
    generation: u64,
    delay_remaining: u64,
    consumer: Option<(u64, SegmentConsumer)>,
    morse_available: bool,
    fallback_morse_pending: bool,
}

impl StreamReader {
    fn retire_consumer(&mut self) -> bool {
        let Some((generation, consumer)) = self.consumer.take() else {
            return true;
        };
        match self.retired.push((generation, consumer)) {
            Ok(()) => true,
            Err(rtrb::PushError::Full((generation, consumer))) => {
                self.consumer = Some((generation, consumer));
                false
            }
        }
    }

    fn retire_available(&mut self) {
        if !self.retire_consumer() {
            return;
        }
        while let Ok((generation, _, consumer)) = self.ready.pop() {
            if let Err(rtrb::PushError::Full((generation, consumer))) =
                self.retired.push((generation, consumer))
            {
                self.consumer = Some((generation, consumer));
                return;
            }
        }
    }

    fn acquire_consumer(&mut self) {
        if self.consumer.is_some() {
            return;
        }
        while let Ok((generation, delay, consumer)) = self.ready.pop() {
            if generation == self.generation {
                self.delay_remaining = delay;
                self.consumer = Some((generation, consumer));
                return;
            }
            if let Err(rtrb::PushError::Full((generation, consumer))) =
                self.retired.push((generation, consumer))
            {
                self.consumer = Some((generation, consumer));
                return;
            }
        }
    }
    fn finish_read(&mut self, produced: usize) -> PcmRead {
        let failed = self.control.failed.load(Ordering::Acquire) == self.generation;
        if produced != 0 {
            // Preserve valid partial PCM, but never acknowledge a failed stream as EOF.
            if failed {
                PcmRead::Samples(produced)
            } else {
                PcmRead::FinalSamples(produced)
            }
        } else if !self.retire_consumer() {
            PcmRead::Pending
        } else if failed {
            PcmRead::Failed
        } else {
            PcmRead::Finished
        }
    }
}

impl PcmStreamReader for StreamReader {
    fn start(&mut self) {
        let next = self.generation.wrapping_add(1).max(1);
        self.generation = next;
        self.delay_remaining = 0;
        self.control.fallback_morse.store(
            if self.fallback_morse_pending { next } else { 0 },
            Ordering::Relaxed,
        );
        self.fallback_morse_pending = false;
        self.control.requested.store(next, Ordering::Release);
    }

    fn defer_before_output(&mut self) -> bool {
        // Zero suspends production without cancelling the restartable recipe.
        // Playback must call start before it reads this deferred attempt again.
        self.control.requested.store(0, Ordering::Release);
        self.retire_available();
        true
    }

    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        if self.control.registration_failed.load(Ordering::Acquire) {
            return PcmRead::Failed;
        }
        if output.is_empty() || self.generation == 0 {
            return PcmRead::Pending;
        }
        if self.control.cancelled.load(Ordering::Acquire) == self.generation {
            self.retire_consumer();
            return PcmRead::Pending;
        }
        let mut produced = 0;
        while produced < output.len() {
            // Completion is published after the last endpoint. Acquire it before
            // checking ready, so a completed program cannot hide its last segment.
            let completed = self.control.completed.load(Ordering::Acquire) == self.generation
                || self.control.failed.load(Ordering::Acquire) == self.generation;
            self.acquire_consumer();
            if self
                .consumer
                .as_ref()
                .is_some_and(|(generation, _)| *generation != self.generation)
            {
                self.retire_consumer();
                return if produced == 0 {
                    PcmRead::Pending
                } else {
                    PcmRead::Samples(produced)
                };
            }
            let Some((_, consumer)) = self.consumer.as_mut() else {
                return if completed {
                    self.finish_read(produced)
                } else if produced == 0 {
                    PcmRead::Pending
                } else {
                    PcmRead::Samples(produced)
                };
            };
            if consumer.finished(completed) {
                if !self.retire_consumer() {
                    return if produced == 0 {
                        PcmRead::Pending
                    } else {
                        PcmRead::Samples(produced)
                    };
                }
                continue;
            }
            let sample = consumer.render_sample();
            match sample {
                Ok(Some(_)) if self.delay_remaining != 0 => self.delay_remaining -= 1,
                Ok(Some(sample)) => {
                    output[produced] = sample;
                    produced += 1;
                }
                Ok(None) => {
                    return if produced == 0 {
                        PcmRead::Pending
                    } else {
                        PcmRead::Samples(produced)
                    };
                }
                Err(_) => {
                    self.control
                        .failed
                        .store(self.generation, Ordering::Release);
                    return self.finish_read(produced);
                }
            }
        }
        PcmRead::Samples(produced)
    }

    fn cancel(&mut self) {
        if self.generation == 0 {
            self.control.abandoned.store(true, Ordering::Release);
            return;
        }
        self.control
            .cancelled
            .store(self.generation, Ordering::Release);
        if self.control.requested.load(Ordering::Acquire) == 0 {
            // Terminal cancellation of a held recipe must permit one-shot reclamation.
            self.control
                .requested
                .store(self.generation, Ordering::Release);
        }
        self.retire_available();
    }

    fn select_morse_fallback(&mut self) -> MorseSwitch {
        if !self.morse_available {
            return MorseSwitch::Unavailable;
        }
        if self.generation == 0 {
            self.fallback_morse_pending = true;
            return MorseSwitch::Selected;
        }
        if self.control.fallback_morse.load(Ordering::Acquire) != self.generation {
            let next = self.generation.wrapping_add(1).max(1);
            self.generation = next;
            self.delay_remaining = 0;
            // Replacement invalidates the old producer without terminating this job.
            // Publish before retirement; retries must keep the same Morse generation.
            self.control.fallback_morse.store(next, Ordering::Relaxed);
            self.control.requested.store(next, Ordering::Release);
        }
        if self
            .consumer
            .as_ref()
            .is_some_and(|(generation, _)| *generation != self.generation)
            && !self.retire_consumer()
        {
            return MorseSwitch::Pending;
        }
        self.acquire_consumer();
        if self
            .consumer
            .as_ref()
            .is_some_and(|(generation, _)| *generation != self.generation)
        {
            MorseSwitch::Pending
        } else {
            MorseSwitch::Selected
        }
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Configuration-time builder retained by the runtime generation on its control owner.
pub(super) struct StationSession {
    node: String,
    generation: u64,
    file: Arc<Context>,
    speech: Arc<Context>,
    jobs: Vec<WorkerJob>,
    requests: Inbox,
    lifecycle: Arc<Lifecycle>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl StationSession {
    #[cfg(test)]
    pub(super) fn new(
        node: String,
        generation: u64,
        file: Arc<Context>,
        speech: Arc<Context>,
    ) -> Result<Self, MediaError> {
        Self::with_lifecycle(node, generation, file, speech, Arc::new(Lifecycle::new()?))
    }

    pub(super) fn with_lifecycle(
        node: String,
        generation: u64,
        file: Arc<Context>,
        speech: Arc<Context>,
        lifecycle: Arc<Lifecycle>,
    ) -> Result<Self, MediaError> {
        if node.is_empty() || generation == 0 {
            return Err(MediaError::InvalidRequest);
        }
        Ok(Self {
            node,
            generation,
            file,
            speech,
            jobs: Vec::new(),
            requests: Inbox::default(),
            lifecycle,
            stopping: Arc::new(AtomicBool::new(false)),
            worker: None,
        })
    }
}

impl StationMediaSession for StationSession {
    fn register_program(
        &mut self,
        program: MediaProgram,
        one_shot: bool,
    ) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        self.register_source(program, one_shot)
    }

    fn register_prepared(
        &mut self,
        audio: PreparedAudio,
    ) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        if self.worker.is_none()
            || self.stopping.load(Ordering::Acquire)
            || !(8_000..=192_000).contains(&audio.sample_rate_hz())
        {
            return Err(MediaError::InvalidRequest);
        }
        let (ready, reader_ready) = RingBuffer::new(1);
        let (reader_retired, retired) = RingBuffer::new(1);
        let control = Arc::new(JobControl::default());
        self.requests.push(WorkerJob {
            source: WorkerSource::Prepared(audio),
            control: Arc::clone(&control),
            ready,
            retired,
            handled: 0,
            one_shot: true,
        })?;
        Ok(Box::new(StreamReader {
            control,
            ready: reader_ready,
            retired: reader_retired,
            generation: 0,
            delay_remaining: 0,
            consumer: None,
            morse_available: false,
            fallback_morse_pending: false,
        }))
    }

    fn start(&mut self) -> Result<(), MediaError> {
        if self.worker.is_some() {
            return Err(MediaError::InvalidRequest);
        }
        let requests = self.requests.clone();
        let lifecycle = Arc::clone(&self.lifecycle);
        let file = Arc::clone(&self.file);
        let speech = Arc::clone(&self.speech);
        let stopping = Arc::clone(&self.stopping);
        let jobs = std::mem::take(&mut self.jobs);
        let name = format!("rptadv-media-{}-{}", self.node, self.generation);
        self.worker = Some(
            thread::Builder::new()
                .name(name)
                .spawn(move || schedule(jobs, requests, file, speech, stopping, lifecycle))
                .map_err(|_| MediaError::Io)?,
        );
        Ok(())
    }
}

impl StationSession {
    fn register_source(
        &mut self,
        program: MediaProgram,
        one_shot: bool,
    ) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        if self.stopping.load(Ordering::Acquire) || program.primary.is_empty() {
            return Err(MediaError::InvalidRequest);
        }
        for source in &program.primary {
            validate_source(source)?;
        }
        for morse in &program.morse {
            MorseRenderer::new(
                &morse.text,
                morse.speed_wpm,
                morse.frequency_hz,
                morse.level_db,
            )
            .map_err(|_| MediaError::InvalidRequest)?;
        }
        let (ready, reader_ready) = RingBuffer::new(1);
        let (reader_retired, retired) = RingBuffer::new(1);
        let control = Arc::new(JobControl::default());
        let morse_available = !program.morse.is_empty();
        let job = WorkerJob {
            source: WorkerSource::Program(program),
            control: Arc::clone(&control),
            ready,
            retired,
            handled: 0,
            one_shot,
        };
        if one_shot {
            self.requests.push(job)?;
        } else if self.worker.is_some() {
            return Err(MediaError::InvalidRequest);
        } else {
            self.jobs.try_reserve(1).map_err(|_| MediaError::Io)?;
            self.jobs.push(job);
        }
        Ok(Box::new(StreamReader {
            control,
            ready: reader_ready,
            retired: reader_retired,
            generation: 0,
            delay_remaining: 0,
            consumer: None,
            morse_available,
            fallback_morse_pending: false,
        }))
    }
}

impl Drop for StationSession {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Default)]
struct Inbox(Arc<Mutex<VecDeque<WorkerJob>>>);
impl Inbox {
    fn push(&self, job: WorkerJob) -> Result<(), MediaError> {
        let mut q = self.0.lock().unwrap();
        q.try_reserve(1).map_err(|_| MediaError::Io)?;
        q.push_back(job);
        Ok(())
    }
    fn pop(&self) -> Option<WorkerJob> {
        self.0.lock().unwrap().pop_front()
    }
}

type Running = Pin<Box<dyn Future<Output = WorkerJob>>>;
enum Slot {
    Idle(WorkerJob),
    Running(Running),
}

async fn yield_visit() {
    let mut yielded = false;
    poll_fn(|_| {
        if std::mem::replace(&mut yielded, true) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}

fn schedule(
    initial: Vec<WorkerJob>,
    requests: Inbox,
    file: Arc<Context>,
    speech: Arc<Context>,
    stopping: Arc<AtomicBool>,
    lifecycle: Arc<Lifecycle>,
) {
    let mut jobs: Vec<Option<Slot>> = initial
        .into_iter()
        .map(|job| Some(Slot::Idle(job)))
        .collect();
    let mut cx = std::task::Context::from_waker(Waker::noop());
    while !stopping.load(Ordering::Acquire) {
        if let Some(incoming) = requests.pop() {
            if jobs.try_reserve(1).is_ok() {
                jobs.push(Some(Slot::Idle(incoming)));
            } else {
                incoming
                    .control
                    .registration_failed
                    .store(true, Ordering::Release);
                let generation = incoming.control.requested.load(Ordering::Acquire);
                if generation != 0 {
                    fail(&incoming.control, generation);
                }
            }
        }
        jobs.retain_mut(|slot| {
            let next = match slot.take().unwrap() {
                Slot::Idle(mut job) => {
                    reclaim(&job.control, &mut job.retired);
                    if one_shot_done(&job) {
                        return false;
                    }
                    let generation = job.control.requested.load(Ordering::Acquire);
                    if generation != 0
                        && generation != job.handled
                        && job.control.consumer_outstanding.load(Ordering::Acquire) == 0
                    {
                        job.handled = generation;
                        let (file, speech, stop, life) = (
                            file.clone(),
                            speech.clone(),
                            stopping.clone(),
                            lifecycle.clone(),
                        );
                        Slot::Running(Box::pin(async move {
                            produce(
                                &mut job,
                                generation,
                                &file,
                                &speech,
                                &stop,
                                &life,
                                InboundRing::open,
                            )
                            .await;
                            job
                        }))
                    } else {
                        Slot::Idle(job)
                    }
                }
                Slot::Running(mut run) => match run.as_mut().poll(&mut cx) {
                    Poll::Pending => Slot::Running(run),
                    Poll::Ready(job) => Slot::Idle(job),
                },
            };
            *slot = Some(next);
            true
        });
        thread::sleep(Duration::from_millis(1));
    }
    drop(jobs);
    lifecycle.drain_station(&stopping);
}

struct Cancellation<'a> {
    control: &'a JobControl,
    stopping: &'a AtomicBool,
    generation: u64,
}

unsafe extern "C" fn is_cancelled(context: *const c_void) -> u32 {
    // SAFETY: raw_cancellation is used only during synchronous provider calls.
    let cancellation = unsafe { &*context.cast::<Cancellation<'_>>() };
    u32::from(
        cancellation
            .control
            .is_cancelled(cancellation.stopping, cancellation.generation),
    )
}

fn raw_cancellation(cancellation: &Cancellation<'_>) -> ffi::rptadv_media_cancellation {
    ffi::rptadv_media_cancellation {
        context: ptr::from_ref(cancellation).cast(),
        is_cancelled: Some(is_cancelled),
    }
}

enum WorkerStream {
    Provider(lifecycle::Ticket, f32),
    Prepared(PreparedAudio, usize),
    Tone(ToneSequence),
    Morse(MorseRenderer),
}
impl WorkerStream {
    fn rate(&self) -> u32 {
        match self {
            Self::Provider(ticket, _) => ticket
                .stream
                .as_ref()
                .unwrap()
                .rate
                .expect("real samples follow format"),
            Self::Prepared(clip, _) => clip.sample_rate_hz(),
            _ => 48000,
        }
    }
    async fn read(
        &mut self,
        out: &mut [f32],
        cancel: &Cancellation<'_>,
    ) -> Result<usize, MediaError> {
        match self {
            Self::Provider(ticket, gain) => loop {
                if cancel
                    .control
                    .is_cancelled(cancel.stopping, cancel.generation)
                {
                    return Err(MediaError::Cancelled);
                }
                if ticket.stream.is_none() && !ticket.take_ready()? {
                    yield_visit().await;
                    continue;
                }
                let event = ticket
                    .stream
                    .as_mut()
                    .unwrap()
                    .step(out, &raw_cancellation(cancel))?;
                match event {
                    StreamStep::Format | StreamStep::Pending => yield_visit().await,
                    StreamStep::Eof => return Ok(0),
                    StreamStep::Samples(n) => {
                        for sample in &mut out[..n] {
                            *sample *= *gain;
                        }
                        return Ok(n);
                    }
                }
            },
            Self::Prepared(clip, pos) => {
                let n = out.len().min(clip.samples().len().saturating_sub(*pos));
                out[..n].copy_from_slice(&clip.samples()[*pos..*pos + n]);
                *pos += n;
                Ok(n)
            }
            Self::Tone(tone) => Ok(tone.render(out)),
            Self::Morse(morse) => Ok(morse.render(out)),
        }
    }
}

/// Check every alternative on control, before audio can request the program.
fn validate_source(source: &MediaSource) -> Result<(), MediaError> {
    if source.file.is_none()
        && source.speech.is_none()
        && source.tone.is_none()
        && source.morse.is_none()
    {
        return Err(MediaError::InvalidRequest);
    }
    if source
        .file
        .as_deref()
        .is_some_and(|path| CString::new(path.as_os_str().as_encoded_bytes()).is_err())
        || source.speech.as_ref().is_some_and(|speech| {
            CString::new(speech.text.as_bytes()).is_err()
                || CString::new(speech.model.as_os_str().as_encoded_bytes()).is_err()
                || !(1..=1000).contains(&speech.speed_percent)
                || !(-60..=0).contains(&speech.level_db)
        })
        || !(-60..=0).contains(&source.provider_gain_db)
        || source
            .tone
            .as_ref()
            .is_some_and(|tone| ToneSequence::new(&tone.sequence, tone.level_db).is_err())
        || source.morse.as_ref().is_some_and(|morse| {
            MorseRenderer::new(
                &morse.text,
                morse.speed_wpm,
                morse.frequency_hz,
                morse.level_db,
            )
            .is_err()
        })
    {
        return Err(MediaError::InvalidRequest);
    }
    Ok(())
}

/// Select a segment's first playable source. Fallback is allowed only before PCM.
async fn open_source(
    source: &MediaSource,
    (file, speech): (&Arc<Context>, &Arc<Context>),
    control: &Arc<JobControl>,
    stopping: &Arc<AtomicBool>,
    generation: u64,
    life: &Lifecycle,
    first: &mut [f32],
) -> Option<(WorkerStream, usize)> {
    let cancellation = Cancellation {
        control,
        stopping,
        generation,
    };
    let gain = 10_f32.powf(f32::from(source.provider_gain_db) / 20.0);
    for provider in 0..2 {
        if control.is_cancelled(stopping, generation) {
            return None;
        }
        let (context, input) = match provider {
            0 => match &source.file {
                Some(p) => (file, lifecycle::Input::File(p.clone())),
                None => continue,
            },
            _ => match &source.speech {
                Some(s) => (speech, lifecycle::Input::Speech(s.clone())),
                None => continue,
            },
        };
        let request = lifecycle::Request {
            context: context.clone(),
            input,
            control: control.clone(),
            station: stopping.clone(),
            generation,
        };
        if let Ok(ticket) = life.submit(request) {
            let mut stream = WorkerStream::Provider(ticket, gain);
            if let Ok(n) = stream.read(first, &cancellation).await {
                if n != 0 {
                    return Some((stream, n));
                }
            }
        }
        yield_visit().await;
    }
    let mut stream = if let Some(tone) = &source.tone {
        WorkerStream::Tone(
            ToneSequence::new(&tone.sequence, tone.level_db).expect("validated tone"),
        )
    } else if let Some(morse) = &source.morse {
        morse_stream(morse)
    } else {
        return None;
    };
    let count = stream.read(first, &cancellation).await.ok()?;
    (count != 0).then_some((stream, count))
}

fn morse_stream(source: &rpt_advanced_core::media::MorseSource) -> WorkerStream {
    WorkerStream::Morse(
        MorseRenderer::new(
            &source.text,
            source.speed_wpm,
            source.frequency_hz,
            source.level_db,
        )
        .expect("validated Morse"),
    )
}

/// Reclaim all endpoint storage on the producer, never on the audio callback.
fn reclaim(control: &JobControl, retired: &mut Consumer<(u64, SegmentConsumer)>) {
    while let Ok((_, consumer)) = retired.pop() {
        drop(consumer);
        control.consumer_outstanding.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn write_chunk(
    writer: &mut FiniteWriter,
    mut samples: &[f32],
    cancelled: &impl Fn() -> bool,
) -> Result<(), MediaError> {
    while !samples.is_empty() {
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        let count = writer.offer(samples)?;
        samples = &samples[count..];
        if !samples.is_empty() {
            yield_visit().await;
        }
    }
    Ok(())
}

async fn produce(
    job: &mut WorkerJob,
    generation: u64,
    file: &Arc<Context>,
    speech: &Arc<Context>,
    stopping: &Arc<AtomicBool>,
    lifecycle: &Lifecycle,
    open_ring: fn(
        u32,
        InboundPolicy,
    ) -> Result<(InboundProducer, InboundConsumer), crate::link::ring::RingError>,
) {
    let control = &job.control;
    control.failed.store(0, Ordering::Release);
    control.completed.store(0, Ordering::Release);
    let cancellation = Cancellation {
        control,
        stopping,
        generation,
    };
    let fallback = control.fallback_morse.load(Ordering::Acquire) == generation;
    let segments = match &job.source {
        WorkerSource::Prepared(_) => 1,
        WorkerSource::Program(program) => {
            if fallback {
                program.morse.len()
            } else {
                program.primary.len()
            }
        }
    };
    'segments: for index in 0..segments {
        // One playing ring plus one prepared successor. Reclaim before preparing
        // another; bounded lookahead avoids a forced worker wakeup between segments.
        loop {
            reclaim(control, &mut job.retired);
            if control.is_cancelled(stopping, generation) {
                return;
            }
            if control.consumer_outstanding.load(Ordering::Acquire) < 2
                && (index == 0 || job.ready.slots() != 0)
            {
                break;
            }
            yield_visit().await;
        }
        yield_visit().await;
        let mut first = [0.0; STREAM_CHUNK];
        let selected = match &job.source {
            WorkerSource::Prepared(audio) => {
                let mut stream = WorkerStream::Prepared(audio.clone(), 0);
                stream
                    .read(&mut first, &cancellation)
                    .await
                    .ok()
                    .filter(|count| *count != 0)
                    .map(|count| (stream, count))
            }
            WorkerSource::Program(program) if fallback => {
                let mut stream = morse_stream(&program.morse[index]);
                stream
                    .read(&mut first, &cancellation)
                    .await
                    .ok()
                    .filter(|count| *count != 0)
                    .map(|count| (stream, count))
            }
            WorkerSource::Program(program) => {
                open_source(
                    &program.primary[index],
                    (file, speech),
                    control,
                    stopping,
                    generation,
                    lifecycle,
                    &mut first,
                )
                .await
            }
        };
        let Some((mut stream, count)) = selected else {
            if !control.is_cancelled(stopping, generation) {
                fail(control, generation);
            }
            break 'segments;
        };
        let Ok((producer, ring)) = open_ring(stream.rate(), InboundPolicy::Media) else {
            fail(control, generation);
            break 'segments;
        };
        let progress = Arc::new(SegmentProgress::default());
        let Ok(mut writer) = FiniteWriter::new(producer, Arc::clone(&progress), stream.rate())
        else {
            fail(control, generation);
            break 'segments;
        };
        let delay = writer.delay();
        let cancelled = || control.is_cancelled(stopping, generation);
        if write_chunk(&mut writer, &first[..count], &cancelled)
            .await
            .is_err()
        {
            if !control.is_cancelled(stopping, generation) {
                fail(control, generation);
            }
            break 'segments;
        }
        control.consumer_outstanding.fetch_add(1, Ordering::AcqRel);
        let consumer = SegmentConsumer {
            ring,
            progress: Arc::clone(&progress),
            consumed: 0,
        };
        if let Err(rtrb::PushError::Full((_, _, consumer))) =
            job.ready.push((generation, delay, consumer))
        {
            drop(consumer);
            control.consumer_outstanding.fetch_sub(1, Ordering::AcqRel);
            fail(control, generation);
            break 'segments;
        }
        let mut chunk = [0.0; STREAM_CHUNK];
        let mut failed = false;
        loop {
            if control.is_cancelled(stopping, generation) {
                return;
            }
            yield_visit().await;
            match stream.read(&mut chunk, &cancellation).await {
                Ok(0) => break,
                Ok(count)
                    if write_chunk(&mut writer, &chunk[..count], &cancelled)
                        .await
                        .is_ok() => {}
                _ => {
                    failed = true;
                    break;
                }
            }
        }
        if control.is_cancelled(stopping, generation) {
            return;
        }
        // EOF or error ends the provider lease even if the reader cannot accept padding.
        drop(stream);
        // Publish segment end only after duration and padding; readers drain the
        // finite budget even if the converter temporarily reports no output.
        let mut tail = writer.start_tail();
        while tail != 0 && !cancelled() {
            yield_visit().await;
            if writer.offer_tail(&mut tail).is_err() {
                failed = true;
                break;
            }
        }
        drop(writer);
        yield_visit().await;
        if control.is_cancelled(stopping, generation) {
            return;
        }
        if failed {
            fail(control, generation);
            break 'segments;
        }
    }
    if control.is_cancelled(stopping, generation) {
        return;
    }
    control.completed.store(generation, Ordering::Release);
    while !control.is_cancelled(stopping, generation) {
        reclaim(control, &mut job.retired);
        if control.consumer_outstanding.load(Ordering::Acquire) == 0 {
            break;
        }
        yield_visit().await;
    }
}

fn one_shot_done(job: &WorkerJob) -> bool {
    job.one_shot
        && (job.ready.is_abandoned()
            || if job.handled == 0 {
                job.control.abandoned.load(Ordering::Acquire)
            } else {
                job.control.requested.load(Ordering::Acquire) == job.handled
                    // A live reader may still request a Morse switch after producer EOF.
                    && job.control.cancelled.load(Ordering::Acquire) == job.handled
                    && job.control.consumer_outstanding.load(Ordering::Acquire) == 0
            })
}

fn fail(control: &JobControl, generation: u64) {
    control.failed.store(generation, Ordering::Release);
    control.completed.store(generation, Ordering::Release);
}

#[cfg(test)]
fn run_job(
    job: &mut WorkerJob,
    generation: u64,
    file: &Arc<Context>,
    speech: &Arc<Context>,
    stop: &Arc<AtomicBool>,
    open: fn(
        u32,
        InboundPolicy,
    ) -> Result<(InboundProducer, InboundConsumer), crate::link::ring::RingError>,
) {
    let life = Lifecycle::new().unwrap();
    let mut run = std::pin::pin!(produce(job, generation, file, speech, stop, &life, open));
    while run
        .as_mut()
        .poll(&mut std::task::Context::from_waker(Waker::noop()))
        .is_pending()
    {
        thread::sleep(Duration::from_millis(1));
    }
    life.drain_station(stop);
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
