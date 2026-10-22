//! One serialized, non-audio media producer per station generation.

use super::{Context, status};
use crate::{
    abi as ffi,
    link::ring::{InboundConsumer, InboundPolicy, InboundProducer, InboundRing},
};
use rpt_advanced_core::{
    audio::{MorseRenderer, MorseSwitch, PcmRead, PcmStreamReader, ToneSequence},
    link::PeerInput,
    media::{MediaError, MediaProgram, MediaSource, PreparedAudio, StationMediaSession},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    ffi::{CString, c_void},
    path::Path,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

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

/// A finite segment's sample budget excludes resampler flush padding. The producer
/// publishes accepted source duration; the audio owner never guesses EOF from an
/// empty FIFO, which may still hide converter input or output.
struct SegmentConsumer {
    ring: InboundConsumer,
    progress: Arc<SegmentProgress>,
    consumed: u64,
}

/// Count only real source duration; failed flushes cannot hold playback forever.
#[derive(Default)]
struct SegmentProgress {
    budget: AtomicU64,
    undrainable: AtomicBool,
}

impl SegmentConsumer {
    fn finished(&self, completed: bool) -> bool {
        // Observe end before duration: final writes cannot appear after this fence.
        let ended = self.ring.signals().ended() || completed;
        ended && self.consumed >= self.progress.budget.load(Ordering::Acquire)
    }

    fn render_sample(&mut self) -> Result<Option<f32>, crate::link::ring::RingError> {
        if self.consumed >= self.progress.budget.load(Ordering::Acquire) {
            return Ok(None);
        }
        let sample = self.ring.render_sample()?;
        self.consumed += u64::from(sample.is_some());
        if sample.is_none() && self.progress.undrainable.load(Ordering::Acquire) {
            Err(crate::link::ring::RingError)
        } else {
            Ok(sample)
        }
    }
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
    pending: Producer<WorkerJob>,
    requests: Option<Consumer<WorkerJob>>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl StationSession {
    pub(super) fn new(
        node: String,
        generation: u64,
        file: Arc<Context>,
        speech: Arc<Context>,
    ) -> Result<Self, MediaError> {
        if node.is_empty() || generation == 0 {
            return Err(MediaError::InvalidRequest);
        }
        let (pending, requests) = RingBuffer::new(4);
        Ok(Self {
            node,
            generation,
            file,
            speech,
            jobs: Vec::new(),
            pending,
            requests: Some(requests),
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
        self.pending
            .push(WorkerJob {
                source: WorkerSource::Prepared(audio),
                control: Arc::clone(&control),
                ready,
                retired,
                handled: 0,
                one_shot: true,
            })
            .map_err(|_| MediaError::QueueFull)?;
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
        let requests = self.requests.take().ok_or(MediaError::InvalidRequest)?;
        let file = Arc::clone(&self.file);
        let speech = Arc::clone(&self.speech);
        let stopping = Arc::clone(&self.stopping);
        let mut jobs = std::mem::take(&mut self.jobs);
        jobs.reserve(4);
        let name = format!("rptadv-media-{}-{}", self.node, self.generation);
        self.worker = Some(
            thread::Builder::new()
                .name(name)
                .spawn(move || worker_loop(jobs, requests, file, speech, stopping))
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
            self.pending.push(job).map_err(|_| MediaError::QueueFull)?;
        } else if self.worker.is_some() {
            return Err(MediaError::InvalidRequest);
        } else {
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

fn worker_loop(
    mut jobs: Vec<WorkerJob>,
    mut requests: Consumer<WorkerJob>,
    file: Arc<Context>,
    speech: Arc<Context>,
    stopping: Arc<AtomicBool>,
) {
    while !stopping.load(Ordering::Acquire) {
        let mut worked = false;
        while let Ok(job) = requests.pop() {
            jobs.push(job);
            worked = true;
        }
        let mut index = 0;
        while index < jobs.len() {
            let job = &mut jobs[index];
            while let Ok((_, consumer)) = job.retired.pop() {
                drop(consumer);
                job.control
                    .consumer_outstanding
                    .fetch_sub(1, Ordering::AcqRel);
                worked = true;
            }
            let requested = job.control.requested.load(Ordering::Acquire);
            // A cancelled producer can publish late. Retire its endpoint before
            // starting the replacement so the bounded ready queue cannot overflow.
            if requested != 0
                && requested != job.handled
                && job.control.consumer_outstanding.load(Ordering::Acquire) == 0
            {
                job.handled = requested;
                run_job(job, requested, &file, &speech, &stopping, InboundRing::open);
                worked = true;
            }
            if one_shot_done(job) {
                jobs.swap_remove(index);
            } else {
                index += 1;
            }
        }
        if !worked {
            thread::sleep(Duration::from_millis(1));
        }
    }
    for job in &mut jobs {
        while let Ok((_, consumer)) = job.retired.pop() {
            drop(consumer);
            job.control
                .consumer_outstanding
                .fetch_sub(1, Ordering::AcqRel);
        }
    }
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

struct OpenStream<'a> {
    context: &'a Context,
    handle: *mut c_void,
    rate: u32,
}

impl Drop for OpenStream<'_> {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: this worker owns the opened provider stream and closes it exactly once.
            unsafe { (self.context.close_stream)(self.handle) };
        }
    }
}

impl OpenStream<'_> {
    fn read(
        &mut self,
        output: &mut [f32],
        cancellation: &Cancellation<'_>,
    ) -> Result<usize, MediaError> {
        let raw = raw_cancellation(cancellation);
        let mut count = 0;
        // SAFETY: the worker owns this stream; output and cancellation outlive the call.
        status(unsafe {
            (self.context.read_stream)(
                self.handle,
                &raw,
                output.as_mut_ptr(),
                output.len(),
                &mut count,
            )
        })?;
        if count > output.len()
            || output[..count]
                .iter()
                .any(|sample| !sample.is_finite() || sample.abs() > 1.0)
        {
            return Err(MediaError::InvalidOutput);
        }
        Ok(count)
    }
}

enum WorkerStream<'a> {
    Provider(OpenStream<'a>, f32),
    Prepared(PreparedAudio, usize),
    Tone(ToneSequence),
    Morse(MorseRenderer),
}

impl WorkerStream<'_> {
    fn rate(&self) -> u32 {
        match self {
            Self::Provider(stream, _) => stream.rate,
            Self::Prepared(audio, _) => audio.sample_rate_hz(),
            Self::Tone(_) | Self::Morse(_) => 48_000,
        }
    }

    fn read(
        &mut self,
        output: &mut [f32],
        cancellation: &Cancellation<'_>,
    ) -> Result<usize, MediaError> {
        match self {
            Self::Provider(stream, gain) => {
                let count = stream.read(output, cancellation)?;
                for sample in &mut output[..count] {
                    *sample *= *gain;
                }
                Ok(count)
            }
            Self::Prepared(audio, offset) => {
                let count = output
                    .len()
                    .min(audio.samples().len().saturating_sub(*offset));
                output[..count].copy_from_slice(&audio.samples()[*offset..*offset + count]);
                *offset += count;
                Ok(count)
            }
            Self::Tone(source) => Ok(source.render(output)),
            Self::Morse(source) => Ok(source.render(output)),
        }
    }
}

fn open_file<'a>(
    context: &'a Context,
    path: &Path,
    cancellation: &Cancellation<'_>,
) -> Result<OpenStream<'a>, MediaError> {
    let open = context.open_file.ok_or(MediaError::IncompatibleAdapter)?;
    let path = super::path_string(path)?;
    let raw = raw_cancellation(cancellation);
    let mut stream = ffi::rptadv_media_stream {
        handle: ptr::null_mut(),
        sample_rate_hz: 0,
    };
    // SAFETY: all borrowed request data remains live through the synchronous open call.
    let result = status(unsafe { open(context.handle.as_ptr(), path.as_ptr(), &raw, &mut stream) });
    let opened = OpenStream {
        context,
        handle: stream.handle,
        rate: stream.sample_rate_hz,
    };
    result?;
    if !valid_stream(opened.handle, opened.rate) {
        return Err(MediaError::InvalidOutput);
    }
    Ok(opened)
}

