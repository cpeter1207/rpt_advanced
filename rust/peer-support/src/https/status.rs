//! Bounded, read-only status streaming on an authenticated upgraded connection.
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use hyper::header::{self, HeaderName, HeaderValue};
use hyper::{HeaderMap, Method, StatusCode, Version, upgrade};
use hyper_util::rt::TokioIo;
use ring::digest::{Context, SHA1_FOR_LEGACY_USE_ONLY};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::{Notify, watch};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Error, Message,
        protocol::{CloseFrame, Role, WebSocketConfig, frame::coding::CloseCode},
    },
};

const PROTOCOL: &str = "rptadv.status.v1";
const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const STATE_LIMIT: usize = 128;
const DATA_BYTES: usize = super::http::RESPONSE_BYTES + 4096;
const IDLE: Duration = Duration::from_secs(15);
const WRITE_TIME: Duration = Duration::from_secs(2);
const FINISH_TIME: Duration = Duration::from_millis(150);

fn tokens(headers: &HeaderMap, name: HeaderName) -> impl Iterator<Item = &str> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(',').map(str::trim))
}

/// Validate an upgrade independently of Product's selection and route authorization.
pub(super) fn accept_key(
    path: &str,
    method: &Method,
    version: Version,
    headers: &HeaderMap,
) -> Result<Option<HeaderValue>, StatusCode> {
    let offered = headers.contains_key(header::UPGRADE)
        || headers.contains_key(header::SEC_WEBSOCKET_KEY)
        || headers.contains_key(header::SEC_WEBSOCKET_PROTOCOL)
        || headers.contains_key(header::SEC_WEBSOCKET_VERSION);
    if method != Method::GET || !path.ends_with("/status/stream/") || !offered {
        return Ok(None);
    }
    let bad = StatusCode::BAD_REQUEST;
    if version != Version::HTTP_11
        || !tokens(headers, header::CONNECTION).any(|v| v.eq_ignore_ascii_case("upgrade"))
        || !super::http::single(headers, header::UPGRADE)?
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        || super::http::single(headers, header::SEC_WEBSOCKET_VERSION)?
            .is_none_or(|v| v.as_bytes() != b"13")
        || !tokens(headers, header::SEC_WEBSOCKET_PROTOCOL).any(|v| v == PROTOCOL)
    {
        return Err(bad);
    }
    let key = super::http::single(headers, header::SEC_WEBSOCKET_KEY)?.ok_or(bad)?;
    if !STANDARD
        .decode(key.as_bytes())
        .is_ok_and(|bytes| bytes.len() == 16)
    {
        return Err(bad);
    }
    let mut digest = Context::new(&SHA1_FOR_LEGACY_USE_ONLY);
    digest.update(key.as_bytes());
    digest.update(GUID);
    HeaderValue::from_str(&STANDARD.encode(digest.finish()))
        .map(Some)
        .map_err(|_| bad)
}

pub(super) fn switch(
    accept: HeaderValue,
) -> hyper::Response<http_body_util::Full<hyper::body::Bytes>> {
    hyper::Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_ACCEPT, accept)
        .header(header::SEC_WEBSOCKET_PROTOCOL, PROTOCOL)
        .header(header::CACHE_CONTROL, "no-store")
        .body(http_body_util::Full::default())
        .expect("validated websocket response")
}

/// Reason a subscriber needs a new capture.
#[derive(Clone, Copy)]
pub enum StatusResyncReason {
    /// A producer lost a state edge.
    ProducerLoss,
    /// A subscriber exhausted its reliable state lane.
    StateOverflow,
    /// A stream writer exceeded its deadline.
    SlowReader,
}
impl StatusResyncReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProducerLoss => "producer_loss",
            Self::StateOverflow => "state_overflow",
            Self::SlowReader => "slow_reader",
        }
    }
}

#[derive(Clone)]
struct MeterBatch {
    records: Arc<[Arc<Value>]>,
    serial: u64,
    offset: usize,
}

