//! Prepare named JSON messages from registered authoring resources and installed Fluent catalogs.

/// Public controller telemetry consumers and render defaults.
pub mod telemetry;

use super::{
    CandidateWarning, ModularCandidate, ResourceBinding, ResourceId, ResourceKind, ResourceScope,
    normalize::{self, Fields, Rule},
};
use crate::{
    audio::ToneSequence,
    composition::{Composition, MorseRequirement, PublicMessageSource},
    config::{
        BindingSource, ResolvedMessageSettings, ResolvedMorseSettings, ResolvedSpeechSettings,
        resources::ResourceWarning,
    },
    controller::{PlaybackDestination, PlaybackPolicy, PoliteBusy, PoliteDelivery, PoliteExpiry},
    media::{MediaSource, MorseSource, SpeechSource, ToneSource},
    messages::{
        MessageCatalog,
        patterns::{NativePattern, identifier},
        projection::LocalizedMessage,
    },
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use unic_langid::LanguageIdentifier;

type Object = Map<String, Value>;
fn text(v: &Value) -> Option<Value> {
    v.as_str().filter(|s| !s.contains('\0')).map(Value::from)
}
fn level(v: &Value) -> Option<Value> {
    v.as_i64()
        .filter(|n| (-60..=0).contains(n))
        .map(Value::from)
}
fn pattern(v: &Value) -> Option<Value> {
    let s = v.as_str()?;
    NativePattern::parse(s).ok().map(|_| Value::from(s))
}
fn destination_name(destination: PlaybackDestination) -> &'static str {
    match destination {
        PlaybackDestination::Local => "local",
        PlaybackDestination::LocalAndLinks => "local_and_links",
        PlaybackDestination::Peers => "peers",
    }
}
fn selector(v: &Value) -> Option<Value> {
    v.as_str()
        .filter(|s| s.len() <= 63 && crate::config::message_selector_valid(s))
        .map(Value::from)
}
fn sequence(v: &Value) -> Option<Value> {
    v.as_str()
        .filter(|s| s.is_empty() || ToneSequence::new(s, -6).is_ok())
        .map(Value::from)
}
const FIELDS: Fields = &[
    ("content", Rule::Value(pattern)),
    ("file", Rule::Value(text)),
    ("dtmf_id", Rule::Value(selector)),
    (
        "speech",
        Rule::Object(&[
            ("text", Rule::Value(pattern)),
            ("model", Rule::Value(text)),
            ("level_db", Rule::Value(level)),
            (
                "timing",
                Rule::Object(&[("speed_percent", Rule::Unsigned(1, 1000))]),
            ),
        ]),
    ),
    (
        "morse",
        Rule::Object(&[
            ("text", Rule::Value(pattern)),
            ("frequency_hz", Rule::Unsigned(1, 23999)),
            ("level_db", Rule::Value(level)),
            (
                "timing",
                Rule::Object(&[("speed_wpm", Rule::Unsigned(1, 100))]),
            ),
        ]),
    ),
    (
        "tone",
        Rule::Object(&[
            ("sequence", Rule::Value(sequence)),
            ("level_db", Rule::Value(level)),
        ]),
    ),
    (
        "playback",
        Rule::Object(&[
            (
                "destination",
                Rule::Choice(&["local", "peers", "local_and_links"]),
            ),
            ("repeat", Rule::Bool),
        ]),
    ),
    (
        "polite",
        Rule::Object(&[
            (
                "timing",
                Rule::Object(&[("maximum_wait_ms", Rule::Unsigned(0, u64::MAX))]),
            ),
            ("expiry", Rule::Choice(&["skip", "morse"])),
            ("busy", Rule::Choice(&["skip", "wait"])),
        ]),
    ),
];

/// A named message could not acquire a safe effective payload, ID, selector or reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicMessageError {
    /// Scope not present in the real registry.
    MissingScope,
    /// A registered member has an invalid identity or effective payload.
    Definition(ResourceBinding),
    /// Effective Fluent catalog or installed English could not be prepared.
    Catalog,
    /// A structural dependency or media chain is unusable.
    Composition,
}

