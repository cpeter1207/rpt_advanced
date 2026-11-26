//! Hardware-free registry captures for the aggregate's modular runtime tests.
use crate::{
    config::resources::{
        candidate::{ResourceKind, ResourceScope},
        generation::{RuntimeConfiguration, ScopeConfiguration},
        profiles::equipment::ScopedEquipment,
    },
    messages::MessageCatalog,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) const PORT: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

pub(crate) const SITE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
pub(crate) const SECOND: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
pub(crate) const THIRD: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
pub(crate) const FOURTH: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";

pub(crate) fn single_port(enabled: bool) -> Arc<RuntimeConfiguration> {
    configuration(
        &[(PORT, "radio")],
        &[(PORT, ResourceKind::Metadata, "", json!({"enabled":enabled}))],
    )
}

pub(crate) fn configuration(
    stations: &[(&str, &str)],
    resources: &[(&str, ResourceKind, &str, Value)],
) -> Arc<RuntimeConfiguration> {
    let fixture = crate::capture::Fixture::new(stations, resources);
    let registry = &fixture.registry;
    let candidate = fixture.candidate();
    let radio = candidate.resolve_radio_profiles().unwrap();
    let protocols = candidate.resolve_protocol_profiles().unwrap();
    let mut scopes = BTreeMap::new();
    for scope in std::iter::once(ResourceScope::Site(registry.site_id()))
        .chain(registry.port_keys().map(|(_, id)| ResourceScope::Port(id)))
    {
        let metadata = match scope {
            ResourceScope::Site(_) => candidate.site_metadata().effective(),
            ResourceScope::Port(id) => candidate
                .port_metadata(id)
                .unwrap()
                .effective(candidate.site_metadata()),
        };
        let commands = candidate.command_language(scope).unwrap();
        let catalog = MessageCatalog::from_sources(
            &metadata.language,
            include_str!("../../../../messages/en-US.ftl"),
            None,
        )
        .unwrap();
        let (telemetry, messages, media) = candidate
            .prepare_telemetry(scope, catalog, &metadata.language.parse().unwrap())
            .unwrap();
        let command_messages = Arc::new(messages.command_messages(&commands).unwrap());
        let equipment = candidate
            .equipment_profiles(scope, |_, _| None, &commands, &messages)
            .unwrap();
        let events = candidate.event_profiles(&commands, Some("UTC")).unwrap();
        scopes.insert(
            scope,
            ScopeConfiguration {
                metadata,
                commands,
                command_messages,
                telemetry,
                media,
                equipment,
                events,
                policy: candidate.command_policy(scope).unwrap(),
                mailboxes: candidate.mailbox_profiles(scope).unwrap(),
                messages: Arc::new(messages),
                echo_status: None,
                permanent: candidate
                    .permanent_groups(scope, &Default::default())
                    .unwrap(),
            },
        );
    }
    let acquisition =
        ScopedEquipment::acquisition_plan(scopes.values().map(|s| &s.equipment)).unwrap();
    Arc::new(RuntimeConfiguration {
        registry: registry.clone(),
        radio,
        protocols,
        scopes,
        acquisition,
    })
}

#[test]
fn unsupported_dsp_inversion_warns_without_rejecting_other_signal_settings() {
    use crate::config::resources::ResourceWarning;
    let fixture = crate::capture::Fixture::new(
        &[(PORT, "radio"), (SECOND, "other")],
        &[
            (
                SITE,
                ResourceKind::RxSignaling,
                "",
                json!({"squelch":{"source":"sound_chip_gpio","inverted":true}}),
            ),
            (
                PORT,
                ResourceKind::RxSignaling,
                "",
                json!({
                    "squelch":{"source":"dsp","level":345},
                    "ctcss":{"source":"dsp","inverted":true,"enabled":true},
                }),
            ),
        ],
    );
    let profiles = fixture.candidate().resolve_radio_profiles().unwrap();
    let scope = ResourceScope::Port(PORT.parse().unwrap());
    let owner = profiles.source(scope, ResourceKind::RxSignaling).unwrap();
    let selected = &profiles.port(PORT.parse().unwrap()).unwrap().signals.rx;
    assert!(!selected.squelch.binding.inverted);
    assert_eq!(selected.squelch.level, 345);
    assert!(!selected.ctcss.binding.inverted);
    assert!(selected.ctcss.enabled);
    assert_eq!(
        profiles
            .warnings()
            .iter()
            .filter(
                |w| w.resource == owner && w.warning == ResourceWarning::InvalidValue("inverted")
            )
            .count(),
        2,
    );
    assert!(
        profiles
            .port(SECOND.parse().unwrap())
            .unwrap()
            .signals
            .rx
            .squelch
            .binding
            .inverted
    );
}