struct Inbox {
    events: VecDeque<Arc<Value>>,
    meters: Option<MeterBatch>,
    meter_serial: u64,
    superseded_meters: u64,
    resync: Option<StatusResyncReason>,
    publisher_alive: bool,
    reader_alive: bool,
}

struct Lanes {
    inbox: Mutex<Inbox>,
    wake: Notify,
}

/// Per-subscriber publication handle. No socket work happens in its methods.
pub struct StatusPublisher {
    shared: Arc<Lanes>,
    meters_enabled: bool,
}

impl StatusPublisher {
    /// True if this subscriber still accepts observations.
    pub fn is_open(&self) -> bool {
        let inbox = self.shared.inbox.lock().expect("status stream");
        inbox.reader_alive && inbox.resync.is_none()
    }

    /// Publish an ordered event. Product supplies envelope fields other than type, boot and seq.
    pub fn event(&self, event: Arc<Value>) -> bool {
        let accepted = {
            let mut inbox = self.shared.inbox.lock().expect("status stream");
            if !inbox.reader_alive || inbox.resync.is_some() {
                return false;
            }
            if inbox.events.len() == STATE_LIMIT {
                inbox.resync = Some(StatusResyncReason::StateOverflow);
                false
            } else if event.as_object().is_none_or(|fields| {
                ["type", "boot_id", "seq"]
                    .iter()
                    .any(|key| fields.contains_key(*key))
            }) {
                inbox.resync = Some(StatusResyncReason::ProducerLoss);
                false
            } else {
                inbox.events.push_back(event);
                true
            }
        };
        self.shared.wake.notify_one();
        accepted
    }

    /// Replace the pending meter batch, if the client selected meters.
    pub fn meters(&self, meters: Arc<[Arc<Value>]>) {
        if !self.meters_enabled || meters.is_empty() {
            return;
        }
        let retired = {
            let mut inbox = self.shared.inbox.lock().expect("status stream");
            if !inbox.reader_alive || inbox.resync.is_some() {
                return;
            }
            inbox.meter_serial = inbox.meter_serial.saturating_add(1);
            let batch = MeterBatch {
                records: meters,
                serial: inbox.meter_serial,
                offset: 0,
            };
            let retired = inbox.meters.replace(batch);
            if retired.is_some() {
                inbox.superseded_meters = inbox.superseded_meters.saturating_add(1);
            }
            retired
        };
        drop(retired);
        self.shared.wake.notify_one();
    }

    /// Retire pending old-epoch meters with the Product state transition that invalidates them.
    pub fn invalidate_meters(&self) {
        let retired = self
            .shared
            .inbox
            .lock()
            .expect("status stream")
            .meters
            .take();
        drop(retired);
    }

    /// Report a state gap without waiting for the subscriber.
    pub fn resync(&self, reason: StatusResyncReason) {
        self.shared
            .inbox
            .lock()
            .expect("status stream")
            .resync
            .get_or_insert(reason);
        self.shared.wake.notify_one();
    }
}

impl Drop for StatusPublisher {
    fn drop(&mut self) {
        self.shared
            .inbox
            .lock()
            .expect("status stream")
            .publisher_alive = false;
        self.shared.wake.notify_one();
    }
}

/// Snapshot pages and ordered publications for one admitted status WebSocket.
pub struct StatusStream {
    shared: Arc<Lanes>,
    boot_id: String,
    pages: VecDeque<Value>,
    interval: Option<Duration>,
    meter_due: Instant,
}

enum Outgoing {
    Snapshot(Value),
    Event(Arc<Value>),
    Meters(MeterBatch, u64),
    Resync(StatusResyncReason),
    Closed,
}

