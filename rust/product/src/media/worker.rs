//! One serialized, non-audio media producer per station generation.

use super::{Context, status};
use crate::{
    abi as ffi,
    link::ring::{InboundConsumer, InboundPolicy, InboundProducer, InboundRing},
};
use rpt_advanced_core::{
    audio::{PcmRead, PcmStreamReader},
    media::{MediaError, MediaSource, StationMediaSession},
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
}

impl JobControl {
    fn is_cancelled(&self, stopping: &AtomicBool, generation: u64) -> bool {
        stopping.load(Ordering::Acquire)
            || self.requested.load(Ordering::Acquire) != generation
            || self.cancelled.load(Ordering::Acquire) == generation
    }
}

struct WorkerJob {
    source: MediaSource,
    control: Arc<JobControl>,
    ready: Producer<(u64, u64, InboundConsumer)>,
    retired: Consumer<(u64, InboundConsumer)>,
    handled: u64,
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
                        PcmRead::Samples(produced)
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
                        PcmRead::Samples(produced)
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
    controls: Vec<Arc<JobControl>>,
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
        Ok(Self {
            node,
            generation,
            file,
            speech,
            jobs: Vec::new(),
            controls: Vec::new(),
            stopping: Arc::new(AtomicBool::new(false)),
            worker: None,
        })
    }
}

