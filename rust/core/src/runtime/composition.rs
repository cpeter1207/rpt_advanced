//! Control-only expansion of a compiled message against one immutable attempt snapshot.

use super::render;
use crate::{
    composition::{Composition, CompositionError, MorseRequirement, Segment},
    config::{BindingSource, ResolvedMessageSettings},
    media::{MediaProgram, MediaSource, MorseSource, SpeechSource},
    messages::{
        Daypart, MAX_MESSAGE_BYTES, Message, MessageArgument, MessageCatalog, MessageForms,
    },
    schedule::CivilTime,
    site::{Observation, ReadingQuality},
    time::TimeFormat,
};
use std::collections::BTreeMap;

/// Immutable recipes and selectors retained with their controller/media generation.
#[derive(Default)]
pub(super) struct MessageRecipes {
    prefix_length: usize,
    pub items: Vec<MessageRecipe>,
    pub selectors: BTreeMap<String, usize>,
    pub names: BTreeMap<String, usize>,
    pub site_selectors: BTreeMap<String, usize>,
    pub alarm_messages: BTreeMap<(String, &'static str), usize>,
    pub site_unavailable: usize,
    pub warnings: Vec<crate::config::ConfigWarning>,
}

impl MessageRecipes {
    pub fn modular(
        scope: &crate::config::resources::generation::ScopeConfiguration,
        section: &str,
    ) -> Result<Self, super::RuntimeError> {
        let mut result = Self::default();
        for (name, recipe) in &scope.messages.recipes {
            let index = result.register(
                MessageRecipe::Composition(Box::new(recipe.clone())),
                recipe.settings_for(None).dtmf_id.clone(),
                section,
            )?;
            result.names.insert(name.clone(), index);
        }
        for (key, recipe) in &scope.equipment.alarm_messages {
            let index = result.items.len();
            result
                .items
                .push(MessageRecipe::Composition(Box::new(recipe.clone())));
            result.alarm_messages.insert(key.clone(), index);
        }
        result.register_site(
            &scope.equipment,
            scope.messages.renderer_defaults().clone(),
            0,
            section,
            &scope.messages.catalog,
        );
        Ok(result)
    }

    fn register_site(
        &mut self,
        equipment: &crate::config::resources::profiles::equipment::ScopedEquipment,
        defaults: ResolvedMessageSettings,
        site_prefix: usize,
        section: &str,
        catalog: &MessageCatalog,
    ) {
        let site = &equipment.settings;
        for (name, source, selector, unit, precision) in site
            .sensors
            .iter()
            .map(|sensor| {
                (
                    sensor.name.as_str(),
                    BindingSource::Sensor(sensor.name.clone()),
                    &sensor.dtmf_id,
                    sensor.unit.as_str(),
                    sensor.precision,
                )
            })
            .chain(site.inputs.iter().map(|input| {
                (
                    input.name.as_str(),
                    BindingSource::Input(input.name.clone()),
                    &input.dtmf_id,
                    "",
                    0,
                )
            }))
        {
            let Some(selector) = selector else {
                continue;
            };
            if site_prefix + selector.len() > 127 {
                self.warnings.push(crate::config::ConfigWarning::new(
                    0,
                    section,
                    "dtmf_id",
                    selector,
                    "selector and effective prefix exceed the command length",
                    "not DTMF addressable",
                ));
                continue;
            }
            let mut unit = catalog
                .format_named(&format!("site-unit-{unit}"), &BTreeMap::new())
                .unwrap_or_else(|_| forms(unit));
            for text in [&mut unit.text, &mut unit.tts, &mut unit.morse] {
                if !text.is_empty() {
                    text.insert(0, ' ');
                }
            }
            let index = self.items.len();
            self.items.push(MessageRecipe::Site(Box::new(SiteReport {
                source: Some(source),
                label: forms(name),
                label_recipe: equipment.labels.get(name).cloned(),
                unit,
                precision,
                settings: defaults.clone(),
            })));
            self.site_selectors.insert(selector.clone(), index);
        }
        self.site_unavailable = self.items.len();
        self.items.push(MessageRecipe::Site(Box::new(SiteReport {
            source: None,
            label: forms(""),
            label_recipe: None,
            unit: forms(""),
            precision: 0,
            settings: defaults,
        })));
    }

