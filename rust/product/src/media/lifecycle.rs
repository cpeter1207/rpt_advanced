//! Shared setup and retirement workers for exclusively owned, transferable provider streams.
use super::{Cancellation, Context, JobControl, ProviderStream, raw_cancellation};
use rpt_advanced_core::media::{MediaError, SpeechSource};
use std::{
    collections::VecDeque,
    ffi::CString,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, atomic::AtomicBool},
    thread::{self, JoinHandle},
};

pub(super) enum Input {
    File(PathBuf),
    Speech(SpeechSource),
}

pub(super) struct Request {
    pub(super) context: Arc<Context>,
    pub(super) input: Input,
    pub(super) control: Arc<JobControl>,
    pub(super) station: Arc<AtomicBool>,
    pub(super) generation: u64,
}

impl Request {
    fn open(self) -> Result<ProviderStream, MediaError> {
        if self.control.is_cancelled(&self.station, self.generation) {
            return Err(MediaError::Cancelled);
        }
        let cancel = Cancellation {
            control: &self.control,
            stopping: &self.station,
            generation: self.generation,
        };
        let cancel = raw_cancellation(&cancel);
        let context = self.context;
        match self.input {
            Input::File(path) => {
                let path = super::super::path_string(&path)?;
                let open = context.open_file.ok_or(MediaError::IncompatibleAdapter)?;
                // SAFETY: the context is pinned and strings and token are borrowed by this call.
                context.open(|out| unsafe {
                    open(context.handle.as_ptr(), path.as_ptr(), &cancel, out)
                })
            }
            Input::Speech(input) => {
                let text = CString::new(input.text).map_err(|_| MediaError::InvalidRequest)?;
                let model = super::super::path_string(&input.model)?;
                let input = crate::abi::rptadv_media_speech_request {
                    text: text.as_ptr(),
                    model: model.as_ptr(),
                    speed_percent: input.speed_percent,
                    level_db: input.level_db,
                };
                let open = context.open_speech.ok_or(MediaError::IncompatibleAdapter)?;
                // SAFETY: the borrowed fields, cancellation and context remain valid through open.
                context.open(|out| unsafe { open(context.handle.as_ptr(), &input, &cancel, out) })
            }
        }
    }
}

enum Phase {
    Queued(Request),
    Opening,
    Ready(Result<ProviderStream, MediaError>),
    Active,
    Retire(ProviderStream),
    Closing,
}

struct Entry {
    id: u64,
    station: Arc<AtomicBool>,
    phase: Phase,
    abandoned: bool,
}

#[derive(Default)]
struct Entries {
    next_id: u64,
    jobs: VecDeque<Entry>,
    stop: bool,
}

impl Entries {
    fn index(&self, id: u64) -> usize {
        // IDs increase on append, and removal preserves the remaining order.
        self.jobs
            .binary_search_by_key(&id, |entry| entry.id)
            .unwrap()
    }
}

#[derive(Default)]
struct Registry {
    state: Mutex<Entries>,
    changed: Condvar,
}

pub(in crate::media) struct Lifecycle {
    shared: Arc<Registry>,
    workers: Vec<JoinHandle<()>>,
}

enum Work {
    Open(u64, Request),
    Close(u64, ProviderStream),
}

impl Lifecycle {
    pub(in crate::media) fn new() -> Result<Self, MediaError> {
        let mut this = Self {
            shared: Arc::new(Registry::default()),
            workers: Vec::new(),
        };
        // One setup and one cleanup lane are independent. Additional workers scale with available CPUs.
        let lanes = thread::available_parallelism().map_or(1, usize::from);
        for retire in [true, false] {
            for _ in 0..lanes {
                let shared = Arc::clone(&this.shared);
                let name = if retire {
                    "rpt-media-reap"
                } else {
                    "rpt-media-open"
                };
                this.workers.push(
                    thread::Builder::new()
                        .name(name.into())
                        .spawn(move || run(&shared, retire))
                        .map_err(|_| MediaError::Io)?,
                );
            }
        }
        Ok(this)
    }

