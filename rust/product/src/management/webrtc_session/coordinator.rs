//! Exact browser authorization, boot-local session state and private HAL dispatch.
use super::{Access, Approval, Book, Direction, OfferWork, Receipt, Transport, WorkKey, hal};
use crate::management::{
    AdmittedRequest, Api, Fault, Id, Method, Observed, Parameters, Response, Route, StatusCode,
    Target, json_response,
};
use std::time::Instant;

const RECEIPTS_PER_POLL: usize = 32;
const HAL: &str = "application/hal+json";

pub(in crate::management) struct Signaling {
    boot: Id,
    book: Book,
    transport: Box<dyn Transport>,
}

impl Signaling {
    pub(in crate::management) fn new(boot: Id, transport: Box<dyn Transport>) -> Self {
        Self {
            boot,
            book: Book::default(),
            transport,
        }
    }

    /// Advance provider receipts without holding up the next management request.
    pub(in crate::management) fn poll(&mut self, live: &Observed, now: Instant) -> bool {
        self.sweep(live, now);
        let mut progressed = false;
        for _ in 0..RECEIPTS_PER_POLL {
            let Some(result) = self.transport.next_receipt() else {
                break;
            };
            progressed = true;
            let (key, applied) = match result {
                Receipt::Answer { key, sdp } => (key, self.book.answer_ready(key, live, now, sdp)),
                Receipt::LocalCandidate {
                    key,
                    sequence,
                    contents,
                } => (
                    key,
                    self.book
                        .local_candidate(key, live, now, sequence, contents),
                ),
                Receipt::CandidateAccepted(key) => {
                    (key.offer, self.book.acknowledge_candidate(key, live, now))
                }
                Receipt::Attached(key) => (key, self.book.activate(key, live, now)),
                Receipt::Closed(key) => (key, self.book.revoke(key)),
            };
            if applied.is_err() {
                // A stale generation cannot revoke the current one.
                let _ = self.book.revoke(key);
            }
        }
        if progressed {
            self.sweep(live, now);
        }
        progressed
    }

    fn sweep(&mut self, live: &Observed, now: Instant) {
        self.book.sweep(live, now, |key| self.transport.close(key));
    }

    pub(in crate::management) fn request(
        &mut self,
        req: &AdmittedRequest,
        route: &Route,
        params: Parameters<'_>,
        api: &Api,
        live: &Observed,
        port_present: bool,
    ) -> Result<Response<Vec<u8>>, Fault> {
        let access = Access {
            principal: req.browser_principal().ok_or(Fault::Forbidden)?,
            active: live,
            now: Instant::now(),
        };
        let port = params.port.ok_or(Fault::Missing)?;
        let query = req.target.query();
        if query.is_some() && req.method != Method::DELETE {
            return Err(Fault::Malformed);
        }
        let reply = if route.target == Target::WebRtcSessions {
            if !port_present {
                return Err(Fault::Missing);
            }
            match req.method {
                Method::GET => Ok(json_response(
                    StatusCode::OK,
                    &hal::collection_document(api, port),
                    Some(route),
                    HAL,
                )),
                Method::POST => (|| {
                    let input = hal::offer(req.body.as_bytes())?;
                    let (revision, station) = active_owner(live, port)?;
                    let approval =
                        self.approve(access, port, input.direction, revision, station)?;
                    let work =
                        self.book
                            .offer(access, port, input.direction, approval, input.sdp)?;
                    let key = work.key;
                    self.offer(work, revision, station, approval.until)?;
                    let mut reply =
                        self.session_reply(access, api, route, key, StatusCode::CREATED)?;
                    reply.headers_mut().insert(
                        "location",
                        hal::session_location(api, self.boot, key)
                            .parse()
                            .expect("canonical session location"),
                    );
                    Ok(reply)
                })(),
                _ => Err(Fault::Method),
            }
        } else {
            self.session_request(req, route, params, api, access, port)
        };
        self.sweep(live, Instant::now());
        reply
    }

