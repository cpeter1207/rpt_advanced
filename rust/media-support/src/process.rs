//! Owned offline subprocesses and incremental PCM streams.
use crate::{Config, MediaError};
#[cfg(file_adapter)]
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(file_adapter)]
use std::path::Path;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
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
    loop {
        match poll() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(io_error(error)),
            result => return result.map_err(io_error),
        }
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
    _reaper: ReaperGuard,
    started: Instant,
    timeout: Duration,
    status: Option<ExitStatus>,
}

impl ChildStream {
    /// Spawn an output stream, optionally writing and closing its stdin first.
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
            _reaper: guard,
            started,
            timeout: config.process_timeout,
            status: None,
        };
        stream.stdout = stream.child.0.stdout.take();
        let stdout = stream.stdout.as_ref().ok_or(MediaError::Io)?;
        set_nonblocking(stdout).map_err(io_error)?;
        if let Some(input) = input {
            let Some(mut stdin) = stream.child.0.stdin.take() else {
                return Err(MediaError::Io);
            };
            stdin.write_all(input).map_err(io_error)?;
        }
        Ok(stream)
    }

    /// Read available bytes; returns zero only after a successful child exit.
    pub(crate) fn read(
        &mut self,
        output: &mut [u8],
        cancelled: &impl Fn() -> bool,
    ) -> Result<usize, MediaError> {
        loop {
            self.check_cancel_and_timeout(cancelled)?;
            match self.stdout.as_mut().ok_or(MediaError::Io)?.read(output) {
                Ok(0) => return self.finish(cancelled),
                Ok(count) => return Ok(count),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let _ = self.try_wait()?;
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(io_error(error)),
            }
        }
    }

    fn finish(&mut self, cancelled: &impl Fn() -> bool) -> Result<usize, MediaError> {
        loop {
            self.check_cancel_and_timeout(cancelled)?;
            if let Some(status) = self.try_wait()? {
                return if status.success() {
                    Ok(0)
                } else {
                    Err(MediaError::ProcessFailed)
                };
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn check_cancel_and_timeout(&self, cancelled: &impl Fn() -> bool) -> Result<(), MediaError> {
        if cancelled() {
            Err(MediaError::Cancelled)
        } else if self.started.elapsed() >= self.timeout {
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
    }
}

fn set_nonblocking(stdout: &ChildStdout) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: fcntl only observes and updates flags on the owned pipe descriptor.
        let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
        if flags == -1
            // SAFETY: the descriptor remains owned by stdout during this call.
            || unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                == -1
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Incremental normalized PCM decoded from one owned child process.
pub(crate) struct PcmStream {
    child: ChildStream,
    sample_rate_hz: u32,
    width: usize,
    gain: f32,
    remaining_bytes: Option<u64>,
    input: [u8; 8192],
    input_pos: usize,
    input_len: usize,
    carry: [u8; 4],
    carry_len: usize,
    samples_read: usize,
    empty_error: MediaError,
    ended: bool,
    deferred_error: Option<MediaError>,
}

impl PcmStream {
    /// Read a WAV header and return an incremental mono F32 decoder stream.
    #[cfg(file_adapter)]
    pub(crate) fn f32_wave(
        mut child: ChildStream,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, MediaError> {
        let mut header = [0_u8; 12];
        read_exact(&mut child, &mut header, cancelled)?;
        if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
            return Err(MediaError::InvalidOutput);
        }
        let mut sample_rate_hz = None;
        let data_size = loop {
            let mut chunk_header = [0_u8; 8];
            read_exact(&mut child, &mut chunk_header, cancelled)?;
            let size = u32::from_le_bytes(chunk_header[4..8].try_into().unwrap());
            match &chunk_header[..4] {
                b"fmt " => {
                    if sample_rate_hz.is_some() || !(16..=256).contains(&size) {
                        return Err(MediaError::InvalidOutput);
                    }
                    let mut format = [0_u8; 256];
                    read_exact(&mut child, &mut format[..size as usize], cancelled)?;
                    let rate = u32::from_le_bytes(format[4..8].try_into().unwrap());
                    if u16::from_le_bytes(format[0..2].try_into().unwrap()) != 3
                        || u16::from_le_bytes(format[2..4].try_into().unwrap()) != 1
                        || rate == 0
                        || u16::from_le_bytes(format[12..14].try_into().unwrap()) != 4
                        || u16::from_le_bytes(format[14..16].try_into().unwrap()) != 32
                        || Some(u32::from_le_bytes(format[8..12].try_into().unwrap()))
                            != rate.checked_mul(4)
                    {
                        return Err(MediaError::InvalidOutput);
                    }
                    sample_rate_hz = Some(rate);
                    if size % 2 != 0 {
                        skip_bytes(&mut child, 1, cancelled)?;
                    }
                }
                b"data" => {
                    if sample_rate_hz.is_none() || size % 4 != 0 && size != u32::MAX {
                        return Err(MediaError::InvalidOutput);
                    }
                    break (size != u32::MAX).then_some(u64::from(size));
                }
                _ => {
                    skip_bytes(&mut child, u64::from(size) + u64::from(size % 2), cancelled)?;
                }
            }
        };
        Ok(Self::new(
            child,
            sample_rate_hz.unwrap(),
            4,
            1.0,
            data_size,
            MediaError::InvalidOutput,
        ))
    }

    /// Wrap mono S16 raw output from Piper without waiting for synthesis to finish.
    #[cfg(speech_adapter)]
    pub(crate) fn s16_raw(child: ChildStream, sample_rate_hz: u32, gain: f32) -> Self {
        Self::new(
            child,
            sample_rate_hz,
            2,
            gain,
            None,
            MediaError::ProcessFailed,
        )
    }

    fn new(
        child: ChildStream,
        sample_rate_hz: u32,
        width: usize,
        gain: f32,
        remaining_bytes: Option<u64>,
        empty_error: MediaError,
    ) -> Self {
        Self {
            child,
            sample_rate_hz,
            width,
            gain,
            remaining_bytes,
            input: [0; 8192],
            input_pos: 0,
            input_len: 0,
            carry: [0; 4],
            carry_len: 0,
            samples_read: 0,
            empty_error,
            ended: false,
            deferred_error: None,
        }
    }

    pub(crate) fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Return currently available samples; this waits only for the next pipe data.
    pub(crate) fn read(
        &mut self,
        output: &mut [f32],
        cancelled: &impl Fn() -> bool,
    ) -> Result<usize, MediaError> {
        if output.is_empty() {
            return Err(MediaError::InvalidRequest);
        }
        if let Some(error) = self.deferred_error.take() {
            return Err(error);
        }
        if self.ended {
            return Ok(0);
        }
        let mut written = 0;
        loop {
            while self.input_len - self.input_pos >= self.width && written < output.len() {
                let sample = &self.input[self.input_pos..self.input_pos + self.width];
                let value = if self.width == 2 {
                    f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0
                } else {
                    f32::from_le_bytes(sample.try_into().unwrap())
                } * self.gain;
                self.input_pos += self.width;
                if !value.is_finite() {
                    return self.fail_or_return(written, MediaError::InvalidOutput);
                }
                output[written] = value;
                written += 1;
                self.samples_read += 1;
            }
            if written != 0 {
                return Ok(written);
            }
            if self.input_pos < self.input_len {
                self.carry_len = self.input_len - self.input_pos;
                self.carry[..self.carry_len]
                    .copy_from_slice(&self.input[self.input_pos..self.input_len]);
            }
            self.input_pos = 0;
            self.input_len = self.carry_len;

            if self.remaining_bytes == Some(0) {
                let mut tail = [0_u8; 256];
                while self.child.read(&mut tail, cancelled)? != 0 {}
                return self.end_or_error(0);
            }
            let capacity = self.input.len() - self.carry_len;
            let limit = self
                .remaining_bytes
                .map(|remaining| remaining.min(capacity as u64) as usize)
                .unwrap_or(capacity);
            if limit == 0 {
                return self.fail_or_return(written, MediaError::InvalidOutput);
            }
            let count = match self.child.read(
                &mut self.input[self.carry_len..self.carry_len + limit],
                cancelled,
            ) {
                Ok(count) => count,
                Err(error) => return self.fail_or_return(written, error),
            };
            if let Some(remaining) = &mut self.remaining_bytes {
                *remaining -= count as u64;
            }
            self.input_len += count;
            if count == 0 {
                return self.end_or_error(written);
            }
        }
    }

    fn end_or_error(&mut self, written: usize) -> Result<usize, MediaError> {
        if self.carry_len != 0 || self.remaining_bytes.is_some_and(|remaining| remaining != 0) {
            return self.fail_or_return(written, MediaError::InvalidOutput);
        }
        if self.samples_read == 0 {
            return self.fail_or_return(written, self.empty_error);
        }
        self.ended = true;
        Ok(written)
    }

    fn fail_or_return(&mut self, written: usize, error: MediaError) -> Result<usize, MediaError> {
        if written == 0 {
            Err(error)
        } else {
            self.deferred_error = Some(error);
            Ok(written)
        }
    }
}

#[cfg(file_adapter)]
fn read_exact(
    child: &mut ChildStream,
    bytes: &mut [u8],
    cancelled: &impl Fn() -> bool,
) -> Result<(), MediaError> {
    let mut filled = 0;
    while filled < bytes.len() {
        let count = child.read(&mut bytes[filled..], cancelled)?;
        if count == 0 {
            return Err(MediaError::InvalidOutput);
        }
        filled += count;
    }
    Ok(())
}

#[cfg(file_adapter)]
fn skip_bytes(
    child: &mut ChildStream,
    mut count: u64,
    cancelled: &impl Fn() -> bool,
) -> Result<(), MediaError> {
    let mut scratch = [0_u8; 256];
    while count != 0 {
        let length = count.min(scratch.len() as u64) as usize;
        read_exact(child, &mut scratch[..length], cancelled)?;
        count -= length as u64;
    }
    Ok(())
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
