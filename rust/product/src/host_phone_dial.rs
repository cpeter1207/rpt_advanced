//! Selected registration, exact-attempt PCM isolation and outgoing media ownership.
use super::*;
use crate::{
    lifecycle::Engine,
    management::status::edges::{Edge, Payload},
    phone::accounts::DialAccount,
    services::PhoneDial,
};
use rpt_advanced_core::{link::MediaIsolationClaim, runtime::dtmf::ActionPermit};
use std::{sync::Weak, time::Duration};

enum Phase {
    Dialing(PhoneDial),
    Answered(Option<PeerIo>),
    Publishing {
        source: PhoneSource,
        remaining: u32,
        submitted: bool,
    },
}

pub(super) struct Pending {
    local: String,
    account: DialAccount,
    engine: Weak<Engine>,
    permit: ActionPermit,
    revision: u64,
    deadline: Option<Instant>,
    isolation: Option<MediaIsolationClaim>,
    report: bool,
    phase: Phase,
    presentation: Arc<super::phone_progress::State>,
    failure_status: u32,
    observed_generation: Option<u64>,
    observed_phase: &'static str,
}

pub(super) struct Failure {
    local: String,
    status: u32,
    permit: ActionPermit,
    engine: Weak<Engine>,
    revision: u64,
    account: Option<DialAccount>,
}

impl Pending {
    /// Reuse the same sanitized progress in latest captures and edge events.
    pub(super) fn status_observation(
        &self,
    ) -> (
        rpt_advanced_core::config::resources::candidate::ResourceId,
        Option<u64>,
        u64,
        serde_json::Value,
    ) {
        (
            self.account.port,
            self.observed_generation,
            self.presentation.id(),
            serde_json::json!({"direction":"outgoing", "phase":self.observed_phase}),
        )
    }

    fn authorized(&self) -> bool {
        self.engine.upgrade().is_some_and(|engine| {
            engine.current(self.revision) && self.permit.current(Some(engine.elapsed_ms()))
        })
    }

    fn current(&self) -> bool {
        self.authorized()
            && self
                .deadline
                .is_none_or(|deadline| Instant::now() < deadline)
    }

    fn ready(&mut self) -> Result<bool, RuntimeError> {
        if let Phase::Dialing(dial) = &mut self.phase {
            let mut pcm = [0.0; MAXIMUM_FRAMES];
            let mut progress = crate::abi::rptadv_phone_progress_v1 {
                struct_size: size_of::<crate::abi::rptadv_phone_progress_v1>() as u32,
                abi_version: 1,
                reserved: 0,
                pcm: pcm.as_mut_ptr(),
                capacity: pcm.len(),
                ringing: 0,
                final_status: 0,
                frames: 0,
                sample_rate: 0,
            };
            let polled_at = Instant::now();
            let result = dial.poll(&mut progress);
            self.failure_status = progress.final_status;
            if progress.frames > pcm.len()
                || progress.ringing > 1
                || !(progress.final_status == 0 || (200..=699).contains(&progress.final_status))
                || (progress.frames > 0 && !(8000..=48000).contains(&progress.sample_rate))
                || !pcm[..progress.frames].iter().all(|v| v.is_finite())
            {
                return Err(RuntimeError::Rejected);
            }
            let Some(peer) = result.map_err(|_| RuntimeError::Rejected)? else {
                if progress.final_status != 0 {
                    self.presentation.end();
                } else {
                    self.observed_phase = if progress.frames > 0 {
                        "early_media"
                    } else if progress.ringing != 0 {
                        "ringing"
                    } else {
                        self.observed_phase
                    };
                    self.presentation.update(
                        progress.ringing != 0,
                        &pcm[..progress.frames],
                        progress.sample_rate,
                        polled_at,
                    );
                }
                return Ok(false);
            };
            self.presentation.end();
            if !self.current() {
                return Err(RuntimeError::Rejected);
            }
            self.phase = Phase::Answered(Some(peer));
            self.observed_phase = "answered";
            self.deadline = Some(Instant::now() + Duration::from_secs(2));
        }
        let Phase::Answered(Some(peer)) = &mut self.phase else {
            return Ok(true);
        };
        let ready = peer.ready().map_err(|_| RuntimeError::Rejected)?;
        if ready {
            self.deadline = None;
        } else if self.deadline.is_none() {
            return Err(RuntimeError::Rejected);
        }
        Ok(ready)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.presentation.end();
    }
}