    fn session_request(
        &mut self,
        req: &AdmittedRequest,
        route: &Route,
        params: Parameters<'_>,
        api: &Api,
        access: Access<'_>,
        port: Id,
    ) -> Result<Response<Vec<u8>>, Fault> {
        if params.boot != Some(self.boot) {
            return Err(Fault::Gone);
        }
        let session = params.session.ok_or(Fault::Missing)?;
        let current = self.book.current_key(access, port, session)?;
        match (&req.method, route.target) {
            (&Method::GET, Target::WebRtcSession) => {
                self.session_reply(access, api, route, current, StatusCode::OK)
            }
            (&Method::PUT, Target::WebRtcSession) => {
                let update = hal::update(req.body.as_bytes())?;
                let key = self.update(access, current, update)?;
                self.session_reply(access, api, route, key, StatusCode::OK)
            }
            (&Method::DELETE, Target::WebRtcSession) => {
                let key = WorkKey {
                    ice_generation: hal::close_generation(req.target.query())?,
                    ..current
                };
                self.book.close(access, key)?;
                self.session_reply(access, api, route, key, StatusCode::OK)
            }
            (&Method::GET, Target::WebRtcCandidates) => {
                self.candidates_reply(access, api, route, current)
            }
            (&Method::POST, Target::WebRtcCandidates) => {
                let input = hal::candidate(req.body.as_bytes())?;
                let key = WorkKey {
                    ice_generation: input.ice_generation,
                    ..current
                };
                if let Some(work) =
                    self.book
                        .candidate(access, key, input.sequence, input.contents)?
                    && let Err(error) = self.transport.candidate(work)
                {
                    let _ = self.book.revoke(key);
                    return Err(error);
                }
                self.candidates_reply(access, api, route, key)
            }
            _ => Err(Fault::Method),
        }
    }

    fn approve(
        &mut self,
        access: Access<'_>,
        port: Id,
        direction: Direction,
        revision: &str,
        station: u64,
    ) -> Result<Approval, Fault> {
        self.transport.approve(
            access.principal,
            port,
            direction,
            revision,
            station,
            access.now,
        )
    }

    fn offer(
        &mut self,
        work: OfferWork,
        revision: &str,
        station: u64,
        until: Instant,
    ) -> Result<(), Fault> {
        let key = work.key;
        self.transport
            .offer(work, revision, station, until)
            .inspect_err(|_| {
                let _ = self.book.revoke(key);
            })
    }

    fn update(
        &mut self,
        access: Access<'_>,
        current: WorkKey,
        update: hal::Update,
    ) -> Result<WorkKey, Fault> {
        let (ice_generation, sdp) = match update {
            hal::Update::Renew { ice_generation } => (ice_generation, None),
            hal::Update::Restart {
                ice_generation,
                sdp,
            } => (ice_generation, Some(sdp)),
        };
        let key = WorkKey {
            ice_generation,
            ..current
        };
        let direction = self.book.direction(access, key)?;
        let (revision, station) = active_owner(access.active, key.port)?;
        let approval = self
            .approve(access, key.port, direction, revision, station)
            .inspect_err(|_| {
                let _ = self.book.revoke(key);
            })?;
        if let Some(sdp) = sdp {
            let next = self.book.restart(access, key, approval, sdp)?;
            self.transport.close(key);
            let key = next.key;
            self.offer(next, revision, station, approval.until)?;
            Ok(key)
        } else {
            self.book.renew(access, key, approval)?;
            self.transport.renew(key, approval.until).inspect_err(|_| {
                let _ = self.book.revoke(key);
            })?;
            Ok(key)
        }
    }

    fn session_reply(
        &mut self,
        access: Access<'_>,
        api: &Api,
        route: &Route,
        key: WorkKey,
        status: StatusCode,
    ) -> Result<Response<Vec<u8>>, Fault> {
        let state = self.book.state(access, key)?;
        let document = hal::session_document(api, self.boot, key, state)?;
        Ok(json_response(status, &document, Some(route), HAL))
    }

    fn candidates_reply(
        &mut self,
        access: Access<'_>,
        api: &Api,
        route: &Route,
        key: WorkKey,
    ) -> Result<Response<Vec<u8>>, Fault> {
        let contents = self.book.local_candidates(access, key)?;
        let mut candidates = Vec::new();
        candidates
            .try_reserve_exact(contents.len())
            .map_err(|_| Fault::Unavailable)?;
        for (sequence, candidate) in contents.iter().enumerate() {
            let candidate = candidate.as_ref().map(|value| {
                std::str::from_utf8(value.expose_bytes()).expect("checked ICE candidate text")
            });
            candidates.push(hal::LocalCandidate {
                sequence,
                candidate,
            });
        }
        let document = hal::candidate_document(api, self.boot, key, &candidates);
        Ok(json_response(StatusCode::OK, &document, Some(route), HAL))
    }
}

impl Drop for Signaling {
    fn drop(&mut self) {
        for session in self.book.0.values().filter(|row| !row.retired) {
            self.transport.close(session.key);
        }
    }
}

fn active_owner(live: &Observed, port: Id) -> Result<(&str, u64), Fault> {
    if !live.status_ready {
        return Err(Fault::Unavailable);
    }
    Ok((
        live.active_revision.as_deref().ok_or(Fault::Unavailable)?,
        *live.owners.get(&port).ok_or(Fault::Unavailable)?,
    ))
}
