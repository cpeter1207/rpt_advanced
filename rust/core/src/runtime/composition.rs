//! Control-only expansion of a compiled message against one immutable attempt snapshot.

use super::render;
use crate::{
    composition::{Composition, CompositionError, Segment},
    config::{BindingSource, ResolvedMessageSettings},
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
    pub items: Vec<Composition>,
    pub selectors: BTreeMap<String, usize>,
    pub names: BTreeMap<String, usize>,
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
        let mut result = Self {
            definitions: Arc::new(definitions),
            ..Self::default()
        };
        let definitions = result.definitions.clone();
        for (name, settings) in definitions.iter() {
            let section = format!("message {} {name}", node.as_str());
            if let Some(index) = result.add(settings.clone(), &section, catalog) {
                result.names.insert(name.clone(), index);
                if let Some(selector) = &settings.dtmf_id {
                    if prefix_length + selector.len() > 127 {
                        result.warnings.push(crate::config::ConfigWarning::new(
                            0,
                            &section,
                            "dtmf_id",
                            selector,
                            "selector and effective prefix exceed the command length",
                            "not DTMF addressable",
                        ));
                        continue;
                    }
                    if result.selectors.insert(selector.clone(), index).is_some() {
                        return Err(crate::config::ConfigError::structure(
                            &section,
                            "duplicate effective message DTMF selector",
                        )
                        .into());
                    }
                }
            }
        }
        Ok(result)
    }

    pub fn add(
        &mut self,
        settings: ResolvedMessageSettings,
        section: &str,
        catalog: &MessageCatalog,
    ) -> Option<usize> {
        let recipe = Composition::compile(settings, self.definitions.clone()).and_then(|recipe| {
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
                            return None;
                        }
                        for source in bindings.values() {
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
                self.items.push(recipe);
                Some(index)
            }
            Err(error) => {
                self.disable(section, &format!("invalid composition: {error:?}"));
                None
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

impl MessageValueSnapshot {
    fn binding(
        &self,
        source: &BindingSource,
        catalog: &MessageCatalog,
    ) -> Result<Option<MessageArgument>, CompositionError> {
        let (year, month, day, weekday, hour, minute) = self.civil.components();
        let forms = match source {
            BindingSource::Sensor(name) | BindingSource::Input(name) => {
                let values = if matches!(source, BindingSource::Sensor(_)) {
                    &self.sensors
                } else {
                    &self.inputs
                };
                return Ok(values.get(name).filter(|reading| {
                    reading.quality == ReadingQuality::Valid && reading.observed_at_ms.is_some()
                }).and_then(|reading| reading.value.as_ref()).filter(|value| {
                    !matches!(value, MessageArgument::Number(number) if !number.is_finite())
                }).cloned());
            }
            BindingSource::TimeRemaining => {
                return self
                    .time_remaining
                    .clone()
                    .map(Some)
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
        Ok(Some(MessageArgument::Forms(forms)))
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
        let Some(value) = values.binding(source, catalog)? else {
            let label = match source {
                BindingSource::Sensor(name) | BindingSource::Input(name) => name,
                _ => name,
            };
            return catalog
                .format_named(
                    "site-value-unavailable",
                    &BTreeMap::from([("label".into(), uniform(label))]),
                )
                .map_err(|_| CompositionError::Preparation);
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
            }
            program.primary.push(source);
        }
        Ok(program)
    }
}

#[cfg(test)]
#[path = "composition_tests.rs"]
mod tests;