impl Host {
    pub(super) fn recheck_phone_dials(&mut self) {
        for _ in 0..self.phone_dials.len() {
            let attempt = self.phone_dials.pop_front().expect("retained pending dial");
            if attempt.current()
                && self.phone_accounts.registered(
                    self.services,
                    &attempt.local,
                    &attempt.account.identity,
                    attempt.account.generation,
                )
            {
                self.phone_dials.push_back(attempt);
            } else {
                self.report_expired_dial(&attempt);
                self.finish_pending(attempt, false);
            }
        }
    }

    fn report_expired_dial(&mut self, attempt: &Pending) {
        if attempt.deadline.is_some_and(|end| Instant::now() >= end) {
            self.report_phone_failure(attempt, 408);
        }
    }

    fn report_phone_failure(&mut self, attempt: &Pending, status: u32) {
        attempt.presentation.end();
        if attempt.authorized() {
            self.enqueue_phone_failure(
                &attempt.local,
                status,
                &attempt.permit,
                &attempt.engine,
                attempt.revision,
                Some(&attempt.account),
            );
        }
    }

    fn enqueue_phone_failure(
        &mut self,
        local: &str,
        status: u32,
        permit: &ActionPermit,
        engine: &Weak<Engine>,
        revision: u64,
        account: Option<&DialAccount>,
    ) {
        if self.phone_failures.try_reserve(1).is_ok() {
            self.phone_failures.push_back(Failure {
                local: local.to_owned(),
                status,
                permit: permit.clone(),
                engine: engine.clone(),
                revision,
                account: account.cloned(),
            });
        }
    }

    fn service_phone_failure(&mut self) {
        let Some(failure) = self.phone_failures.pop_front() else {
            return;
        };
        let Some(engine) = failure.engine.upgrade() else {
            return;
        };
        if !engine.current(failure.revision)
            || !failure.permit.current(Some(engine.elapsed_ms()))
            || failure.account.as_ref().is_some_and(|account| {
                !self.phone_accounts.registered(
                    self.services,
                    &failure.local,
                    &account.identity,
                    account.generation,
                )
            })
        {
            return;
        }
        let clock = Instant::now();
        let base = engine.elapsed_ms();
        let permit = failure.permit.clone();
        let admission = engine.media_admission(failure.revision);
        let result =
            self.runtime
                .queue_phone_dial_failure(&failure.local, failure.status, move || {
                    admission.current()
                        && permit.current(Some(
                            base.saturating_add(clock.elapsed().as_millis() as u64),
                        ))
                });
        if matches!(result, Err(RuntimeError::Busy)) {
            self.phone_failures.push_back(failure);
        }
    }

    fn finish_pending(&mut self, attempt: Pending, completed: bool) {
        let terminal = if completed {
            "phone_patch.dial_connected"
        } else if attempt.authorized() {
            "phone_patch.dial_failed"
        } else {
            "phone_patch.dial_cancelled"
        };
        self.record_phone_progress(&attempt, Some(terminal));
        if !completed && let Phase::Publishing { source, .. } = &attempt.phase {
            self.retire_phone(source);
        }
        if attempt.report {
            self.services.command_notice(&attempt.local, completed);
        }
    }