impl StatusStream {
    /// Allocate one pair. A missing meter rate disables the meter lane.
    pub fn new(
        boot_id: String,
        snapshot_pages: Vec<Value>,
        meter_hz: Option<u8>,
    ) -> (StatusPublisher, Self) {
        let shared = Arc::new(Lanes {
            inbox: Mutex::new(Inbox {
                events: VecDeque::with_capacity(STATE_LIMIT),
                meters: None,
                meter_serial: 0,
                superseded_meters: 0,
                resync: None,
                publisher_alive: true,
                reader_alive: true,
            }),
            wake: Notify::new(),
        });
        let interval = meter_hz
            .filter(|hz| [1, 2, 5, 10, 20].contains(hz))
            .map(|hz| Duration::from_millis(1_000 / u64::from(hz)));
        (
            StatusPublisher {
                shared: shared.clone(),
                meters_enabled: interval.is_some(),
            },
            Self {
                boot_id,
                shared,
                pages: snapshot_pages.into(),
                interval,
                meter_due: Instant::now(),
            },
        )
    }

    /// Complete the initial capture outside the publisher's state boundary.
    pub fn set_snapshot_pages(&mut self, snapshot_pages: Vec<Value>) {
        self.pages = snapshot_pages.into();
    }

    pub(super) fn ready(&self) -> bool {
        let inbox = self.shared.inbox.lock().expect("status stream");
        !self.pages.is_empty() && inbox.publisher_alive && inbox.resync.is_none()
    }

    async fn next(&mut self, first: bool) -> Outgoing {
        // A gap during the opening handshake can interrupt later pages, but the
        // first frame still identifies the captured cut established by 101.
        if first && let Some(page) = self.pages.pop_front() {
            return Outgoing::Snapshot(page);
        }
        loop {
            let wait_for_meter = {
                let mut inbox = self.shared.inbox.lock().expect("status stream");
                if let Some(reason) = inbox.resync {
                    return Outgoing::Resync(reason);
                }
                if !inbox.publisher_alive {
                    return Outgoing::Closed;
                }
                if let Some(page) = self.pages.pop_front() {
                    return Outgoing::Snapshot(page);
                }
                if let Some(event) = inbox.events.pop_front() {
                    return Outgoing::Event(event);
                }
                if let Some(batch) = &inbox.meters
                    && (batch.offset != 0 || Instant::now() >= self.meter_due)
                {
                    return Outgoing::Meters(batch.clone(), inbox.superseded_meters);
                }
                inbox.meters.is_some()
            };
            tokio::select! {
                _ = self.shared.wake.notified() => {}
                _ = sleep_until(self.meter_due), if wait_for_meter => {}
            }
        }
    }

    // A newer batch, loss, or state edge can arrive while this chunk is encoded.
    // Check the slot again before attempting any write, without waiting under the lock.
    fn admit_meter_chunk(&mut self, batch: &MeterBatch, count: usize) -> bool {
        let mut inbox = self.shared.inbox.lock().expect("status stream");
        if inbox.resync.is_some() || !inbox.events.is_empty() || !inbox.publisher_alive {
            return false;
        }
        let Some(current) = inbox
            .meters
            .as_mut()
            .filter(|current| current.serial == batch.serial && current.offset == batch.offset)
        else {
            return false;
        };
        if count != 0 {
            if current.offset == 0 {
                self.meter_due = Instant::now() + self.interval.expect("meter rate selected");
            }
            current.offset += count;
            if current.offset == current.records.len() {
                inbox.meters = None;
            }
        }
        true
    }
}