fn open_speech<'a>(
    context: &'a Context,
    source: &rpt_advanced_core::media::SpeechSource,
    cancellation: &Cancellation<'_>,
) -> Result<OpenStream<'a>, MediaError> {
    let open = context.open_speech.ok_or(MediaError::IncompatibleAdapter)?;
    let text = CString::new(source.text.as_bytes()).map_err(|_| MediaError::InvalidRequest)?;
    let model = super::path_string(&source.model)?;
    let request = ffi::rptadv_media_speech_request {
        text: text.as_ptr(),
        model: model.as_ptr(),
        speed_percent: source.speed_percent,
        level_db: source.level_db,
    };
    let raw = raw_cancellation(cancellation);
    let mut stream = ffi::rptadv_media_stream {
        handle: ptr::null_mut(),
        sample_rate_hz: 0,
    };
    // SAFETY: request strings, cancellation context and output remain live through open.
    let result = status(unsafe { open(context.handle.as_ptr(), &request, &raw, &mut stream) });
    let opened = OpenStream {
        context,
        handle: stream.handle,
        rate: stream.sample_rate_hz,
    };
    result?;
    if !valid_stream(opened.handle, opened.rate) {
        return Err(MediaError::InvalidOutput);
    }
    Ok(opened)
}

/// Enforce the shared handle and source-rate contract for file and speech streams.
fn valid_stream(handle: *mut c_void, rate: u32) -> bool {
    !handle.is_null() && (8000..=192000).contains(&rate)
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
fn open_source<'a>(
    source: &MediaSource,
    file: &'a Context,
    speech: &'a Context,
    cancellation: &Cancellation<'_>,
    first: &mut [f32],
) -> Option<(WorkerStream<'a>, usize)> {
    let gain = 10_f32.powf(f32::from(source.provider_gain_db) / 20.0);
    for provider in 0..2 {
        if cancellation
            .control
            .is_cancelled(cancellation.stopping, cancellation.generation)
        {
            return None;
        }
        let stream = if provider == 0 {
            source
                .file
                .as_deref()
                .map(|path| open_file(file, path, cancellation))
        } else {
            source
                .speech
                .as_ref()
                .map(|request| open_speech(speech, request, cancellation))
        };
        if let Some(Ok(stream)) = stream {
            let mut stream = WorkerStream::Provider(stream, gain);
            if let Ok(count) = stream.read(first, cancellation) {
                if count != 0 {
                    return Some((stream, count));
                }
            }
        }
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
    let count = stream.read(first, cancellation).ok()?;
    (count != 0).then_some((stream, count))
}

fn morse_stream(source: &rpt_advanced_core::media::MorseSource) -> WorkerStream<'static> {
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

fn run_job(
    job: &mut WorkerJob,
    generation: u64,
    file: &Context,
    speech: &Context,
    stopping: &AtomicBool,
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
            thread::sleep(Duration::from_millis(1));
        }
        let mut first = [0.0; STREAM_CHUNK];
        let selected = match &job.source {
            WorkerSource::Prepared(audio) => {
                let mut stream = WorkerStream::Prepared(audio.clone(), 0);
                stream
                    .read(&mut first, &cancellation)
                    .ok()
                    .filter(|count| *count != 0)
                    .map(|count| (stream, count))
            }
            WorkerSource::Program(program) if fallback => {
                let mut stream = morse_stream(&program.morse[index]);
                stream
                    .read(&mut first, &cancellation)
                    .ok()
                    .filter(|count| *count != 0)
                    .map(|count| (stream, count))
            }
            WorkerSource::Program(program) => open_source(
                &program.primary[index],
                file,
                speech,
                &cancellation,
                &mut first,
            ),
        };
        let Some((mut stream, count)) = selected else {
            if !control.is_cancelled(stopping, generation) {
                fail(control, generation);
            }
            break 'segments;
        };
        let Ok((mut producer, ring)) = open_ring(stream.rate(), InboundPolicy::Media) else {
            fail(control, generation);
            break 'segments;
        };
        let Ok(delay) = producer.output_delay_samples() else {
            fail(control, generation);
            break 'segments;
        };
        let progress = Arc::new(SegmentProgress::default());
        let mut source_samples = 0_u64;
        let rate = stream.rate();
        let mut publish = |accepted: usize| {
            source_samples = source_samples.saturating_add(accepted as u64);
            progress.budget.store(
                delay.saturating_add(source_samples.saturating_mul(48_000) / u64::from(rate)),
                Ordering::Release,
            );
        };
        if !push_counted(&mut producer, &first[..count], &cancellation, &mut publish) {
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
            match stream.read(&mut chunk, &cancellation) {
                Ok(0) => break,
                Ok(count)
                    if push_counted(
                        &mut producer,
                        &chunk[..count],
                        &cancellation,
                        &mut publish,
                    ) => {}
                _ => {
                    failed = true;
                    break;
                }
            }
        }
        if control.is_cancelled(stopping, generation) {
            return;
        }
        progress.budget.store(
            delay.saturating_add(
                source_samples
                    .saturating_mul(48_000)
                    .div_ceil(u64::from(rate)),
            ),
            Ordering::Release,
        );
        let mut remaining = delay
            .saturating_add(1)
            .saturating_mul(u64::from(rate))
            .div_ceil(48_000);
        let zeros = [0.0; 256];
        while remaining != 0 {
            let count = remaining.min(zeros.len() as u64) as usize;
            if !push_all(&mut producer, &zeros[..count], &cancellation) {
                progress.undrainable.store(true, Ordering::Release);
                failed = true;
                break;
            }
            remaining -= count as u64;
        }
        // Publish segment end only after duration and padding; readers drain the
        // finite budget even if the converter temporarily reports no output.
        drop(producer);
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
        thread::sleep(Duration::from_millis(1));
    }
}

