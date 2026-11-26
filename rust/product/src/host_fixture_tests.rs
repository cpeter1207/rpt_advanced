//! Drive the real station transaction with explicit JSON and mocked host edges.
use super::*;
use crate::{management::PreparedConfiguration, management::native::AccountCredentials};
use rpt_advanced_core::config::resources::{
    candidate::ResourceScope, generation::RuntimeConfiguration,
};
use std::{collections::BTreeMap, time::Duration};

fn until(
    host: &mut Host,
    mut poll: impl FnMut(&mut Host) -> Result<bool, RuntimeError>,
) -> Result<(), RuntimeError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if poll(host)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(RuntimeError::Busy);
        }
        // The adoption worker polls the current owner only. A general Host pump
        // can reclaim a prepared media dispatcher before its first station publishes.
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Retain the real reservation while a test observes adoption or rollback.
pub(crate) struct FixtureAdoption {
    ticket: RadioTicket,
    domain: Arc<RuntimeConfiguration>,
    revision: String,
    finished: bool,
}
impl FixtureAdoption {
    pub(crate) fn poll(
        &mut self,
        host: &mut Host,
        clock: RuntimeClock,
    ) -> Result<bool, RuntimeError> {
        if self.finished {
            return Ok(true);
        }
        match host.poll_reload(clock) {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(error) => {
                restore_attempt(host, &self.ticket, Some(&self.revision))?;
                return Err(error);
            }
        }
        drop(host.abort_sites());
        if !host.resume_sites() {
            return Ok(false);
        }
        let attempt = self.ticket.attempt();
        host.runtime.check_station_confirmation(&self.domain)?;
        let finish_phone = host.prepare_account_release(attempt)?;
        if !host.step_network_accounts(attempt, 2)? {
            return Ok(false);
        }
        host.begin_account_release(attempt)?;
        if finish_phone {
            host.finish_private_accounts(attempt, |local, mailbox| {
                let Ok(port) = local.parse() else {
                    return false;
                };
                self.domain
                    .scopes
                    .get(&ResourceScope::Port(port))
                    .is_some_and(|scope| {
                        scope.mailboxes.effective.values().any(|current| {
                            Some(current.binding.id) == mailbox.binding
                                && current.number == mailbox.id
                                && current.first_name == mailbox.first_name
                                && current.callsign == mailbox.callsign
                        })
                    })
            })?;
        }
        if !host.step_network_accounts(attempt, 4)? {
            return Ok(false);
        }
        host.runtime.confirm_station_replacements(&self.domain)?;
        host.confirm_private_media();
        host.finish_asl_selection(attempt)?;
        self.finished = true;
        Ok(true)
    }
}

// Match Engine::apply_modular: release paused sites first, then reverse accounts
// only after confirming that the incumbent radio configuration still owns the station.
fn restore_attempt(
    host: &mut Host,
    ticket: &RadioTicket,
    revision: Option<&str>,
) -> Result<(), RuntimeError> {
    drop(host.abort_sites());
    until(host, |host| Ok(host.resume_sites())).map_err(|_| RuntimeError::Restore)?;
    let restored = match revision {
        Some(revision) => host.modular_rollback_confirmed(revision),
        None => host.verify_private_basis(ticket).is_ok(),
    };
    if !restored {
        return Err(RuntimeError::Restore);
    }
    let attempt = ticket.attempt();
    until(host, |host| host.step_network_accounts(attempt, 3))?;
    until(host, |host| host.restore_private_accounts(attempt))?;
    host.finish_private_accounts(attempt, |_, _| true)?;
    until(host, |host| host.step_network_accounts(attempt, 4))?;
    host.finish_asl_selection(attempt)
}

impl Host {
    /// Borrow the real current connection's reply token for source-lifetime tests.
    pub(crate) fn fixture_peer_reply(
        &self,
        local: &str,
        remote: &str,
    ) -> Option<rpt_advanced_core::link::PeerReply> {
        self.peers
            .iter()
            .find(|peer| peer.local == local && peer.remote == remote && !peer.superseded)
            .map(|peer| peer.control.reply_target())
    }

