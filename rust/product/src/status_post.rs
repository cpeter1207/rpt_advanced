//! ASL-compatible status snapshots and request formatting.

use rpt_advanced_core::link::LinkStatus;
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::Duration,
};

/// Owned state captured by the control pump for one status post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StatusSnapshot {
    node: String,
    wall_seconds: i64,
    sequence: u64,
    keyed: bool,
    links: Vec<LinkStatus>,
}

/// One node's effective publication destination and periodic interval.
#[derive(Clone)]
pub(crate) struct PostConfig {
    pub(crate) node: String,
    pub(crate) url: String,
    pub(crate) interval_seconds: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct State {
    keyed: bool,
    links: Vec<LinkStatus>,
}

struct Schedule {
    interval_ms: u64,
    last_state: Option<State>,
    last_sent_state: Option<State>,
    changed_at_ms: u64,
    last_sent_ms: Option<u64>,
    sequence: u64,
}

impl Schedule {
    fn with_sequence(interval_seconds: u64, sequence: u64) -> Self {
        Self {
            interval_ms: interval_seconds.saturating_mul(1000),
            last_state: None,
            last_sent_state: None,
            changed_at_ms: 0,
            last_sent_ms: None,
            sequence,
        }
    }

    fn snapshot(
        &mut self,
        node: &str,
        now_ms: u64,
        wall_seconds: i64,
        keyed: bool,
        links: Vec<LinkStatus>,
    ) -> Option<StatusSnapshot> {
        let state = State { keyed, links };
        if self.last_state.as_ref() != Some(&state) {
            self.last_state = Some(state.clone());
            self.changed_at_ms = now_ms;
        }
        let initial = self.last_sent_ms.is_none();
        let changed = self.last_sent_state.as_ref() != Some(&state)
            && now_ms.saturating_sub(self.changed_at_ms) >= 200;
        let interval = self.last_sent_ms.is_some_and(|last| {
            let period = if keyed { 30_000 } else { self.interval_ms };
            now_ms.saturating_sub(last) >= period
        });
        if !initial && !changed && !interval {
            return None;
        }
        Some(StatusSnapshot::new(
            node,
            wall_seconds,
            self.sequence.saturating_add(1),
            state.keyed,
            state.links,
        ))
    }

    fn sent(&mut self, now_ms: u64, snapshot: &StatusSnapshot) {
        self.sequence = snapshot.sequence;
        self.last_sent_ms = Some(now_ms);
        self.last_sent_state = Some(State {
            keyed: snapshot.keyed,
            links: snapshot.links.clone(),
        });
    }
}

/// Control-plane scheduler and bounded, coalescing asynchronous HTTP publisher.
#[derive(Default)]
pub(crate) struct StatusPostService {
    generation: u64,
    schedules: HashMap<String, Schedule>,
    sequences: HashMap<String, u64>,
    destinations: Vec<PostConfig>,
    worker: Option<StatusPostWorker>,
}

impl StatusPostService {
    pub(crate) fn configure(&mut self, config: Vec<PostConfig>) {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.schedules.clear();
        let config = config
            .into_iter()
            .filter(|node| !node.url.is_empty())
            .collect::<Vec<_>>();
        self.destinations.clone_from(&config);
        if config.is_empty() {
            if let Some(worker) = &self.worker {
                worker.replace(self.generation, config);
            }
            return;
        }
        self.schedules.extend(config.iter().map(|node| {
            (
                node.node.clone(),
                Schedule::with_sequence(
                    node.interval_seconds,
                    self.sequences.get(&node.node).copied().unwrap_or(0),
                ),
            )
        }));
        self.ensure_worker();
        if let Some(worker) = &self.worker {
            worker.replace(self.generation, config);
        }
    }

    pub(crate) fn observe(
        &mut self,
        node: &str,
        now_ms: u64,
        wall_seconds: i64,
        keyed: bool,
        links: Vec<LinkStatus>,
    ) {
        let Some(snapshot) = self
            .schedules
            .get_mut(node)
            .and_then(|schedule| schedule.snapshot(node, now_ms, wall_seconds, keyed, links))
        else {
            return;
        };
        self.ensure_worker();
        if let Some(worker) = &self.worker {
            if worker.submit(self.generation, &snapshot) {
                if let Some(schedule) = self.schedules.get_mut(node) {
                    schedule.sent(now_ms, &snapshot);
                    self.sequences.insert(node.to_owned(), snapshot.sequence);
                }
            }
        }
    }

    fn ensure_worker(&mut self) {
        if self.worker.is_none() {
            self.worker = StatusPostWorker::new();
            if let Some(worker) = &self.worker {
                worker.replace(self.generation, self.destinations.clone());
            }
        }
    }