impl StationMediaSession for StationSession {
    fn register(&mut self, source: MediaSource) -> Result<Box<dyn PcmStreamReader>, MediaError> {
        if self.worker.is_some() || (source.file.is_none() && source.speech.is_none()) {
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
        {
            return Err(MediaError::InvalidRequest);
        }
        let (ready, reader_ready) = RingBuffer::new(1);
        let (reader_retired, retired) = RingBuffer::new(1);
        let control = Arc::new(JobControl::default());
        self.controls.push(Arc::clone(&control));
        self.jobs.push(WorkerJob {
            source,
            control: Arc::clone(&control),
            ready,
            retired,
            handled: 0,
        });
        Ok(Box::new(StreamReader {
            control,
            ready: reader_ready,
            retired: reader_retired,
            generation: 0,
            delay_remaining: 0,
            consumer: None,
            emitted_audio: false,
        }))
    }

    fn start(&mut self) -> Result<(), MediaError> {
        if self.worker.is_some() {
            return Err(MediaError::InvalidRequest);
        }
        let jobs = std::mem::take(&mut self.jobs);
        if jobs.is_empty() {
            return Ok(());
        }
        let file = Arc::clone(&self.file);
        let speech = Arc::clone(&self.speech);
        let stopping = Arc::clone(&self.stopping);
        let name = format!("rptadv-media-{}-{}", self.node, self.generation);
        self.worker = Some(
            thread::Builder::new()
                .name(name)
                .spawn(move || worker_loop(jobs, file, speech, stopping))
                .map_err(|_| MediaError::Io)?,
        );
        Ok(())
    }
}

impl Drop for StationSession {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        for control in &self.controls {
            let generation = control.requested.load(Ordering::Acquire);
            control.cancelled.store(generation, Ordering::Release);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_loop(
    mut jobs: Vec<WorkerJob>,
    file: Arc<Context>,
    speech: Arc<Context>,
    stopping: Arc<AtomicBool>,
) {
    while !stopping.load(Ordering::Acquire) {
        let mut worked = false;
        for job in &mut jobs {
            while let Ok((_, consumer)) = job.retired.pop() {
                drop(consumer);
                worked = true;
            }
            let requested = job.control.requested.load(Ordering::Acquire);
            if requested != 0 && requested != job.handled {
                job.handled = requested;
                run_job(job, requested, &file, &speech, &stopping);
                worked = true;
            }
        }
        if !worked {
            thread::sleep(Duration::from_millis(1));
        }
    }
    for job in &mut jobs {
        while let Ok((_, consumer)) = job.retired.pop() {
            drop(consumer);
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
    if opened.handle.is_null() || !(8000..=192000).contains(&opened.rate) {
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
    if opened.handle.is_null() || !(8000..=192000).contains(&opened.rate) {
        return Err(MediaError::InvalidOutput);
    }
    Ok(opened)
}

fn run_job(
    job: &mut WorkerJob,
    generation: u64,
    file: &Context,
    speech: &Context,
    stopping: &AtomicBool,
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
    let mut source_stream = None;
    if let Some(path) = &job.source.file {
        if let Ok(mut stream) = open_file(file, path, &cancellation) {
            if let Ok(count) = stream.read(&mut first, &cancellation) {
                if count != 0 {
                    source_stream = Some((stream, count));
                }
            }
        }
    }
    if source_stream.is_none() && !cancellation.control.is_cancelled(stopping, generation) {
        if let Some(source) = &job.source.speech {
            if let Ok(mut stream) = open_speech(speech, source, &cancellation) {
                if let Ok(count) = stream.read(&mut first, &cancellation) {
                    if count != 0 {
                        source_stream = Some((stream, count));
                    }
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
    let Ok((mut producer, consumer)) = InboundRing::open(stream.rate, InboundPolicy::Media) else {
        fail(control, generation);
        return;
    };
    let Ok(delay) = producer.output_delay_samples() else {
        fail(control, generation);
        return;
    };
    if !push_all(&mut producer, &first[..count], &cancellation) {
        if !cancellation.control.is_cancelled(stopping, generation) {
            fail(control, generation);
        }
        return;
    }
    if job.ready.push((generation, delay, consumer)).is_err() {
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
        .saturating_mul(u64::from(stream.rate))
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
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static READS: AtomicUsize = AtomicUsize::new(0);
    static READING: AtomicBool = AtomicBool::new(false);
    static RELEASE_READ: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn open_test_file(
        _: *const c_void,
        _: *const std::ffi::c_char,
        _: *const ffi::rptadv_media_cancellation,
        output: *mut ffi::rptadv_media_stream,
    ) -> i32 {
        unsafe {
            (*output).handle = 1_usize as *mut c_void;
            (*output).sample_rate_hz = 48000;
        }
        0
    }

    unsafe extern "C" fn read_test_file(
        _: *mut c_void,
        cancellation: *const ffi::rptadv_media_cancellation,
        output: *mut f32,
        capacity: usize,
        count: *mut usize,
    ) -> i32 {
        if READS.fetch_add(1, Ordering::AcqRel) == 0 {
            unsafe {
                std::slice::from_raw_parts_mut(output, capacity).fill(0.25);
                *count = capacity;
            }
            return 0;
        }
        READING.store(true, Ordering::Release);
        while !RELEASE_READ.load(Ordering::Acquire) {
            let cancelled = unsafe { &*cancellation };
            if unsafe {
                cancelled
                    .is_cancelled
                    .is_some_and(|check| check(cancelled.context) != 0)
            } {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        unsafe { *count = 0 };
        0
    }

    unsafe extern "C" fn close_test_stream(_: *mut c_void) {}
    unsafe extern "C" fn destroy_test_context(_: *mut c_void) {}

    struct ReleaseRead;
    impl Drop for ReleaseRead {
        fn drop(&mut self) {
            RELEASE_READ.store(true, Ordering::Release);
        }
    }

    fn context(
        open_file: Option<
            unsafe extern "C" fn(
                *const c_void,
                *const std::ffi::c_char,
                *const ffi::rptadv_media_cancellation,
                *mut ffi::rptadv_media_stream,
            ) -> i32,
        >,
    ) -> Arc<Context> {
        Arc::new(Context {
            handle: std::ptr::NonNull::new(1_usize as *mut c_void).unwrap(),
            destroy: destroy_test_context,
            read_stream: read_test_file,
            close_stream: close_test_stream,
            open_file,
            open_speech: None,
        })
    }

    #[test]
    fn producer_streams_first_pcm_while_provider_is_still_decoding() {
        READS.store(0, Ordering::Release);
        READING.store(false, Ordering::Release);
        RELEASE_READ.store(false, Ordering::Release);
        let _release = ReleaseRead;
        let mut session = StationSession::new(
            "1000".into(),
            1,
            context(Some(open_test_file)),
            context(None),
        )
        .unwrap();
        let mut reader = session
            .register(MediaSource {
                file: Some("memory.wav".into()),
                speech: None,
            })
            .unwrap();
        session.start().unwrap();
        reader.start();

        let mut output = [0.0; 64];
        let mut streamed = false;
        for _ in 0..1000 {
            if matches!(reader.render(&mut output), PcmRead::Samples(count) if count != 0) {
                streamed = READING.load(Ordering::Acquire)
                    && session.controls[0].completed.load(Ordering::Acquire) != 1
                    && (output[0] - 0.25).abs() < 0.003;
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            streamed,
            "audio must reach the consumer before provider EOF; reads={}, reading={}, completed={}, failed={}",
            READS.load(Ordering::Acquire),
            READING.load(Ordering::Acquire),
            session.controls[0].completed.load(Ordering::Acquire),
            session.controls[0].failed.load(Ordering::Acquire)
        );
        RELEASE_READ.store(true, Ordering::Release);
        let mut finished = false;
        for _ in 0..1000 {
            match reader.render(&mut output) {
                PcmRead::Samples(_) | PcmRead::Pending => {
                    thread::sleep(Duration::from_millis(1));
                }
                PcmRead::Finished => {
                    finished = true;
                    break;
                }
                PcmRead::Failed => panic!("valid streamed media failed"),
            }
        }
        assert!(
            finished,
            "stream must finish after source EOF and ring drain"
        );
    }

    #[test]
    fn station_without_file_or_speech_jobs_does_not_spawn_a_worker() {
        let mut session = StationSession::new(
            "1000".into(),
            1,
            context(Some(open_test_file)),
            context(None),
        )
        .unwrap();

        session.start().unwrap();

        assert!(session.worker.is_none());
    }

    #[test]
    fn reader_fills_requested_output_from_available_ring_samples() {
        let (mut producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
        assert_eq!(producer.write(&[0.25; 4096]).unwrap(), 4096);
        let delay = producer.output_delay_samples().unwrap();
        let control = Arc::new(JobControl::default());
        control.requested.store(1, Ordering::Release);
        control.completed.store(1, Ordering::Release);
        let (_ready, ready_rx) = RingBuffer::new(1);
        let (retired, _retired_rx) = RingBuffer::new(1);
        let mut reader = StreamReader {
            control,
            ready: ready_rx,
            retired,
            generation: 1,
            delay_remaining: delay,
            consumer: Some((1, consumer)),
            emitted_audio: false,
        };
        let mut output = [0.0; 4];

        assert_eq!(reader.render(&mut output), PcmRead::Samples(4));
        assert!(
            output.iter().all(|sample| (*sample - 0.25).abs() < 0.003),
            "delay={delay}; output={output:?}"
        );
    }

    #[test]
    fn cancel_retires_a_consumer_published_before_callback_acquired_it() {
        let (_producer, consumer) = InboundRing::open(48000, InboundPolicy::Media).unwrap();
        let control = Arc::new(JobControl::default());
        control.requested.store(1, Ordering::Release);
        let (mut ready, ready_rx) = RingBuffer::new(1);
        let (retired, mut retired_rx) = RingBuffer::new(1);
        ready.push((1, 0, consumer)).unwrap();
        let mut reader = StreamReader {
            control,
            ready: ready_rx,
            retired,
            generation: 1,
            delay_remaining: 0,
            consumer: None,
            emitted_audio: false,
        };

        reader.cancel();

        assert_eq!(retired_rx.pop().unwrap().0, 1);
    }
}