    /// Select and cancel in the same serialized turn so a successor cannot be substituted.
    pub(crate) fn hangup_outgoing_phone(&mut self, local: &str) -> Result<(), RuntimeError> {
        enum Outgoing {
            Pending(usize),
            Active(PhoneSource),
        }
        self.recheck_phone_dials();
        let selected = {
            let pending = self
                .phone_dials
                .iter()
                .enumerate()
                .filter(|(_, attempt)| {
                    attempt.local == local && !matches!(&attempt.phase, Phase::Publishing { .. })
                })
                .map(|(index, _)| Outgoing::Pending(index));
            let active = self
                .phone_calls
                .outgoing_sources(local)
                .cloned()
                .map(Outgoing::Active);
            let mut candidates = pending.chain(active);
            let selected = candidates.next().ok_or(RuntimeError::Rejected)?;
            if candidates.next().is_some() {
                return Err(RuntimeError::Rejected);
            }
            selected
        };
        let pending = match &selected {
            Outgoing::Pending(index) => Some(*index),
            Outgoing::Active(source) => self.phone_dials.iter().position(|attempt| {
                matches!(&attempt.phase, Phase::Publishing { source: pending, .. } if pending == source)
            }),
        };
        if let Some(index) = pending {
            let attempt = self
                .phone_dials
                .remove(index)
                .expect("selected outgoing attempt");
            self.record_phone_progress(&attempt, Some("phone_patch.dial_cancelled"));
            if attempt.report {
                self.services.command_notice(local, false);
            }
        }
        let ended = match selected {
            Outgoing::Pending(_) => true,
            Outgoing::Active(source) => self.retire_phone(&source),
        };
        ended.then_some(()).ok_or(RuntimeError::Rejected)
    }

    pub(crate) fn begin_phone_dial(
        &mut self,
        local: String,
        number: &str,
        engine: Weak<Engine>,
        permit: ActionPermit,
        revision: u64,
        report: bool,
    ) -> Result<(), RuntimeError> {
        self.phone_dials
            .try_reserve(1)
            .map_err(|_| RuntimeError::Busy)?;
        let live = engine
            .upgrade()
            .filter(|owner| owner.current(revision) && permit.current(Some(owner.elapsed_ms())))
            .ok_or(RuntimeError::Rejected)?;
        let Some(account) = self.phone_accounts.dial_selection(self.services, &local) else {
            self.enqueue_phone_failure(&local, 0, &permit, &engine, revision, None);
            return Err(RuntimeError::Rejected);
        };
        let observed_generation = self.handoff_owner(&local);
        let plan = self.radio_preparation.active_plan(&local)?;
        let policy = plan
            .phone
            .as_ref()
            .filter(|policy| {
                policy.account == account.binding && policy.confirmed.load(Ordering::Acquire)
            })
            .ok_or(RuntimeError::Rejected)?;
        let isolation = if policy.settings.isolate_peers {
            Some(plan.media_isolation.hold().ok_or(RuntimeError::Busy)?)
        } else {
            None
        };
        let presentation = plan.progress.begin(
            permit.clone(),
            &live,
            revision,
            Arc::clone(&policy.confirmed),
        )?;
        let dial = match self.services.begin_phone(&account, number) {
            Ok(dial) => dial,
            Err(_) => {
                presentation.end();
                self.enqueue_phone_failure(&local, 0, &permit, &engine, revision, Some(&account));
                return Err(RuntimeError::Rejected);
            }
        };
        let pending = Pending {
            local,
            account,
            engine,
            permit,
            revision,
            report,
            isolation,
            presentation,
            failure_status: 0,
            observed_generation,
            observed_phase: "dialing",
            deadline: Some(Instant::now() + Duration::from_secs(35)),
            phase: Phase::Dialing(dial),
        };
        self.record_phone_progress(&pending, None);
        self.phone_dials.push_back(pending);
        Ok(())
    }

    /// Publish only the attempt identity and proven progress phase.
    fn record_phone_progress(&self, attempt: &Pending, terminal: Option<&'static str>) {
        let (port, station, id, value) = attempt.status_observation();
        if let Some(generation) = station {
            self.status_edges.push(Edge {
                port,
                generation,
                at_ms: self.epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                payload: Payload::PhoneProgress {
                    id: id.to_string(),
                    generation: id,
                    kind: terminal.unwrap_or("phone_patch.dial_progress"),
                    value: terminal.is_none().then_some(value),
                },
            });
        }
    }

