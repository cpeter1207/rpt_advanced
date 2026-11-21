//! Public shape is built exclusively from validated domain settings.
mod radio;
mod sources;
use super::{Diagnostic, Fault, ProfileInputs};
use rpt_advanced_core::config::{
    EchoLinkAnnouncement, EchoLinkSettings, EchoLinkTelemetry, EchoLinkTransport, LinkLookupMethod,
    resources::{
        ScopeParent,
        candidate::{
            ModularCandidate, ResourceBinding as Binding, ResourceId as Id, ResourceKind as Kind,
            ResourceScope as Scope,
        },
        events::{Boundary, Trigger},
        generation::{RuntimeConfiguration, ScopeConfiguration},
        profiles::*,
    },
};
use serde_json::{Map, Value, json};
pub(super) use sources::Origin;
use std::{collections::BTreeMap, sync::Arc};

pub(super) struct EventLinks {
    pub start: Binding,
    pub end: Option<Binding>,
    pub warnings: Vec<Binding>,
}
type Fields = Map<String, Value>;
#[derive(Default)]
pub(super) struct Projections {
    pub sources: BTreeMap<(Scope, Kind, Option<Id>), sources::Origins>,
    pub message_links: BTreeMap<(Scope, Kind, Option<Id>), Vec<Binding>>,
    pub site_diagnostics: BTreeMap<Kind, Vec<&'static str>>,
    pub warnings: Vec<Diagnostic>,
    pub local: BTreeMap<Id, Fields>,
    pub event_links: BTreeMap<(Scope, Id), EventLinks>,
    pub fixed_effective: BTreeMap<(Scope, Kind), Value>,
    pub effective: BTreeMap<(Scope, Id), Value>,
}

fn parent(p: ScopeParent) -> Value {
    match p {
        ScopeParent::BuiltIn => Value::Null,
        ScopeParent::Site => json!("site"),
    }
}

/// Public views and native configuration obtained from one checked capture and installation.
pub struct PreparedConfiguration {
    pub(super) public: Projections,
    runtime: Arc<RuntimeConfiguration>,
    inputs: Arc<ProfileInputs>,
}
impl PreparedConfiguration {
    /// Value-free normalization diagnostics produced by this checked generation.
    pub(crate) fn diagnostics(&self) -> &[super::Diagnostic] {
        &self.public.warnings
    }

