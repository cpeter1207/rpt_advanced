//! Build a station controller from the same checked settings as the management views.
use super::{
    Announcement, ControllerControl, ControllerSettings, CourtesySettings, Identifier,
    MessagePreparation, MorseSettings, NodeController, PreparedMedia, RuntimeError,
};
use crate::{
    composition::Composition,
    config::resources::{
        candidate::{ResourceBinding, ResourceId, ResourceKind, ResourceScope},
        generation::RuntimeConfiguration,
    },
    controller::Tail,
};

impl RuntimeConfiguration {
    /// Controller and peer policy for a real registered port. The internal station slot is
    /// its UUID. Only explicitly attached protocol accounts supply network settings.
    pub fn station_settings(
        &self,
        port: ResourceId,
    ) -> Result<crate::config::ResolvedNodeSettings, RuntimeError> {
        let radio = self.radio.port(port).ok_or(RuntimeError::MissingNode)?;
        let scope = self
            .scopes
            .get(&ResourceScope::Port(port))
            .ok_or(RuntimeError::MissingNode)?;
        let accounts = self.protocols.port(port).ok_or(RuntimeError::MissingNode)?;
        let asl = accounts
            .allstarlink
            .as_ref()
            .map(|profile| &profile.settings);
        Ok(crate::config::ResolvedNodeSettings {
            enabled: scope.metadata.enabled,
            full_duplex: radio.general.full_duplex(),
            dtmf_muting: scope.policy.muting,
            dtmf_regeneration: scope.policy.regeneration,
            parrot_enabled: radio.general.parrot_enabled,
            squelch_delay_ms: radio.signals.rx.squelch_delay_ms.into(),
            status_snapshot_interval_ms: radio.general.status_snapshot_interval_ms.into(),
            hang_ms: radio.signals.tx.hang_ms.into(),
            ctcss_encode_on_input: radio.signals.tx.ctcss.on_input,
            ctcss_hang_ms: radio.signals.tx.ctcss.hang_ms.into(),
            transmit_timeout_ms: radio.signals.tx.timeout_ms.into(),
            kerchunk_max_ms: radio.signals.rx.kerchunk_max_ms.into(),
            telemetry_duck_db: scope.telemetry.duck_db,
            courtesy_delay_ms: scope.telemetry.courtesy_delay_ms,
            channel: port.to_string(),
            radio: Default::default(),
            language: scope.metadata.language.clone(),
            callsign: scope.metadata.callsign.clone(),
            statpost_url: asl
                .and_then(|a| a.status_post_url.clone())
                .unwrap_or_default(),
            statpost_time: asl.map_or(0, |a| a.status_post_interval_s.into()),
            iax_registration_url: asl
                .and_then(|a| a.registration_url.clone())
                .unwrap_or_default(),
            iax_registration_interval_s: asl.map_or(0, |a| a.registration_interval_s.into()),
            iax_local_port: asl.map_or(0, |a| a.local_port),
            link_allow_nodes: asl.map(|a| a.allow_nodes.join(",")).unwrap_or_default(),
            link_deny_nodes: asl.map(|a| a.deny_nodes.join(",")).unwrap_or_default(),
            link_static_directory_file: String::new(),
            link_directory_file: asl.map(|a| a.directory_file.clone()).unwrap_or_default(),
            link_lookup_method: asl.map(|a| a.lookup_method).unwrap_or_default(),
            link_commands: Default::default(),
            dtmf_admin_unlock_hash: String::new(),
            dtmf_admin_lock_hash: String::new(),
            dtmf_admin_timeout_ms: scope.policy.admin_timeout_ms,
        })
    }