fn normalized(
    binding: ResourceBinding,
    body: &Object,
    warnings: &mut Vec<CandidateWarning>,
) -> Result<Object, PublicMessageError> {
    let mut fields = normalize::normalize_fields(
        binding,
        body.iter()
            .filter(|(k, _)| !matches!(k.as_str(), "locales" | "bindings")),
        FIELDS,
        warnings,
    );
    let fail = PublicMessageError::Definition(binding);
    if body.get("content").is_some_and(Value::is_string) && !fields.contains_key("content") {
        return Err(fail);
    }
    if let Some(raw) = body.get("bindings") {
        if let Some(map) = raw.as_object() {
            let mut accepted = Object::new();
            for (alias, v) in map {
                if !identifier(alias) {
                    return Err(fail);
                }
                let Some(source) = v.as_str().and_then(BindingSource::from_public_path) else {
                    normalize::warn(warnings, binding, "bindings");
                    continue;
                };
                if BindingSource::standard_argument(alias).is_some_and(|s| s != source) {
                    return Err(fail);
                }
                accepted.insert(alias.clone(), v.clone());
            }
            fields.insert("bindings".into(), Value::Object(accepted));
        } else {
            normalize::warn(warnings, binding, "bindings");
        }
    }
    if let Some(raw) = body.get("locales") {
        if let Some(map) = raw.as_object() {
            let mut locales = Object::new();
            for (name, v) in map {
                let Ok(lang) = name.parse::<LanguageIdentifier>() else {
                    normalize::warn(warnings, binding, "locales");
                    continue;
                };
                let canonical = lang.to_string();
                if locales.contains_key(&canonical) {
                    return Err(fail);
                }
                let Some(forms) = v.as_object() else {
                    normalize::warn(warnings, binding, "locales");
                    continue;
                };
                let mut accepted = Object::new();
                for (form, value) in forms {
                    if !matches!(form.as_str(), "text" | "tts" | "morse") {
                        warnings.push(CandidateWarning {
                            resource: binding,
                            warning: ResourceWarning::UnknownField,
                        });
                    } else if let Some(pattern) = pattern(value) {
                        accepted.insert(form.clone(), pattern);
                    } else if canonical == "en-US" {
                        return Err(fail);
                    } else {
                        normalize::warn(warnings, binding, "locales");
                    }
                }
                locales.insert(canonical, Value::Object(accepted));
            }
            fields.insert("locales".into(), Value::Object(locales));
        } else {
            normalize::warn(warnings, binding, "locales");
        }
    }
    Ok(fields)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Payload {
    Locales,
    Content,
    Media,
}

fn payload(body: &Object, binding: ResourceBinding) -> Result<Option<Payload>, PublicMessageError> {
    let mut kinds = Vec::new();
    if body.contains_key("content") {
        kinds.push(Payload::Content);
    }
    if body
        .get("locales")
        .and_then(Value::as_object)
        .is_some_and(|m| {
            m.values()
                .any(|v| v.as_object().is_some_and(|attrs| !attrs.is_empty()))
        })
    {
        kinds.push(Payload::Locales);
    }
    if body.contains_key("file")
        || [("speech", "text"), ("morse", "text"), ("tone", "sequence")]
            .iter()
            .any(|(k, f)| body.get(*k).and_then(|v| v.get(*f)).is_some())
    {
        kinds.push(Payload::Media);
    }
    if kinds.len() > 1 {
        return Err(PublicMessageError::Definition(binding));
    }
    Ok(kinds.pop())
}

fn merge(base: &mut Object, overlay: &Object) {
    for (k, v) in overlay {
        if let Some(Value::Object(old)) = base.get_mut(k)
            && let Value::Object(new) = v
        {
            merge(old, new);
        } else {
            base.insert(k.clone(), v.clone());
        }
    }
}

fn replace_payload(fields: &mut Object) {
    for k in ["content", "file", "locales"] {
        fields.remove(k);
    }
    for (k, f) in [("speech", "text"), ("morse", "text"), ("tone", "sequence")] {
        if let Some(nested) = fields.get_mut(k).and_then(Value::as_object_mut) {
            nested.remove(f);
        }
    }
}

/// Prepared named members, their inherited bodies and one immutable playback catalog.
pub struct ScopedMessages {
    /// Exact invoking site or port for this generation.
    pub scope: ResourceScope,
    /// Recognized sparse fields on each declaring resource.
    pub local: BTreeMap<ResourceId, Object>,
    /// Effective public configuration with the nearest declaring binding by exact name.
    pub effective: BTreeMap<String, (ResourceBinding, Object)>,
    /// Checked named message recipes retaining all media owners.
    pub recipes: BTreeMap<String, Composition>,
    /// Installed and JSON-projected locale forms.
    pub catalog: MessageCatalog,
    /// Recoverable setting warnings tied to the authoring resource.
    pub warnings: Vec<CandidateWarning>,
    owners: BTreeMap<String, Vec<ResourceBinding>>,
    definitions: Arc<BTreeMap<String, ResolvedMessageSettings>>,
    sources: BTreeMap<String, PublicMessageSource>,
    localized: BTreeMap<String, LocalizedMessage>,
    defaults: ResolvedMessageSettings,
}

fn localized(fields: &Object) -> Result<LocalizedMessage, PublicMessageError> {
    let mut message = LocalizedMessage::default();
    if let Some(bindings) = fields.get("bindings").and_then(Value::as_object) {
        for (name, path) in bindings {
            message.bindings.insert(
                name.clone(),
                BindingSource::from_public_path(path.as_str().expect("normalized path"))
                    .expect("normalized source"),
            );
        }
    }
    if let Some(locales) = fields.get("locales").and_then(Value::as_object) {
        for (locale, forms) in locales {
            let effective = message
                .locales
                .entry(locale.parse().expect("normalized locale"))
                .or_default();
            for (form, source) in forms.as_object().expect("normalized forms") {
                effective.insert(
                    form.clone(),
                    NativePattern::parse(source.as_str().expect("normalized pattern"))
                        .map_err(|_| PublicMessageError::Catalog)?,
                );
            }
        }
    }
    Ok(message)
}

fn settings(
    name: &str,
    fields: &Object,
    speech_default: &ResolvedSpeechSettings,
    morse_default: &ResolvedMorseSettings,
) -> ResolvedMessageSettings {
    let nested = |k: &str, f: &str| fields.get(k)?.get(f);
    let timing = |k: &str, f: &str| nested(k, "timing")?.get(f)?.as_u64();
    let mut speech = speech_default.clone();
    if let Some(model) = nested("speech", "model").and_then(Value::as_str) {
        speech.voice = model.to_owned();
    }
    if let Some(speed) = timing("speech", "speed_percent") {
        speech.speed_percent = speed;
    }
    if let Some(level) = nested("speech", "level_db").and_then(Value::as_i64) {
        speech.level_db = level;
    }
    let mut morse = morse_default.clone();
    if let Some(hz) = nested("morse", "frequency_hz").and_then(Value::as_u64) {
        morse.frequency_hz = hz;
    }
    if let Some(speed) = timing("morse", "speed_wpm") {
        morse.speed_wpm = speed;
    }
    if let Some(level) = nested("morse", "level_db").and_then(Value::as_i64) {
        morse.level_db = level;
    }
    let nonempty = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let media = MediaSource {
        file: nonempty(fields.get("file")).map(Into::into),
        speech: nonempty(nested("speech", "text")).map(|text| SpeechSource {
            text,
            model: speech.voice.clone().into(),
            speed_percent: speech.speed_percent as u32,
            level_db: speech.level_db as i32,
        }),
        morse: nonempty(nested("morse", "text")).map(|text| MorseSource {
            text,
            frequency_hz: morse.frequency_hz as f32,
            speed_wpm: morse.speed_wpm as u32,
            level_db: morse.level_db as i8,
        }),
        tone: nonempty(nested("tone", "sequence")).map(|sequence| ToneSource {
            sequence,
            level_db: nested("tone", "level_db")
                .and_then(Value::as_i64)
                .unwrap_or(-6) as i8,
        }),
        ..Default::default()
    };
    ResolvedMessageSettings {
        content: fields
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        message_id: Some(name.into()),
        dtmf_id: nonempty(fields.get("dtmf_id")),
        bindings: Default::default(),
        media,
        speech_defaults: speech,
        morse_defaults: morse,
        playback: PlaybackPolicy {
            repeat_during_playback: nested("playback", "repeat")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            destination: match nested("playback", "destination").and_then(Value::as_str) {
                Some("peers") => PlaybackDestination::Peers,
                Some("local_and_links") => PlaybackDestination::LocalAndLinks,
                _ => PlaybackDestination::Local,
            },
        },
        delivery: PoliteDelivery {
            maximum_wait_ms: timing("polite", "maximum_wait_ms").unwrap_or(60_000),
            expiry: if nested("polite", "expiry").and_then(Value::as_str) == Some("morse") {
                PoliteExpiry::Morse
            } else {
                PoliteExpiry::Skip
            },
            busy: if nested("polite", "busy").and_then(Value::as_str) == Some("wait") {
                PoliteBusy::Wait
            } else {
                PoliteBusy::Skip
            },
        },
    }
}

impl ModularCandidate<'_> {
    /// Prepare every effective named member in one invoking scope using its selected installed
    /// catalog, language and typed renderer defaults. Site membership is valid with no ports.
    pub fn message_profiles(
        &self,
        scope: ResourceScope,
        catalog: MessageCatalog,
        language: &LanguageIdentifier,
        speech: &ResolvedSpeechSettings,
        morse: &ResolvedMorseSettings,
    ) -> Result<ScopedMessages, PublicMessageError> {
        let site = ResourceScope::Site(self.registry().site_id());
        let port = match scope {
            ResourceScope::Site(_) if scope == site => None,
            ResourceScope::Port(id) if self.port_metadata(id).is_some() => Some(scope),
            _ => return Err(PublicMessageError::MissingScope),
        };
        let mut local = BTreeMap::new();
        let mut owners = BTreeMap::<String, Vec<ResourceBinding>>::new();
        let mut warnings = Vec::new();
        let mut effective = BTreeMap::<String, (ResourceBinding, Object)>::new();
        for owner in [Some(site), port].into_iter().flatten() {
            for (binding, key) in self
                .registry()
                .members()
                .iter()
                .filter(|(b, _)| b.scope == owner && b.kind == ResourceKind::Message)
            {
                let fail = PublicMessageError::Definition(*binding);
                if !identifier(key.as_str()) {
                    return Err(fail);
                }
                let body = normalized(
                    *binding,
                    self.member_fields(binding.id).ok_or(fail)?,
                    &mut warnings,
                )?;
                let (identity, inherited) = effective
                    .entry(key.as_str().into())
                    .or_insert((*binding, Object::new()));
                if let Some(new) = payload(&body, *binding)?
                    && payload(inherited, *identity)?.is_some_and(|old| new != old)
                {
                    replace_payload(inherited);
                }
                merge(inherited, &body);
                *identity = *binding;
                local.insert(binding.id, body);
                owners
                    .entry(key.as_str().into())
                    .or_default()
                    .push(*binding);
            }
        }
        let mut definitions = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut locales = BTreeMap::new();
        let mut selectors = BTreeMap::new();
        for (name, (binding, fields)) in &effective {
            let fail = PublicMessageError::Definition(*binding);
            let locale = localized(fields)?;
            let mut config = settings(name, fields, speech, morse);
            config.bindings = locale.bindings.clone();
            if let Some(key) = &config.dtmf_id
                && selectors.insert(key.clone(), *binding).is_some()
            {
                return Err(fail);
            }
            let source = match payload(fields, *binding)? {
                Some(Payload::Content) => PublicMessageSource::Content(
                    NativePattern::parse(&config.content).map_err(|_| fail)?,
                ),
                Some(Payload::Media) => PublicMessageSource::Media {
                    speech: config
                        .media
                        .speech
                        .as_ref()
                        .map(|s| NativePattern::parse(&s.text))
                        .transpose()
                        .map_err(|_| fail)?,
                    morse: config
                        .media
                        .morse
                        .as_ref()
                        .map(|m| NativePattern::parse(&m.text))
                        .transpose()
                        .map_err(|_| fail)?,
                },
                Some(Payload::Locales) | None => {
                    locales.insert(name.clone(), locale);
                    PublicMessageSource::Localized
                }
            };
            definitions.insert(name.clone(), config);
            sources.insert(name.clone(), source);
        }
        let media = sources
            .iter()
            .filter(|(_, source)| !matches!(source, PublicMessageSource::Localized))
            .map(|(key, _)| key.clone())
            .collect::<BTreeSet<_>>();
        let mut previous = BTreeMap::new();
        for (name, history) in &owners {
            if port.is_some()
                && let Some(parent) = history.first()
                && parent.scope == site
                && matches!(
                    payload(&local[&parent.id], *parent)?,
                    Some(Payload::Locales) | None
                )
            {
                previous.insert(name.clone(), localized(&local[&parent.id])?);
            }
        }
        let (catalog, bindings, rejected) = catalog
            .project_message_sources(language, &locales, &media, &previous)
            .map_err(|_| PublicMessageError::Catalog)?;
        for item in rejected {
            let language = item.language.to_string();
            let path = |fields: &Object| {
                fields
                    .get("locales")?
                    .get(&language)?
                    .get(&item.form)
                    .cloned()
            };
            let history = &owners[&item.name];
            let author = history
                .iter()
                .rev()
                .find(|b| path(&local[&b.id]).is_some())
                .expect("authored locale");
            normalize::warn(&mut warnings, *author, "locales");
            let replacement = if item.inherited {
                history.first().and_then(|b| path(&local[&b.id]))
            } else {
                None
            };
            let change = |fields: &mut Object, value: Option<Value>| {
                if let Some(translations) = fields
                    .get_mut("locales")
                    .and_then(|v| v.get_mut(&language))
                    .and_then(Value::as_object_mut)
                {
                    if let Some(value) = value {
                        translations.insert(item.form.clone(), value);
                    } else {
                        translations.remove(&item.form);
                    }
                }
            };
            change(
                &mut effective.get_mut(&item.name).expect("registered message").1,
                replacement,
            );
            if author.scope == scope {
                change(local.get_mut(&author.id).expect("local message"), None);
            }
        }
        for id in catalog.named_ids() {
            if !definitions.contains_key(id) {
                definitions.insert(id.into(), settings(id, &Object::new(), speech, morse));
                sources.insert(id.into(), PublicMessageSource::Localized);
            }
        }
        for (id, aliases) in bindings {
            if let Some(definition) = definitions.get_mut(&id) {
                definition.bindings = aliases;
            }
        }
        let definitions = Arc::new(definitions);
        let mut recipes = BTreeMap::new();
        for name in effective.keys() {
            let source = &sources[name];
            let recipe = Composition::compile_public(
                definitions[name].clone(),
                source,
                definitions.clone(),
                &sources,
                &locales,
                &catalog,
                MorseRequirement::Required,
            )
            .map_err(|_| PublicMessageError::Composition)?;
            recipes.insert(name.clone(), recipe);
        }
        for (name, (_, fields)) in &mut effective {
            let s = &definitions[name];
            merge(fields,json!({
                "playback":{"repeat":s.playback.repeat_during_playback,"destination":destination_name(s.playback.destination)},
                "polite":{"timing":{"maximum_wait_ms":s.delivery.maximum_wait_ms},"expiry":match s.delivery.expiry {PoliteExpiry::Skip=>"skip",PoliteExpiry::Morse=>"morse"},"busy":match s.delivery.busy {PoliteBusy::Skip=>"skip",PoliteBusy::Wait=>"wait"}},
                "speech":{"model":s.speech_defaults.voice,"level_db":s.speech_defaults.level_db,"timing":{"speed_percent":s.speech_defaults.speed_percent}},
                "morse":{"frequency_hz":s.morse_defaults.frequency_hz,"level_db":s.morse_defaults.level_db,"timing":{"speed_wpm":s.morse_defaults.speed_wpm}}
            }).as_object().expect("canonical message fields"));
        }
        let mut defaults = settings("", &Object::new(), speech, morse);
        defaults.message_id = None;
        Ok(ScopedMessages {
            scope,
            local,
            effective,
            recipes,
            catalog,
            warnings,
            owners,
            definitions,
            sources,
            localized: locales,
            defaults,
        })
    }
}

