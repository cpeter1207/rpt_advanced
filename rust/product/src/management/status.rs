//! Single publication boundary and read-only management routes for status.
pub(crate) mod edges;
mod model;
mod pages;
mod peer;
mod query;

use super::{Api, Fault, Id, Observed, Snapshot, fresh, response};
use model::{Capture, Owner, RecordKey, Stamp};
use pages::{Pages, scoped};
pub(super) fn scoped_view(path: &str, query: &str) -> String {
    scoped(path, query)
}
use query::{Query, Selection};
use rpt_advanced_core::management::routes::{Route, Target};
use rptadv_peer_support::https::{
    AdmittedRequest, Method, Response, ServiceId, StatusCode, StatusPublisher, StatusResyncReason,
    StatusStream,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(super) const SNAPSHOT: &str = "/status/snapshot/";
pub(super) const STREAM: &str = "/status/stream/";

/// Capability descriptor for HAL and generated OpenAPI discovery.
pub(super) fn profile(stream: bool) -> Value {
    let mut fields = json!({
        "default_scope":"site",
        "site_value":true,
        "repeatable_parameters":["port"],
        "topics":query::Topic::ALL,
        "default_topics":query::Topic::STATE,
        "meters_opt_in":true,
    });
    if stream {
        fields["subprotocol"] = json!("rptadv.status.v1");
        fields["frame_types"] = json!(["snapshot", "event", "meters", "resync_required"]);
        fields["default_meter_hz"] = json!(20);
        fields["meter_hz"] = json!([1, 2, 5, 10, 20]);
        fields["connection"] = json!({"authentication":"mutual_tls","browser_origin":"allow_list",
            "read_only":true,"idle_ping_seconds":15,"pong_timeout_seconds":15});
    }
    fields
}

const PAGE_TTL: Duration = Duration::from_secs(20);
const CACHE_BYTES: usize = 32 * 1024 * 1024;
const CACHE_CAPTURES: usize = 8;
const FAST_RECORDS: usize = 4096;
const LINK_DIAGNOSTIC_INTERVAL_MS: u64 = 1_000;

struct Subscriber {
    selection: Selection,
    sink: StatusPublisher,
}

struct Continuation {
    id: Id,
    reader: ServiceId,
    expires: Instant,
    pages: Arc<Pages>,
}

struct FastRecord {
    station_generation: u64,
    value: Option<Arc<Value>>,
}

#[derive(Default)]
struct Published {
    latest: Option<Arc<Capture>>,
    cursor: u64,
    subscribers: Vec<Subscriber>,
    cache: VecDeque<Continuation>,
    fast: BTreeMap<RecordKey, FastRecord>,
}

impl Published {
    fn capture(&self, selection: &Selection) -> Result<Arc<Capture>, Fault> {
        let capture = self.latest.as_ref().ok_or(Fault::Unavailable)?;
        if selection
            .ports
            .iter()
            .any(|id| !capture.visible_ports.contains(id))
        {
            return Err(Fault::Missing);
        }
        Ok(Arc::clone(capture))
    }

    fn subscribe(
        &mut self,
        selection: Selection,
        sink: StatusPublisher,
    ) -> Result<Arc<Capture>, Fault> {
        let capture = self.capture(&selection)?;
        self.subscribers
            .retain(|subscriber| subscriber.sink.is_open());
        self.subscribers
            .try_reserve(1)
            .map_err(|_| Fault::Unavailable)?;
        self.subscribers.push(Subscriber { selection, sink });
        Ok(capture)
    }

    fn publish(&mut self, mut next: Capture, owners: &BTreeMap<Id, u64>) {
        if let Some(previous) = self.latest.as_ref()
            && previous.stamp.monotonic_ms / LINK_DIAGNOSTIC_INTERVAL_MS
                == next.stamp.monotonic_ms / LINK_DIAGNOSTIC_INTERVAL_MS
        {
            // Packet and jitter counters change almost every block. Preserve
            // their latest diagnostic sample without delaying key or link edges.
            for (key, record) in &mut next.records {
                if key.polled_fields().contains(&"media")
                    && let Some(media) = previous.records.get(key).and_then(|row| row.get("media"))
                    && media.is_object()
                    && let Some(current) = record.get("media").filter(|value| value.is_object())
                    && current != media
                {
                    Arc::make_mut(record)["media"] = media.clone();
                }
            }
        }
        self.fast.retain(|key, record| {
            let Owner::Port(port) = key.owner else {
                return false;
            };
            if owners.get(&port) != Some(&record.station_generation) {
                return false;
            }
            // The polling projection catches up to removal once its old row disappears.
            if record.value.is_none() && !next.records.contains_key(key) {
                return false;
            }
            match &record.value {
                Some(value) => {
                    let mut observed = Arc::clone(value);
                    if let Some(poll) = next.records.get(key) {
                        for field in key.polled_fields() {
                            if let Some(new) = poll.get(field)
                                && observed.get(field) != Some(new)
                            {
                                Arc::make_mut(&mut observed)[*field] = new.clone();
                            }
                        }
                    }
                    if value["quality"] == "valid" && next.source_stale(key) {
                        Arc::make_mut(&mut observed)["quality"] = json!("stale");
                    }
                    next.records.insert(key.clone(), observed);
                }
                None => {
                    next.records.remove(key);
                }
            }
            true
        });
        let attached = next
            .records
            .iter()
            .filter_map(|(key, value)| key.live_entity(value))
            .collect::<BTreeSet<_>>();
        next.records.retain(|key, _| {
            key.dependency
                .as_ref()
                .is_none_or(|entity| attached.contains(entity))
        });
        self.subscribers
            .retain(|subscriber| subscriber.sink.is_open());
        next.cursor = self.cursor;
        if let Some(previous) = self.latest.as_ref() {
            let removals = previous.records.iter().filter_map(|(key, old)| {
                (!key.is_meter() && !next.records.contains_key(key)).then_some((
                    key,
                    Some(old.as_ref()),
                    None,
                ))
            });
            let upserts = next.records.iter().filter_map(|(key, new)| {
                let old = previous.records.get(key).map(Arc::as_ref);
                (!key.is_meter() && old != Some(new.as_ref())).then_some((
                    key,
                    old,
                    Some(new.as_ref()),
                ))
            });
            // Drop the pending measurement lane before any topology event can retire its key.
            if removals.clone().next().is_some() {
                for subscriber in &self.subscribers {
                    subscriber.sink.invalidate_meters();
                }
            }
            for (key, old, new) in removals.chain(upserts) {
                next.cursor = next.cursor.saturating_add(1);
                let mut event = None;
                for subscriber in self
                    .subscribers
                    .iter()
                    .filter(|sub| key.selected(&sub.selection))
                {
                    let value = event.get_or_insert_with(|| {
                        Arc::new(key.event(next.cursor, &next.stamp, old, new))
                    });
                    subscriber.sink.event(Arc::clone(value));
                }
            }
        }
        let mut batches = BTreeMap::<&Selection, Arc<[Arc<Value>]>>::new();
        for subscriber in self
            .subscribers
            .iter()
            .filter(|sub| sub.selection.topics.contains(&query::Topic::Meters))
        {
            let selection = &subscriber.selection;
            let meters = batches.entry(selection).or_insert_with(|| {
                next.records
                    .iter()
                    .filter(|(key, _)| key.is_meter() && key.selected(selection))
                    .map(|(_, value)| Arc::clone(value))
                    .collect()
            });
            subscriber.sink.meters(Arc::clone(meters));
        }
        self.cursor = next.cursor;
        self.latest = Some(Arc::new(next));
    }

    fn apply(
        &mut self,
        edges: Vec<edges::Edge>,
        owners: &BTreeMap<Id, u64>,
        clock: Option<rpt_advanced_core::runtime::RuntimeClock>,
        lost: bool,
    ) {
        if lost {
            self.lose_edges();
            return;
        }
        let Some(current) = self.latest.as_mut() else {
            return;
        };
        if edges.is_empty() {
            return;
        }
        let current = Arc::make_mut(current);
        for edge in edges {
            let station_generation = edge.generation;
            if owners.get(&edge.port) != Some(&station_generation) {
                continue;
            }
            let sample = edge.at_ms;
            let stamp = stamp_at(clock, sample);
            let (key, kind, mut value, terminal) = edge.record();
            if let Some(value) = value.as_mut() {
                value["observed_at"] = json!(stamp.utc);
                value["monotonic_ms"] = json!(sample.to_string());
            }
            let value = value.map(Arc::new);
            if let Some(entity) = key.entity()
                && value
                    .as_ref()
                    .is_none_or(|value| key.live_entity(value).is_none())
            {
                // A stale slow publication cannot bring back this generation's measurement.
                let retired = current
                    .records
                    .keys()
                    .filter(|record| record.dependency.as_ref() == Some(&entity))
                    .cloned()
                    .collect::<Vec<_>>();
                for record in retired {
                    current.records.remove(&record);
                    if record.is_meter() {
                        continue;
                    }
                    current.cursor = current.cursor.saturating_add(1);
                    let mut event = None;
                    for subscriber in self
                        .subscribers
                        .iter()
                        .filter(|sub| record.selected(&sub.selection))
                    {
                        subscriber.sink.invalidate_meters();
                        let row = event.get_or_insert_with(|| {
                            Arc::new(record.event(current.cursor, &stamp, None, None))
                        });
                        subscriber.sink.event(Arc::clone(row));
                    }
                }
            }
            match value.as_ref() {
                Some(value) => {
                    current.records.insert(key.clone(), Arc::clone(value));
                }
                None => {
                    current.records.remove(&key);
                }
            }
            if terminal.is_some() {
                // Playback IDs are unique within their epoch and never need tombstones.
                self.fast.remove(&key);
            } else {
                self.fast.insert(
                    key.clone(),
                    FastRecord {
                        station_generation,
                        value: value.clone(),
                    },
                );
            }
            current.cursor = current.cursor.saturating_add(1);
            let mut event = None;
            for subscriber in self
                .subscribers
                .iter()
                .filter(|sub| key.selected(&sub.selection))
            {
                if value.is_none() {
                    subscriber.sink.invalidate_meters();
                }
                let document = event.get_or_insert_with(|| {
                    Arc::new(key.edge(
                        current.cursor,
                        &stamp,
                        kind,
                        value.as_deref(),
                        terminal.as_ref(),
                    ))
                });
                subscriber.sink.event(Arc::clone(document));
            }
        }
        if let Some(clock) = clock {
            current.stamp = stamp_at(Some(clock), clock.now_ms);
        }
        self.cursor = current.cursor;
        if self.fast.len() > FAST_RECORDS {
            self.lose_edges();
        }
    }

    fn lose_edges(&mut self) {
        for subscriber in &self.subscribers {
            subscriber.sink.resync(StatusResyncReason::ProducerLoss);
        }
        self.fast.clear();
        self.latest = None;
        self.cursor = self.cursor.saturating_add(1);
    }

    fn prune(&mut self, now: Instant) {
        self.cache.retain(|entry| entry.expires > now);
    }

    fn continuation(
        &mut self,
        id: Id,
        reader: &ServiceId,
        selection: &Selection,
        now: Instant,
    ) -> Result<Option<Arc<Pages>>, Fault> {
        self.prune(now);
        match self.cache.iter().find(|entry| entry.id == id) {
            Some(entry) if &entry.reader == reader && entry.pages.selection() == selection => {
                Ok(Some(Arc::clone(&entry.pages)))
            }
            Some(_) => Err(Fault::Missing),
            None => Ok(None),
        }
    }

    fn retain_capture(&mut self, id: Id, reader: ServiceId, pages: Pages, now: Instant) {
        self.prune(now);
        while self.cache.len() >= CACHE_CAPTURES
            || self
                .cache
                .iter()
                .map(|entry| entry.pages.bytes())
                .sum::<usize>()
                + pages.bytes()
                > CACHE_BYTES
        {
            self.cache.pop_front();
        }
        self.cache.push_back(Continuation {
            id,
            reader,
            expires: now + PAGE_TTL,
            pages: Arc::new(pages),
        });
    }
}

struct Previous {
    domain: Arc<super::live::Domain>,
    active_revision: Option<String>,
    saved: Arc<Snapshot>,
    owners: std::collections::BTreeMap<Id, u64>,
}

/// The controller coordinator reads immutable owner publications without polling hardware.
pub(super) struct StatusApi {
    published: Mutex<Published>,
    started: Instant,
    previous: Mutex<Option<Previous>>,
    last_tick: Mutex<Option<Instant>>,
}

impl StatusApi {
    pub(super) fn new() -> Self {
        Self {
            published: Mutex::default(),
            started: Instant::now(),
            previous: Mutex::default(),
            last_tick: Mutex::default(),
        }
    }

    pub(super) fn tick(&self, source: impl FnOnce() -> Option<(Observed, Arc<Snapshot>)>) {
        let now = Instant::now();
        let mut last_tick = self.last_tick.lock().expect("status cadence");
        if last_tick.is_some_and(|last| now.duration_since(last) < Duration::from_millis(50)) {
            return;
        }
        *last_tick = Some(now);
        drop(last_tick);
        if let Some((observed, saved)) = source() {
            self.publish(observed, saved);
        }
    }

    fn publish(&self, observed: Observed, saved: Arc<Snapshot>) {
        let (lost, edges) = observed
            .status_edges
            .as_ref()
            .map(|queue| queue.drain())
            .unwrap_or_default();
        let no_owners = BTreeMap::new();
        let owners = if observed.status_ready {
            &observed.owners
        } else {
            &no_owners
        };
        let clock = observed.clock;
        let monotonic_ms = clock.map_or_else(
            || u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            |clock| clock.now_ms,
        );
        let mut previous = self.previous.lock().expect("status publisher");
        let unchanged = previous.as_ref().is_some_and(|prev| {
            Arc::ptr_eq(&prev.domain, &observed.domain)
                && prev.active_revision == observed.active_revision
                && Arc::ptr_eq(&prev.saved, &saved)
                && &prev.owners == owners
        });
        let next = (!lost && !unchanged).then(|| {
            let stamp = stamp_at(clock, monotonic_ms);
            Capture::project(&observed, &saved, stamp)
        });
        let mut publication = self.published.lock().expect("status observations");
        if let Some(next) = next {
            publication.publish(next, owners);
        }
        publication.apply(edges, owners, clock, lost);
        if publication.latest.is_none() {
            // A lost edge requires a new cut even if every polling source stays idle.
            *previous = None;
        } else if !unchanged {
            *previous = Some(Previous {
                domain: observed.domain,
                active_revision: observed.active_revision,
                saved,
                owners: owners.clone(),
            });
        }
    }

    pub(super) fn handle(
        &self,
        request: &AdmittedRequest,
        route: &Route,
        api: &Api,
        boot: Id,
    ) -> Result<Output, Fault> {
        if request.method != Method::GET {
            return Err(Fault::Method);
        }
        let stream = route.target == Target::StatusStream;
        if request.is_status_upgrade() && !stream {
            return Err(Fault::Malformed);
        }
        let query = Query::parse(request.target.query(), stream)?;
        // The normalized selector plus any emitted cursor must fit the listener target bound.
        if api.mount.len() + SNAPSHOT.len() + query.selection.query().len() + 64 > 2048 {
            return Err(Fault::TooLarge);
        }
        if stream {
            self.stream(request, route, api, boot, query)
        } else {
            self.snapshot(request, route, api, boot, query)
        }
    }

    fn stream(
        &self,
        request: &AdmittedRequest,
        route: &Route,
        api: &Api,
        boot: Id,
        query: Query,
    ) -> Result<Output, Fault> {
        if !request.is_status_upgrade() {
            if !query.selection.ports.is_empty() {
                self.published
                    .lock()
                    .expect("status observations")
                    .capture(&query.selection)?;
            }
            let target = scoped(STREAM, &query.selection.query());
            let mut links = api.links(&target, &route.profile);
            links["rpt:status-snapshot"] = api.href(&scoped(SNAPSHOT, &query.selection.query()));
            let mut reply = response(
                StatusCode::UPGRADE_REQUIRED,
                json!({"error":"upgrade_required","_links":links}),
                Some(route),
                "application/hal+json",
            );
            reply
                .headers_mut()
                .insert("upgrade", "websocket".parse().expect("upgrade token"));
            return Ok(Output::Http(reply));
        }
        let (sink, mut stream) = StatusStream::new(boot.to_string(), Vec::new(), query.meter_hz);
        let capture = self
            .published
            .lock()
            .expect("status observations")
            .subscribe(query.selection.clone(), sink)?;
        let pages = Pages::new(capture, query.selection, fresh()?)?;
        let documents = (0..pages.len())
            .map(|index| pages.document(api, boot, index, true))
            .collect::<Result<Vec<_>, _>>()?;
        stream.set_snapshot_pages(documents);
        Ok(Output::Stream(stream))
    }

    fn snapshot(
        &self,
        request: &AdmittedRequest,
        route: &Route,
        api: &Api,
        boot: Id,
        query: Query,
    ) -> Result<Output, Fault> {
        let now = Instant::now();
        let (status, document) = if let Some((id, index)) = query.page {
            let pages = self
                .published
                .lock()
                .expect("status observations")
                .continuation(id, &request.actor, &query.selection, now)?;
            match pages {
                Some(pages) => (StatusCode::OK, pages.document(api, boot, index, false)?),
                None => {
                    let fresh = scoped(SNAPSHOT, &query.selection.query());
                    let mut links = api.links(&fresh, &route.profile);
                    links["rpt:status-snapshot"] = api.href(&fresh);
                    (
                        StatusCode::GONE,
                        json!({"error":"expired_snapshot","_links":links}),
                    )
                }
            }
        } else {
            let capture = self
                .published
                .lock()
                .expect("status observations")
                .capture(&query.selection)?;
            let id = fresh()?;
            let pages = Pages::new(capture, query.selection, id)?;
            let document = pages.document(api, boot, 0, false)?;
            if pages.len() > 1 {
                self.published
                    .lock()
                    .expect("status observations")
                    .retain_capture(id, request.actor.clone(), pages, Instant::now());
            }
            (StatusCode::OK, document)
        };
        let response = response(status, document, Some(route), "application/hal+json");
        Ok(Output::Http(response))
    }
}

pub(super) enum Output {
    Http(Response<Vec<u8>>),
    Stream(StatusStream),
}

fn stamp_at(clock: Option<rpt_advanced_core::runtime::RuntimeClock>, monotonic_ms: u64) -> Stamp {
    let wall_ms = if let Some(clock) = clock {
        i128::from(clock.wall_seconds) * 1000
            + i128::from(clock.wall_subsecond_ms)
            + i128::from(monotonic_ms)
            - i128::from(clock.now_ms)
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|now| i128::try_from(now.as_millis()).ok())
            .unwrap_or(-1)
    };
    let utc = i64::try_from(wall_ms.div_euclid(1000))
        .ok()
        .and_then(|seconds| {
            super::resources::utc_timestamp(seconds, wall_ms.rem_euclid(1000) as u16)
        });
    Stamp { utc, monotonic_ms }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::management::status::edges::{Edge, Payload, RadioSignal};
    use std::collections::BTreeSet;

    const PORT: &str = "361c602c-7664-4600-990d-6bd6cae4b32c";

    fn capture(port: Id) -> Capture {
        Capture::fixture(
            port,
            Stamp {
                utc: Some("2026-11-25T18:00:00.000Z".into()),
                monotonic_ms: 100,
            },
        )
    }

    fn scope(port: Id) -> Selection {
        Selection {
            site: false,
            ports: BTreeSet::from([port]),
            topics: BTreeSet::from(query::Topic::STATE),
        }
    }

    #[test]
    fn live_subscriptions_grow_and_closed_subscriptions_are_retired() {
        let port = PORT.parse().unwrap();
        let mut hub = Published::default();
        hub.publish(capture(port), &BTreeMap::new());
        let readers = (0..32)
            .map(|_| {
                let (sink, stream) = StatusStream::new(PORT.to_owned(), Vec::new(), None);
                assert_eq!(hub.subscribe(scope(port), sink).unwrap().cursor, 0);
                stream
            })
            .collect::<Vec<_>>();
        assert_eq!(hub.subscribers.len(), readers.len());
        drop(readers);
        hub.publish(capture(port), &BTreeMap::new());
        assert!(hub.subscribers.is_empty());
    }

    #[test]
    fn terminal_playback_removes_the_active_record_without_losing_its_public_cause() {
        let port: Id = PORT.parse().unwrap();
        let owners = BTreeMap::from([(port, 7)]);
        let mut hub = Published::default();
        hub.publish(capture(port), &owners);
        let edge = |kind, value, terminal| Edge {
            port,
            generation: 7,
            at_ms: 101,
            payload: Payload::Telemetry {
                id: "14".into(),
                playback_generation: 7,
                kind,
                value,
                terminal,
            },
        };
        hub.apply(
            vec![edge(
                "telemetry.queued",
                Some(json!({"phase":"queued"})),
                None,
            )],
            &owners,
            None,
            false,
        );
        assert_eq!(hub.fast.len(), 1);
        let retired = edge(
            "telemetry.cancelled",
            None,
            Some(json!({"id":"14","category":"courtesy","reason":"source_rekeyed"})),
        );
        let (key, kind, value, terminal) = edge(
            "telemetry.cancelled",
            None,
            Some(json!({"id":"14","category":"courtesy","reason":"source_rekeyed"})),
        )
        .record();
        let event = key.edge(
            2,
            &stamp_at(None, 102),
            kind,
            value.as_ref(),
            terminal.as_ref(),
        );
        assert_eq!(event["operation"], "remove");
        assert_eq!(event["data"]["reason"], "source_rekeyed");
        hub.apply(vec![retired], &owners, None, false);
        assert!(hub.fast.is_empty());
        assert!(hub.capture(&scope(port)).unwrap().records.is_empty());
    }

    #[test]
    fn registration_holds_an_immutable_cut_and_an_unkey_pulse_advances_twice() {
        let port: Id = PORT.parse().unwrap();
        let owners = BTreeMap::from([(port, 7)]);
        let mut hub = Published::default();
        hub.publish(capture(port), &owners);
        let (sink, _stream) = StatusStream::new(PORT.to_owned(), Vec::new(), None);
        let cut = hub.subscribe(scope(port), sink).unwrap();
        hub.apply(
            vec![
                Edge {
                    port,
                    generation: 7,
                    at_ms: 101,
                    payload: Payload::Radio(RadioSignal::Receive {
                        qualified: true,
                        duration_ms: None,
                    }),
                },
                Edge {
                    port,
                    generation: 7,
                    at_ms: 102,
                    payload: Payload::Radio(RadioSignal::Receive {
                        qualified: false,
                        duration_ms: Some(1),
                    }),
                },
                Edge {
                    port,
                    generation: 6,
                    at_ms: 103,
                    payload: Payload::Radio(RadioSignal::Receive {
                        qualified: true,
                        duration_ms: None,
                    }),
                },
            ],
            &owners,
            None,
            false,
        );
        assert_eq!(cut.cursor, 0);
        assert!(cut.records.is_empty());
        let next = hub.capture(&scope(port)).unwrap();
        assert_eq!(next.cursor, 2);
        let reading = next.records.values().next().unwrap();
        assert_eq!(reading["qualified"], false);
        assert_eq!(reading["duration_ms"], "1");
        assert_eq!(reading["monotonic_ms"], "102");
        hub.publish(capture(port), &BTreeMap::from([(port, 8)]));
        assert!(hub.capture(&scope(port)).unwrap().records.is_empty());
    }

    #[test]
    fn peer_diagnostics_keep_cadence_without_replacing_fast_key_state() {
        let port: Id = PORT.parse().unwrap();
        let owners = BTreeMap::from([(port, 7)]);
        let peer = "allstarlink:9182";
        let mut hub = Published::default();
        hub.publish(capture(port), &owners);
        hub.apply(
            vec![Edge {
                port,
                generation: 7,
                at_ms: 101,
                payload: Payload::Peer {
                    id: peer.to_owned(),
                    connection: 3,
                    kind: "link.connected",
                    value: Some(
                        json!({"state":"connected","transmit_keyed":true,"media":{"lost":"2"}}),
                    ),
                },
            }],
            &owners,
            None,
            false,
        );
        let mut polled = capture(port);
        polled.records.insert(
            RecordKey::new(
                Owner::Port(port),
                Some(query::Topic::Links),
                &["links", peer],
                "link",
                peer.to_owned(),
                Some(3),
            ),
            Arc::new(json!({"state":"connected","transmit_keyed":null,"media":{"lost":"4"}})),
        );
        polled.stamp.monotonic_ms = 500;
        hub.publish(polled.clone(), &owners);
        let latest = hub.capture(&scope(port)).unwrap();
        let value = latest.records.values().next().unwrap();
        assert_eq!(value["transmit_keyed"], true);
        assert_eq!(value["media"]["lost"], "2");
        assert_eq!(latest.cursor, 1);

        polled.stamp.monotonic_ms = 1_000;
        hub.publish(polled, &owners);
        let latest = hub.capture(&scope(port)).unwrap();
        let value = latest.records.values().next().unwrap();
        assert_eq!(value["transmit_keyed"], true);
        assert_eq!(value["media"]["lost"], "4");
        assert_eq!(latest.cursor, 2);
    }

    #[test]
    fn dependent_meters_cannot_return_after_a_peer_retires() {
        let port: Id = PORT.parse().unwrap();
        let owners = BTreeMap::from([(port, 7)]);
        let public_peer = "allstarlink:9182";
        let owner = Owner::Port(port);
        let mut capture = capture(port);
        capture.records.insert(
            RecordKey::new(
                owner,
                Some(query::Topic::Links),
                &["links", public_peer],
                "link",
                public_peer.to_owned(),
                Some(3),
            ),
            Arc::new(json!({"state":"connected"})),
        );
        capture.records.insert(
            RecordKey::new(
                owner,
                Some(query::Topic::Meters),
                &["meters", "sample"],
                "meter",
                "sample".to_owned(),
                Some(64),
            )
            .depends_on("link", public_peer, 3),
            Arc::new(json!({"peak_dbfs":-4})),
        );
        let mut hub = Published::default();
        hub.publish(capture.clone(), &owners);
        assert_eq!(hub.latest.as_ref().unwrap().records.len(), 2);
        hub.apply(
            vec![Edge {
                port,
                generation: 7,
                at_ms: 101,
                payload: Payload::Peer {
                    id: public_peer.to_owned(),
                    connection: 3,
                    kind: "link.disconnected",
                    value: None,
                },
            }],
            &owners,
            None,
            false,
        );
        assert!(hub.latest.as_ref().unwrap().records.is_empty());
        // The latest polling view has yet to see the retirement.
        hub.publish(capture, &owners);
        assert!(hub.latest.as_ref().unwrap().records.is_empty());
    }

    #[test]
    fn edge_loss_invalidates_fast_rows_and_never_rewinds_the_cursor() {
        let port: Id = PORT.parse().unwrap();
        let owners = BTreeMap::from([(port, 7)]);
        let mut hub = Published::default();
        hub.publish(capture(port), &owners);
        hub.apply(
            vec![Edge {
                port,
                generation: 7,
                at_ms: 101,
                payload: Payload::Radio(RadioSignal::Receive {
                    qualified: true,
                    duration_ms: None,
                }),
            }],
            &owners,
            None,
            false,
        );
        let old = hub.capture(&scope(port)).unwrap();
        hub.apply(Vec::new(), &owners, None, true);
        assert!(matches!(hub.capture(&scope(port)), Err(Fault::Unavailable)));
        hub.publish(capture(port), &owners);
        let recovered = hub.capture(&scope(port)).unwrap();
        assert!(recovered.cursor > old.cursor);
        assert!(recovered.records.is_empty());
    }

    #[test]
    fn paging_keeps_one_large_ports_original_rows_and_expires() {
        let port: Id = PORT.parse().unwrap();
        let mut captured = capture(port);
        // One radio port exceeds the transport response budget.
        let message = "x".repeat(256 * 1024);
        for node in 10..35 {
            let name = node.to_string();
            let key = RecordKey::new(
                Owner::Port(port),
                Some(query::Topic::Links),
                &["links", &name],
                "link",
                name.clone(),
                Some(5),
            );
            captured
                .records
                .insert(key, Arc::new(json!({"node":name,"test":message})));
        }
        let mut hub = Published::default();
        let owners = BTreeMap::from([(port, 5)]);
        hub.publish(captured, &owners);
        let pages = Pages::new(hub.capture(&scope(port)).unwrap(), scope(port), port).unwrap();
        assert!(pages.len() > 1);
        let api = Api::new("/api/v1").unwrap();
        let start = Instant::now();
        let reader = ServiceId::new("cli").unwrap();
        hub.retain_capture(port, reader.clone(), pages, start);
        hub.publish(capture(port), &owners);
        assert!(matches!(
            hub.continuation(port, &ServiceId::new("nginx").unwrap(), &scope(port), start),
            Err(Fault::Missing)
        ));
        let mut wrong_selection = scope(port);
        wrong_selection.site = true;
        assert!(matches!(
            hub.continuation(port, &reader, &wrong_selection, start),
            Err(Fault::Missing)
        ));
        let saved = hub
            .continuation(port, &reader, &scope(port), start)
            .unwrap()
            .unwrap();
        let mut rows = 0;
        for index in 0..saved.len() {
            let page = saved.document(&api, port, index, false).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= 4 * 1024 * 1024);
            assert_eq!(page["snapshot_id"], PORT);
            assert_eq!(page["state_cursor"], "0");
            let links = &page["status"]["ports"][PORT]["links"];
            rows += links.as_object().unwrap().len();
            assert_eq!(page["complete"], index + 1 == saved.len());
        }
        assert_eq!(rows, 25);
        assert!(
            hub.continuation(port, &reader, &scope(port), start + PAGE_TTL)
                .unwrap()
                .is_none()
        );
    }
}
