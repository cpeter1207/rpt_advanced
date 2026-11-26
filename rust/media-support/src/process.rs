//! Owned offline subprocesses and incremental PCM streams.
use crate::{Config, MediaError, abi::*};
#[cfg(file_adapter)]
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
#[cfg(file_adapter)]
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub(crate) fn io_error(error: io::Error) -> MediaError {
    if error.kind() == io::ErrorKind::NotFound {
        MediaError::Unavailable
    } else {
        MediaError::Io
    }
}

#[cfg(file_adapter)]
pub(crate) fn open_local(path: &Path) -> Result<File, MediaError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let input = options.open(path).map_err(io_error)?;
    if !input.metadata().map_err(io_error)?.file_type().is_file() {
        return Err(MediaError::Unavailable);
    }
    Ok(input)
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // The child remains owned until reaped; kill never targets a reused PID.
        let _ = self.0.kill();
        wait_until_reaped(&mut || self.0.wait().map(|_| ()))
    }
}

fn wait_until_reaped(wait: &mut dyn FnMut() -> io::Result<()>) {
    while let Err(error) = wait() {
        if error.kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
}

fn poll_child(
    poll: &mut dyn FnMut() -> io::Result<Option<ExitStatus>>,
) -> Result<Option<ExitStatus>, MediaError> {
    match poll() {
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(None),
        result => result.map_err(io_error),
    }
}

struct ReaperGuard(Option<crate::ChildReaper>);
impl Drop for ReaperGuard {
    fn drop(&mut self) {
        if let Some(reaper) = self.0 {
            (reaper.release)();
        }
    }
}

/// A bounded-read subprocess stream owned until it exits or is cancelled.
pub(crate) struct ChildStream {
    child: OwnedChild,
    stdout: Option<ChildStdout>,
    stdin: Option<ChildStdin>,
    pending_input: Vec<u8>,
    input_position: usize,
    _reaper: ReaperGuard,
    started: Instant,
    timeout: Duration,
    status: Option<ExitStatus>,
    stdout_eof: bool,
}

impl ChildStream {
    /// Spawn without blocking on stdin; reads interleave bounded input/output progress.
    pub(crate) fn spawn(
        config: &Config,
        command: &mut Command,
        input: Option<&[u8]>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, MediaError> {
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        let started = Instant::now();
        let input_bytes = input.unwrap_or_default();
        let mut pending_input = Vec::new();
        pending_input
            .try_reserve_exact(input_bytes.len())
            .map_err(|_| MediaError::Io)?;
        pending_input.extend_from_slice(input_bytes);
        if let Some(reaper) = config.child_reaper {
            (reaper.acquire)();
        }
        let guard = ReaperGuard(config.child_reaper);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;

            // Real-time host threads must not pass their policy to media children.
            unsafe {
                command.pre_exec(normal_scheduler);
            }
        }
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(io_error)?;
        let mut stream = Self {
            child: OwnedChild(child),
            stdout: None,
            stdin: None,
            pending_input,
            input_position: 0,
            _reaper: guard,
            started,
            timeout: config.process_timeout,
            status: None,
            stdout_eof: false,
        };
        stream.stdout = stream.child.0.stdout.take();
        let stdout = stream.stdout.as_ref().ok_or(MediaError::Io)?;
        set_nonblocking(stdout).map_err(io_error)?;
        stream.stdin = stream
            .child
            .0
            .stdin
            .take()
            .filter(|_| !stream.pending_input.is_empty());
        if let Some(stdin) = &stream.stdin {
            set_nonblocking(stdin).map_err(io_error)?;
        }
        Ok(stream)
    }

    /// Offer one block of input and attempt one output read without waiting.
    /// None means pending; zero means stdout closed and the child succeeded.
    pub(crate) fn read(
        &mut self,
        output: &mut [u8],
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<usize>, MediaError> {
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        self.check_timeout()?;
        self.pump_input()?;
        if !self.stdout_eof {
            let stdout = self.stdout.as_mut().ok_or(MediaError::Io)?;
            match stdout.read(output) {
                Ok(0) => self.stdout_eof = true,
                Ok(count) => return Ok(Some(count)),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let _ = self.try_wait()?;
                    return Ok(None);
                }
                Err(error) => return Err(io_error(error)),
            }
        }
        match self.try_wait()? {
            Some(status) if status.success() => Ok(Some(0)),
            Some(_) => Err(MediaError::ProcessFailed),
            None => Ok(None),
        }
    }

    fn pump_input(&mut self) -> Result<(), MediaError> {
        let Some(stdin) = &mut self.stdin else {
            return Ok(());
        };
        let end = (self.input_position + 8192).min(self.pending_input.len());
        match stdin.write(&self.pending_input[self.input_position..end]) {
            // A nonempty write to this owned nonblocking pipe makes progress or errors.
            Ok(count) => self.input_position += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(io_error(error)),
        }
        if self.input_position == self.pending_input.len() {
            self.stdin.take();
            self.pending_input.clear();
        }
        Ok(())
    }

    fn check_timeout(&self) -> Result<(), MediaError> {
        if self.started.elapsed() >= self.timeout {
            Err(MediaError::TimedOut)
        } else {
            Ok(())
        }
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, MediaError> {
        if self.status.is_none() {
            self.status = poll_child(&mut || self.child.0.try_wait())?;
        }
        Ok(self.status)
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> u32 {
        self.child.0.id()
    }
}

impl Drop for ChildStream {
    fn drop(&mut self) {
        // Close the pipe first so a child blocked writing can be killed and reaped.
        self.stdout.take();
        self.stdin.take();
    }
}

#[cfg(unix)]
fn set_nonblocking(pipe: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    set_nonblocking_fd(pipe.as_raw_fd())
}

#[cfg(not(unix))]
fn set_nonblocking<T>(_pipe: &T) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(unix)]
fn set_nonblocking_fd(fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: fcntl only observes and updates flags on the owned pipe descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1
            // SAFETY: the descriptor remains owned by stdout during this call.
            || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Incremental normalized PCM decoded from one owned child process.
pub(crate) struct PcmStream {
    child: ChildStream,
    #[cfg(file_adapter)]
    wave: Option<WaveHeader>,
    sample_rate_hz: u32,
    format_announced: bool,
    width: usize,
    gain: f32,
    remaining_bytes: Option<u64>,
    input: [u8; 8192],
    input_pos: usize,
    input_len: usize,
    has_samples: bool,
    empty_error: MediaError,
    ended: bool,
    deferred_error: Option<MediaError>,
}

impl PcmStream {
    /// Own the WAV child before any header data has arrived.
    #[cfg(file_adapter)]
    pub(crate) fn f32_wave(child: ChildStream) -> Self {
        let mut stream = Self::new(child, 0, 4, 1.0, MediaError::InvalidOutput);
        stream.wave = Some(WaveHeader::new());
        stream
    }

    /// Wrap mono S16 raw output from Piper without waiting for synthesis.
    #[cfg(speech_adapter)]
    pub(crate) fn s16_raw(child: ChildStream, sample_rate_hz: u32, gain: f32) -> Self {
        Self::new(child, sample_rate_hz, 2, gain, MediaError::ProcessFailed)
    }

    fn new(
        child: ChildStream,
        sample_rate_hz: u32,
        width: usize,
        gain: f32,
        empty_error: MediaError,
    ) -> Self {
        Self {
            child,
            #[cfg(file_adapter)]
            wave: None,
            sample_rate_hz,
            format_announced: false,
            width,
            gain,
            remaining_bytes: None,
            input: [0; 8192],
            input_pos: 0,
            input_len: 0,
            has_samples: false,
            empty_error,
            ended: false,
            deferred_error: None,
        }
    }

    /// Advance at most one input buffer or emit the next format/terminal event.
    pub(crate) fn step(
        &mut self,
        output: &mut [MaybeUninit<f32>],
        cancelled: &impl Fn() -> bool,
    ) -> Result<RawStep, MediaError> {
        if output.is_empty() {
            return Err(MediaError::InvalidRequest);
        }
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        if let Some(error) = self.deferred_error.take() {
            return Err(error);
        }
        if self.ended {
            return Ok(RawStep {
                kind: STEP_EOF,
                ..RawStep::default()
            });
        }
        self.child.check_timeout()?;

        #[cfg(file_adapter)]
        if let Some(wave) = &mut self.wave {
            let Some(count) = self.child.read(&mut self.input, cancelled)? else {
                return Ok(RawStep::default());
            };
            if count == 0 {
                return Err(MediaError::InvalidOutput);
            }
            let mut bytes = &self.input[..count];
            let Some((rate, remaining)) = wave.consume(&mut bytes)? else {
                return Ok(RawStep::default());
            };
            self.sample_rate_hz = rate;
            self.remaining_bytes = remaining;
            self.input_pos = count - bytes.len();
            self.input_len = count;
            self.wave = None;
        }
        if !self.format_announced {
            self.format_announced = true;
            return Ok(RawStep {
                kind: STEP_FORMAT_READY,
                sample_rate_hz: self.sample_rate_hz,
                ..RawStep::default()
            });
        }

        // Keep incomplete samples across reads, but discard bytes past a finite
        // data chunk. The child must still exit successfully before EOF.
        let mut available = self.input_len - self.input_pos;
        if self.remaining_bytes == Some(0) {
            available = 0;
            self.input_pos = self.input_len;
        }
        if available < self.width {
            self.input.copy_within(self.input_pos..self.input_len, 0);
            self.input_pos = 0;
            self.input_len = available;
            let Some(count) = self.child.read(&mut self.input[available..], cancelled)? else {
                return Ok(RawStep::default());
            };
            if count == 0 {
                return self.finish();
            }
            if self.remaining_bytes == Some(0) {
                return Ok(RawStep::default());
            }
            self.input_len += count;
            available += count;
        }

        let mut count = (available / self.width).min(output.len());
        if let Some(remaining) = self.remaining_bytes {
            count = count.min((remaining / self.width as u64) as usize);
        }
        for (written, output) in output[..count].iter_mut().enumerate() {
            let bytes = &self.input[self.input_pos..self.input_pos + self.width];
            let sample = if self.width == 2 {
                f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0
            } else {
                f32::from_le_bytes(bytes.try_into().unwrap())
            } * self.gain;
            self.input_pos += self.width;
            if let Some(remaining) = &mut self.remaining_bytes {
                *remaining -= self.width as u64;
            }
            if !sample.is_finite() {
                if written == 0 {
                    return Err(MediaError::InvalidOutput);
                }
                self.deferred_error = Some(MediaError::InvalidOutput);
                return Ok(sample_step(written));
            }
            output.write(sample);
            self.has_samples = true;
        }
        Ok(if count == 0 {
            RawStep::default()
        } else {
            sample_step(count)
        })
    }

    fn finish(&mut self) -> Result<RawStep, MediaError> {
        if self.input_len != self.input_pos
            || self.remaining_bytes.is_some_and(|remaining| remaining != 0)
        {
            return Err(MediaError::InvalidOutput);
        }
        if !self.has_samples {
            return Err(self.empty_error);
        }
        self.ended = true;
        Ok(RawStep {
            kind: STEP_EOF,
            ..RawStep::default()
        })
    }
}

fn sample_step(samples: usize) -> RawStep {
    RawStep {
        kind: STEP_SAMPLES,
        samples,
        ..RawStep::default()
    }
}

#[cfg(file_adapter)]
#[derive(Clone, Copy)]
enum WavePart {
    Riff,
    Chunk,
    Format(u32),
    Skip(u64),
}

#[cfg(file_adapter)]
struct WaveHeader {
    part: WavePart,
    bytes: [u8; 256],
    filled: usize,
    rate: Option<u32>,
}

#[cfg(file_adapter)]
impl WaveHeader {
    fn new() -> Self {
        Self {
            part: WavePart::Riff,
            bytes: [0; 256],
            filled: 0,
            rate: None,
        }
    }

    fn consume(&mut self, input: &mut &[u8]) -> Result<Option<(u32, Option<u64>)>, MediaError> {
        while !input.is_empty() {
            self.part = match self.part {
                WavePart::Riff => {
                    if !self.collect(input, 12) {
                        break;
                    }
                    if &self.bytes[..4] != b"RIFF" || &self.bytes[8..12] != b"WAVE" {
                        return Err(MediaError::InvalidOutput);
                    }
                    WavePart::Chunk
                }
                WavePart::Chunk => {
                    if !self.collect(input, 8) {
                        break;
                    }
                    let size = u32::from_le_bytes(self.bytes[4..8].try_into().unwrap());
                    match &self.bytes[..4] {
                        b"fmt " => {
                            if self.rate.is_some() || !(16..=256).contains(&size) {
                                return Err(MediaError::InvalidOutput);
                            }
                            WavePart::Format(size)
                        }
                        b"data" => {
                            let rate = self.rate.ok_or(MediaError::InvalidOutput)?;
                            if size % 4 != 0 && size != u32::MAX {
                                return Err(MediaError::InvalidOutput);
                            }
                            return Ok(Some((rate, (size != u32::MAX).then_some(u64::from(size)))));
                        }
                        _ if size == 0 => WavePart::Chunk,
                        _ => WavePart::Skip(u64::from(size) + u64::from(size % 2)),
                    }
                }
                WavePart::Format(size) => {
                    if !self.collect(input, size as usize) {
                        break;
                    }
                    self.rate = Some(wave_rate(&self.bytes)?);
                    if size % 2 == 0 {
                        WavePart::Chunk
                    } else {
                        WavePart::Skip(1)
                    }
                }
                WavePart::Skip(remaining) => {
                    let count = remaining.min(input.len() as u64) as usize;
                    *input = &input[count..];
                    match remaining - count as u64 {
                        0 => WavePart::Chunk,
                        rest => WavePart::Skip(rest),
                    }
                }
            };
        }
        Ok(None)
    }

    /// Consume this fixed-size part while retaining any incomplete prefix.
    fn collect(&mut self, input: &mut &[u8], length: usize) -> bool {
        let count = (length - self.filled).min(input.len());
        self.bytes[self.filled..self.filled + count].copy_from_slice(&input[..count]);
        self.filled += count;
        *input = &input[count..];
        if self.filled != length {
            return false;
        }
        self.filled = 0;
        true
    }
}

#[cfg(file_adapter)]
fn wave_rate(format: &[u8]) -> Result<u32, MediaError> {
    let rate = u32::from_le_bytes(format[4..8].try_into().unwrap());
    if u16::from_le_bytes(format[0..2].try_into().unwrap()) != 3
        || u16::from_le_bytes(format[2..4].try_into().unwrap()) != 1
        || rate == 0
        || u16::from_le_bytes(format[12..14].try_into().unwrap()) != 4
        || u16::from_le_bytes(format[14..16].try_into().unwrap()) != 32
        || Some(u32::from_le_bytes(format[8..12].try_into().unwrap())) != rate.checked_mul(4)
    {
        return Err(MediaError::InvalidOutput);
    }
    Ok(rate)
}

#[cfg(target_os = "linux")]
fn set_normal_scheduler(
    set: impl FnOnce(libc::c_int, &libc::sched_param) -> libc::c_int,
) -> io::Result<()> {
    if set(libc::SCHED_OTHER, &libc::sched_param { sched_priority: 0 }) == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
/// Normalize the calling execution context before it becomes a media subprocess.
fn normal_scheduler() -> io::Result<()> {
    set_normal_scheduler(|policy, parameters| unsafe {
        libc::sched_setscheduler(0, policy, parameters)
    })
}

#[cfg(all(test, unix))]
#[path = "process_tests.rs"]
mod tests;