impl ScopedMessages {
    /// Source binding for a normalized effective leaf, including inherited locale attributes.
    pub fn source(&self, name: &str, path: &[&str]) -> Option<ResourceBinding> {
        fn has(fields: &Object, path: &[&str]) -> bool {
            let Some((first, rest)) = path.split_first() else {
                return false;
            };
            fields
                .get(*first)
                .and_then(|v| rest.iter().try_fold(v, |value, k| value.get(*k)))
                .is_some()
        }
        has(&self.effective.get(name)?.1, path).then_some(())?;
        self.owners
            .get(name)?
            .iter()
            .rev()
            .find(|b| {
                self.local
                    .get(&b.id)
                    .is_some_and(|fields| has(fields, path))
            })
            .copied()
    }

    /// Prepare unique fixed macro and mapping messages before admitting the command generation.
    pub fn command_messages(
        &self,
        commands: &crate::config::resources::commands::ScopedCommands,
    ) -> Result<BTreeMap<String, Composition>, PublicMessageError> {
        if commands.scope != self.scope {
            return Err(PublicMessageError::MissingScope);
        }
        let mut checked = BTreeMap::new();
        for action in commands.language.static_actions() {
            if let crate::command_language::Action::Message(content, _) = action
                && !checked.contains_key(content)
            {
                checked.insert(
                    content.clone(),
                    self.compile_inline(content, MorseRequirement::Optional)?,
                );
            }
        }
        Ok(checked)
    }