    /// Poll each signaling handle once and advance at most one expensive station route per tick.
    pub(crate) fn poll_phone_dials(&mut self, clock: RuntimeClock) {
        self.service_phone_failure();
        let mut selected = None;
        let mut refresh = None;
        for _ in 0..self.phone_dials.len() {
            let mut attempt = self.phone_dials.pop_front().expect("retained pending dial");
            if !attempt.current() {
                self.report_expired_dial(&attempt);
                self.finish_pending(attempt, false);
                continue;
            }
            let phase = attempt.observed_phase;
            let ready = attempt.ready();
            if attempt.observed_phase != phase {
                self.record_phone_progress(&attempt, None);
            }
            match ready {
                Err(_) => {
                    self.report_phone_failure(&attempt, attempt.failure_status);
                    self.finish_pending(attempt, false);
                }
                Ok(true) if selected.is_none() => selected = Some(attempt),
                Ok(_) => {
                    if refresh.is_none() && attempt.presentation.needs_reader() {
                        refresh = Some(attempt.local.clone());
                    }
                    self.phone_dials.push_back(attempt);
                }
            }
        }
        if let Some(mut attempt) = selected {
            match self.publish_phone_dial(&mut attempt, clock) {
                Ok(false) => self.phone_dials.push_back(attempt),
                Ok(true) => self.finish_pending(attempt, true),
                Err(_) => {
                    self.report_phone_failure(&attempt, 0);
                    self.finish_pending(attempt, false);
                }
            }
        } else {
            let cleanup = self
                .radio_preparation
                .active
                .0
                .iter()
                .find(|(_, plan)| plan.progress.needs_cleanup())
                .map(|(id, _)| id.to_string());
            if let Some(local) = refresh.or(cleanup)
                && matches!(self.try_refresh(&local, clock), Ok(true))
                && let Ok(plan) = self.radio_preparation.active_plan(&local)
            {
                plan.progress.cleaned();
            }
            if self.phone_dials.len() > 1 {
                self.phone_dials.rotate_left(1);
            }
        }
    }

    fn publish_phone_dial(
        &mut self,
        attempt: &mut Pending,
        clock: RuntimeClock,
    ) -> Result<bool, RuntimeError> {
        if !attempt.current() {
            return Err(RuntimeError::Rejected);
        }
        let local = &attempt.local;
        let account = &attempt.account;
        self.authorize_phone_owner(local, &account.identity, account.generation)?;
        if let Phase::Answered(answer) = &mut attempt.phase {
            let io = answer.take().ok_or(RuntimeError::Rejected)?;
            if io.local() != local
                || io.identity().protocol() != ProtocolId::Phone
                || io.phone_account() != Some(&account.identity)
                || io.generations().0 != account.generation
                || io.generations().1 == 0
                || !io.source_media()
                || io.async_owner().is_none()
            {
                return Err(RuntimeError::Rejected);
            }
            let source = self
                .phone_calls
                .admit_outgoing(io, clock.now_ms, attempt.isolation.take())
                .map_err(|_| RuntimeError::Rejected)?;
            attempt.phase = Phase::Publishing {
                source: source.clone(),
                remaining: 100,
                submitted: false,
            };
            attempt.observed_phase = "publishing";
            self.record_phone_progress(attempt, None);
            let reply = self
                .phone_calls
                .reply(&source)
                .expect("new phone call owns its reply");
            self.phone_calls
                .start_transport(&source, &reply, &mut self.peer_io)
                .map_err(|_| RuntimeError::Rejected)?;
            self.collect_phone_status_edges(clock.now_ms);
        }
        let Phase::Publishing {
            source,
            remaining,
            submitted,
        } = &mut attempt.phase
        else {
            return Err(RuntimeError::Rejected);
        };
        let reply = self
            .phone_calls
            .reply(source)
            .ok_or(RuntimeError::Rejected)?;
        if reply.ended() {
            return Err(RuntimeError::Rejected);
        }
        if self.phone_calls.radio_attachment_confirmed(source, &reply) {
            return Ok(true);
        }
        if !*submitted {
            *submitted = self.try_refresh(local, clock)?;
        }
        *remaining -= 1;
        if *remaining == 0 {
            return Err(RuntimeError::Busy);
        }
        Ok(false)
    }
}
