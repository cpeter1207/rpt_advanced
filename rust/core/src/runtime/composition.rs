//! Control-only expansion of a compiled message against one immutable attempt snapshot.

use super::render;
use crate::{
    composition::{Composition, CompositionError, MorseRequirement, Segment},
    config::{BindingSource, ResolvedMessageSettings, ResolvedSiteSettings},
    media::{MediaProgram, MediaSource, MorseSource, SpeechSource},
    messages::{
        Daypart, MAX_MESSAGE_BYTES, Message, MessageArgument, MessageCatalog, MessageForms,
    },
    schedule::CivilTime,
    site::{Observation, ReadingQuality},
    time::TimeFormat,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Immutable recipes and selectors retained with their controller/media generation.
#[derive(Default)]
pub(super) struct MessageRecipes {
    definitions: Arc<BTreeMap<String, ResolvedMessageSettings>>,
    prefix_length: usize,
    // Configuration identity is independent of enablement or current read quality.
    site_sources: Vec<BindingSource>,
    pub items: Vec<MessageRecipe>,
    pub selectors: BTreeMap<String, usize>,
    pub names: BTreeMap<String, usize>,
    pub site_selectors: BTreeMap<String, usize>,
    pub site_unavailable: usize,
    pub warnings: Vec<crate::config::ConfigWarning>,
}

impl MessageRecipes {
    pub fn new(
        document: &crate::config::ConfigDocument,
        node: &crate::config::NodeId,
        catalog: &MessageCatalog,
    ) -> Result<Self, super::RuntimeError> {
        let prefix_length = crate::config::ResolvedNodeSettings::resolve(document, node)?
            .value
            .link_commands["link_command_message"]
            .len();
        let names = document
            .named_sections("message", None)
            .into_iter()
            .chain(document.named_sections("message", Some(node.as_str())))
            .filter_map(|section| section.rsplit(' ').next())
            .collect::<BTreeSet<_>>();
        let mut definitions = BTreeMap::new();
        for name in names {
            definitions.insert(
                name.to_owned(),
                ResolvedMessageSettings::resolve(document, node, name)?.value,
            );
        }
        let site = ResolvedSiteSettings::resolve(document, node)?.value;
        let mut result = Self {
            definitions: Arc::new(definitions),
            prefix_length,
            site_sources: site
                .sensors
                .iter()
                .map(|sensor| BindingSource::Sensor(sensor.name.clone()))
                .chain(
                    site.inputs
                        .iter()
                        .map(|input| BindingSource::Input(input.name.clone())),
                )
                .collect(),
            ..Self::default()
        };
        let definitions = result.definitions.clone();
        for (name, settings) in definitions.iter() {
            let section = format!("message {} {name}", node.as_str());
            if let Some(index) = result.add(
                settings.clone(),
                &section,
                catalog,
                MorseRequirement::Required,
            )? {
                result.names.insert(name.clone(), index);
            }
        }
        let defaults = ResolvedMessageSettings::from_scopes(document, node, &[], "")?;
        let site_prefix = crate::config::ResolvedNodeSettings::resolve(document, node)?
            .value
            .link_commands["link_command_site_status"]
            .len();
        for (name, source, selector, label, unit, precision) in site
            .sensors
            .iter()
            .map(|sensor| {
                (
                    sensor.name.as_str(),
                    BindingSource::Sensor(sensor.name.clone()),
                    &sensor.dtmf_id,
                    sensor.label_message_id.as_str(),
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
                    "",
                    0,
                )
            }))
        {
            let Some(selector) = selector else {
                continue;
            };
            if site_prefix + selector.len() > 127 {
                result.warnings.push(crate::config::ConfigWarning::new(
                    0,
                    node.as_str(),
                    "dtmf_id",
                    selector,
                    "selector and effective prefix exceed the command length",
                    "not DTMF addressable",
                ));
                continue;
            }
            let label = catalog
                .format_named(label, &BTreeMap::new())
                .unwrap_or_else(|_| forms(name));
            let mut unit = catalog
                .format_named(&format!("site-unit-{unit}"), &BTreeMap::new())
                .unwrap_or_else(|_| forms(unit));
            for text in [&mut unit.text, &mut unit.tts, &mut unit.morse] {
                if !text.is_empty() {
                    text.insert(0, ' ');
                }
            }
            let index = result.items.len();
            result.items.push(MessageRecipe::Site(Box::new(SiteReport {
                source: Some(source),
                label,
                unit,
                precision,
                settings: defaults.clone(),
            })));
            result.site_selectors.insert(selector.clone(), index);
        }
        result.site_unavailable = result.items.len();
        result.items.push(MessageRecipe::Site(Box::new(SiteReport {
            source: None,
            label: forms(""),
            unit: forms(""),
            precision: 0,
            settings: defaults,
        })));
        Ok(result)
    }

    /// Compile one usable source and register its selector in the shared node namespace.
    pub fn add(
        &mut self,
        settings: ResolvedMessageSettings,
        section: &str,
        catalog: &MessageCatalog,
        morse: MorseRequirement,
    ) -> Result<Option<usize>, super::RuntimeError> {
        let selector = settings.dtmf_id.clone();
        let recipe =
            Composition::compile(settings, self.definitions.clone(), morse).and_then(|recipe| {
                recipe.validate_catalog(catalog)?;
                Ok(recipe)
            });
        match recipe {
            Ok(recipe) => {
                for segment in recipe.segments() {
                    if let Segment::Message(owner) = segment {
                        let bindings = &recipe.settings_for(owner.as_deref()).bindings;
                        if bindings
                            .values()
                            .any(|source| !source.available_in_context(false))
                        {
                            self.disable(section, "time_remaining requires a deadline");
                            return Ok(None);
                        }
                        for source in bindings.values() {
                            if self.site_sources.contains(source) {
                                continue;
                            }
                            if let BindingSource::Sensor(name) | BindingSource::Input(name) = source
                            {
                                self.warnings.push(crate::config::ConfigWarning::new(
                                    0,
                                    section,
                                    "content",
                                    name,
                                    "site reading is unavailable until its source is configured",
                                    "unavailable reading message",
                                ));
                            }
                        }
                    }
                }
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
                self.items
                    .push(MessageRecipe::Composition(Box::new(recipe)));
                Ok(Some(index))
            }
            Err(error) => {
                self.disable(section, &format!("invalid composition: {error:?}"));
                Ok(None)
            }
        }
    }

    fn disable(&mut self, section: &str, reason: &str) {
        self.warnings.push(crate::config::ConfigWarning::new(
            0,
            section,
            "content",
            "",
            reason,
            "definition disabled",
        ));
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
    unit: MessageForms,
    precision: u8,
    settings: ResolvedMessageSettings,
}

impl MessageRecipe {
    pub fn prepare(
        &self,
        catalog: &MessageCatalog,
        values: &MessageValueSnapshot,
    ) -> Result<MediaProgram, CompositionError> {
        match self {
            Self::Composition(recipe) => recipe.prepare(catalog, values),
            Self::Site(report) => {
                let mut arguments = BTreeMap::from([(
                    "label".into(),
                    MessageArgument::Forms(report.label.clone()),
                )]);
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
    let mut arguments = BTreeMap::new();
    for (name, source) in &settings.bindings {
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
    catalog
        .format_named(
            settings
                .message_id
                .as_deref()
                .ok_or(CompositionError::MissingDefinition)?,
            &arguments,
        )
        .map_err(|_| CompositionError::Preparation)
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
    fn validate_catalog(&self, catalog: &MessageCatalog) -> Result<(), CompositionError> {
        for segment in self.segments() {
            match segment {
                Segment::Literal { text, .. } => {
                    render::morse(text).ok_or(CompositionError::Preparation)?;
                }
                Segment::Message(owner) => {
                    let settings = self.settings_for(owner.as_deref());
                    let arguments = settings
                        .bindings
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
                        .collect();
                    let forms = catalog
                        .format_named(
                            settings
                                .message_id
                                .as_deref()
                                .ok_or(CompositionError::MissingDefinition)?,
                            &arguments,
                        )
                        .map_err(|_| CompositionError::Preparation)?;
                    spoken_source(forms, settings)?;
                }
                Segment::Media(_) | Segment::Tone(_) => {}
            }
        }
        Ok(())
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
                Segment::Literal { text, owner } => spoken_source(
                    MessageForms {
                        text: text.clone(),
                        tts: render::telemetry_speech(text, &[]),
                        morse: text.clone(),
                    },
                    self.settings_for(owner.as_deref()),
                )?,
                Segment::Message(owner) => {
                    let settings = self.settings_for(owner.as_deref());
                    spoken_source(message_forms(settings, values, catalog)?, settings)?
                }
                Segment::Media(owner) => self.settings_for(owner.as_deref()).media.clone(),
                Segment::Tone(owner) => MediaSource {
                    tone: self.settings_for(owner.as_deref()).media.tone.clone(),
                    ..MediaSource::default()
                },
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