    /// Borrow the installed input snapshot used throughout this candidate's preparation.
    pub fn inputs(&self) -> &Arc<ProfileInputs> {
        &self.inputs
    }
    /// Retain the verified native graph without borrowing captured files or HAL fields.
    pub fn runtime(&self) -> &Arc<RuntimeConfiguration> {
        &self.runtime
    }
    /// Prepare registered owners, compile their relationships and project their public views.
    pub fn prepare(
        candidate: &ModularCandidate<'_>,
        zone: Option<&str>,
        inputs: &Arc<ProfileInputs>,
    ) -> Result<Self, Fault> {
        #[cfg(test)]
        super::FULL_PREPARATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let registry = candidate.registry();
        let mut out = Projections::default();
        let profiles = candidate
            .resolve_protocol_profiles()
            .map_err(|_| Fault::Conflict)?;
        out.warnings.extend(
            profiles
                .warnings()
                .iter()
                .copied()
                .map(Diagnostic::Resource),
        );
        for local in profiles.locals() {
            out.local
                .insert(local.binding.id, local.configuration.clone());
        }
        let radio = candidate
            .resolve_radio_profiles()
            .map_err(|_| Fault::Conflict)?;
        out.warnings.extend(
            radio
                .warnings()
                .iter()
                .chain(radio.site_warnings())
                .copied()
                .map(Diagnostic::Resource),
        );
        for binding in registry.bindings() {
            if let Some(local) = radio.local(binding.id) {
                out.local.insert(binding.id, local.configuration.clone());
            }
        }
        for d in radio.site_diagnostics() {
            out.site_diagnostics
                .entry(d.resource.kind)
                .or_default()
                .push(d.field);
        }
        let mut scopes = BTreeMap::new();
        let site = Scope::Site(registry.site_id());
        for (scope, id) in std::iter::once((site, registry.site_id()))
            .chain(registry.port_keys().map(|(_, p)| (Scope::Port(p), p)))
        {
            let site_meta = candidate.site_metadata();
            let (m, e) = match scope {
                Scope::Site(_) => (site_meta.local(), site_meta.effective()),
                Scope::Port(id) => {
                    let port = candidate.port_metadata(id).ok_or(Fault::Invalid)?;
                    (port.local(), port.effective(site_meta))
                }
            };
            let mut fields = json!({"name":m.name,"inherit_from":parent(m.inherit_from)});
            if let Some(v) = m.enabled {
                fields["enabled"] = json!(v)
            }
            if let Some(v) = &m.callsign {
                fields["callsign"] = json!(v)
            }
            if let Some(v) = &m.language {
                fields["language"] = json!(v)
            }
            out.local
                .insert(id, fields.as_object().expect("metadata object").clone());
            out.effective.insert((scope,id),json!({"name":e.name,"inherit_from":parent(e.inherit_from),"enabled":e.enabled,"callsign":e.callsign,"language":e.language}));
            radio::scope(&mut out, scope, &radio)?;
            let (asl, echo, phone, proxy) = match scope {
                Scope::Site(_) => {
                    let p = profiles.site_accounts();
                    (
                        &p.allstarlink,
                        &p.echolink,
                        &p.phone_patch,
                        p.echolink_proxy,
                    )
                }
                Scope::Port(id) => {
                    let p = profiles.port(id).ok_or(Fault::Invalid)?;
                    (
                        &p.allstarlink,
                        &p.echolink,
                        &p.phone_patch,
                        p.echolink_proxy,
                    )
                }
            };
            let mut aliases = std::collections::BTreeSet::new();
            if matches!(scope, Scope::Port(_))
                && let Some(n) = asl.as_ref().and_then(|p| p.settings.node_number)
            {
                aliases.insert(n.to_string());
            }
            if matches!(scope, Scope::Port(_))
                && let Some(p) = echo
                && p.settings.node_number != 0
            {
                aliases.insert(rpt_advanced_core::link::EchoAlias::format(
                    p.settings.node_number,
                ));
            }
            let groups = candidate
                .permanent_groups(scope, &aliases)
                .map_err(|_| Fault::Conflict)?;
            out.warnings
                .extend(groups.warnings.iter().copied().map(Diagnostic::Resource));
            out.local.extend(groups.local.clone());
            for (b, g) in groups.effective.values() {
                let mut fields = json!({"remote_node":g.remote_nodes});
                if let Some(name) = &g.group_name {
                    fields["group_name"] = json!(name);
                }
                out.effective.insert((scope, b.id), fields);
            }
            if let Some(p) = asl {
                let s = &p.settings;
                let mut public = json!({"node_number":s.node_number,"callsign":s.callsign,"allow_nodes":s.allow_nodes,"deny_nodes":s.deny_nodes,
                    "directory_file":s.directory_file,"lookup_method":match s.lookup_method{LinkLookupMethod::Both=>"both",LinkLookupMethod::Dns=>"dns",LinkLookupMethod::File=>"file"},
                    "iax":{"local_port":s.local_port,"timing":{"registration_interval_s":s.registration_interval_s}},
                    "status_post":{"timing":{"interval_s":s.status_post_interval_s}}});
                if let Some(url) = &s.registration_url {
                    public["iax"]["registration_url"] = json!(url);
                }
                if let Some(url) = &s.status_post_url {
                    public["status_post"]["url"] = json!(url);
                }
                out.protocol(scope, p, public);
            }
            if let Some(p) = phone {
                let s = &p.settings;
                out.protocol(scope,p,json!({"provider":{"protocol":s.provider.protocol.map(|p|match p{PhoneProtocol::Sip=>"sip",PhoneProtocol::Iax2=>"iax2"}),
                        "server":s.provider.server,"port":s.provider.port,"username":s.provider.username},
                    "playout_destination":match s.playout_destination{PhonePlayoutDestination::Local=>"local",PhonePlayoutDestination::LocalAndLinks=>"local_and_links"},
                    "isolate_peers":s.isolate_peers,"timing":{"target_latency_ms":s.timing.target_latency_ms,"maximum_latency_ms":s.timing.maximum_latency_ms},
                    "vad":{"enabled":s.vad.enabled,"mode":match s.vad.mode{VadMode::Continuous=>"continuous",VadMode::Kerchunk=>"kerchunk"},"timing":{"hang_ms":s.vad.hang_ms}}}));
            }
            if let Some(p) = echo {
                out.protocol(scope, p, echo_public(&p.settings));
                if proxy.is_some() || !p.settings.proxy_host.is_empty() {
                    out.fixed_effective.insert(
                        (scope, Kind::EcholinkProxy),
                        json!({"host":p.settings.proxy_host,"port":p.settings.proxy_port}),
                    );
                }
            }
            let policy = candidate.command_policy(scope).ok_or(Fault::Invalid)?;
            out.warnings
                .extend(policy.warnings.iter().copied().map(Diagnostic::Resource));
            let mailboxes = candidate
                .mailbox_profiles(scope)
                .map_err(|_| Fault::Conflict)?;
            out.warnings
                .extend(mailboxes.warnings.iter().copied().map(Diagnostic::Resource));
            for mailbox in mailboxes.effective.values() {
                let public = mailbox.public_fields();
                if mailbox.binding.scope == scope {
                    out.local.insert(mailbox.binding.id, public.clone());
                }
                out.effective
                    .insert((scope, mailbox.binding.id), Value::Object(public));
            }
            let mut mappings = Map::new();
            let commands = candidate
                .command_language_with_mappings(scope, |name, fields| {
                    mappings.insert(name.to_owned(), fields.clone());
                })
                .map_err(|_| Fault::Conflict)?;
            {
                let language = &commands.language;
                let limits = &language.limits;
                let mut command_view = json!({"terminator":language.terminator.to_string(),"maximum_digits":language.maximum_digits,"inactivity_timeout_ms":language.inactivity_timeout_ms,
                    "macro_limits":{"max_symbols":limits.max_symbols,"max_actions":limits.max_actions,"max_depth":limits.max_depth,"timing":{"max_duration_ms":limits.max_duration_ms,"pause_ms":limits.pause_ms}},"mappings":mappings});
                command_view
                    .as_object_mut()
                    .expect("command fields")
                    .extend(policy.effective_fields());
                out.fixed_effective
                    .insert((scope, Kind::Command), command_view);
            }
            for key in commands.language.macros.keys() {
                let (binding, _) = commands.named_macro(key).ok_or(Fault::Conflict)?;
                let actions = commands
                    .language
                    .local
                    .iter()
                    .rev()
                    .find_map(|v| v.get("macros")?.get(key)?.get("actions"))
                    .ok_or(Fault::Invalid)?;
                out.effective
                    .insert((scope, binding.id), json!({"actions":actions}));
            }
            for b in registry
                .bindings()
                .filter(|b| b.scope == scope && matches!(b.kind, Kind::Command | Kind::Macro))
            {
                if let Some(body) = commands.local(b.id) {
                    out.local.insert(b.id, body.clone());
                }
                if b.kind == Kind::Command
                    && let Some(body) = policy.local(b.id)
                {
                    out.local.entry(b.id).or_default().extend(body.clone());
                }
            }
            out.warnings.extend(
                commands
                    .language
                    .warnings
                    .iter()
                    .map(|w| Diagnostic::Command(scope, *w)),
            );
            let language = e.language.parse().map_err(|_| Fault::Invalid)?;
            let installed = inputs
                .messages
                .catalog(&e.language)
                .map_err(|_| Fault::Unavailable)?;
            let (telemetry, messages, media) = candidate
                .prepare_telemetry(scope, installed, &language)
                .map_err(|_| Fault::Conflict)?;
            let command_recipes = messages
                .command_messages(&commands)
                .map_err(|_| Fault::Conflict)?;
            let station = candidate
                .equipment_profiles(
                    scope,
                    |kind, b| {
                        inputs
                            .interfaces
                            .get(&(kind, b.port, b.property.clone()))
                            .copied()
                            .or_else(|| {
                                crate::site::radio::declared_pin_capability(
                                    radio.port(b.port)?,
                                    b,
                                    kind,
                                )
                            })
                    },
                    &commands,
                    &messages,
                )
                .map_err(|_| Fault::Conflict)?;
            let mut add_recipe =
                |kind, id, recipe: &rpt_advanced_core::composition::Composition| {
                    let links = out.message_links.entry((scope, kind, id)).or_default();
                    for key in recipe.referenced_messages() {
                        if let Some((binding, _)) = messages.effective.get(key)
                            && !links.contains(binding)
                        {
                            links.push(*binding);
                        }
                    }
                };
            let echo_status = if let Some(account) = echo
                && !account.settings.status_message.is_empty()
            {
                let status = messages
                    .compile_text_only(&account.settings.status_message)
                    .map_err(|_| Fault::Conflict)?;
                add_recipe(Kind::Echolink, None, &status);
                Some(status)
            } else {
                None
            };
            for key in commands.language.mapping_messages() {
                if let Some(recipe) = command_recipes.get(key) {
                    add_recipe(Kind::Command, None, recipe);
                }
            }
            for name in commands.language.macros.keys() {
                let (b, actions) = commands.named_macro(name).ok_or(Fault::Invalid)?;
                for a in actions {
                    if let rpt_advanced_core::command_language::Action::Message(content, _) =
                        &a.action
                        && let Some(recipe) = command_recipes.get(content)
                    {
                        add_recipe(Kind::Macro, Some(b.id), recipe);
                    }
                }
            }
            for (name, recipe) in &messages.recipes {
                add_recipe(Kind::Message, Some(messages.effective[name].0.id), recipe);
            }
            for ((kind, name), (b, _)) in &media.members {
                let family = match kind {
                    Kind::Identifier => "identifiers",
                    Kind::Announcement => "announcements",
                    Kind::Tail => "tails",
                    Kind::Courtesy => "courtesies",
                    _ => continue,
                };
                if let Some(recipe) = media.recipes.get(&(family, name.clone())) {
                    add_recipe(*kind, Some(b.id), recipe);
                }
            }
            for ((kind, name), (b, _)) in &station.effective {
                if *kind == Kind::Sensor
                    && let Some(recipe) = station.labels.get(name.as_str())
                {
                    add_recipe(*kind, Some(b.id), recipe);
                }
                if *kind == Kind::Alarm {
                    for field in ["trip_message", "clear_message", "repeat_message"] {
                        if let Some(recipe) = station
                            .alarm_messages
                            .get(&(name.as_str().to_owned(), field))
                        {
                            add_recipe(*kind, Some(b.id), recipe);
                        }
                    }
                }
            }
            for warnings in [&telemetry.warnings, &messages.warnings, &station.warnings] {
                out.warnings
                    .extend(warnings.iter().copied().map(Diagnostic::Resource));
            }
            out.sources.insert(
                (scope, Kind::TelemetrySettings, None),
                sources::selected(&media.effective, |p| telemetry.source(p)),
            );
            for ((kind, key), (b, fields)) in &media.members {
                out.sources.insert(
                    (scope, *kind, Some(b.id)),
                    sources::selected(fields, |p| telemetry.member_source(*kind, key, p)),
                );
            }
            for (key, (b, fields)) in &messages.effective {
                out.sources.insert(
                    (scope, Kind::Message, Some(b.id)),
                    sources::selected(fields, |p| {
                        messages.source(key, p).or_else(|| match p {
                            ["speech", "model"] => telemetry.source(&["speech", "voice"]),
                            ["speech" | "morse", ..] => telemetry.source(p),
                            _ => None,
                        })
                    }),
                );
            }
            for ((kind, key), (b, fields)) in &station.effective {
                out.sources.insert(
                    (scope, *kind, Some(b.id)),
                    sources::selected(fields, |p| station.source(*kind, key, p)),
                );
            }
            out.fixed_effective.insert(
                (scope, Kind::TelemetrySettings),
                Value::Object(media.effective.clone()),
            );
            for (binding, fields) in media
                .members
                .values()
                .chain(messages.effective.values())
                .chain(station.effective.values())
            {
                out.effective
                    .insert((scope, binding.id), Value::Object(fields.clone()));
            }
            out.local.extend(telemetry.local.clone());
            out.local.extend(messages.local.clone());
            out.local.extend(station.local.clone());
            let events = candidate
                .event_profiles(&commands, zone)
                .map_err(|_| Fault::Conflict)?;
            out.warnings
                .extend(events.warnings.iter().copied().map(Diagnostic::Resource));
            out.local.extend(events.local.clone());
            for event in &events.events {
                let warnings = event
                    .start
                    .warnings
                    .iter()
                    .chain(event.end.iter().flat_map(|end| &end.warnings))
                    .map(|w| w.command.binding)
                    .collect();
                out.event_links.insert(
                    (scope, event.binding.id),
                    EventLinks {
                        start: event.start.command.binding,
                        end: event.end.as_ref().map(|end| end.command.binding),
                        warnings,
                    },
                );
                let mut body = json!({"start":boundary(&event.start),"restart_recovery":event.restart_recovery});
                if let Some(end) = &event.end {
                    body["end"] = boundary(end);
                }
                if let Some(v) = event.sequence_number {
                    body["sequence_number"] = json!(v);
                }
                if let Some((_, key)) = &event.replace_permanent {
                    body["replace_permanent"] = json!(key.as_str());
                }
                out.effective.insert((scope, event.binding.id), body);
            }
            scopes.insert(
                scope,
                ScopeConfiguration {
                    metadata: e,
                    commands,
                    policy,
                    mailboxes,
                    messages: Arc::new(messages),
                    command_messages: Arc::new(command_recipes),
                    echo_status,
                    telemetry,
                    media,
                    equipment: station,
                    permanent: groups,
                    events,
                },
            );
        }
        let acquisition =
            equipment::ScopedEquipment::acquisition_plan(scopes.values().map(|s| &s.equipment))
                .map_err(|_| Fault::Conflict)?;
        out.add_sources(registry);
        let runtime = Arc::new(RuntimeConfiguration {
            registry: registry.clone(),
            radio,
            protocols: profiles,
            scopes,
            acquisition,
        });
        Ok(Self {
            public: out,
            runtime,
            inputs: Arc::clone(inputs),
        })
    }
}

