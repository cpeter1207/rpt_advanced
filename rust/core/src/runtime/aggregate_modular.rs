//! Stage the checked domain on the existing aggregate transaction; physical admission is separate.
use super::*;
use crate::config::resources::candidate::{ResourceId, ResourceScope};

/// Candidate-owned generations prepared against one active configuration and adapter epoch.
pub struct PreparedConfigurationTransition<A: Send, C: Send> {
    transaction: ReloadTransaction<A, C>,
    previous_revision: u64,
    previous_generation: u64,
}

impl<A: Send, C: Send> Runtime<A, C> {
    /// Exact modular allocation published by the last confirmed aggregate adoption.
    pub fn modular_configuration(&self) -> Option<&Arc<RuntimeConfiguration>> {
        self.modular.as_ref()
    }

    /// Release affected TX gates after the native owner confirms this exact prepared configuration.
    pub fn confirm_station_replacements(
        &mut self,
        configuration: &Arc<RuntimeConfiguration>,
    ) -> Result<(), RuntimeError> {
        if self.stopping
            || self.reload_pending()
            || !self
                .modular
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, configuration))
        {
            return Err(RuntimeError::Rejected);
        }
        for node in &mut self.nodes {
            if node.failed_station_replacement {
                node.failed_station_replacement = false;
                node.publish_rf_inhibit();
            }
        }
        Ok(())
    }

    /// Prepare each registered station without opening or switching a physical device.
    pub fn prepare_configuration(
        &mut self,
        configuration: Arc<RuntimeConfiguration>,
        media: &dyn NativeMediaPreparer,
        mut adapter: impl FnMut(
            ResourceId,
            &ResolvedNodeSettings,
        ) -> Result<(PreparedAdapter<A, C>, bool), RuntimeError>,
        mut device: impl FnMut(
            ResourceId,
            &ResolvedNodeSettings,
        ) -> Result<Box<dyn DeviceHandoff>, RuntimeError>,
        clock: RuntimeClock,
    ) -> Result<PreparedConfigurationTransition<A, C>, RuntimeError> {
        let (revision, generation_id) = self.next_configuration(clock)?;
        let mut candidates = Vec::new();
        for (_, port) in configuration.registry.port_keys() {
            let settings = configuration.station_settings(port)?;
            if !settings.enabled {
                continue;
            }
            let name = port.to_string();
            let scope = configuration
                .scopes
                .get(&ResourceScope::Port(port))
                .ok_or(RuntimeError::MissingNode)?;
            let accounts = configuration
                .protocols
                .port(port)
                .ok_or(RuntimeError::MissingNode)?;
            let existing = self.nodes.iter().find(|n| n.name == name);
            let mut routes = Vec::new();
            for (_, group) in scope.permanent.effective.values() {
                for (priority, remote) in group.remote_nodes.iter().enumerate() {
                    routes.push(RouteSpec {
                        local: name.clone(),
                        remote: remote.clone(),
                        permanent: true,
                        group_label: Some(group.name.clone()),
                        group_name: group.group_name.clone(),
                        group_priority: Some(priority),
                    });
                }
            }
            let links = LinkScheduler::new(
                revision,
                routes,
                Vec::new(),
                existing.and_then(|n| n.links.schedule()),
            )
            .map_err(|_| RuntimeError::Preparation)?;
            let (mut controller, telemetry, status, media_session, messages) =
                prepare::modular_controller(
                    &configuration,
                    port,
                    &settings,
                    media,
                    generation_id,
                    clock.civil.map(|(civil, _)| civil),
                )?;
            controller.bind_configuration_gate(Arc::clone(&self.configuration_gate));
            let (prepared, station_changed) = adapter(port, &settings)?;
            let mut commands = super::super::dtmf::DtmfCommands::new(
                settings
                    .command_map()
                    .map_err(|_| RuntimeError::Preparation)?,
            );
            commands
                .configure_radio_owner(prepared.remote_commands.as_ref())
                .map_err(|_| RuntimeError::Preparation)?;
            AccessPolicy::new(&settings.link_allow_nodes, &settings.link_deny_nodes)
                .map_err(|_| RuntimeError::Preparation)?;
            crate::link::LinkManager::new(&name).map_err(RuntimeError::Link)?;
            let bounds = GenerationSettings {
                node: name.clone(),
                device: settings.channel.clone(),
                receive_maximum: prepared.receive_maximum,
                transmit_maximum: prepared.transmit_maximum,
                squelch_delay_ms: settings.squelch_delay_ms,
                status_snapshot_interval_ms: settings.status_snapshot_interval_ms,
            };
            let (mut receive, digits) = DtmfWorker::new(generation_id, settings.dtmf_muting)
                .map_err(|_| RuntimeError::Preparation)?;
            receive.bind_receive_mute(telemetry.rx_mute().clone());
            let activity = controller.activity();
            let generation = RuntimeGeneration::prepare(
                generation_id,
                bounds.clone(),
                receive,
                RuntimeTransmit {
                    controller,
                    adapter: prepared.state,
                    settings: settings.clone(),
                },
            )
            .map_err(|_| RuntimeError::Preparation)?;
            let device = if existing.is_none() {
                Some(device(port, &settings)?)
            } else {
                None
            };
            let control = ControlState {
                generation: generation_id,
                adapter: prepared.control,
                adapter_exposed: false,
                telemetry,
                media_session: Some(media_session),
                digits,
                activity,
                started_ms: clock.now_ms,
                prior_activity_ms: existing.and_then(|n| n.control.activity_ms()),
                catalog: Arc::clone(&scope.messages.catalog),
                messages,
            };
            let echo = accounts.echolink.as_ref().map(|p| &p.settings);
            candidates.push(Candidate {
                scope: Some(port),
                commands: None,
                admin: None,
                dtmf_macros: Vec::new(),
                dtmf_macro_limits: Default::default(),
                remote_commands: prepared.remote_commands,
                name,
                settings,
                site: scope.equipment.settings.clone(),
                bounds,
                generation,
                control,
                status,
                time_format: scope.telemetry.time.format,
                station_changed,
                echo_telemetry: crate::link::EchoTelemetryPolicy::new(
                    echo.map_or(crate::config::EchoLinkTelemetry::Off, |e| e.telemetry),
                    echo.is_some_and(|e| e.telemetry_dynamic),
                ),
                echo_announcement: echo
                    .map_or(crate::config::EchoLinkAnnouncement::Off, |e| e.announcement),
                links,
                device,
            });
        }
        let scheduler = EventScheduler::new(revision, Vec::new(), self.scheduler.as_ref())
            .map_err(|_| RuntimeError::Preparation)?;
        let mut staged = PreparedConfigurationTransition {
            previous_revision: self.revision,
            previous_generation: self.generation,
            transaction: ReloadTransaction::new(
                ConfigurationSource::Modular(configuration),
                Vec::new(),
                candidates,
                Vec::new(),
                scheduler,
                (revision, generation_id),
            ),
        };
        staged.prepare_command_owners(self, clock)?;
        Ok(staged)
    }

    /// Start physical adoption of a candidate staged against this exact active generation.
    /// Poll through the existing reload owner until every device accepts or restores.
    pub fn adopt_configuration(
        &mut self,
        mut staged: PreparedConfigurationTransition<A, C>,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        if self.revision != staged.previous_revision
            || self.generation != staged.previous_generation
        {
            return Err(RuntimeError::Rejected);
        }
        self.next_configuration(clock)?;
        // The old scheduler can advance between validation and the selector commit.
        // Carry its latest admissions and activity into the generation that will run.
        staged.prepare_command_owners(self, clock)?;
        self.pending_reload = Some(staged.transaction);
        self.poll_reload(clock).map(|_| ())
    }
}