    /// Prepare one enabled registry port and register its checked recipes with this generation.
    pub fn prepare_station_controller(
        &self,
        port: ResourceId,
        generation: u64,
        mut register: impl FnMut(ResourceBinding, &str, &Composition) -> Result<usize, RuntimeError>,
    ) -> Result<(NodeController, ControllerControl), RuntimeError> {
        if generation == 0 {
            return Err(RuntimeError::Preparation);
        }
        let radio = self
            .radio
            .port(port)
            .filter(|radio| radio.metadata.enabled)
            .ok_or(RuntimeError::MissingNode)?;
        let scope = self
            .scopes
            .get(&ResourceScope::Port(port))
            .ok_or(RuntimeError::MissingNode)?;
        let telemetry = &scope.telemetry;
        let settings = ControllerSettings {
            receive_disabled: !radio.rx_audio.enabled,
            transmit_disabled: !radio.tx_audio.enabled,
            parrot_enabled: radio.general.parrot_enabled,
            full_duplex: radio.general.full_duplex(),
            hang_ms: radio.signals.tx.hang_ms.into(),
            ctcss_encode_on_input: radio.signals.tx.ctcss.on_input,
            ctcss_hang_ms: radio.signals.tx.ctcss.hang_ms.into(),
            transmit_timeout_ms: radio.signals.tx.timeout_ms.into(),
            kerchunk_max_ms: radio.signals.rx.kerchunk_max_ms.into(),
            courtesy_delay_ms: telemetry.courtesy_delay_ms,
            telemetry_duck_db: telemetry
                .duck_db
                .try_into()
                .map_err(|_| RuntimeError::Preparation)?,
            status_morse: MorseSettings {
                speed_wpm: telemetry
                    .morse
                    .speed_wpm
                    .try_into()
                    .map_err(|_| RuntimeError::Preparation)?,
                frequency_hz: telemetry.morse.frequency_hz as f32,
                level_db: telemetry
                    .morse
                    .level_db
                    .try_into()
                    .map_err(|_| RuntimeError::Preparation)?,
            },
        };
        let mut messages = MessagePreparation::new(generation);
        let mut media_for = |kind,
                             family: &'static str,
                             name: &String|
         -> Result<Option<PreparedMedia>, RuntimeError> {
            let Some(recipe) = scope.media.recipes.get(&(family, name.clone())) else {
                return Ok(None);
            };
            let (binding, _) = scope
                .media
                .members
                .get(&(kind, name.clone()))
                .ok_or(RuntimeError::Preparation)?;
            let index = register(*binding, name, recipe)?;
            let media = messages
                .register(index)
                .map_err(|_| RuntimeError::Preparation)?;
            Ok(Some(
                media.with_playback_policy(recipe.settings_for(None).playback),
            ))
        };

        let mut identifiers = Vec::new();
        for (name, id) in &scope.media.identifiers {
            if let Some(media) = media_for(ResourceKind::Identifier, "identifiers", name)? {
                identifiers.push(Identifier {
                    media,
                    interval_ms: id.interval_ms,
                    priority: id
                        .priority
                        .try_into()
                        .map_err(|_| RuntimeError::Preparation)?,
                    first_key_only: id.first_key_only,
                    regardless_of_activity: id.regardless_of_activity,
                    polite_maximum_wait_ms: id.polite.then_some(id.polite_maximum_wait_ms),
                });
            }
        }
        let mut announcements = Vec::new();
        for (name, announcement) in &scope.media.announcements {
            if let Some(media) = media_for(ResourceKind::Announcement, "announcements", name)? {
                announcements.push(Announcement {
                    media,
                    interval_ms: announcement.interval_ms,
                });
            }
        }
        let mut courtesy = CourtesySettings::default();
        for (name, entry) in &scope.media.courtesies {
            if let Some(media) = media_for(ResourceKind::Courtesy, "courtesies", name)? {
                if entry.input == "receiver" {
                    courtesy.receiver = Some(media);
                } else if entry.remote_node.is_empty() {
                    courtesy.link = Some(media);
                } else {
                    courtesy.peers.push((entry.remote_node.clone(), media));
                }
            }
        }
        let mut tails = Vec::new();
        for (name, tail) in &scope.media.tails {
            if let Some(media) = media_for(ResourceKind::Tail, "tails", name)? {
                tails.push(Tail {
                    media,
                    interval_ms: tail.interval_ms,
                    delay_ms: tail.delay_ms,
                });
            }
        }
        let (mut controller, mut control) =
            NodeController::new(settings, identifiers, announcements, courtesy)
                .map_err(|_| RuntimeError::Preparation)?;
        controller
            .set_tails(tails)
            .map_err(|_| RuntimeError::Preparation)?;
        control.set_message_preparation(messages);
        Ok((controller, control))
    }
}

/// Prepare controller and status on their existing generation-owned media session.
pub(in crate::runtime) fn controller(
    domain: &RuntimeConfiguration,
    port: ResourceId,
    settings: &crate::config::ResolvedNodeSettings,
    media: &dyn super::NativeMediaPreparer,
    generation: u64,
    civil: Option<crate::schedule::CivilTime>,
) -> Result<super::PreparedController, RuntimeError> {
    use crate::runtime::composition::{MessageRecipe, MessageRecipes};
    let scope = domain
        .scopes
        .get(&ResourceScope::Port(port))
        .ok_or(RuntimeError::MissingNode)?;
    let name = port.to_string();
    let mut recipes = MessageRecipes::modular(scope, &name)?;
    let (mut controller, control) =
        domain.prepare_station_controller(port, generation, |binding, _, recipe| {
            recipes.register(
                MessageRecipe::Composition(Box::new(recipe.clone())),
                recipe.settings_for(None).dtmf_id.clone(),
                &binding.id.to_string(),
            )
        })?;
    let telemetry = &scope.telemetry;
    // Built-in status and timeout media consume only the scope's renderer defaults.
    let status = super::ResolvedIdentifierSettings {
        dtmf_id: None,
        playback: Default::default(),
        delivery: Default::default(),
        content: String::new(),
        message_id: None,
        bindings: Default::default(),
        interval_ms: 0,
        priority: 0,
        first_key_only: false,
        regardless_of_activity: false,
        polite: false,
        polite_maximum_wait_ms: 0,
        sound_file: String::new(),
        speech_text: String::new(),
        speech_model: telemetry.speech.voice.clone(),
        speech_speed_percent: telemetry.speech.speed_percent,
        speech_level_db: telemetry.speech.level_db,
        morse_text: String::new(),
        morse_speed_wpm: telemetry.morse.speed_wpm,
        morse_frequency_hz: telemetry.morse.frequency_hz,
        morse_level_db: telemetry.morse.level_db,
    };
    let mut station = media
        .station(&name, generation)
        .map_err(|_| RuntimeError::Preparation)?;
    super::set_timeout_media(
        &mut controller,
        station.as_mut(),
        settings.transmit_timeout_ms,
        &status,
        &scope.messages.catalog,
        civil,
    )?;
    station.start().map_err(|_| RuntimeError::Preparation)?;
    Ok((controller, control, status, station, recipes))
}
