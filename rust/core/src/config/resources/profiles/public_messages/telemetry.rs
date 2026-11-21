//! Typed controller telemetry and the named consumers of a scope's Fluent catalog.

use super::*;
use crate::config::{
    ResolvedAnnouncementSettings, ResolvedCourtesySettings, ResolvedIdentifierSettings,
    ResolvedTailSettings, ResolvedTimeSettings,
};

const FAMILIES: [(ResourceKind, &str); 4] = [
    (ResourceKind::Identifier, "identifiers"),
    (ResourceKind::Announcement, "announcements"),
    (ResourceKind::Tail, "tails"),
    (ResourceKind::Courtesy, "courtesies"),
];
type MemberKey = (ResourceKind, String);
type Members = BTreeMap<MemberKey, (ResourceBinding, Object)>;
const SCALARS: Fields = &[
    ("duck_db", Rule::Value(level)),
    (
        "timing",
        Rule::Object(&[
            ("courtesy_delay_ms", Rule::Unsigned(0, u64::MAX / 48)),
            (
                "clock_format",
                Rule::Value(|v| v.as_u64().filter(|n| matches!(n, 12 | 24)).map(Value::from)),
            ),
        ]),
    ),
    (
        "morse",
        Rule::Object(&[
            ("frequency_hz", Rule::Unsigned(1, 23999)),
            ("level_db", Rule::Value(level)),
            (
                "timing",
                Rule::Object(&[("speed_wpm", Rule::Unsigned(1, 100))]),
            ),
        ]),
    ),
    (
        "speech",
        Rule::Object(&[
            ("voice", Rule::Value(text)),
            ("level_db", Rule::Value(level)),
            (
                "timing",
                Rule::Object(&[("speed_percent", Rule::Unsigned(1, 1000))]),
            ),
        ]),
    ),
];
// Automatic identifiers and tails support local playback with optional link fanout.
const TELEMETRY_PLAYBACK: Rule = Rule::Object(&[
    ("destination", Rule::Choice(&["local", "local_and_links"])),
    ("repeat", Rule::Bool),
]);
const ID: Fields = &[
    ("playback", TELEMETRY_PLAYBACK),
    (
        "timing",
        Rule::Object(&[("interval_ms", Rule::Unsigned(1, u64::MAX / 48))]),
    ),
    ("priority", Rule::Unsigned(0, i32::MAX as u64)),
    ("first_key_only", Rule::Bool),
    ("regardless_of_activity", Rule::Bool),
];
const ANNOUNCEMENT: Fields = &[(
    "timing",
    Rule::Object(&[("interval_ms", Rule::Unsigned(0, u64::MAX / 48))]),
)];
const TAIL: Fields = &[
    ("playback", TELEMETRY_PLAYBACK),
    (
        "timing",
        Rule::Object(&[
            ("interval_ms", Rule::Unsigned(0, u64::MAX / 48)),
            ("delay_ms", Rule::Unsigned(0, u64::MAX / 48)),
        ]),
    ),
];
const COURTESY: Fields = &[
    ("input", Rule::Choice(&["receiver", "link"])),
    ("level_db", Rule::Value(level)),
    (
        "remote_node",
        Rule::Value(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.len() <= 63 && s.bytes().all(|c| c.is_ascii_digit()))
                .map(Value::from)
        }),
    ),
];