impl Projections {
    fn protocol<T>(&mut self, scope: Scope, p: &ProtocolProfile<T>, mut fields: Value) {
        let public = fields.as_object_mut().expect("public protocol fields");
        public.retain(|_, v| !v.is_null());
        if let Some(provider) = public.get_mut("provider").and_then(Value::as_object_mut) {
            provider.retain(|_, v| !v.is_null());
        }
        if p.binding.kind == Kind::Echolink
            && public.get("node_number").and_then(Value::as_u64) == Some(0)
        {
            public.remove("node_number");
        }
        let t = p.jitter.timing();
        fields["jitter"] = json!({"timing":{"minimum_ms":t.minimum_ms,"initial_ms":t.initial_ms,"maximum_ms":t.maximum_ms}});
        for (key, field) in [
            ("initial_ms", JitterField::Initial),
            ("minimum_ms", JitterField::Minimum),
            ("maximum_ms", JitterField::Maximum),
        ] {
            self.sources
                .entry((scope, p.binding.kind, None))
                .or_default()
                .insert(
                    format!("/jitter/timing/{key}"),
                    p.jitter.selected_source(field).resource.into(),
                );
        }
        self.effective.insert((scope, p.binding.id), fields);
    }

    pub fn normalized(&self, b: Binding) -> Result<Fields, Fault> {
        self.local.get(&b.id).cloned().ok_or(Fault::Unavailable)
    }
}