pub(crate) fn start<A: Send, C: Send>(
    configuration: Arc<RuntimeConfiguration>,
    media: &dyn crate::runtime::NativeMediaPreparer,
    adapter: impl FnMut(
        &str,
        &crate::config::ResolvedNodeSettings,
    )
        -> Result<crate::runtime::PreparedAdapter<A, C>, crate::runtime::RuntimeError>,
    device: impl FnMut(
        &str,
        &crate::config::ResolvedNodeSettings,
    )
        -> Result<Box<dyn crate::runtime::DeviceHandoff>, crate::runtime::RuntimeError>,
    clock: crate::runtime::RuntimeClock,
) -> Result<crate::runtime::Runtime<A, C>, crate::runtime::RuntimeError> {
    let mut runtime = crate::runtime::Runtime::idle();
    reload(&mut runtime, configuration, media, adapter, device, clock)?;
    Ok(runtime)
}

pub(crate) fn reload<A: Send, C: Send>(
    runtime: &mut crate::runtime::Runtime<A, C>,
    configuration: Arc<RuntimeConfiguration>,
    media: &dyn crate::runtime::NativeMediaPreparer,
    mut adapter: impl FnMut(
        &str,
        &crate::config::ResolvedNodeSettings,
    ) -> Result<
        crate::runtime::PreparedAdapter<A, C>,
        crate::runtime::RuntimeError,
    >,
    mut device: impl FnMut(
        &str,
        &crate::config::ResolvedNodeSettings,
    ) -> Result<
        Box<dyn crate::runtime::DeviceHandoff>,
        crate::runtime::RuntimeError,
    >,
    clock: crate::runtime::RuntimeClock,
) -> Result<(), crate::runtime::RuntimeError> {
    let previous = runtime.modular_configuration().cloned();
    let transition = runtime.prepare_configuration(
        Arc::clone(&configuration),
        media,
        |id, settings| {
            let changed = previous
                .as_ref()
                .and_then(|p| p.radio.port(id))
                .map(|p| &p.interface)
                != configuration.radio.port(id).map(|p| &p.interface);
            Ok((adapter(&id.to_string(), settings)?, changed))
        },
        |id, settings| device(&id.to_string(), settings),
        clock,
    )?;
    runtime.adopt_configuration(transition, clock)?;
    if !runtime.reload_pending() {
        runtime.confirm_station_replacements(&configuration)?;
    }
    Ok(())
}

pub(crate) fn equipment(
    rows: &[(ResourceKind, &str, Value)],
) -> crate::config::ResolvedSiteSettings {
    let resources: Vec<_> = rows
        .iter()
        .map(|(kind, key, fields)| (PORT, *kind, *key, fields.clone()))
        .collect();
    let domain = configuration(&[(PORT, "1000")], &resources);
    domain.scopes[&ResourceScope::Port(PORT.parse().unwrap())]
        .equipment
        .settings
        .clone()
}

/// A modeled hardware lease binds its incumbent and replacement plans by port UUID,
/// just as the Product adapter does. The fake device receives the selected interface
/// identifier on open and the incumbent identifier on restore.
#[derive(Default)]
pub(crate) struct DevicePlans {
    selectors: Arc<std::sync::Mutex<BTreeMap<String, (String, String)>>>,
}

pub(crate) fn selected_device(configuration: &RuntimeConfiguration, port: &str) -> String {
    let id = port.parse().unwrap();
    let selected = &configuration
        .radio
        .port(id)
        .unwrap()
        .interface
        .audio
        .device_identifier;
    if selected.is_empty() {
        configuration
            .registry
            .port_keys()
            .find(|(_, candidate)| *candidate == id)
            .unwrap()
            .0
            .as_str()
            .into()
    } else {
        selected.clone()
    }
}