fn common(family: &str, key: &str) -> bool {
    matches!(key, "file" | "speech" | "morse" | "bindings")
        || (matches!(family, "identifiers" | "tails")
            && matches!(key, "content" | "dtmf_id" | "polite"))
        || (matches!(family, "courtesies" | "tails") && key == "tone")
}
fn normalize_entry(
    binding: ResourceBinding,
    family: &str,
    body: &Object,
    warnings: &mut Vec<CandidateWarning>,
) -> Result<Object, PublicMessageError> {
    let mut shared = body
        .iter()
        .filter(|(k, _)| common(family, k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<Object>();
    let mut enabled = None;
    if family == "identifiers"
        && let Some(polite) = shared.get_mut("polite").and_then(Value::as_object_mut)
    {
        if let Some(v) = polite.remove("enabled") {
            if let Some(v) = v.as_bool() {
                enabled = Some(v);
            } else {
                normalize::warn(warnings, binding, "enabled");
            }
        }
        if let Some(t) = polite.get_mut("timing").and_then(Value::as_object_mut)
            && t.get("maximum_wait_ms")
                .and_then(Value::as_u64)
                .is_some_and(|wait| !(1..=u64::MAX / 48).contains(&wait))
        {
            t.remove("maximum_wait_ms");
            normalize::warn(warnings, binding, "maximum_wait_ms");
        }
    }
    if family == "courtesies" {
        for k in ["speech", "morse", "tone"] {
            if shared
                .get_mut(k)
                .and_then(Value::as_object_mut)
                .and_then(|s| s.remove("level_db"))
                .is_some()
            {
                warnings.push(CandidateWarning {
                    resource: binding,
                    warning: ResourceWarning::UnknownField,
                });
            }
        }
    }
    let mut fields = normalized(binding, &shared, warnings)?;
    if let Some(v) = enabled {
        fields
            .entry("polite")
            .or_insert(json!({}))
            .as_object_mut()
            .expect("normalized polite object")
            .insert("enabled".into(), v.into());
    }
    let schema = match family {
        "identifiers" => ID,
        "announcements" => ANNOUNCEMENT,
        "tails" => TAIL,
        "courtesies" => COURTESY,
        _ => unreachable!(),
    };
    fields.extend(normalize::normalize_fields(
        binding,
        body.iter().filter(|(k, _)| !common(family, k)),
        schema,
        warnings,
    ));
    Ok(fields)
}

/// Public authoring layers and scalar settings required before building this scope's catalog.
pub struct ScopedTelemetry {
    /// Exact site or port identity, including an empty registry of ports.
    pub scope: ResourceScope,
    /// Public, recognized local declarations keyed by real telemetry section ID.
    pub local: BTreeMap<ResourceId, Object>,
    /// Gain applied to repeated traffic while local telemetry plays.
    pub duck_db: i64,
    /// Delay before the selected courtesy begins.
    pub courtesy_delay_ms: u64,
    /// Clock presentation consumed by existing time bindings.
    pub time: ResolvedTimeSettings,
    /// Effective speech defaults passed to message_profiles.
    pub speech: ResolvedSpeechSettings,
    /// Effective Morse defaults passed to message_profiles.
    pub morse: ResolvedMorseSettings,
    /// Value-free fallback and unknown field warnings.
    pub warnings: Vec<CandidateWarning>,
    fields: Object,
    layers: Vec<ResourceBinding>,
    members: Members,
    authors: BTreeMap<MemberKey, Vec<ResourceBinding>>,
}

/// All controller telemetry consumers checked against the same generation's media namespace.
pub struct PreparedTelemetry {
    /// Canonical rendering for the effective singleton section.
    pub effective: Object,
    /// Checked effective named resource fields and their real declaring identities.
    pub members: BTreeMap<(ResourceKind, String), (ResourceBinding, Object)>,
    /// ID media, scheduling, polite policy and manual selector.
    pub identifiers: BTreeMap<String, ResolvedIdentifierSettings>,
    /// Release announcement media and interval.
    pub announcements: BTreeMap<String, ResolvedAnnouncementSettings>,
    /// Composable tails, repeat/scope policy and release timing.
    pub tails: BTreeMap<String, ResolvedTailSettings>,
    /// Link/receiver selection and common courtesy gain.
    pub courtesies: BTreeMap<String, ResolvedCourtesySettings>,
    /// Compiled per-family named recipe for the existing controller's playback owner.
    pub recipes: BTreeMap<(&'static str, String), Composition>,
}

fn requires_deadline(recipe: &Composition) -> bool {
    use crate::composition::Segment;
    // Playback requires the active bindings; lower renderings can be skipped when context is absent.
    let required = |sources: &BTreeMap<String, BindingSource>| {
        sources
            .values()
            .any(|source| !source.available_in_context(false))
    };
    recipe.segments().iter().any(|segment| match segment {
        Segment::Fluent { text, .. } => required(&text.bindings),
        Segment::FluentMedia { speech, morse, .. } => speech
            .iter()
            .chain(morse)
            .any(|text| required(&text.bindings)),
        Segment::Message(owner) => required(&recipe.settings_for(owner.as_deref()).bindings),
        _ => false,
    })
}

impl ModularCandidate<'_> {
    /// Prepare the telemetry settings, message catalog, and consumer recipes from this single capture.
    pub fn prepare_telemetry(
        &self,
        scope: ResourceScope,
        catalog: MessageCatalog,
        language: &LanguageIdentifier,
    ) -> Result<(ScopedTelemetry, ScopedMessages, PreparedTelemetry), PublicMessageError> {
        let settings = self.telemetry_profile(scope)?;
        let messages =
            self.message_profiles(scope, catalog, language, &settings.speech, &settings.morse)?;
        let prepared = settings.prepare_members(&messages)?;
        Ok((settings, messages, prepared))
    }

    /// Resolve renderer and timing defaults before projecting named messages.
    pub fn telemetry_profile(
        &self,
        scope: ResourceScope,
    ) -> Result<ScopedTelemetry, PublicMessageError> {
        let site = ResourceScope::Site(self.registry().site_id());
        let port = match scope {
            ResourceScope::Site(_) if scope == site => None,
            ResourceScope::Port(id) if self.port_metadata(id).is_some() => Some(scope),
            _ => return Err(PublicMessageError::MissingScope),
        };
        let mut local = BTreeMap::new();
        let mut layers = Vec::new();
        let mut warnings = Vec::new();
        let mut fields = Object::new();
        let mut members = Members::new();
        let mut authors = BTreeMap::<MemberKey, Vec<ResourceBinding>>::new();
        for owner in [Some(site), port].into_iter().flatten() {
            if let Some(section) = self.staged(owner, ResourceKind::TelemetrySettings) {
                let binding = section.binding();
                let sparse =
                    normalize::normalize(binding, section.fields(), SCALARS, &mut warnings);
                merge(&mut fields, &sparse);
                local.insert(binding.id, sparse);
                layers.push(binding);
            }
            for (binding, name) in self
                .registry()
                .members()
                .iter()
                .filter(|(b, _)| b.scope == owner)
            {
                let Some((_, family)) = FAMILIES.iter().find(|(kind, _)| *kind == binding.kind)
                else {
                    continue;
                };
                let name = name.as_str();
                if name.is_empty()
                    || name.chars().count() > 63
                    || name.chars().any(|c| c.is_whitespace() || "[]:".contains(c))
                {
                    return Err(PublicMessageError::Definition(*binding));
                }
                let checked = normalize_entry(
                    *binding,
                    family,
                    self.member_fields(binding.id)
                        .ok_or(PublicMessageError::Definition(*binding))?,
                    &mut warnings,
                )?;
                let key = (binding.kind, name.to_owned());
                let (origin, inherited) = members
                    .entry(key.clone())
                    .or_insert((*binding, Object::new()));
                if let Some(choice) = payload(&checked, *binding)?
                    && payload(inherited, *origin)?.is_some_and(|prior| choice != prior)
                {
                    replace_payload(inherited);
                }
                merge(inherited, &checked);
                *origin = *binding;
                authors.entry(key).or_default().push(*binding);
                local.insert(binding.id, checked);
            }
        }
        let mut canonical=json!({"duck_db":-20,"timing":{"courtesy_delay_ms":250,"clock_format":12},"speech":{"voice":"en_US-lessac-medium.onnx","timing":{"speed_percent":100},"level_db":0},"morse":{"frequency_hz":800,"timing":{"speed_wpm":20},"level_db":-6}}).as_object().unwrap().clone();
        merge(&mut canonical, &fields);
        let value = |k: &str| &canonical[k];
        let speech = ResolvedSpeechSettings {
            voice: value("speech")["voice"].as_str().unwrap().into(),
            speed_percent: value("speech")["timing"]["speed_percent"].as_u64().unwrap(),
            level_db: value("speech")["level_db"].as_i64().unwrap(),
        };
        let morse = ResolvedMorseSettings {
            frequency_hz: value("morse")["frequency_hz"].as_u64().unwrap(),
            speed_wpm: value("morse")["timing"]["speed_wpm"].as_u64().unwrap(),
            level_db: value("morse")["level_db"].as_i64().unwrap(),
        };
        Ok(ScopedTelemetry {
            scope,
            local,
            layers,
            members,
            authors,
            warnings,
            speech,
            morse,
            duck_db: value("duck_db").as_i64().unwrap(),
            courtesy_delay_ms: value("timing")["courtesy_delay_ms"].as_u64().unwrap(),
            time: ResolvedTimeSettings {
                format: value("timing")["clock_format"].as_u64().unwrap(),
            },
            fields: canonical,
        })
    }
}

impl ScopedTelemetry {
    /// Return the authoring section for a checked leaf, or None for a built-in.
    pub fn source(&self, path: &[&str]) -> Option<ResourceBinding> {
        let (first, rest) = path.split_first()?;
        rest.iter()
            .try_fold(self.fields.get(*first)?, |v, k| v.get(*k))?;
        self.layers
            .iter()
            .rev()
            .find(|binding| {
                self.local[&binding.id]
                    .get(*first)
                    .and_then(|v| rest.iter().try_fold(v, |v, k| v.get(*k)))
                    .is_some()
            })
            .copied()
    }

    /// Return the named author or inherited telemetry scalar supplying an effective leaf.
    pub fn member_source(
        &self,
        kind: ResourceKind,
        name: &str,
        path: &[&str],
    ) -> Option<ResourceBinding> {
        let key = (kind, name.to_owned());
        let (first, rest) = path.split_first()?;
        let has = |fields: &Object| {
            fields
                .get(*first)
                .and_then(|v| rest.iter().try_fold(v, |v, k| v.get(*k)))
                .is_some()
        };
        if has(&self.members.get(&key)?.1) {
            return self
                .authors
                .get(&key)?
                .iter()
                .rev()
                .find(|b| has(&self.local[&b.id]))
                .copied();
        }
        match path {
            ["speech", "model"] => self.source(&["speech", "voice"]),
            ["speech", "timing", "speed_percent"]
            | ["morse", "timing", "speed_wpm"]
            | ["morse", "frequency_hz"] => self.source(path),
            ["speech" | "morse", "level_db"] if kind != ResourceKind::Courtesy => self.source(path),
            _ => None,
        }
    }
    /// Check every identifier, announcement, tail and courtesy against the shared catalog.
    fn prepare_members(
        &self,
        messages: &ScopedMessages,
    ) -> Result<PreparedTelemetry, PublicMessageError> {
        let mut result = PreparedTelemetry {
            effective: self.fields.clone(),
            members: self.members.clone(),
            identifiers: BTreeMap::new(),
            announcements: BTreeMap::new(),
            tails: BTreeMap::new(),
            courtesies: BTreeMap::new(),
            recipes: BTreeMap::new(),
        };
        let mut courtesy_routes = BTreeSet::new();
        let mut selectors = messages
            .recipes
            .values()
            .filter_map(|recipe| recipe.settings_for(None).dtmf_id.clone())
            .collect::<BTreeSet<_>>();
        for ((kind, name), (binding, fields)) in &mut result.members {
            let family = FAMILIES
                .iter()
                .find(|(k, _)| k == kind)
                .expect("typed telemetry member")
                .1;
            let binding = *binding;
            let error = PublicMessageError::Definition(binding);
            let mut config = settings(name, fields, &self.speech, &self.morse);
            config.message_id = None;
            config.bindings = localized(fields)?.bindings;
            let timing = |key: &str, default: u64| {
                fields
                    .get("timing")
                    .and_then(|v| v.get(key))
                    .and_then(Value::as_u64)
                    .unwrap_or(default)
            };
            let interval_ms = timing(
                "interval_ms",
                if family == "identifiers" { 600_000 } else { 0 },
            );
            let delay_ms = timing("delay_ms", 0);
            let assigned = |k: &str| {
                fields
                    .get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let mut canon = Object::new();
            let canonical_renderers = json!({"speech":{"model":config.speech_defaults.voice,"timing":{"speed_percent":config.speech_defaults.speed_percent}},"morse":{"frequency_hz":config.morse_defaults.frequency_hz,"timing":{"speed_wpm":config.morse_defaults.speed_wpm}}});
            merge(&mut canon, canonical_renderers.as_object().unwrap());
            if family != "courtesies" {
                canon["speech"]["level_db"] = config.speech_defaults.level_db.into();
                canon["morse"]["level_db"] = config.morse_defaults.level_db.into();
            }
            if matches!(family, "identifiers" | "tails") {
                merge(&mut canon,json!({"playback":{"repeat":config.playback.repeat_during_playback,"destination":destination_name(config.playback.destination)},"polite":{"timing":{"maximum_wait_ms":config.delivery.maximum_wait_ms},"expiry":match config.delivery.expiry{PoliteExpiry::Skip=>"skip",PoliteExpiry::Morse=>"morse"},"busy":match config.delivery.busy{PoliteBusy::Skip=>"skip",PoliteBusy::Wait=>"wait"}}}).as_object().unwrap());
            }
            let speech_text = config
                .media
                .speech
                .as_ref()
                .map(|s| s.text.clone())
                .unwrap_or_default();
            let morse_text = config
                .media
                .morse
                .as_ref()
                .map(|s| s.text.clone())
                .unwrap_or_default();
            let sound_file = assigned("file");
            if config
                .dtmf_id
                .as_ref()
                .is_some_and(|id| !selectors.insert(id.clone()))
            {
                return Err(error);
            }
            match family {
                "identifiers" => {
                    let flag = |k| fields.get(k).and_then(Value::as_bool).unwrap_or(false);
                    let priority = fields.get("priority").and_then(Value::as_u64).unwrap_or(0);
                    let first_key_only = flag("first_key_only");
                    let regardless_of_activity = flag("regardless_of_activity");
                    let polite = fields
                        .get("polite")
                        .and_then(|p| p.get("enabled"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    merge(&mut canon,json!({"priority":priority,"first_key_only":first_key_only,"regardless_of_activity":regardless_of_activity,"polite":{"enabled":polite},"timing":{"interval_ms":interval_ms}}).as_object().unwrap());
                    result.identifiers.insert(
                        name.clone(),
                        ResolvedIdentifierSettings {
                            dtmf_id: config.dtmf_id.clone(),
                            playback: config.playback,
                            delivery: config.delivery,
                            content: config.content.clone(),
                            message_id: None,
                            bindings: config.bindings.clone(),
                            interval_ms,
                            priority,
                            first_key_only,
                            regardless_of_activity,
                            polite,
                            polite_maximum_wait_ms: config.delivery.maximum_wait_ms,
                            sound_file,
                            speech_text,
                            morse_text,
                            speech_model: config.speech_defaults.voice.clone(),
                            speech_level_db: config.speech_defaults.level_db,
                            speech_speed_percent: config.speech_defaults.speed_percent,
                            morse_frequency_hz: config.morse_defaults.frequency_hz,
                            morse_speed_wpm: config.morse_defaults.speed_wpm,
                            morse_level_db: config.morse_defaults.level_db,
                        },
                    );
                }
                "tails" => {
                    if let Some(tone) = &config.media.tone {
                        canon.insert("tone".into(), json!({"level_db":tone.level_db}));
                    }
                    canon.insert(
                        "timing".into(),
                        json!({"interval_ms":interval_ms,"delay_ms":delay_ms}),
                    );
                    result.tails.insert(
                        name.clone(),
                        ResolvedTailSettings {
                            message: config.clone(),
                            interval_ms,
                            delay_ms,
                        },
                    );
                }
                "announcements" => {
                    canon.insert("timing".into(), json!({"interval_ms":interval_ms}));
                    result.announcements.insert(
                        name.clone(),
                        ResolvedAnnouncementSettings {
                            interval_ms,
                            sound_file,
                            speech_text,
                            morse_text,
                            speech_model: config.speech_defaults.voice.clone(),
                            speech_level_db: config.speech_defaults.level_db,
                            speech_speed_percent: config.speech_defaults.speed_percent,
                            morse_frequency_hz: config.morse_defaults.frequency_hz,
                            morse_speed_wpm: config.morse_defaults.speed_wpm,
                            morse_level_db: config.morse_defaults.level_db,
                        },
                    );
                }
                "courtesies" => {
                    let input = assigned("input");
                    if input.is_empty() {
                        return Err(error);
                    }
                    let level_db = fields
                        .get("level_db")
                        .and_then(Value::as_i64)
                        .unwrap_or(-20);
                    canon.insert("level_db".into(), level_db.into());
                    let raw = assigned("remote_node");
                    let remote_node =
                        crate::link::PeerIdentity::canonical_operator(&raw).unwrap_or(raw);
                    if (input == "receiver" && !remote_node.is_empty())
                        || !courtesy_routes.insert((input.clone(), remote_node.clone()))
                    {
                        return Err(error);
                    }
                    if !remote_node.is_empty() {
                        canon.insert("remote_node".into(), remote_node.clone().into());
                    }
                    let tone_sequence = config
                        .media
                        .tone
                        .as_ref()
                        .map(|t| t.sequence.clone())
                        .unwrap_or_default();
                    config.media.provider_gain_db = level_db as i8;
                    if let Some(s) = &mut config.media.speech {
                        s.level_db = 0;
                    }
                    if let Some(m) = &mut config.media.morse {
                        m.level_db = level_db as i8;
                    }
                    if let Some(t) = &mut config.media.tone {
                        t.level_db = level_db as i8;
                    }
                    result.courtesies.insert(
                        name.clone(),
                        ResolvedCourtesySettings {
                            input,
                            remote_node,
                            level_db,
                            sound_file,
                            speech_text,
                            morse_text,
                            speech_model: config.speech_defaults.voice.clone(),
                            speech_level_db: 0,
                            speech_speed_percent: config.speech_defaults.speed_percent,
                            morse_frequency_hz: config.morse_defaults.frequency_hz,
                            morse_speed_wpm: config.morse_defaults.speed_wpm,
                            morse_level_db: level_db,
                            tone_sequence,
                        },
                    );
                }
                _ => unreachable!(),
            }
            let source = match payload(fields, binding)? {
                Some(Payload::Content)
                    if !config.content.trim().is_empty() || config.dtmf_id.is_some() =>
                {
                    Some(PublicMessageSource::Content(
                        NativePattern::parse(&config.content).map_err(|_| error)?,
                    ))
                }
                Some(Payload::Media)
                    if config.media.file.is_some()
                        || config.media.speech.is_some()
                        || config.media.morse.is_some()
                        || config.media.tone.is_some() =>
                {
                    Some(PublicMessageSource::Media {
                        speech: config
                            .media
                            .speech
                            .as_ref()
                            .map(|s| NativePattern::parse(&s.text))
                            .transpose()
                            .map_err(|_| error)?,
                        morse: config
                            .media
                            .morse
                            .as_ref()
                            .map(|s| NativePattern::parse(&s.text))
                            .transpose()
                            .map_err(|_| error)?,
                    })
                }
                _ => None,
            };
            if let Some(source) = source {
                let requirement = if family == "identifiers" {
                    MorseRequirement::Required
                } else {
                    MorseRequirement::Optional
                };
                let recipe = Composition::compile_public(
                    config,
                    &source,
                    messages.definitions.clone(),
                    &messages.sources,
                    &messages.localized,
                    &messages.catalog,
                    requirement,
                )
                .map_err(|_| PublicMessageError::Definition(binding))?;
                if requires_deadline(&recipe) {
                    return Err(error);
                }
                result.recipes.insert((family, name.clone()), recipe);
            } else if config.dtmf_id.is_some() {
                return Err(error);
            }
            merge(fields, &canon);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resources::{
        PortKey,
        candidate::{CapturedResource, ResourceRegistry},
    };
    #[test]
    fn zero_port_settings_have_no_named_or_synthetic_resource_identities() {
        let id = "929e52be-6854-4c46-85f2-000000000001"
            .parse::<ResourceId>()
            .unwrap();
        let scope = ResourceScope::Site(id);
        let binding = ResourceBinding {
            id,
            scope,
            kind: ResourceKind::Metadata,
        };
        let registry = ResourceRegistry::new(id, &[], &[]).unwrap();
        let body=serde_json::to_vec(&json!({"id":id.to_string(),"kind":"metadata","configuration":{"inherit_from":null,"name":"Site"}})).unwrap();
        let cap = [CapturedResource {
            binding,
            json: &body,
        }];
        let candidate = ModularCandidate::assemble(&registry, &cap).unwrap();
        let telemetry = candidate.telemetry_profile(scope).unwrap();
        assert!(telemetry.local.is_empty());
        assert_eq!(telemetry.time.format, 12);
        assert_eq!(telemetry.courtesy_delay_ms, 250);
        assert!(telemetry.members.is_empty());
        assert!(telemetry.source(&["duck_db"]).is_none());
        assert!(
            candidate
                .telemetry_profile(ResourceScope::Port(id))
                .is_err()
        );
    }
    #[test]
    fn port_and_site_telemetry_reuse_the_same_named_message_namespace() {
        let id = |n: u8| {
            format!("929e52be-6854-4c46-85f2-{n:012}")
                .parse::<ResourceId>()
                .unwrap()
        };
        let site = ResourceScope::Site(id(1));
        let port = ResourceScope::Port(id(2));
        let b = |n, scope, kind| ResourceBinding {
            id: id(n),
            scope,
            kind,
        };
        let rows = [
            (
                b(1, site, ResourceKind::Metadata),
                None,
                json!({"name":"site","inherit_from":null}),
            ),
            (
                b(2, port, ResourceKind::Metadata),
                None,
                json!({"name":"port","inherit_from":"site"}),
            ),
            (
                b(3, site, ResourceKind::TelemetrySettings),
                None,
                json!({"duck_db":-17,"speech":{"voice":"station.onnx"},"timing":{"clock_format":24}}),
            ),
            (
                b(4, port, ResourceKind::TelemetrySettings),
                None,
                json!({"duck_db":"invalid","speech":{"timing":{"speed_percent":150}}}),
            ),
            (
                b(5, site, ResourceKind::Message),
                Some("own-message"),
                json!({"content":"Hello { $node-name }"}),
            ),
            (
                b(6, site, ResourceKind::Identifier),
                Some("main"),
                json!({"morse":{"text":"CQ"},"timing":{"interval_ms":314159},"dtmf_id":"7"}),
            ),
            (
                b(7, port, ResourceKind::Identifier),
                Some("main"),
                json!({"content":"{ own-message }","priority":3,"polite":{"enabled":true}}),
            ),
            (
                b(8, site, ResourceKind::Announcement),
                Some("main"),
                json!({"speech":{"text":"Hello { $node-name }"}}),
            ),
            (
                b(9, site, ResourceKind::Tail),
                Some("night"),
                json!({"content":"{ own-message }","timing":{"delay_ms":510}}),
            ),
            (
                b(10, site, ResourceKind::Courtesy),
                Some("incoming"),
                json!({"input":"link","remote_node":"001","tone":{"sequence":"440/20"}}),
            ),
        ];
        let members = rows
            .iter()
            .filter_map(|(b, k, _)| k.map(|key| (*b, PortKey::new(key))))
            .collect::<Vec<_>>();
        let registry = ResourceRegistry::with_members(
            id(1),
            &[(id(2), PortKey::new("port"))],
            &[rows[2].0, rows[3].0],
            &members,
        )
        .unwrap();
        let mut duplicate = members.clone();
        duplicate.push((b(12, site, ResourceKind::Identifier), PortKey::new("main")));
        assert!(
            ResourceRegistry::with_members(
                id(1),
                &[(id(2), PortKey::new("port"))],
                &[],
                &duplicate
            )
            .is_err()
        );
        assert!(
            crate::config::resources::catalog::classify("site/telemetry/definitions/new-old.json")
                .is_none()
        );
        assert!(ResourceKind::from_wire("telemetry").is_none());
        for (kind, dir) in FAMILIES {
            let path = format!("site/telemetry/{dir}/{}.json", id(12));
            assert_eq!(
                crate::config::resources::catalog::relative_path(b(12, site, kind)),
                path
            );
            assert!(
                matches!(crate::config::resources::catalog::classify(&path),Some(crate::config::resources::catalog::CatalogPath::File(file)) if file.kind==kind)
            );
        }
        let docs = rows
            .iter()
            .map(|(b, k, fields)| {
                let mut doc =
                    json!({"kind":b.kind.as_str(),"id":b.id.to_string(),"configuration":fields});
                if let Some(k) = k {
                    doc["key"] = json!(k);
                }
                (*b, serde_json::to_vec(&doc).unwrap())
            })
            .collect::<Vec<_>>();
        let caps = docs
            .iter()
            .map(|(binding, json)| CapturedResource {
                binding: *binding,
                json,
            })
            .collect::<Vec<_>>();
        let candidate = ModularCandidate::assemble(&registry, &caps).unwrap();
        for scope in [site, port] {
            let (settings, _, profile) = candidate
                .prepare_telemetry(scope, english(), &"en-US".parse().unwrap())
                .unwrap();
            assert_eq!(settings.duck_db, -17);
            assert_eq!(settings.source(&["duck_db"]), Some(rows[2].0));
            assert_eq!(settings.time.format, 24);
            assert_eq!(profile.recipes.len(), 4);
            assert!(profile.effective.get("identifiers").is_none());
            assert_eq!(profile.members.len(), 4);
            assert_eq!(
                settings
                    .member_source(ResourceKind::Announcement, "main", &["speech", "model"])
                    .unwrap()
                    .id,
                id(3)
            );
            assert_eq!(
                settings
                    .member_source(ResourceKind::Identifier, "main", &["timing", "interval_ms"])
                    .unwrap()
                    .id,
                id(6)
            );
            assert_eq!(profile.identifiers["main"].interval_ms, 314159);
            assert_eq!(profile.announcements["main"].speech_model, "station.onnx");
            assert_eq!(profile.tails["night"].delay_ms, 510);
            assert_eq!(profile.courtesies["incoming"].input, "link");
            if scope == port {
                assert_eq!(profile.identifiers["main"].content, "{ own-message }");
                assert!(profile.identifiers["main"].polite);
                assert_eq!(profile.announcements["main"].speech_speed_percent, 150);
                assert!(
                    settings
                        .member_source(ResourceKind::Identifier, "main", &["morse", "text"])
                        .is_none()
                );
                assert!(settings.local[&id(4)].get("duck_db").is_none());
                assert!(!settings.warnings.is_empty());
            }
        }
    }

    type FixtureRow = (bool, ResourceKind, &'static str, Value);

    fn candidate_fixture<T>(
        entries: Vec<FixtureRow>,
        visit: impl FnOnce(&ModularCandidate<'_>, ResourceScope) -> T,
    ) -> T {
        let id = |n: usize| {
            format!("929e52be-6854-4c46-85f2-{n:012}")
                .parse::<ResourceId>()
                .unwrap()
        };
        let site = ResourceScope::Site(id(1));
        let port = ResourceScope::Port(id(2));
        let mut rows = vec![
            (
                ResourceBinding {
                    id: id(1),
                    scope: site,
                    kind: ResourceKind::Metadata,
                },
                None,
                json!({"name":"Site","inherit_from":null}),
            ),
            (
                ResourceBinding {
                    id: id(2),
                    scope: port,
                    kind: ResourceKind::Metadata,
                },
                None,
                json!({"name":"Port","inherit_from":"site"}),
            ),
        ];
        rows.extend(
            entries
                .into_iter()
                .enumerate()
                .map(|(i, (child, kind, key, value))| {
                    (
                        ResourceBinding {
                            id: id(i + 3),
                            scope: if child { port } else { site },
                            kind,
                        },
                        Some(key),
                        value,
                    )
                }),
        );
        let members = rows
            .iter()
            .filter_map(|(binding, key, _)| key.map(|key| (*binding, PortKey::new(key))))
            .collect::<Vec<_>>();
        let registry =
            ResourceRegistry::with_members(id(1), &[(id(2), PortKey::new("port"))], &[], &members)
                .unwrap();
        let docs=rows.into_iter().map(|(binding,key,body)|{
            let mut doc=json!({"kind":binding.kind.as_str(),"id":binding.id.to_string(),"configuration":body});
            if let Some(key)=key{doc["key"]=key.into();}
            (binding,serde_json::to_vec(&doc).unwrap())
        }).collect::<Vec<_>>();
        let caps = docs
            .iter()
            .map(|(binding, json)| CapturedResource {
                binding: *binding,
                json,
            })
            .collect::<Vec<_>>();
        let candidate = ModularCandidate::assemble(&registry, &caps).unwrap();
        visit(&candidate, port)
    }
    fn english() -> MessageCatalog {
        MessageCatalog::from_sources(
            "en-US",
            include_str!("../../../../../../../messages/en-US.ftl"),
            None,
        )
        .unwrap()
    }
    fn prepare_fixture(
        c: &ModularCandidate<'_>,
        scope: ResourceScope,
    ) -> Result<(ScopedTelemetry, ScopedMessages, PreparedTelemetry), PublicMessageError> {
        c.prepare_telemetry(scope, english(), &"en-US".parse().unwrap())
    }
    fn accepts(entries: Vec<FixtureRow>) -> bool {
        candidate_fixture(entries, |c, scope| prepare_fixture(c, scope).is_ok())
    }

    #[test]
    fn peers_audience_is_available_to_named_messages_only() {
        use ResourceKind::{Identifier, Message, Tail};
        candidate_fixture(
            vec![
                (
                    false,
                    Message,
                    "notice",
                    json!({"morse":{"text":"CQ"},"playback":{"destination":"peers"}}),
                ),
                (
                    false,
                    Identifier,
                    "id",
                    json!({"morse":{"text":"CQ"},"playback":{"destination":"local_and_links"}}),
                ),
                (
                    true,
                    Identifier,
                    "id",
                    json!({"playback":{"destination":"peers","repeat":false}}),
                ),
                (
                    true,
                    Tail,
                    "tail",
                    json!({"content":"{ notice }","playback":{"destination":"peers"}}),
                ),
            ],
            |candidate, scope| {
                let (telemetry, messages, media) = prepare_fixture(candidate, scope).unwrap();
                let (message, fields) = &messages.effective["notice"];
                assert_eq!(
                    messages.local[&message.id]["playback"]["destination"],
                    "peers"
                );
                assert_eq!(fields["playback"]["destination"], "peers");
                assert_eq!(
                    messages.recipes["notice"]
                        .settings_for(None)
                        .playback
                        .destination,
                    PlaybackDestination::Peers
                );
                for (kind, key, expected) in [
                    (Identifier, "id", "local_and_links"),
                    (Tail, "tail", "local"),
                ] {
                    let (binding, fields) = &media.members[&(kind, key.into())];
                    assert!(
                        telemetry.local[&binding.id]["playback"]
                            .get("destination")
                            .is_none()
                    );
                    assert_eq!(fields["playback"]["destination"], expected);
                    assert!(
                        telemetry
                            .warnings
                            .iter()
                            .any(|warning| warning.resource == *binding
                                && warning.warning == ResourceWarning::InvalidValue("destination"))
                    );
                }
                assert!(!media.identifiers["id"].playback.repeat_during_playback);
                assert_eq!(
                    media.identifiers["id"].playback.destination,
                    PlaybackDestination::LocalAndLinks
                );
                assert_eq!(
                    media.tails["tail"].message.playback.destination,
                    PlaybackDestination::Local
                );
            },
        );
    }

    #[test]
    fn round_one_courtesy_routes_are_unique_across_effective_members() {
        use ResourceKind::Courtesy;
        let route = |child, name, input: &str, peer: &str| {
            let mut body = json!({"input":input,"tone":{"sequence":"440/20"}});
            if !peer.is_empty() {
                body["remote_node"] = peer.into();
            }
            (child, Courtesy, name, body)
        };
        assert!(accepts(vec![
            route(false, "link", "link", ""),
            route(true, "peer", "link", "3000001")
        ]));
        assert!(!accepts(vec![
            route(false, "first", "receiver", ""),
            route(true, "second", "receiver", "")
        ]));
        assert!(!accepts(vec![
            route(false, "first", "link", "3000001"),
            route(true, "second", "link", "30000001")
        ]));
        assert!(!accepts(vec![route(false, "first", "receiver", "4")]));
        assert!(!accepts(vec![
            route(false, "first", "link", ""),
            route(true, "second", "link", "")
        ]));
    }
    #[test]
    fn round_one_only_deadline_capable_callers_can_use_remaining_time() {
        use ResourceKind::{Identifier, Message, Tail};
        let message = (
            false,
            Message,
            "deadline",
            json!({"content":"WAIT { $time-remaining }"}),
        );
        assert!(accepts(vec![message.clone()]));
        assert!(accepts(vec![(
            false,
            Identifier,
            "legal",
            json!({"content":"Ready","bindings":{"unused":"time_remaining"}})
        )]));
        assert!(!accepts(vec![(
            false,
            Identifier,
            "illegal",
            json!({"content":"WAIT { $time-remaining }"})
        )]));
        assert!(!accepts(vec![
            message,
            (false, Tail, "delay", json!({"content":"{ deadline }"}))
        ]));
        assert!(accepts(vec![(
            false,
            Tail,
            "report",
            json!({"speech":{"text":"WAIT { $node-name }"}})
        )]));
        assert!(accepts(vec![(
            false,
            Identifier,
            "cw",
            json!({"morse":{"text":"WAIT { $node-name }"}})
        )]));
        assert!(!accepts(vec![(
            false,
            Tail,
            "report",
            json!({"speech":{"text":"WAIT { $time-remaining }"}})
        )]));
        assert!(!accepts(vec![(
            false,
            Identifier,
            "cw",
            json!({"morse":{"text":"WAIT { $time-remaining }"}})
        )]));
        let forms =
            json!({"locales":{"en-US":{"text":"{ $time-remaining }","tts":"Ready","morse":"R"}}});
        assert!(!accepts(vec![
            (false, Message, "localized", forms),
            (
                false,
                Tail,
                "delay",
                json!({"content":"WAIT { localized.text }"})
            )
        ]));
    }
    #[test]
    fn round_one_malformed_composition_rejects_new_member_and_override() {
        use ResourceKind::Identifier;
        let site = (false, Identifier, "auto", json!({"content":"Valid"}));
        for body in [json!({}), json!({"content":""})] {
            candidate_fixture(vec![(false, Identifier, "empty", body)], |c, scope| {
                let (_, _, prepared) = prepare_fixture(c, scope).unwrap();
                assert!(prepared.identifiers.contains_key("empty"));
                assert!(
                    !prepared
                        .recipes
                        .contains_key(&("identifiers", "empty".into()))
                );
            });
        }
        candidate_fixture(
            vec![
                site.clone(),
                (true, Identifier, "auto", json!({"content":""})),
            ],
            |c, scope| {
                let (_, _, prepared) = prepare_fixture(c, scope).unwrap();
                assert!(prepared.identifiers["auto"].content.is_empty());
                assert!(
                    !prepared
                        .recipes
                        .contains_key(&("identifiers", "auto".into()))
                );
            },
        );
        candidate_fixture(
            vec![
                site.clone(),
                (true, Identifier, "auto", json!({"content":22})),
            ],
            |c, scope| {
                let (settings, _, prepared) = prepare_fixture(c, scope).unwrap();
                assert_eq!(prepared.identifiers["auto"].content, "Valid");
                assert_ne!(
                    settings
                        .member_source(Identifier, "auto", &["content"])
                        .unwrap()
                        .scope,
                    scope
                );
                assert!(!settings.warnings.is_empty());
            },
        );
        assert!(!accepts(vec![(
            false,
            Identifier,
            "auto",
            json!({"content":"ID { broken"})
        )]));
        assert!(!accepts(vec![
            site,
            (true, Identifier, "auto", json!({"content":"ID { broken"}))
        ]));
    }
    #[test]
    fn round_one_id_wait_range_preserves_sparse_effective_and_source() {
        let max = u64::MAX / 48;
        for (child, expected, owner) in [(max, max, true), (max + 1, max - 1, false)] {
            let id = |v| json!({"polite":{"enabled":true,"timing":{"maximum_wait_ms":v}}});
            let mut site = id(max - 1);
            site["content"] = "Ready".into();
            candidate_fixture(
                vec![
                    (false, ResourceKind::Identifier, "auto", site),
                    (true, ResourceKind::Identifier, "auto", id(child)),
                ],
                |c, scope| {
                    let (settings, _, prepared) = prepare_fixture(c, scope).unwrap();
                    let (binding, body) =
                        &prepared.members[&(ResourceKind::Identifier, "auto".into())];
                    assert_eq!(body["polite"]["timing"]["maximum_wait_ms"], expected);
                    assert_eq!(
                        prepared.identifiers["auto"].polite_maximum_wait_ms,
                        expected
                    );
                    assert_eq!(
                        settings.local[&binding.id]["polite"]
                            .get("timing")
                            .and_then(|v| v.get("maximum_wait_ms"))
                            .is_some(),
                        owner
                    );
                    assert_eq!(
                        settings
                            .member_source(
                                ResourceKind::Identifier,
                                "auto",
                                &["polite", "timing", "maximum_wait_ms"]
                            )
                            .unwrap()
                            .scope
                            == scope,
                        owner
                    );
                    assert_eq!(settings.warnings.is_empty(), owner);
                },
            );
        }
    }
    #[test]
    fn round_one_removed_message_cannot_be_resurrected_from_an_old_snapshot() {
        use ResourceKind::{Identifier, Message};
        let member = (false, Identifier, "auto", json!({"content":"{ signoff }"}));
        let old = candidate_fixture(
            vec![
                member.clone(),
                (false, Message, "signoff", json!({"content":"GOODBYE"})),
            ],
            |c, scope| prepare_fixture(c, scope).unwrap().1,
        );
        assert!(old.recipes.contains_key("signoff"));
        candidate_fixture(vec![member], |c, scope| {
            assert!(prepare_fixture(c, scope).is_err());
        });
    }

    fn attempt_without_deadline() -> crate::runtime::MessageValueSnapshot {
        use crate::{
            messages::MessageForms,
            schedule::{CivilTime, Weekday},
            time::TimeFormat,
        };
        crate::runtime::MessageValueSnapshot {
            civil: CivilTime::new(2026, 10, 18, Weekday::Sunday, 13, 25).unwrap(),
            time_format: TimeFormat::TwelveHour,
            node: "123".into(),
            callsign: "N0CALL".into(),
            port: Default::default(),
            event: None,
            builtin: Default::default(),
            links: MessageForms {
                text: "No links".into(),
                tts: "No links".into(),
                morse: "NO LINKS".into(),
            },
            sensors: BTreeMap::new(),
            inputs: BTreeMap::new(),
            time_remaining: None,
        }
    }
    #[test]
    fn round_two_telemetry_deadline_free_effective_message_can_keep_optional_deadline_fallback() {
        use ResourceKind::{Identifier, Message};
        let site = (
            false,
            Message,
            "notice",
            json!({"locales":{"en-US":{"text":"WAIT { $time-remaining }","tts":"Ready","morse":"R"}}}),
        );
        let child = (
            true,
            Message,
            "notice",
            json!({"locales":{"en-US":{"text":"READY"}}}),
        );
        for (content, expected) in [
            ("{ notice }", "READY"),
            ("STATUS { notice.text }", "STATUS READY"),
        ] {
            let member = (true, Identifier, "auto", json!({"content":content}));
            let (_, messages, prepared) =
                candidate_fixture(vec![site.clone(), child.clone(), member], prepare_fixture)
                    .unwrap();
            assert_eq!(
                messages
                    .catalog
                    .format_named("notice", &BTreeMap::new())
                    .unwrap()
                    .text,
                "READY"
            );
            let recipe = &prepared.recipes[&("identifiers", "auto".into())];
            let forms = recipe
                .prepare_text(&messages.catalog, &attempt_without_deadline())
                .unwrap();
            assert_eq!(forms[0].text, expected);
        }
    }

    #[test]
    fn round_two_telemetry_ordinary_release_callers_must_have_their_required_values() {
        use ResourceKind::{Announcement, Courtesy};
        for kind in [Announcement, Courtesy] {
            let entry = |pattern: &str| {
                let mut body = json!({"speech":{"text":pattern}});
                if kind == Courtesy {
                    body["input"] = "receiver".into();
                }
                (false, kind, "release", body)
            };
            assert!(accepts(vec![entry("FROM { $node-name }")]));
            assert!(!accepts(vec![entry("WAIT { $time-remaining }")]));
        }
    }

    #[test]
    fn round_two_telemetry_empty_selector_recipe_suppresses_only_the_child() {
        use ResourceKind::{Identifier, Tail};
        for (kind, family) in [(Identifier, "identifiers"), (Tail, "tails")] {
            let rows = vec![
                (
                    false,
                    kind,
                    "status",
                    json!({"content":"Ready","dtmf_id":"7"}),
                ),
                (true, kind, "status", json!({"content":""})),
            ];
            candidate_fixture(rows, |candidate, port| {
                let site = ResourceScope::Site(candidate.registry().site_id());
                let (_, _, before) = prepare_fixture(candidate, site).unwrap();
                let key = (family, "status".into());
                assert!(!before.recipes[&key].segments().is_empty());
                let (settings, messages, after) = prepare_fixture(candidate, port).unwrap();
                let (binding, fields) = &after.members[&(kind, "status".into())];
                assert_eq!(fields["content"], "");
                assert_eq!(settings.local[&binding.id]["content"], "");
                assert_eq!(
                    settings.member_source(kind, "status", &["content"]),
                    Some(*binding)
                );
                assert_eq!(fields["dtmf_id"], "7");
                let suppressed = &after.recipes[&key];
                assert!(suppressed.segments().is_empty());
                let program = suppressed
                    .prepare(&messages.catalog, &attempt_without_deadline())
                    .unwrap();
                assert!(program.primary.is_empty() && program.morse.is_empty());
            });
            assert!(!accepts(vec![(
                false,
                kind,
                "selector-only",
                json!({"dtmf_id":"7"})
            )]));
        }
    }

    #[test]
    fn round_two_telemetry_reused_projected_catalog_cannot_revive_deleted_localized_message() {
        use ResourceKind::{Identifier, Message};
        let member = (false, Identifier, "auto", json!({"content":"{ signoff }"}));
        let named = (
            false,
            Message,
            "signoff",
            json!({"locales":{"en-US":{"text":"Bye","tts":"Goodbye","morse":"BYE"}}}),
        );
        let (_, old, _) = candidate_fixture(vec![member.clone(), named], prepare_fixture).unwrap();
        assert_eq!(
            old.catalog
                .format_named("signoff", &BTreeMap::new())
                .unwrap()
                .text,
            "Bye"
        );
        assert!(accepts(vec![(
            false,
            Identifier,
            "installed-only",
            json!({"content":"{ no-links }"})
        )]));
        let projected =
            std::sync::Arc::try_unwrap(old.catalog).expect("fixture owns the retired catalog");
        candidate_fixture(vec![member], |candidate, scope| {
            assert!(
                candidate
                    .prepare_telemetry(scope, projected, &"en-US".parse().unwrap())
                    .is_err()
            );
        });
    }
}