    /// Poll the real managed shutdown through account withdrawal and physical detachment.
    pub(crate) fn fixture_stop(&mut self, now_ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.stop(now_ms) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    /// Supply an existing fake acquisition edge in an isolated ownership test.
    pub(crate) fn fixture_install_sites(
        &mut self,
        acquisition: crate::site::acquisition::Acquisition,
        radio: crate::site::radio::RadioSite,
    ) {
        self.site_acquisition = acquisition;
        self.site_radio = radio;
    }

    /// Start an explicit typed JSON fixture, including the actual physical handoff.
    pub(crate) fn fixture_start(
        configuration: PreparedConfiguration,
        media: NativeMediaPreparer,
        services: HostServices,
        epoch: Instant,
        clock: RuntimeClock,
    ) -> Result<Self, RuntimeError> {
        Self::fixture_start_with_site(configuration, media, services, epoch, clock, None)
    }

    pub(crate) fn fixture_start_with_site(
        configuration: PreparedConfiguration,
        media: NativeMediaPreparer,
        services: HostServices,
        epoch: Instant,
        clock: RuntimeClock,
        backends: Option<Arc<crate::site::backends::SiteBackends>>,
    ) -> Result<Self, RuntimeError> {
        Self::fixture_start_with_credentials(
            configuration,
            media,
            services,
            epoch,
            clock,
            backends,
            AccountCredentials::default(),
        )
    }

    pub(crate) fn fixture_start_with_credentials(
        configuration: PreparedConfiguration,
        media: NativeMediaPreparer,
        services: HostServices,
        epoch: Instant,
        clock: RuntimeClock,
        backends: Option<Arc<crate::site::backends::SiteBackends>>,
        credentials: AccountCredentials,
    ) -> Result<Self, RuntimeError> {
        let mut host = Self::management_only(media, services, epoch)?;
        host.fixture_reload_with_credentials(configuration, clock, backends, credentials)?;
        Ok(host)
    }

    /// Finish a normal test edit through real account, site, callback and confirmation APIs.
    pub(crate) fn fixture_reload(
        &mut self,
        configuration: PreparedConfiguration,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        self.fixture_reload_with_site(configuration, clock, None)
    }

    pub(crate) fn fixture_reload_with_site(
        &mut self,
        configuration: PreparedConfiguration,
        clock: RuntimeClock,
        backends: Option<Arc<crate::site::backends::SiteBackends>>,
    ) -> Result<(), RuntimeError> {
        self.fixture_reload_with_credentials(
            configuration,
            clock,
            backends,
            AccountCredentials::default(),
        )
    }

    pub(crate) fn fixture_reload_with_credentials(
        &mut self,
        configuration: PreparedConfiguration,
        clock: RuntimeClock,
        backends: Option<Arc<crate::site::backends::SiteBackends>>,
        credentials: AccountCredentials,
    ) -> Result<(), RuntimeError> {
        let mut pending = self.fixture_begin_reload_with_credentials(
            configuration,
            clock,
            backends,
            credentials,
        )?;
        until(self, |host| pending.poll(host, clock))
    }

    pub(crate) fn fixture_begin_reload(
        &mut self,
        configuration: PreparedConfiguration,
        clock: RuntimeClock,
    ) -> Result<FixtureAdoption, RuntimeError> {
        self.fixture_begin_reload_with_credentials(
            configuration,
            clock,
            None,
            AccountCredentials::default(),
        )
    }

    pub(crate) fn fixture_begin_reload_with_credentials(
        &mut self,
        configuration: PreparedConfiguration,
        clock: RuntimeClock,
        backends: Option<Arc<crate::site::backends::SiteBackends>>,
        credentials: AccountCredentials,
    ) -> Result<FixtureAdoption, RuntimeError> {
        let domain = configuration.runtime().clone();
        let inputs = configuration.inputs().clone();
        let ticket = self.stage_radio_plans(&domain)?;
        let attempt = ticket.attempt();
        self.prepare_core(&ticket, domain.clone(), clock)?;
        let credentials = self.bind_core_credentials(&ticket, &domain, credentials)?;
        let basis = self.modular_site_basis(&ticket)?;
        let settings: BTreeMap<_, _> = domain
            .acquisition
            .iter()
            .map(|(scope, settings)| {
                let id = match scope {
                    ResourceScope::Site(id) | ResourceScope::Port(id) => id,
                };
                (id.to_string(), settings.clone())
            })
            .collect();
        let backends = match backends {
            Some(backends) => backends,
            None => Arc::new(crate::site::backends::SiteBackends::load_scopes(
                settings.values(),
            )?),
        };
        let mut acquisition = Some(
            crate::site::acquisition::Acquisition::prepare_reload(
                &settings,
                backends,
                self.epoch,
                basis.known,
            )
            .map_err(|_| RuntimeError::Preparation)?,
        );
        let mut radio = Some(
            crate::site::radio::RadioSite::prepare(&settings)
                .map_err(|_| RuntimeError::Preparation)?,
        );
        self.verify_private_basis(&ticket)?;
        let revision = format!("fixture-{}", self.runtime.revision().saturating_add(1));
        let mut entered_core = false;
        let started = (|| {
            until(self, |host| host.apply_private_accounts(attempt))?;
            until(self, |host| host.step_network_accounts(attempt, 1))?;
            self.begin_site_reload(
                basis.revision,
                &mut acquisition,
                &mut radio,
                &basis.replaced,
            )?;
            until(self, |host| host.site_barrier(clock.now_ms))?;
            entered_core = true;
            self.begin_core_adoption(ticket.clone(), revision.clone(), credentials, inputs, clock)
        })();
        if let Err(error) = started {
            restore_attempt(self, &ticket, entered_core.then_some(revision.as_str()))?;
            return Err(error);
        }
        Ok(FixtureAdoption {
            ticket,
            domain,
            revision,
            finished: false,
        })
    }
}
