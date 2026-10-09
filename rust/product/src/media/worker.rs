//! One serialized, non-audio media producer per station generation.

use super::{Context, status};
use crate::{
    abi as ffi,
    link::ring::{InboundConsumer, InboundPolicy, InboundProducer, InboundRing},
};
use rpt_advanced_core::{
    audio::{MorseRenderer, PcmRead, PcmStreamReader, ToneSequence},
    media::{MediaError, MediaSource, PreparedAudio, StationMediaSession},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    ffi::{CString, c_void},
    path::Path,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    consumer_outstanding: AtomicBool,
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
    ready: Producer<(u64, u64, InboundConsumer)>,
    retired: Consumer<(u64, InboundConsumer)>,
    handled: u64,
    one_shot: bool,
}

enum WorkerSource {
    Chain(MediaSource),
    Prepared(PreparedAudio),
}

/// Callback-side handle; endpoint transfer prevents ring destruction on audio.
struct StreamReader {
    control: Arc<JobControl>,
    ready: Consumer<(u64, u64, InboundConsumer)>,
    retired: Producer<(u64, InboundConsumer)>,
    generation: u64,
    delay_remaining: u64,
    consumer: Option<(u64, InboundConsumer)>,
    emitted_audio: bool,
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

    fn acquire_consumer(&mut self) {
        if self.consumer.is_some() {
            return;
        }
        while let Ok((generation, delay, consumer)) = self.ready.pop() {
            if generation == self.generation {
                self.delay_remaining = delay;
                self.emitted_audio = false;
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
}

impl PcmStreamReader for StreamReader {
    fn start(&mut self) {
        let prior = self.control.requested.load(Ordering::Relaxed);
        let next = prior.wrapping_add(1).max(1);
        self.generation = next;
        self.delay_remaining = 0;
        self.control.fallback_morse.store(
            if self.fallback_morse_pending { next } else { 0 },
            Ordering::Relaxed,
        );
        self.fallback_morse_pending = false;
        self.control.requested.store(next, Ordering::Release);
    }

    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        if output.is_empty() || self.generation == 0 {
            return PcmRead::Pending;
        }
        if self.control.cancelled.load(Ordering::Acquire) == self.generation {
            self.retire_consumer();
            return PcmRead::Pending;
        }
        if self
            .consumer
            .as_ref()
            .is_some_and(|(generation, _)| *generation != self.generation)
        {
            self.retire_consumer();
            return PcmRead::Pending;
        }
        self.acquire_consumer();
        if self
            .consumer
            .as_ref()
            .is_some_and(|(generation, _)| *generation != self.generation)
        {
            self.retire_consumer();
            return PcmRead::Pending;
        }
        if self.consumer.is_none() {
            if self.control.failed.load(Ordering::Acquire) == self.generation {
                return PcmRead::Failed;
            }
            return PcmRead::Pending;
        }
        let mut produced = 0;
        while produced < output.len() {
            let sample = self
                .consumer
                .as_mut()
                .expect("consumer checked above")
                .1
                .render_sample();
            match sample {
                Ok(Some(_)) if self.delay_remaining != 0 => self.delay_remaining -= 1,
                Ok(Some(sample)) => {
                    output[produced] = sample;
                    produced += 1;
                    self.emitted_audio = true;
                }
                Ok(None) if self.control.completed.load(Ordering::Acquire) == self.generation => {
                    return if produced == 0 {
                        if self.retire_consumer() {
                            PcmRead::Finished
                        } else {
                            PcmRead::Pending
                        }
                    } else {
                        PcmRead::FinalSamples(produced)
                    };
                }
                Ok(None) => {
                    return if produced == 0 {
                        PcmRead::Pending
                    } else {
                        PcmRead::Samples(produced)
                    };
                }
                Err(_) => {
                    return if produced != 0 {
                        PcmRead::FinalSamples(produced)
                    } else if self.retire_consumer() {
                        if self.emitted_audio {
                            PcmRead::Finished
                        } else {
                            PcmRead::Failed
                        }
                    } else {
                        PcmRead::Pending
                    };
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

    fn select_morse_fallback(&mut self) -> bool {
        if !self.morse_available {
            return false;
        }
        if self.generation == 0 {
            self.fallback_morse_pending = true;
            return true;
        }
        let previous = self.generation;
        let next = previous.wrapping_add(1).max(1);
        self.control.cancelled.store(previous, Ordering::Release);
        if !self.retire_consumer() {
            return false;
        }
        while let Ok((generation, _, consumer)) = self.ready.pop() {
            if let Err(rtrb::PushError::Full((generation, consumer))) =
                self.retired.push((generation, consumer))
            {
                self.consumer = Some((generation, consumer));
                return false;
            }
        }
        self.generation = next;
        self.delay_remaining = 0;
        self.emitted_audio = false;
        self.control.completed.store(0, Ordering::Relaxed);
        self.control.failed.store(0, Ordering::Relaxed);
        self.control.fallback_morse.store(next, Ordering::Release);
        self.control.requested.store(next, Ordering::Release);
        true
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
    fn register(&mut self, source: MediaSource) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        self.register_source(source, false)
    }

    fn register_once(
        &mut self,
        source: MediaSource,
    ) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        self.register_source(source, true)
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
            emitted_audio: false,
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
        source: MediaSource,
        one_shot: bool,
    ) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        if self.stopping.load(Ordering::Acquire)
            || (source.file.is_none()
                && source.speech.is_none()
                && source.tone.is_none()
                && source.morse.is_none())
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
        let (ready, reader_ready) = RingBuffer::new(1);
        let (reader_retired, retired) = RingBuffer::new(1);
        let control = Arc::new(JobControl::default());
        let morse_available = source.morse.is_some();
        let job = WorkerJob {
            source: WorkerSource::Chain(source),
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
            emitted_audio: false,
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
                    .store(false, Ordering::Release);
                worked = true;
            }
            let requested = job.control.requested.load(Ordering::Acquire);
            if requested != 0 && requested != job.handled {
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
                .store(false, Ordering::Release);
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
    let mut first = [0.0; STREAM_CHUNK];
    let mut source_stream = match &job.source {
        WorkerSource::Prepared(audio) => {
            let mut stream = WorkerStream::Prepared(audio.clone(), 0);
            let count = stream.read(&mut first, &cancellation).unwrap_or(0);
            (count != 0).then_some((stream, count))
        }
        WorkerSource::Chain(_) => None,
    };
    let fallback_morse = control.fallback_morse.load(Ordering::Acquire) == generation;
    if let WorkerSource::Chain(source) = &job.source {
        if !fallback_morse {
            if let Some(path) = &source.file {
                if let Ok(mut stream) = open_file(file, path, &cancellation) {
                    if let Ok(count) = stream.read(&mut first, &cancellation) {
                        if count != 0 {
                            source_stream = Some((
                                WorkerStream::Provider(
                                    stream,
                                    10_f32.powf(f32::from(source.provider_gain_db) / 20.0),
                                ),
                                count,
                            ));
                        }
                    }
                }
            }
            if source_stream.is_none() && !cancellation.control.is_cancelled(stopping, generation) {
                if let Some(request) = &source.speech {
                    if let Ok(mut stream) = open_speech(speech, request, &cancellation) {
                        if let Ok(count) = stream.read(&mut first, &cancellation) {
                            if count != 0 {
                                source_stream = Some((
                                    WorkerStream::Provider(
                                        stream,
                                        10_f32.powf(f32::from(source.provider_gain_db) / 20.0),
                                    ),
                                    count,
                                ));
                            }
                        }
                    }
                }
            }
        }
        if source_stream.is_none()
            && !fallback_morse
            && !cancellation.control.is_cancelled(stopping, generation)
        {
            if let Some(tone) = &source.tone {
                let mut renderer = ToneSequence::new(&tone.sequence, tone.level_db)
                    .expect("tone source was validated at registration");
                let count = renderer.render(&mut first);
                source_stream = Some((WorkerStream::Tone(renderer), count));
            }
        }
        if source_stream.is_none() && !cancellation.control.is_cancelled(stopping, generation) {
            if let Some(morse) = &source.morse {
                let mut renderer = MorseRenderer::new(
                    &morse.text,
                    morse.speed_wpm,
                    morse.frequency_hz,
                    morse.level_db,
                )
                .expect("Morse source was validated at registration");
                let count = renderer.render(&mut first);
                if count != 0 {
                    source_stream = Some((WorkerStream::Morse(renderer), count));
                }
            }
        }
    }
    let Some((mut stream, mut count)) = source_stream else {
        if !cancellation.control.is_cancelled(stopping, generation) {
            fail(control, generation);
        }
        return;
    };
    if let WorkerStream::Provider(_, gain) = &stream {
        for sample in &mut first[..count] {
            *sample *= *gain;
        }
    }
    let Ok((mut producer, consumer)) = open_ring(stream.rate(), InboundPolicy::Media) else {
        fail(control, generation);
        return;
    };
    let Ok(delay) = producer.output_delay_samples() else {
        fail(control, generation);
        return;
    };
    control.consumer_outstanding.store(true, Ordering::Release);
    if !push_all(&mut producer, &first[..count], &cancellation) {
        control.consumer_outstanding.store(false, Ordering::Release);
        if !cancellation.control.is_cancelled(stopping, generation) {
            fail(control, generation);
        }
        return;
    }
    if let Err(rtrb::PushError::Full((_, _, consumer))) =
        job.ready.push((generation, delay, consumer))
    {
        drop(consumer);
        control.consumer_outstanding.store(false, Ordering::Release);
        fail(control, generation);
        return;
    }
    let mut chunk = [0.0; STREAM_CHUNK];
    let mut failed = false;
    loop {
        if cancellation.control.is_cancelled(stopping, generation) {
            return;
        }
        match stream.read(&mut chunk, &cancellation) {
            Ok(0) => break,
            Ok(read) => {
                count = read;
                if !push_all(&mut producer, &chunk[..count], &cancellation) {
                    if cancellation.control.is_cancelled(stopping, generation) {
                        return;
                    }
                    failed = true;
                    break;
                }
            }
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    if cancellation.control.is_cancelled(stopping, generation) {
        return;
    }
    let padding = delay
        .saturating_add(1)
        .saturating_mul(u64::from(stream.rate()))
        .div_ceil(48000);
    let zeros = [0.0; 256];
    let mut remaining = padding;
    while remaining != 0 {
        let count = remaining.min(zeros.len() as u64) as usize;
        if !push_all(&mut producer, &zeros[..count], &cancellation) {
            if cancellation.control.is_cancelled(stopping, generation) {
                return;
            }
            failed = true;
            break;
        }
        remaining -= count as u64;
    }
    if failed {
        control.failed.store(generation, Ordering::Release);
    }
    control.completed.store(generation, Ordering::Release);
    while !stopping.load(Ordering::Acquire) {
        let mut retired = false;
        while let Ok((retired_generation, consumer)) = job.retired.pop() {
            drop(consumer);
            control.consumer_outstanding.store(false, Ordering::Release);
            retired |= retired_generation == generation;
        }
        if retired {
            break;
        }
        if cancellation.control.is_cancelled(stopping, generation) {
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
                && (job.control.completed.load(Ordering::Acquire) == job.handled
                    || job.control.cancelled.load(Ordering::Acquire) == job.handled)
                && !job.control.consumer_outstanding.load(Ordering::Acquire)
        }
}

fn fail(control: &JobControl, generation: u64) {
    control.failed.store(generation, Ordering::Release);
    control.completed.store(generation, Ordering::Release);
}

fn push_all(
    producer: &mut InboundProducer,
    mut samples: &[f32],
    cancellation: &Cancellation<'_>,
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
            Ok(accepted) => samples = &samples[accepted..],
            Err(_) => return false,
        }
    }
    true
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