fn one_shot_done(job: &WorkerJob) -> bool {
    job.one_shot
        && if job.handled == 0 {
            job.control.abandoned.load(Ordering::Acquire)
        } else {
            job.control.requested.load(Ordering::Acquire) == job.handled
                // Producer EOF is not reader completion: a Morse switch may follow.
                && job.control.cancelled.load(Ordering::Acquire) == job.handled
                && job.control.consumer_outstanding.load(Ordering::Acquire) == 0
        }
}

fn fail(control: &JobControl, generation: u64) {
    control.failed.store(generation, Ordering::Release);
    control.completed.store(generation, Ordering::Release);
}

fn push_all(
    producer: &mut InboundProducer,
    samples: &[f32],
    cancellation: &Cancellation<'_>,
) -> bool {
    push_counted(producer, samples, cancellation, &mut |_| {})
}

fn push_counted(
    producer: &mut InboundProducer,
    mut samples: &[f32],
    cancellation: &Cancellation<'_>,
    accepted_samples: &mut impl FnMut(usize),
) -> bool {
    while !samples.is_empty() {
        if cancellation
            .control
            .is_cancelled(cancellation.stopping, cancellation.generation)
        {
            return false;
        }
        match producer.write(samples) {
            Ok(0) => thread::sleep(Duration::from_millis(1)),
            Ok(accepted) => {
                accepted_samples(accepted);
                samples = &samples[accepted..];
            }
            Err(_) => return false,
        }
    }
    true
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