    /// Stop accepting work without waiting for an in-flight HTTP request.
    pub(crate) fn request_stop(&self) {
        if let Some(worker) = &self.worker {
            worker.request_stop();
        }
    }
}

#[derive(Clone)]
struct Destination {
    generation: u64,
    url: String,
}

#[derive(Default)]
struct Mailbox {
    stopping: bool,
    destinations: HashMap<String, Destination>,
    pending: HashMap<String, (u64, StatusSnapshot)>,
}

/// One bounded worker. Pending snapshots are replaced by node, so slow servers cannot
/// queue stale status or block the serialized Asterisk control executor.
struct StatusPostWorker {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl StatusPostWorker {
    fn new() -> Option<Self> {
        let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let worker_mailbox = Arc::clone(&mailbox);
        let thread = std::thread::Builder::new()
            .name("rptadv-status-post".into())
            .spawn(move || post_loop(worker_mailbox))
            .ok()?;
        Some(Self {
            mailbox,
            thread: Some(thread),
        })
    }

    fn replace(&self, generation: u64, config: Vec<PostConfig>) {
        let (lock, _) = &*self.mailbox;
        let mut mailbox = lock.lock().unwrap_or_else(|error| error.into_inner());
        mailbox.destinations.clear();
        mailbox.pending.clear();
        mailbox.destinations.extend(config.into_iter().map(|node| {
            (
                node.node,
                Destination {
                    generation,
                    url: node.url,
                },
            )
        }));
    }

    fn submit(&self, generation: u64, snapshot: &StatusSnapshot) -> bool {
        let (lock, wake) = &*self.mailbox;
        let mut mailbox = match lock.try_lock() {
            Ok(mailbox) => mailbox,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return false,
        };
        if mailbox
            .destinations
            .get(&snapshot.node)
            .is_some_and(|destination| destination.generation == generation)
        {
            mailbox
                .pending
                .insert(snapshot.node.clone(), (generation, snapshot.clone()));
            wake.notify_one();
            true
        } else {
            false
        }
    }
}

impl Drop for StatusPostWorker {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl StatusPostWorker {
    fn request_stop(&self) {
        let (lock, wake) = &*self.mailbox;
        let mut mailbox = lock.lock().unwrap_or_else(|error| error.into_inner());
        mailbox.stopping = true;
        mailbox.pending.clear();
        drop(mailbox);
        wake.notify_one();
    }
}

fn post_loop(mailbox: Arc<(Mutex<Mailbox>, Condvar)>) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    loop {
        let posts = {
            let (lock, wake) = &*mailbox;
            let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
            while state.pending.is_empty() && !state.stopping {
                state = wake.wait(state).unwrap_or_else(|error| error.into_inner());
            }
            if state.stopping {
                return;
            }
            std::mem::take(&mut state.pending)
        };
        for (node, (generation, snapshot)) in posts {
            let endpoint = {
                let (lock, _) = &*mailbox;
                let state = lock.lock().unwrap_or_else(|error| error.into_inner());
                state
                    .destinations
                    .get(&node)
                    .filter(|destination| !state.stopping && destination.generation == generation)
                    .map(|destination| destination.url.clone())
            };
            if let Some(endpoint) = endpoint {
                if let Ok(url) = request_url(&endpoint, &snapshot) {
                    // Reload can discard pending work, but cannot revoke a request once
                    // it has passed this final generation check and begun network I/O.
                    // Network errors are intentionally nonfatal and URLs may contain private paths.
                    let _ = agent.get(url.as_str()).call();
                }
            }
        }
    }
}

impl StatusSnapshot {
    pub(crate) fn new(
        node: &str,
        wall_seconds: i64,
        sequence: u64,
        keyed: bool,
        links: Vec<LinkStatus>,
    ) -> Self {
        Self {
            node: node.to_owned(),
            wall_seconds,
            sequence,
            keyed,
            links,
        }
    }
}

/// Build an ASL-compatible status URL while preserving endpoint-specific query fields.
pub(crate) fn request_url(
    endpoint: &str,
    snapshot: &StatusSnapshot,
) -> Result<url::Url, url::ParseError> {
    let mut url = url::Url::parse(endpoint)?;
    let retained = url
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "node" | "time" | "seqno" | "keyed" | "nodes"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    let nodes = snapshot
        .links
        .iter()
        .map(|link| {
            let connected = !link.ended && !link.retrying;
            let state = if !connected {
                'C'
            } else if link.mode.transmits() {
                'T'
            } else if link.mode.forwards() {
                'R'
            } else {
                'L'
            };
            format!("{state}{}", link.name)
        })
        .collect::<Vec<_>>()
        .join(",");
    url.set_query(None);
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in retained {
            query.append_pair(&key, &value);
        }
        query
            .append_pair("node", &snapshot.node)
            .append_pair("time", &snapshot.wall_seconds.to_string())
            .append_pair("seqno", &snapshot.sequence.to_string())
            .append_pair("keyed", if snapshot.keyed { "1" } else { "0" })
            .append_pair("nodes", &nodes);
    }
    Ok(url)
}

#[cfg(test)]
#[path = "status_post_tests.rs"]
mod tests;