    /// Admit every configured recipe through the same selector bounds and collision checks.
    pub fn register(
        &mut self,
        recipe: MessageRecipe,
        selector: Option<String>,
        section: &str,
    ) -> Result<usize, super::RuntimeError> {
        let index = self.items.len();
        if let Some(selector) = selector {
            if self.prefix_length + selector.len() > 127 {
                self.warnings.push(crate::config::ConfigWarning::new(
                    0,
                    section,
                    "dtmf_id",
                    &selector,
                    "selector and effective prefix exceed the command length",
                    "not DTMF addressable",
                ));
            } else if self.selectors.insert(selector, index).is_some() {
                return Err(crate::config::ConfigError::structure(
                    section,
                    "duplicate effective message DTMF selector",
                )
                .into());
            }
        }
        self.items.push(recipe);
        Ok(index)
    }
}

/// Recipes share admission, generation fencing and exact-source reply routing.
pub(super) enum MessageRecipe {
    Composition(Box<Composition>),
    Site(Box<SiteReport>),
}

pub(super) struct SiteReport {
    source: Option<BindingSource>,
    label: MessageForms,
    label_recipe: Option<Composition>,
    unit: MessageForms,
    precision: u8,
    settings: ResolvedMessageSettings,
}

impl MessageRecipe {
    /// Configured media select their own audience; site replies retain their origin.
    pub fn playback(
        &self,
    ) -> Option<(
        crate::controller::PlaybackPolicy,
        crate::controller::PoliteDelivery,
    )> {
        match self {
            Self::Composition(recipe) => {
                let settings = recipe.settings_for(None);
                Some((settings.playback, settings.delivery))
            }
            Self::Site(_) => None,
        }
    }

