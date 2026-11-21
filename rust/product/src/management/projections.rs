//! Public shape is built exclusively from validated domain settings.
use super::{Diagnostic, Fault};
use rpt_advanced_core::config::{
    EchoLinkAnnouncement, EchoLinkSettings, EchoLinkTelemetry, EchoLinkTransport, LinkLookupMethod,
    resources::{
        ScopeParent,
        candidate::{
            ModularCandidate, ResourceBinding as Binding, ResourceId as Id, ResourceKind as Kind,
            ResourceRegistry, ResourceScope as Scope,
        },
        events::{Boundary, Trigger},
        profiles::*,
    },
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub(super) struct EventLinks {
    pub start: Binding,
    pub end: Option<Binding>,
    pub warnings: Vec<Binding>,
}
type Fields = Map<String, Value>;
#[derive(Default)]
pub(super) struct Projections {
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

impl Projections {
    pub fn prepare(
        candidate: &ModularCandidate<'_>,
        registry: &ResourceRegistry,
        zone: Option<&str>,
    ) -> Result<Self, Fault> {
        let mut out = Self::default();
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
            if let Some(p) = asl {
                let s = &p.settings;
                out.protocol(scope,p,json!({"node_number":s.node_number,"callsign":s.callsign,"allow_nodes":s.allow_nodes,"deny_nodes":s.deny_nodes,
                    "directory_file":s.directory_file,"lookup_method":match s.lookup_method{LinkLookupMethod::Both=>"both",LinkLookupMethod::Dns=>"dns",LinkLookupMethod::File=>"file"},
                    "iax":{"registration_url":s.registration_url,"local_port":s.local_port,"timing":{"registration_interval_s":s.registration_interval_s}},
                    "status_post":{"url":s.status_post_url,"timing":{"interval_s":s.status_post_interval_s}}}));
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
            let events = candidate
                .event_profiles(&commands, zone)
                .map_err(|_| Fault::Conflict)?;
            out.warnings
                .extend(events.warnings.iter().copied().map(Diagnostic::Resource));
            out.local.extend(events.local);
            for event in events.events {
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
                if let Some(end) = event.end {
                    body["end"] = boundary(&end);
                }
                if let Some(v) = event.sequence_number {
                    body["sequence_number"] = json!(v);
                }
                if let Some((_, key)) = event.replace_permanent {
                    body["replace_permanent"] = json!(key.as_str());
                }
                out.effective.insert((scope, event.binding.id), body);
            }
        }
        Ok(out)
    }

    fn protocol<T>(&mut self, scope: Scope, p: &ProtocolProfile<T>, mut fields: Value) {
        let t = p.jitter.timing();
        fields["jitter"] = json!({"timing":{"minimum_ms":t.minimum_ms,"initial_ms":t.initial_ms,"maximum_ms":t.maximum_ms}});
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