    pub(super) fn submit(&self, request: Request) -> Result<Ticket, MediaError> {
        let mut state = self.shared.state.lock().unwrap();
        let id = state.next_id.checked_add(1).ok_or(MediaError::Io)?;
        state.jobs.try_reserve(1).map_err(|_| MediaError::Io)?;
        state.next_id = id;
        state.jobs.push_back(Entry {
            id,
            station: Arc::clone(&request.station),
            phase: Phase::Queued(request),
            abandoned: false,
        });
        self.shared.changed.notify_all();
        Ok(Ticket {
            shared: Arc::clone(&self.shared),
            id,
            stream: None,
        })
    }

    pub(super) fn drain_station(&self, station: &Arc<AtomicBool>) {
        let state = self.shared.state.lock().unwrap();
        drop(
            self.shared
                .changed
                .wait_while(state, |s| {
                    s.jobs.iter().any(|e| Arc::ptr_eq(&e.station, station))
                })
                .unwrap(),
        );
    }
}

impl Drop for Lifecycle {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().stop = true;
        self.shared.changed.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn run(shared: &Registry, retire: bool) {
    loop {
        let work = {
            let mut state = shared.state.lock().unwrap();
            loop {
                let job = state.jobs.iter_mut().find(|e| {
                    matches!(
                        (&e.phase, retire),
                        (Phase::Retire(_), true) | (Phase::Queued(_), false)
                    )
                });
                if let Some(entry) = job {
                    let phase = std::mem::replace(
                        &mut entry.phase,
                        if retire {
                            Phase::Closing
                        } else {
                            Phase::Opening
                        },
                    );
                    break match phase {
                        Phase::Retire(stream) => Work::Close(entry.id, stream),
                        Phase::Queued(request) => Work::Open(entry.id, request),
                        _ => unreachable!(),
                    };
                }
                if state.stop && state.jobs.is_empty() {
                    return;
                }
                state = shared.changed.wait(state).unwrap();
            }
        };
        match work {
            Work::Open(id, request) => {
                let result = request.open();
                let mut state = shared.state.lock().unwrap();
                let pos = state.index(id);
                if state.jobs[pos].abandoned {
                    match result {
                        Ok(stream) => state.jobs[pos].phase = Phase::Retire(stream),
                        Err(_) => {
                            state.jobs.remove(pos);
                        }
                    }
                } else {
                    state.jobs[pos].phase = Phase::Ready(result);
                }
            }
            Work::Close(id, stream) => {
                drop(stream);
                let mut state = shared.state.lock().unwrap();
                let pos = state.index(id);
                state.jobs.remove(pos);
            }
        }
        shared.changed.notify_all();
    }
}

/// Registration reserves its retirement slot before any descriptor call, so Drop only moves ownership.
pub(super) struct Ticket {
    shared: Arc<Registry>,
    id: u64,
    pub(super) stream: Option<ProviderStream>,
}

impl Ticket {
    pub(super) fn take_ready(&mut self) -> Result<bool, MediaError> {
        let mut state = self.shared.state.lock().unwrap();
        let pos = state.index(self.id);
        if !matches!(state.jobs[pos].phase, Phase::Ready(_)) {
            return Ok(false);
        }
        let Phase::Ready(result) = std::mem::replace(&mut state.jobs[pos].phase, Phase::Active)
        else {
            unreachable!()
        };
        self.stream = Some(result?);
        Ok(true)
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        let pos = state.index(self.id);
        let entry = &mut state.jobs[pos];
        entry.abandoned = true;
        match std::mem::replace(&mut entry.phase, Phase::Opening) {
            Phase::Ready(Ok(s)) => entry.phase = Phase::Retire(s),
            Phase::Active if self.stream.is_some() => {
                entry.phase = Phase::Retire(self.stream.take().unwrap())
            }
            Phase::Opening => {}
            Phase::Queued(_) | Phase::Ready(Err(_)) | Phase::Active => {
                state.jobs.remove(pos);
            }
            _ => unreachable!(),
        }
        self.shared.changed.notify_all();
    }
}