    pub fn prepare(
        &self,
        catalog: &MessageCatalog,
        values: &MessageValueSnapshot,
    ) -> Result<MediaProgram, CompositionError> {
        match self {
            Self::Composition(recipe) => recipe.prepare(catalog, values),
            Self::Site(report) => {
                let label = match &report.label_recipe {
                    Some(recipe) => {
                        let mut label = forms("");
                        for part in recipe.prepare_text(catalog, values)? {
                            label.text.push_str(&part.text);
                            label.tts.push_str(&part.tts);
                            label.morse.push_str(&part.morse);
                        }
                        label
                    }
                    None => report.label.clone(),
                };
                let mut arguments =
                    BTreeMap::from([("label".into(), MessageArgument::Forms(label))]);
                let id = match &report.source {
                    None => "site-query-unavailable",
                    Some(source) => match values.binding(source, catalog)? {
                        BoundValue::Available(MessageArgument::Number(value)) => match source {
                            BindingSource::Input(_) if value == 0.0 => "site-input-inactive",
                            BindingSource::Input(_) => "site-input-active",
                            _ => {
                                arguments.insert(
                                    "value".into(),
                                    uniform(&format!(
                                        "{:.*}",
                                        usize::from(report.precision),
                                        value
                                    )),
                                );
                                arguments.insert(
                                    "unit".into(),
                                    MessageArgument::Forms(report.unit.clone()),
                                );
                                "site-meter-report"
                            }
                        },
                        _ => "site-value-unavailable",
                    },
                };
                let rendered = catalog
                    .format_named(id, &arguments)
                    .map_err(|_| CompositionError::Preparation)?;
                let source = spoken_source(rendered, &report.settings)?;
                Ok(MediaProgram {
                    morse: source.morse.iter().cloned().collect(),
                    primary: vec![source],
                })
            }
        }
    }
}

fn forms(value: &str) -> MessageForms {
    MessageForms {
        text: value.into(),
        tts: value.into(),
        morse: value.into(),
    }
}

/// Local metadata and the public accounts from the effective port generation.
#[derive(Clone, Debug, Default)]
pub struct MessagePortIdentity {
    /// Friendly port name, distinct from its stable local key.
    pub name: String,
    /// Configured local AllStarLink network number.
    pub allstarlink_number: Option<String>,
    /// Configured local AllStarLink callsign.
    pub allstarlink_callsign: Option<String>,
    /// Configured local EchoLink network number.
    pub echolink_number: Option<String>,
    /// Configured local EchoLink callsign.
    pub echolink_callsign: Option<String>,
}

impl From<&crate::config::resources::profiles::ResolvedProtocolPort> for MessagePortIdentity {
    fn from(port: &crate::config::resources::profiles::ResolvedProtocolPort) -> Self {
        let asl = port.allstarlink.as_ref().map(|p| &p.settings);
        let echo = port.echolink.as_ref().map(|p| &p.settings);
        Self {
            name: port.metadata.name.clone(),
            allstarlink_number: asl.and_then(|p| p.node_number).map(|n| n.to_string()),
            allstarlink_callsign: asl.map(|p| p.callsign.clone()).filter(|s| !s.is_empty()),
            echolink_number: echo
                .map(|p| p.node_number)
                .filter(|&n| n != 0)
                .map(|n| n.to_string()),
            echolink_callsign: echo.map(|p| p.callsign.clone()).filter(|s| !s.is_empty()),
        }
    }
}

/// Immutable trigger identity kept across polite wait and replay attempts.
#[derive(Clone, Debug)]
pub struct MessageEventIdentity {
    /// Trigger kind supplied by its owner.
    pub kind: String,
    /// Actual peer protocol, when supplied by the trigger.
    pub peer_protocol: Option<String>,
    /// Actual event peer's number, preserving textual digits.
    pub peer_number: Option<String>,
    /// Actual event peer's callsign.
    pub peer_callsign: Option<String>,
}

/// Owned control snapshot for one attempt; receive interruption reuses its prepared program.
#[derive(Clone)]
pub struct MessageValueSnapshot {
    /// Current local civil minute captured by the host.
    pub civil: CivilTime,
    /// Node-selected twelve- or twenty-four-hour clock.
    pub time_format: TimeFormat,
    /// Local node identity.
    pub node: String,
    /// Local station callsign.
    pub callsign: String,
    /// Effective port profile for this attempt.
    pub port: MessagePortIdentity,
    /// Captured trigger, if this consumer has an event identity.
    pub event: Option<MessageEventIdentity>,
    /// Owner-prepared typed built-in requests available to this playback attempt.
    pub builtin: BTreeMap<String, BTreeMap<String, MessageArgument>>,
    /// Already localized link status, with separate spoken node identities.
    pub links: MessageForms,
    /// Named sensor values with their actual acquisition status and time.
    pub sensors: BTreeMap<String, Observation<MessageArgument>>,
    /// Named digital inputs with their actual acquisition status and time.
    pub inputs: BTreeMap<String, Observation<MessageArgument>>,
    /// Optional deadline context; absence is not a zero-duration deadline.
    pub time_remaining: Option<MessageArgument>,
}

/// An unavailable site reading retains its identity instead of losing it as `None`.
#[derive(Debug, PartialEq)]
enum BoundValue<'a> {
    Available(MessageArgument),
    Unavailable(&'a str),
}

impl MessageValueSnapshot {
    /// Retain the actual caller's typed request with its captured event/deadline arguments.
    pub fn capture_builtin(&mut self, request: &Message<'_>) {
        self.builtin
            .insert(request.id().into(), request.captured_arguments());
    }