impl Drop for StatusStream {
    fn drop(&mut self) {
        let (events, meters) = {
            let mut inbox = self.shared.inbox.lock().expect("status stream");
            inbox.reader_alive = false;
            (std::mem::take(&mut inbox.events), inbox.meters.take())
        };
        drop((events, meters));
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    boot_id: &'a str,
    seq: String,
    #[serde(flatten)]
    payload: Payload<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Payload<'a> {
    Snapshot { data: &'a Value },
    Event(&'a Map<String, Value>),
    MeterCount { superseded_meters: String },
    Resync { reason: &'static str },
}

struct BoundedFrame {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedFrame {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn message(self) -> Option<Message> {
        Some(Message::text(String::from_utf8(self.bytes).ok()?))
    }
}
impl Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MeterBatch {
    fn encode(&self, boot_id: &str, seq: u64, superseded: u64) -> Option<(Message, usize)> {
        let mut frame = BoundedFrame::new(super::http::RESPONSE_BYTES - 2);
        serde_json::to_writer(
            &mut frame,
            &Envelope {
                kind: "meters",
                boot_id,
                seq: seq.to_string(),
                payload: Payload::MeterCount {
                    superseded_meters: superseded.to_string(),
                },
            },
        )
        .ok()?;
        // Append the data array to the generated object. Each item is individually serialized
        // and either retained whole or left for the next bounded message.
        debug_assert_eq!(frame.bytes.last(), Some(&b'}'));
        frame.bytes.pop();
        frame.write_all(br#","data":["#).ok()?;
        let mut count = 0;
        for record in &self.records[self.offset..] {
            let mark = frame.bytes.len();
            if (count != 0 && frame.write_all(b",").is_err())
                || serde_json::to_writer(&mut frame, record.as_ref()).is_err()
            {
                frame.bytes.truncate(mark);
                break;
            }
            count += 1;
        }
        if count == 0 {
            return None;
        }
        frame.limit += 2;
        frame.write_all(b"]}").ok()?;
        Some((frame.message()?, count))
    }
}

impl Outgoing {
    fn encode(&self, boot_id: &str, seq: u64) -> Option<(Message, usize)> {
        let (kind, payload) = match self {
            Self::Snapshot(data) => ("snapshot", Payload::Snapshot { data }),
            Self::Event(event) => ("event", Payload::Event(event.as_object()?)),
            Self::Meters(batch, superseded) => return batch.encode(boot_id, seq, *superseded),
            Self::Resync(reason) => (
                "resync_required",
                Payload::Resync {
                    reason: reason.as_str(),
                },
            ),
            Self::Closed => return None,
        };
        let mut encoded = BoundedFrame::new(DATA_BYTES);
        serde_json::to_writer(
            &mut encoded,
            &Envelope {
                kind,
                boot_id,
                seq: seq.to_string(),
                payload,
            },
        )
        .ok()?;
        Some((encoded.message()?, 0))
    }
}

type Socket = WebSocketStream<TokioIo<upgrade::Upgraded>>;

async fn close(socket: &mut Socket, code: CloseCode, reason: &'static str) {
    let _ = timeout(
        FINISH_TIME,
        socket.send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }))),
    )
    .await;
}

/// Keep the socket and subscription on the exact TLS listener connection owner.
pub(super) async fn run(
    upgraded: upgrade::OnUpgrade,
    mut stream: StatusStream,
    mut authorized: impl FnMut() -> bool,
    mut stop: watch::Receiver<bool>,
) {
    let Ok(Ok(upgraded)) = timeout(WRITE_TIME, upgraded).await else {
        return;
    };
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(DATA_BYTES + 2048)
        .max_message_size(Some(1024))
        .max_frame_size(Some(1024));
    let mut socket =
        WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;
    let mut policy = tokio::time::interval(Duration::from_secs(1));
    let mut idle = Instant::now() + IDLE;
    let mut awaiting_pong = None;
    let mut ping_nonce = 0u64;
    let mut seq = 1u64;
    loop {
        tokio::select! {
            biased;
            _ = async { let _ = stop.wait_for(|stopping| *stopping).await; } => {
                close(&mut socket, CloseCode::Away, "shutdown").await;
                break;
            }
            _ = policy.tick() => {
                if !authorized() {
                    close(&mut socket, CloseCode::Policy, "authorization_expired").await;
                    break;
                }
            }
            _ = sleep_until(idle) => {
                if awaiting_pong.is_some() {
                    close(&mut socket, CloseCode::Away, "heartbeat_timeout").await;
                    break;
                }
                ping_nonce = ping_nonce.wrapping_add(1);
                let payload = ping_nonce.to_be_bytes().to_vec();
                if !matches!(timeout(WRITE_TIME, socket.send(Message::Ping(payload.clone().into()))).await, Ok(Ok(()))) {
                    close(&mut socket, CloseCode::Again, "slow_reader").await;
                    break;
                }
                awaiting_pong = Some(payload);
                idle = Instant::now() + IDLE;
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Ping(_))) => {
                        // Reading Ping queues the required Pong in tungstenite.
                        if !matches!(timeout(WRITE_TIME, socket.flush()).await, Ok(Ok(()))) {
                            close(&mut socket, CloseCode::Again, "slow_reader").await;
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(payload))) => {
                        if awaiting_pong.as_deref() == Some(payload.as_ref()) {
                            awaiting_pong = None;
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        let _ = timeout(FINISH_TIME, socket.flush()).await;
                        break;
                    }
                    Some(Ok(_)) | Some(Err(Error::Capacity(_))) => {
                        close(&mut socket, CloseCode::Policy, "application_frames_forbidden").await;
                        break;
                    }
                    Some(Err(Error::Protocol(_))) => {
                        close(&mut socket, CloseCode::Protocol, "invalid_frame").await;
                        break;
                    }
                    Some(Err(_)) | None => break,
                }
                if awaiting_pong.is_none() {
                    idle = Instant::now() + IDLE;
                }
            }
            outgoing = stream.next(seq == 1) => {
                if matches!(outgoing, Outgoing::Closed) {
                    close(&mut socket, CloseCode::Away, "publisher_stopped").await;
                    break;
                }
                let encoded = outgoing.encode(&stream.boot_id, seq);
                if !authorized() {
                    close(&mut socket, CloseCode::Policy, "authorization_expired").await;
                    break;
                }
                if let Outgoing::Meters(batch, _) = &outgoing
                    && !stream.admit_meter_chunk(batch, encoded.as_ref().map_or(0, |(_, count)| *count))
                {
                    continue;
                }
                let (message, resync) = match encoded {
                    Some((message, _)) => {
                        let reason = match outgoing {
                            Outgoing::Resync(reason) => Some(reason),
                            _ => None,
                        };
                        (message, reason)
                    }
                    None => {
                        let reason = StatusResyncReason::ProducerLoss;
                        let (message, _) = Outgoing::Resync(reason).encode(&stream.boot_id, seq)
                            .expect("bounded resync envelope");
                        (message, Some(reason))
                    }
                };
                if !matches!(timeout(WRITE_TIME, socket.send(message)).await, Ok(Ok(()))) {
                    close(&mut socket, CloseCode::Again, StatusResyncReason::SlowReader.as_str()).await;
                    break;
                }
                if let Some(reason) = resync {
                    close(&mut socket, CloseCode::Again, reason.as_str()).await;
                    break;
                }
                seq = seq.saturating_add(1);
                if awaiting_pong.is_none() {
                    idle = Instant::now() + IDLE;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn state_and_invalidation_can_preempt_the_next_meter_chunk() {
        let (publisher, mut stream) = StatusStream::new("boot".into(), Vec::new(), Some(20));
        publisher.meters(Arc::from([
            Arc::new(json!({"signal":"a"})),
            Arc::new(json!({"signal":"b"})),
        ]));
        let Outgoing::Meters(first, 0) = stream.next(false).await else {
            panic!("expected an initial meter chunk");
        };
        assert_eq!(first.offset, 0);
        assert!(stream.admit_meter_chunk(&first, 1));
        let Outgoing::Meters(second, 0) = stream.next(false).await else {
            panic!("remaining chunks share their initial measurement interval");
        };
        assert_eq!(second.offset, 1);
        assert!(publisher.event(Arc::new(json!({"kind":"graph.removed"}))));
        assert!(!stream.admit_meter_chunk(&second, 1));
        assert!(matches!(stream.next(false).await, Outgoing::Event(_)));
        publisher.invalidate_meters();
        assert!(!stream.admit_meter_chunk(&second, 1));
    }
}