fn boundary(b: &Boundary) -> Value {
    let at = match &b.at {
        Trigger::ControllerStart => "controller_start",
        Trigger::Calendar(r) => r.source(),
    };
    json!({"at":at,"macro":b.command.key,"politeness_minutes":b.politeness_minutes,
        "warnings":b.warnings.iter().map(|w|json!({"at":w.at.source(),"macro":w.command.key})).collect::<Vec<_>>()})
}

fn echo_public(s: &EchoLinkSettings) -> Value {
    json!({"enabled":s.enabled,"callsign":s.callsign,"node_number":s.node_number,"name":s.name,"location":s.location,
        "email":s.email,"status_message":s.status_message,"latitude":s.latitude,"longitude":s.longitude,
        "frequency_mhz":s.frequency_mhz,"tone_hz":s.tone_hz,"power_w":s.power_w,"antenna_height_m":s.antenna_height_m,
        "antenna_gain_db":s.antenna_gain_db,"antenna_direction_deg":s.antenna_direction_deg,"directory_servers":s.directory_servers,
        "aprs_server":s.aprs_server,"max_stations":s.max_stations,"max_pending_handshakes":s.max_pending_handshakes,
        "timing":{"heartbeat_interval_ms":s.heartbeat_interval_ms,"heartbeat_timeout_ms":s.heartbeat_timeout_ms,
            "connect_timeout_ms":s.connect_timeout_ms,"directory_timeout_ms":s.directory_timeout_ms,
            "registration_refresh_ms":s.registration_refresh_ms,"directory_refresh_ms":s.directory_refresh_ms,
            "directory_retry_ms":s.directory_retry_ms,"directory_freshness_ms":s.directory_freshness_ms},
        "rx":{"gain_db":s.receive_gain_db},"tx":{"gain_db":s.transmit_gain_db},
        "announcement":match s.announcement{EchoLinkAnnouncement::Off=>"off",EchoLinkAnnouncement::Node=>"node",EchoLinkAnnouncement::Callsign=>"callsign",EchoLinkAnnouncement::Both=>"both"},
        "telemetry":match s.telemetry{EchoLinkTelemetry::Off=>"off",EchoLinkTelemetry::On=>"on",EchoLinkTelemetry::Follow=>"follow",EchoLinkTelemetry::Demand=>"demand"},
        "telemetry_dynamic":s.telemetry_dynamic,"transport":match s.transport{EchoLinkTransport::Direct=>"direct",EchoLinkTransport::Proxy=>"proxy"},
        "bind_address":s.bind_address.to_string(),"audio_port":s.audio_port,"allow":s.allow,"deny":s.deny})
}
