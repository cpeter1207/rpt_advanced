//! Owned offline subprocesses and private temporary resources.
use crate::Config;
use crate::MediaError;
#[cfg(file_adapter)]
use crate::result::PreparedAudio;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

pub(crate) struct Temporary {
    path: PathBuf,
}

fn temporary_path(parent: &Path, sequence: u64) -> PathBuf {
    parent.join(format!("rptadv-media-{}-{sequence}", std::process::id()))
}

impl Temporary {
    pub(crate) fn new(parent: &Path) -> Result<Self, MediaError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self::new_with_counter(parent, &NEXT)
    }

    fn new_with_counter(parent: &Path, next: &AtomicU64) -> Result<Self, MediaError> {
        for _ in 0..64 {
            let path = temporary_path(parent, next.fetch_add(1, Ordering::Relaxed));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error(error)),
            }
        }
        Err(MediaError::Io)
    }
    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
    pub(crate) fn create(&self, name: &str) -> Result<File, MediaError> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.path(name))
            .map_err(io_error)
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        for name in ["decoded.wav", "speech.wav", "text"] {
            let _ = fs::remove_file(self.path(name));
        }
        let _ = fs::remove_dir(&self.path);
    }
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

pub(crate) fn run(
    config: &Config,
    command: &mut Command,
    cancelled: &impl Fn() -> bool,
) -> Result<(), MediaError> {
    if cancelled() {
        return Err(MediaError::Cancelled);
    }
    let started = Instant::now();
    if let Some(reaper) = config.child_reaper {
        (reaper.acquire)();
    }
    let _reaper = ReaperGuard(config.child_reaper);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;

        // Asterisk host threads may be real-time scheduled. Do not let CPU-heavy
        // Piper or FFmpeg children inherit that policy and compete with audio delivery.
        unsafe {
            command.pre_exec(normal_scheduler);
        }
    }
    let mut child = OwnedChild(
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(io_error)?,
    );
    loop {
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        if started.elapsed() >= config.process_timeout {
            return Err(MediaError::TimedOut);
        }
        if let Some(status) = poll_child(&mut || child.0.try_wait())? {
            return if status.success() {
                Ok(())
            } else {
                Err(MediaError::ProcessFailed)
            };
        }
        std::thread::sleep(
            Duration::from_millis(10).min(config.process_timeout.saturating_sub(started.elapsed())),
        );
    }
}

#[cfg(file_adapter)]
pub(crate) fn decode(
    config: &Config,
    input: File,
    cancelled: &impl Fn() -> bool,
) -> Result<PreparedAudio, MediaError> {
    let temporary = Temporary::new(&config.temporary_directory)?;
    drop(temporary.create("decoded.wav")?);
    let mut command = Command::new(&config.ffmpeg);
    command.args([
        "-nostdin",
        "-v",
        "error",
        "-y",
        "-protocol_whitelist",
        "file,pipe",
        "-i",
        "pipe:0",
        "-map",
        "0:a:0",
        "-vn",
        "-ac",
        "1",
    ]);
    command
        .args(["-c:a", "pcm_f32le", "-f", "wav"])
        .arg(temporary.path("decoded.wav"))
        .stdin(Stdio::from(input));
    run(config, &mut command, cancelled)?;
    let bytes = fs::read(temporary.path("decoded.wav")).map_err(io_error)?;
    crate::wave::parse(&bytes)
}

#[cfg(all(test, unix))]
#[path = "process_tests.rs"]
mod tests;