    /// Prepare an inline macro, alarm or label with the port's shared renderer defaults.
    pub fn compile_inline(
        &self,
        content: &str,
        morse: MorseRequirement,
    ) -> Result<Composition, PublicMessageError> {
        let mut settings = self.defaults.clone();
        settings.content = content.into();
        self.compile_consumer(settings, morse)
    }

    /// Prepare a protocol status or sensor label for a text-only consumer.
    pub fn compile_text_only(&self, content: &str) -> Result<Composition, PublicMessageError> {
        let recipe = self.compile_inline(content, MorseRequirement::TextOnly)?;
        if recipe.segments().iter().any(|s| {
            matches!(
                s,
                crate::composition::Segment::Media(_)
                    | crate::composition::Segment::Tone(_)
                    | crate::composition::Segment::FluentMedia { .. }
            )
        }) {
            return Err(PublicMessageError::Composition);
        }
        Ok(recipe)
    }
    /// Prepare a one-string consumer using this effective namespace and its checked compositor.
    pub fn compile_consumer(
        &self,
        settings: ResolvedMessageSettings,
        morse: MorseRequirement,
    ) -> Result<Composition, PublicMessageError> {
        let pattern =
            NativePattern::parse(&settings.content).map_err(|_| PublicMessageError::Composition)?;
        Composition::compile_public(
            settings,
            &PublicMessageSource::Content(pattern),
            self.definitions.clone(),
            &self.sources,
            &self.localized,
            &self.catalog,
            morse,
        )
        .map_err(|_| PublicMessageError::Composition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::Segment;
    use crate::config::resources::{
        PortKey,
        candidate::{CapturedResource, ResourceRegistry},
    };
    use serde_json::json;

    #[test]
    fn named_registry_carries_media_and_native_forms_through_the_existing_compiler() {
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
                b(3, site, ResourceKind::Message),
                Some("tone"),
                json!({"tone":{"sequence":"440/30"}}),
            ),
            (
                b(4, site, ResourceKind::Message),
                Some("greeting-part"),
                json!({"locales":{"en-US":{"text":"Hello { $node-name }","tts":"Hello { $node-name }","morse":"HI"}}}),
            ),
            (
                b(5, site, ResourceKind::Message),
                Some("announce"),
                json!({"content":"{ tone } { greeting-part } { no-links }","dtmf_id":"9"}),
            ),
            (
                b(6, port, ResourceKind::Message),
                Some("greeting-part"),
                json!({"locales":{"fr":{"tts":"Salut { $node-name }"}}}),
            ),
            (
                b(7, site, ResourceKind::Macro),
                Some("play"),
                json!({"actions":[{"action":"message","message":"{ announce }","destination":"local"}]}),
            ),
            (
                b(8, site, ResourceKind::Message),
                Some("part"),
                json!({"locales":{"en-US":{"text":"PART","tts":"Part","morse":"PART"},"fr":{"text":"COURT"}}}),
            ),
            (
                b(9, site, ResourceKind::Message),
                Some("notice"),
                json!({"locales":{"en-US":{"text":"English","tts":"notice","morse":"EN"},"fr":{"text":"{ part.text }"}}}),
            ),
            (
                b(10, port, ResourceKind::Message),
                Some("notice"),
                json!({"locales":{"fr":{"text":"LONG { $node-name }"}}}),
            ),
        ];
        let members = rows
            .iter()
            .filter_map(|(b, k, _)| k.map(|k| (*b, PortKey::new(k))))
            .collect::<Vec<_>>();
        let registry =
            ResourceRegistry::with_members(id(1), &[(id(2), PortKey::new("local"))], &[], &members)
                .unwrap();
        let docs = rows
            .iter()
            .map(|(b, key, fields)| {
                let mut envelope =
                    json!({"id":b.id.to_string(),"kind":b.kind.as_str(),"configuration":fields});
                if let Some(key) = key {
                    envelope["key"] = json!(key);
                }
                (*b, serde_json::to_vec(&envelope).unwrap())
            })
            .collect::<Vec<_>>();
        let captures = docs
            .iter()
            .map(|(b, json)| CapturedResource { binding: *b, json })
            .collect::<Vec<_>>();
        let candidate = ModularCandidate::assemble(&registry, &captures).unwrap();
        let speech = ResolvedSpeechSettings {
            voice: "voice".into(),
            speed_percent: 100,
            level_db: 0,
        };
        let morse = ResolvedMorseSettings {
            frequency_hz: 800,
            speed_wpm: 20,
            level_db: -6,
        };
        let catalog = MessageCatalog::from_sources(
            "en-US",
            include_str!("../../../../../../messages/en-US.ftl"),
            None,
        )
        .unwrap();
        let scoped = candidate
            .message_profiles(port, catalog, &"fr".parse().unwrap(), &speech, &morse)
            .unwrap();
        let commands = candidate.command_language(port).unwrap();
        assert!(
            scoped
                .command_messages(&commands)
                .unwrap()
                .contains_key("{ announce }")
        );
        assert!(
            scoped
                .command_messages(&candidate.command_language(site).unwrap())
                .is_err()
        );
        assert!(scoped.compile_text_only("{ tone }").is_err());
        let name = crate::messages::MessageArgument::Forms(crate::messages::MessageForms {
            text: "Z".repeat(126),
            tts: "Z".into(),
            morse: "Z".into(),
        });
        assert_eq!(
            scoped
                .catalog
                .format_named("notice", &BTreeMap::from([("node-name".into(), name)]))
                .unwrap()
                .text,
            "COURT"
        );
        assert!(scoped.compile_text_only("你好").is_ok());
        assert!(
            scoped
                .compile_inline("{ site-time }", MorseRequirement::Optional)
                .is_ok()
        );
        assert_eq!(scoped.recipes["announce"].segments().len(), 3);
        assert!(
            matches!(&scoped.recipes["announce"].segments()[0],Segment::FluentMedia{owner:Some(name),..} if name=="tone")
        );
        assert_eq!(scoped.effective["greeting-part"].0.id, id(6));
        assert_eq!(
            scoped.local[&id(6)]["locales"].as_object().unwrap().len(),
            1
        );
        assert!(
            scoped.effective["greeting-part"].1["locales"]
                .get("en-US")
                .is_some()
        );
    }
}