struct BoundDevice {
    inner: Box<dyn crate::runtime::DeviceHandoff>,
    selectors: Arc<std::sync::Mutex<BTreeMap<String, (String, String)>>>,
}
impl BoundDevice {
    fn settings(
        &self,
        settings: &crate::runtime::GenerationSettings,
        restore: bool,
    ) -> crate::runtime::GenerationSettings {
        let selectors = self.selectors.lock().unwrap();
        let (previous, next) = &selectors[&settings.node];
        crate::runtime::GenerationSettings {
            device: if restore {
                previous.clone()
            } else {
                next.clone()
            },
            ..settings.clone()
        }
    }
}
impl crate::runtime::DeviceHandoff for BoundDevice {
    fn quiesce(&mut self) -> bool {
        self.inner.quiesce()
    }
    fn close(&mut self) {
        self.inner.close()
    }
    fn open(&mut self, settings: &crate::runtime::GenerationSettings) -> bool {
        self.inner.open(&self.settings(settings, false))
    }
    fn restore(&mut self, settings: &crate::runtime::GenerationSettings) -> bool {
        self.inner.restore(&self.settings(settings, true))
    }
    fn activation_ready(&mut self) -> Result<bool, crate::runtime::RuntimeError> {
        self.inner.activation_ready()
    }
    fn poll_update(
        &mut self,
        settings: &crate::runtime::GenerationSettings,
    ) -> Result<crate::runtime::DeviceUpdate, crate::runtime::RuntimeError> {
        let replacement = self.settings(settings, false);
        if replacement.device != self.settings(settings, true).device {
            return Ok(crate::runtime::DeviceUpdate::Handoff);
        }
        self.inner.poll_update(&replacement)
    }
    fn poll_finish_update(&mut self, commit: bool) -> Result<bool, crate::runtime::RuntimeError> {
        self.inner.poll_finish_update(commit)
    }
}
impl DevicePlans {
    pub(crate) fn reload<A: Send, C: Send>(
        &mut self,
        runtime: &mut crate::runtime::Runtime<A, C>,
        configuration: Arc<RuntimeConfiguration>,
        media: &dyn crate::runtime::NativeMediaPreparer,
        adapter: impl FnMut(
            &str,
            &crate::config::ResolvedNodeSettings,
        ) -> Result<
            crate::runtime::PreparedAdapter<A, C>,
            crate::runtime::RuntimeError,
        >,
        mut device: impl FnMut(
            &str,
            &crate::config::ResolvedNodeSettings,
        ) -> Result<
            Box<dyn crate::runtime::DeviceHandoff>,
            crate::runtime::RuntimeError,
        >,
        clock: crate::runtime::RuntimeClock,
    ) -> Result<(), crate::runtime::RuntimeError> {
        if !runtime.reload_pending() {
            let mut selectors = self.selectors.lock().unwrap();
            for (_, id) in configuration.registry.port_keys() {
                let port = id.to_string();
                let previous = runtime
                    .modular_configuration()
                    .filter(|old| old.radio.port(id).is_some())
                    .map(|old| selected_device(old, &port))
                    .unwrap_or_default();
                selectors.insert(
                    port.clone(),
                    (previous, selected_device(&configuration, &port)),
                );
            }
        }
        reload(
            runtime,
            configuration,
            media,
            adapter,
            |id, settings| {
                Ok(Box::new(BoundDevice {
                    inner: device(id, settings)?,
                    selectors: self.selectors.clone(),
                }) as Box<dyn crate::runtime::DeviceHandoff>)
            },
            clock,
        )
    }
    pub(crate) fn start<A: Send, C: Send>(
        &mut self,
        configuration: Arc<RuntimeConfiguration>,
        media: &dyn crate::runtime::NativeMediaPreparer,
        adapter: impl FnMut(
            &str,
            &crate::config::ResolvedNodeSettings,
        ) -> Result<
            crate::runtime::PreparedAdapter<A, C>,
            crate::runtime::RuntimeError,
        >,
        device: impl FnMut(
            &str,
            &crate::config::ResolvedNodeSettings,
        ) -> Result<
            Box<dyn crate::runtime::DeviceHandoff>,
            crate::runtime::RuntimeError,
        >,
        clock: crate::runtime::RuntimeClock,
    ) -> Result<crate::runtime::Runtime<A, C>, crate::runtime::RuntimeError> {
        let mut runtime = crate::runtime::Runtime::idle();
        self.reload(&mut runtime, configuration, media, adapter, device, clock)?;
        Ok(runtime)
    }
}

pub(crate) fn projected_telemetry(
    rows: &[(ResourceKind, &str, Value)],
) -> Result<
    (
        crate::config::resources::profiles::public_messages::telemetry::ScopedTelemetry,
        crate::config::resources::profiles::public_messages::ScopedMessages,
        crate::config::resources::profiles::public_messages::telemetry::PreparedTelemetry,
    ),
    crate::config::resources::profiles::public_messages::PublicMessageError,
> {
    let resources: Vec<_> = rows
        .iter()
        .map(|(kind, key, body)| (PORT, *kind, *key, body.clone()))
        .collect();
    let fixture = crate::capture::Fixture::new(&[(PORT, "radio")], &resources);
    let catalog = crate::messages::MessageCatalog::from_sources(
        "en-US",
        include_str!("../../../../messages/en-US.ftl"),
        None,
    )
    .unwrap();
    fixture.candidate().prepare_telemetry(
        ResourceScope::Port(PORT.parse().unwrap()),
        catalog,
        &"en-US".parse().unwrap(),
    )
}