    fn binding<'a>(
        &self,
        source: &'a BindingSource,
        catalog: &MessageCatalog,
    ) -> Result<BoundValue<'a>, CompositionError> {
        let (year, month, day, weekday, hour, minute) = self.civil.components();
        let forms = match source {
            BindingSource::Sensor(name) | BindingSource::Input(name) => {
                let values = if matches!(source, BindingSource::Sensor(_)) {
                    &self.sensors
                } else {
                    &self.inputs
                };
                let value = values.get(name).filter(|reading| {
                    reading.quality == ReadingQuality::Valid && reading.observed_at_ms.is_some()
                }).and_then(|reading| reading.value.as_ref()).filter(|value| {
                    !matches!(value, MessageArgument::Number(number) if !number.is_finite())
                }).cloned();
                return Ok(value.map_or(BoundValue::Unavailable(name), BoundValue::Available));
            }
            BindingSource::TimeRemaining => {
                return self
                    .time_remaining
                    .clone()
                    .map(BoundValue::Available)
                    .ok_or(CompositionError::Preparation);
            }
            BindingSource::Callsign => MessageForms {
                text: self.callsign.clone(),
                tts: spelled(&self.callsign),
                morse: self.callsign.clone(),
            },
            BindingSource::Identity(source) => {
                use crate::config::IdentitySource::*;
                let event = self.event.as_ref();
                let (value, spell) = match source {
                    NodeKey => (Some(self.node.as_str()), true),
                    NodeName => (Some(self.port.name.as_str()), false),
                    AllstarlinkNumber => (self.port.allstarlink_number.as_deref(), true),
                    AllstarlinkCallsign => (self.port.allstarlink_callsign.as_deref(), true),
                    EcholinkNumber => (self.port.echolink_number.as_deref(), true),
                    EcholinkCallsign => (self.port.echolink_callsign.as_deref(), true),
                    PeerNumber => (event.and_then(|e| e.peer_number.as_deref()), true),
                    PeerCallsign => (event.and_then(|e| e.peer_callsign.as_deref()), true),
                    PeerProtocol => (event.and_then(|e| e.peer_protocol.as_deref()), false),
                    EventKind => (event.map(|e| e.kind.as_str()), false),
                };
                let value = value
                    .filter(|v| !v.is_empty())
                    .ok_or(CompositionError::Preparation)?;
                MessageForms {
                    text: value.into(),
                    tts: if spell { spelled(value) } else { value.into() },
                    morse: value.into(),
                }
            }
            BindingSource::Node => catalog
                .format_named(
                    "binding-node",
                    &BTreeMap::from([
                        ("node".into(), uniform(&self.node)),
                        ("digits".into(), uniform(&spelled(&self.node))),
                    ]),
                )
                .map_err(|_| CompositionError::Preparation)?,
            BindingSource::Date => catalog
                .format_named(
                    "binding-date",
                    &BTreeMap::from([
                        (
                            "date".into(),
                            uniform(&format!("{year:04}-{month:02}-{day:02}")),
                        ),
                        ("year".into(), MessageArgument::Number(year.into())),
                        ("month".into(), MessageArgument::Number(month.into())),
                        ("day".into(), MessageArgument::Number(day.into())),
                    ]),
                )
                .map_err(|_| CompositionError::Preparation)?,
            BindingSource::Time => {
                let clock = self
                    .time_format
                    .clock(hour.into(), minute.into())
                    .map_err(|_| CompositionError::Preparation)?;
                catalog
                    .format_named(
                        "binding-time",
                        &BTreeMap::from([
                            ("clock".into(), uniform(&clock)),
                            ("hour".into(), MessageArgument::Number(hour.into())),
                            ("minute".into(), MessageArgument::Number(minute.into())),
                        ]),
                    )
                    .map_err(|_| CompositionError::Preparation)?
            }
            BindingSource::DayOfWeek => catalog
                .format(&Message::DayOfWeek { day: weekday })
                .map_err(|_| CompositionError::Preparation)?,
            BindingSource::Greeting => catalog
                .format(&Message::Greeting {
                    daypart: match hour {
                        0..=11 => Daypart::Morning,
                        12..=16 => Daypart::Afternoon,
                        _ => Daypart::Evening,
                    },
                })
                .map_err(|_| CompositionError::Preparation)?,
            BindingSource::LinkStatus => self.links.clone(),
        };
        Ok(BoundValue::Available(MessageArgument::Forms(forms)))
    }
}