impl<A: Send, C: Send> PreparedConfigurationTransition<A, C> {
    /// Bind the authenticated final administration record to its exact registered command owner.
    /// A missing member disables the pair; malformed complete records reject staging.
    pub fn bind_admin(
        &mut self,
        port: ResourceId,
        unlock: Option<&str>,
        lock: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let node = self
            .transaction
            .candidates
            .iter_mut()
            .find(|c| c.scope == Some(port))
            .ok_or(RuntimeError::MissingNode)?;
        node.admin = match (unlock, lock) {
            (Some(unlock), Some(lock)) => Some(
                super::super::dtmf::AdminCodes::checked(unlock, lock)
                    .ok_or(RuntimeError::Preparation)?,
            ),
            _ => None,
        };
        Ok(())
    }

    fn prepare_command_owners(
        &mut self,
        runtime: &Runtime<A, C>,
        clock: RuntimeClock,
    ) -> Result<(), RuntimeError> {
        let ConfigurationSource::Modular(domain) = &self.transaction.source else {
            return Err(RuntimeError::Preparation);
        };
        for candidate in &mut self.transaction.candidates {
            let port = candidate.scope.ok_or(RuntimeError::MissingNode)?;
            let scope = domain
                .scopes
                .get(&ResourceScope::Port(port))
                .ok_or(RuntimeError::MissingNode)?;
            let accounts = domain
                .protocols
                .port(port)
                .ok_or(RuntimeError::MissingNode)?;
            let existing = runtime.nodes.iter().find(|n| n.name == candidate.name);
            candidate.commands = Some(command_dispatch::PreparedCommands::new(
                accounts,
                &scope.commands,
                Arc::clone(&scope.messages),
                Arc::clone(&scope.command_messages),
                &scope.events,
                existing,
                self.transaction.generation_id,
                Some(candidate.links.generation()),
                clock,
            )?);
        }
        Ok(())
    }
}