fn uniform(text: &str) -> MessageArgument {
    MessageArgument::Forms(MessageForms {
        text: text.into(),
        tts: text.into(),
        morse: text.into(),
    })
}

fn spelled(identity: &str) -> String {
    identity
        .chars()
        .map(|ch| ch.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn message_forms(
    settings: &ResolvedMessageSettings,
    values: &MessageValueSnapshot,
    catalog: &MessageCatalog,
) -> Result<MessageForms, CompositionError> {
    let id = settings
        .message_id
        .as_deref()
        .ok_or(CompositionError::MissingDefinition)?;
    let captured = values.builtin.get(id);
    bound_forms(
        &settings.bindings,
        catalog.fallback_bindings.get(id),
        values,
        catalog,
        |args| {
            let mut arguments = captured.cloned().unwrap_or_default();
            // Typed arguments supplied by this actual request own overlapping contract names.
            for (name, value) in args {
                arguments
                    .entry(name.clone())
                    .or_insert_with(|| value.clone());
            }
            let mut site = inherited_arguments(catalog.inherited_bindings.get(id), values, catalog);
            if let Some(captured) = captured {
                site.available.extend(captured.clone());
            }
            catalog.format_named_scoped(id, &arguments, &site)
        },
    )
}

fn inherited_arguments(
    sources: Option<&BTreeMap<String, BindingSource>>,
    values: &MessageValueSnapshot,
    catalog: &MessageCatalog,
) -> crate::messages::InheritedArguments {
    let mut arguments = crate::messages::InheritedArguments::default();
    for (name, source) in sources.into_iter().flatten() {
        match values.binding(source, catalog) {
            Ok(BoundValue::Available(value)) => {
                arguments.available.insert(name.clone(), value);
            }
            Ok(BoundValue::Unavailable(label)) => {
                arguments.unavailable.insert(name.clone(), label.into());
            }
            Err(_) => {}
        }
    }
    arguments
}

fn fragment_forms(
    text: &crate::messages::projection::CompiledText,
    values: &MessageValueSnapshot,
    catalog: &MessageCatalog,
    spoken: bool,
) -> Result<MessageForms, CompositionError> {
    bound_forms(
        &text.bindings,
        Some(&text.fallback_bindings),
        values,
        catalog,
        |args| {
            let inherited = inherited_arguments(Some(&text.inherited_bindings), values, catalog);
            catalog.format_text_scoped(text, args, Some(&inherited), spoken, false)
        },
    )
}

fn bound_forms(
    bindings: &BTreeMap<String, BindingSource>,
    lower: Option<&BTreeMap<String, BindingSource>>,
    values: &MessageValueSnapshot,
    catalog: &MessageCatalog,
    format: impl FnOnce(
        &BTreeMap<String, MessageArgument>,
    ) -> Result<MessageForms, crate::messages::CatalogError>,
) -> Result<MessageForms, CompositionError> {
    let mut arguments = BTreeMap::new();
    for (name, source) in bindings {
        let value = match values.binding(source, catalog)? {
            BoundValue::Available(value) => value,
            BoundValue::Unavailable(label) => {
                return catalog
                    .format_named(
                        "site-value-unavailable",
                        &BTreeMap::from([("label".into(), uniform(label))]),
                    )
                    .map_err(|_| CompositionError::Preparation);
            }
        };
        arguments.insert(name.clone(), value);
    }
    // A missing value used only by a lower form must not disable a usable higher form.
    for (name, source) in lower.into_iter().flatten() {
        if !arguments.contains_key(name)
            && let Ok(BoundValue::Available(value)) = values.binding(source, catalog)
        {
            arguments.insert(name.clone(), value);
        }
    }
    format(&arguments).map_err(|_| CompositionError::Preparation)
}

fn sample(bindings: &BTreeMap<String, BindingSource>) -> BTreeMap<String, MessageArgument> {
    bindings
        .iter()
        .map(|(name, source)| {
            let value = match source {
                BindingSource::Sensor(_)
                | BindingSource::Input(_)
                | BindingSource::TimeRemaining => MessageArgument::Number(1.0),
                _ => uniform("X"),
            };
            (name.clone(), value)
        })
        .collect()
}

fn spoken_source(
    forms: MessageForms,
    settings: &ResolvedMessageSettings,
) -> Result<MediaSource, CompositionError> {
    if [&forms.text, &forms.tts, &forms.morse]
        .iter()
        .any(|text| text.len() > MAX_MESSAGE_BYTES || text.contains('\0'))
    {
        return Err(CompositionError::TooLarge);
    }
    let speech = &settings.speech_defaults;
    let morse = &settings.morse_defaults;
    let morse_text = render::morse(&forms.morse).ok_or(CompositionError::Preparation)?;
    Ok(MediaSource {
        speech: (!speech.voice.is_empty() && !forms.tts.is_empty()).then(|| SpeechSource {
            text: forms.tts,
            model: speech.voice.clone().into(),
            speed_percent: speech.speed_percent as u32,
            level_db: speech.level_db as i32,
        }),
        morse: Some(MorseSource {
            text: morse_text,
            frequency_hz: morse.frequency_hz as f32,
            speed_wpm: morse.speed_wpm as u32,
            level_db: morse.level_db as i8,
        }),
        ..MediaSource::default()
    })
}

impl Composition {
    /// Check catalog names, required bindings, and fallback forms without capturing live values.
    #[cfg(test)]
    pub(crate) fn validate_catalog(
        &self,
        catalog: &MessageCatalog,
    ) -> Result<(), CompositionError> {
        self.validate_catalog_for(catalog, MorseRequirement::Required)
    }

    pub(crate) fn validate_catalog_for(
        &self,
        catalog: &MessageCatalog,
        usage: MorseRequirement,
    ) -> Result<(), CompositionError> {
        let spoken = usage != MorseRequirement::TextOnly;
        for segment in self.segments() {
            match segment {
                Segment::Message(owner) => {
                    let settings = self.settings_for(owner.as_deref());
                    let arguments = sample(&settings.bindings);
                    let forms = catalog
                        .validate_named(
                            settings
                                .message_id
                                .as_deref()
                                .ok_or(CompositionError::MissingDefinition)?,
                            &arguments,
                        )
                        .map_err(|_| CompositionError::Preparation)?;
                    if spoken {
                        spoken_source(forms, settings)?;
                    }
                }
                Segment::Fluent { owner, text } => {
                    if spoken && !text.static_morse {
                        return Err(CompositionError::Preparation);
                    }
                    let forms = catalog
                        .validate_fragment(text, &sample(&text.bindings), spoken)
                        .map_err(|_| CompositionError::Preparation)?;
                    if spoken {
                        spoken_source(forms, self.settings_for(owner.as_deref()))?;
                    }
                }
                Segment::FluentMedia {
                    owner,
                    speech,
                    morse,
                } => {
                    if !spoken {
                        return Err(CompositionError::Preparation);
                    }
                    if let Some(text) = speech {
                        catalog
                            .validate_fragment(text, &sample(&text.bindings), false)
                            .map_err(|_| CompositionError::Preparation)?;
                    }
                    if let Some(text) = morse {
                        if !text.static_morse {
                            return Err(CompositionError::Preparation);
                        }
                        let forms = catalog
                            .validate_fragment(text, &sample(&text.bindings), true)
                            .map_err(|_| CompositionError::Preparation)?;
                        let source = self
                            .settings_for(owner.as_deref())
                            .media
                            .morse
                            .as_ref()
                            .ok_or(CompositionError::Preparation)?;
                        let converted =
                            render::morse(&forms.morse).ok_or(CompositionError::Preparation)?;
                        crate::audio::MorseRenderer::new(
                            &converted,
                            source.speed_wpm,
                            source.frequency_hz,
                            source.level_db,
                        )
                        .map_err(|_| CompositionError::Preparation)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn text_part<'a>(
        &'a self,
        segment: &Segment,
        catalog: &MessageCatalog,
        values: &MessageValueSnapshot,
        spoken: bool,
    ) -> Result<(&'a ResolvedMessageSettings, MessageForms), CompositionError> {
        let (owner, forms) = match segment {
            Segment::Message(owner) => {
                let settings = self.settings_for(owner.as_deref());
                (owner, message_forms(settings, values, catalog)?)
            }
            Segment::Fluent { owner, text } => {
                (owner, fragment_forms(text, values, catalog, spoken)?)
            }
            _ => return Err(CompositionError::Preparation),
        };
        Ok((self.settings_for(owner.as_deref()), forms))
    }

    /// Return ordered bounded text forms for a label or protocol status using this attempt's
    /// actual snapshot. A media part fails instead of being converted to a file name or tone.
    pub fn prepare_text(
        &self,
        catalog: &MessageCatalog,
        values: &MessageValueSnapshot,
    ) -> Result<Vec<MessageForms>, CompositionError> {
        self.segments()
            .iter()
            .map(|s| {
                self.text_part(s, catalog, values, false)
                    .map(|(_, forms)| forms)
            })
            .collect()
    }

    /// Resolve this immutable recipe once on control, without parsing substituted data.
    ///
    /// Every segment keeps its defining section's effective media settings. The
    /// returned primary/Morse sequences own their values across interruption;
    /// call again only for a genuinely new attempt.
    pub fn prepare(
        &self,
        catalog: &MessageCatalog,
        values: &MessageValueSnapshot,
    ) -> Result<MediaProgram, CompositionError> {
        let mut program = MediaProgram::default();
        let mut complete_morse = true;
        for segment in self.segments() {
            let source = match segment {
                Segment::Message(_) | Segment::Fluent { .. } => {
                    let (settings, forms) = self.text_part(segment, catalog, values, true)?;
                    spoken_source(forms, settings)?
                }
                Segment::FluentMedia {
                    owner,
                    speech,
                    morse,
                } => {
                    let mut media = self.settings_for(owner.as_deref()).media.clone();
                    if let (Some(source), Some(text)) = (&mut media.speech, speech) {
                        source.text = fragment_forms(text, values, catalog, false)?.tts;
                    }
                    if let (Some(source), Some(text)) = (&mut media.morse, morse) {
                        let forms = fragment_forms(text, values, catalog, true)?;
                        source.text =
                            render::morse(&forms.morse).ok_or(CompositionError::Preparation)?;
                        crate::audio::MorseRenderer::new(
                            &source.text,
                            source.speed_wpm,
                            source.frequency_hz,
                            source.level_db,
                        )
                        .map_err(|_| CompositionError::Preparation)?;
                    }
                    media
                }
            };
            if let Some(morse) = &source.morse {
                program.morse.push(morse.clone());
            } else if source.file.is_some() || source.speech.is_some() {
                complete_morse = false;
            }
            program.primary.push(source);
        }
        if !complete_morse {
            program.morse.clear();
        }
        Ok(program)
    }
}

#[cfg(test)]
#[path = "composition_tests.rs"]
mod tests;
