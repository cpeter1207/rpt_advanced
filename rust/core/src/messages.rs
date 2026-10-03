//! Fluent-backed, bounded built-in RF messages, prepared only on the control plane.

use crate::schedule::Weekday;
use fluent_bundle::{FluentArgs, FluentResource, concurrent::FluentBundle};
use std::{
    fs,
    path::{Path, PathBuf},
};
use unic_langid::LanguageIdentifier;

const ENGLISH: &str = include_str!("../../../messages/en-US.ftl");
const PACKAGE_DIR: &str = "/usr/share/asterisk/rpt_advanced/messages";
const ADMIN_DIR: &str = "/etc/asterisk/rpt_advanced/messages";
const MAX_MESSAGE_BYTES: usize = 127;
const ATTRIBUTES: [&str; 3] = ["text", "tts", "morse"];
const REQUIRED_IDS: &[&str] = &[
    "no-links",
    "link-status",
    "last-keyed-none",
    "last-keyed",
    "peer-connected-local",
    "peer-disconnected-local",
    "peer-connected-third-party",
    "peer-disconnected-third-party",
    "day-of-week",
    "greeting-morning",
    "greeting-afternoon",
    "greeting-evening",
    "time-announcement",
    "loop-rejected",
    "priority-group-selected",
    "priority-group-unavailable",
    "scheduled-link-change",
    "duration-seconds",
];

/// A typed built-in RF message request; callers cannot select arbitrary Fluent IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message<'a> {
    /// Report that no peers are connected.
    NoLinks,
    /// Summarize connected peer count, the first peer, and its mode.
    LinkStatus {
        /// Number of non-retiring connected peers.
        count: usize,
        /// First peer in the current link status.
        peer: &'a str,
        /// Mode key selected by the core.
        mode: LinkDisplayMode,
    },
    /// Report that no peer has keyed the node yet.
    LastKeyedNone,
    /// Report the last peer that keyed the node.
    LastKeyed {
        /// Last keyed peer identity.
        peer: &'a str,
    },
    /// Announce a direct peer connection.
    PeerConnectedLocal {
        /// Peer whose connection was accepted.
        peer: &'a str,
    },
    /// Announce a direct peer disconnection.
    PeerDisconnectedLocal {
        /// Peer whose connection ended.
        peer: &'a str,
    },
    /// Announce a third-party connection.
    PeerConnectedThirdParty {
        /// First node in the connection pair.
        first: &'a str,
        /// Second node in the connection pair.
        second: &'a str,
    },
    /// Announce a third-party disconnection.
    PeerDisconnectedThirdParty {
        /// First node in the connection pair.
        first: &'a str,
        /// Second node in the connection pair.
        second: &'a str,
    },
    /// Return the localized daypart greeting.
    Greeting {
        /// Daypart rendered from the catalog.
        daypart: Daypart,
    },
    /// Return the localized weekday name for an existing custom-template value.
    DayOfWeek {
        /// Weekday from the captured local civil time.
        day: Weekday,
    },
    /// Announce the time with localized wording and a separately generated clock value.
    TimeAnnouncement {
        /// Display/status greeting.
        greeting_text: &'a str,
        /// Spoken greeting.
        greeting_tts: &'a str,
        /// Formatted clock text.
        time: &'a str,
    },
    /// Explain a rejected topology loop.
    LoopRejected {
        /// Node whose proposed connection would create a topology loop.
        node: &'a str,
    },
    /// Announce the initially selected member of a named priority group.
    PriorityGroupSelected {
        /// Configured group display name.
        group: &'a str,
        /// Selected node identity.
        peer: &'a str,
    },
    /// Announce that no member of a priority group is reachable.
    PriorityGroupUnavailable {
        /// Configured group display name.
        group: &'a str,
    },
    /// Warn about a scheduled link change.
    ScheduledLinkChange {
        /// Localized remaining-time phrase.
        time_remaining: &'a str,
    },
    /// Format a duration in seconds for another localized message.
    DurationSeconds {
        /// Rounded-up remaining seconds.
        seconds: u64,
    },
}

/// Link mode used by the localized status message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkDisplayMode {
    /// Peer sends and receives linked audio.
    Transceive,
    /// Peer audio is received but not forwarded.
    Monitor,
    /// Peer has local-monitor-only routing.
    Local,
}

/// Daypart selected for the local-time announcement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Daypart {
    /// Midnight through 11:59 AM.
    Morning,
    /// Noon through 4:59 PM.
    Afternoon,
    /// 5 PM through 11:59 PM.
    Evening,
}

/// Independent wording for display/status, speech, and Morse playback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageForms {
    /// Text/status wording.
    pub text: String,
    /// TTS wording.
    pub tts: String,
    /// Morse wording.
    pub morse: String,
}

/// Catalog loading, validation, or formatting failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogError {
    /// The required English catalog is invalid or incomplete.
    InvalidEnglish,
    /// A formatted RF message exceeds the 127-byte telemetry bound.
    OutputTooLong,
}

type Bundle = FluentBundle<FluentResource>;

/// One configuration generation's validated Fluent catalog.
pub struct MessageCatalog {
    english: Bundle,
    translated: Option<Bundle>,
    translation_warnings: Vec<String>,
}

impl MessageCatalog {
    /// Validation issues in the optional locale overlay; bad attributes use English fallback.
    pub fn translation_warnings(&self) -> &[String] {
        &self.translation_warnings
    }
}

impl std::fmt::Debug for MessageCatalog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MessageCatalog")
    }
}

impl MessageCatalog {
    /// Load installed catalogs and admin overrides during configuration preparation.
    ///
    /// Missing translations use English. A malformed English source rejects the candidate
    /// generation, so the caller can keep the currently active catalog.
    pub fn load(locale: &str) -> Result<Self, CatalogError> {
        Self::load_from_dirs(locale, Path::new(ADMIN_DIR), Path::new(PACKAGE_DIR))
    }

    fn load_from_dirs(
        locale: &str,
        admin_dir: &Path,
        package_dir: &Path,
    ) -> Result<Self, CatalogError> {
        let english_admin = catalog_path(admin_dir, "en-US");
        let english_package = catalog_path(package_dir, "en-US");
        let english = read_optional(&english_admin)
            .or_else(|| read_optional(&english_package))
            .unwrap_or_else(|| ENGLISH.to_owned());
        let translation = if locale == "en-US" {
            read_optional(&english_admin).or_else(|| read_optional(&english_package))
        } else {
            read_optional(&catalog_path(admin_dir, locale))
                .or_else(|| read_optional(&catalog_path(package_dir, locale)))
        };
        Self::from_sources(locale, &english, translation.as_deref())
    }

    /// Validate an English source and add a best-effort optional locale overlay.
    pub fn from_sources(
        locale: &str,
        english: &str,
        translation: Option<&str>,
    ) -> Result<Self, CatalogError> {
        let english = parse_bundle("en-US", english).map_err(|_| CatalogError::InvalidEnglish)?;
        validate_english(&english)?;
        let (translated, translation_warnings) = if locale == "en-US" {
            (None, Vec::new())
        } else {
            match translation {
                None => (None, vec!["catalog unavailable".into()]),
                Some(source) => match parse_bundle(locale, source) {
                    Err(()) => (None, vec!["catalog is malformed".into()]),
                    Ok(bundle) => {
                        let warnings = validate_translation(&bundle);
                        (Some(bundle), warnings)
                    }
                },
            }
        };
        Ok(Self {
            english,
            translated,
            translation_warnings,
        })
    }

    /// Format three RF variants, falling back independently to English per attribute.
    pub fn format(&self, message: &Message<'_>) -> Result<MessageForms, CatalogError> {
        let id = message.id();
        let args = message.args();
        let mut output = Vec::with_capacity(3);
        for attribute in ATTRIBUTES {
            let translated = self.translated.as_ref().and_then(|bundle| {
                format_attribute(bundle, id, attribute, &args)
                    .filter(|text| text.len() <= MAX_MESSAGE_BYTES)
            });
            let text = translated
                .or_else(|| format_attribute(&self.english, id, attribute, &args))
                .ok_or(CatalogError::InvalidEnglish)?;
            if text.len() > MAX_MESSAGE_BYTES {
                return Err(CatalogError::OutputTooLong);
            }
            output.push(text);
        }
        let mut output = output.into_iter();
        Ok(MessageForms {
            text: output.next().expect("three requested attributes"),
            tts: output.next().expect("three requested attributes"),
            morse: output.next().expect("three requested attributes"),
        })
    }

    /// Format a schedule warning with locale-specific duration forms for text, speech and Morse.
    pub fn format_schedule_warning(&self, seconds: u64) -> Result<MessageForms, CatalogError> {
        let duration = self.format(&Message::DurationSeconds { seconds })?;
        let text = self
            .format(&Message::ScheduledLinkChange {
                time_remaining: &duration.text,
            })?
            .text;
        let tts = self
            .format(&Message::ScheduledLinkChange {
                time_remaining: &duration.tts,
            })?
            .tts;
        let morse = self
            .format(&Message::ScheduledLinkChange {
                time_remaining: &duration.morse,
            })?
            .morse;
        Ok(MessageForms { text, tts, morse })
    }
}

impl Message<'_> {
    fn id(&self) -> &'static str {
        match self {
            Self::NoLinks => "no-links",
            Self::LinkStatus { .. } => "link-status",
            Self::LastKeyedNone => "last-keyed-none",
            Self::LastKeyed { .. } => "last-keyed",
            Self::PeerConnectedLocal { .. } => "peer-connected-local",
            Self::PeerDisconnectedLocal { .. } => "peer-disconnected-local",
            Self::PeerConnectedThirdParty { .. } => "peer-connected-third-party",
            Self::PeerDisconnectedThirdParty { .. } => "peer-disconnected-third-party",
            Self::Greeting {
                daypart: Daypart::Morning,
            } => "greeting-morning",
            Self::Greeting {
                daypart: Daypart::Afternoon,
            } => "greeting-afternoon",
            Self::Greeting {
                daypart: Daypart::Evening,
            } => "greeting-evening",
            Self::DayOfWeek { .. } => "day-of-week",
            Self::TimeAnnouncement { .. } => "time-announcement",
            Self::LoopRejected { .. } => "loop-rejected",
            Self::PriorityGroupSelected { .. } => "priority-group-selected",
            Self::PriorityGroupUnavailable { .. } => "priority-group-unavailable",
            Self::ScheduledLinkChange { .. } => "scheduled-link-change",
            Self::DurationSeconds { .. } => "duration-seconds",
        }
    }

    fn args(&self) -> FluentArgs<'_> {
        let mut args = FluentArgs::new();
        match self {
            Self::NoLinks | Self::LastKeyedNone | Self::Greeting { .. } => {}
            Self::DayOfWeek { day } => args.set(
                "day",
                match day {
                    Weekday::Sunday => "sunday",
                    Weekday::Monday => "monday",
                    Weekday::Tuesday => "tuesday",
                    Weekday::Wednesday => "wednesday",
                    Weekday::Thursday => "thursday",
                    Weekday::Friday => "friday",
                    Weekday::Saturday => "saturday",
                },
            ),
            Self::LinkStatus { count, peer, mode } => {
                args.set("count", *count as i64);
                args.set("peer", *peer);
                args.set(
                    "mode",
                    match mode {
                        LinkDisplayMode::Transceive => "transceive",
                        LinkDisplayMode::Monitor => "monitor",
                        LinkDisplayMode::Local => "local",
                    },
                );
            }
            Self::LastKeyed { peer }
            | Self::PeerConnectedLocal { peer }
            | Self::PeerDisconnectedLocal { peer } => args.set("peer", *peer),
            Self::PeerConnectedThirdParty { first, second }
            | Self::PeerDisconnectedThirdParty { first, second } => {
                args.set("first", *first);
                args.set("second", *second);
            }
            Self::TimeAnnouncement {
                greeting_text,
                greeting_tts,
                time,
            } => {
                args.set("greeting_text", *greeting_text);
                args.set("greeting_tts", *greeting_tts);
                args.set("time", *time);
            }
            Self::LoopRejected { node } => args.set("node", *node),
            Self::PriorityGroupSelected { group, peer } => {
                args.set("group", *group);
                args.set("peer", *peer);
            }
            Self::PriorityGroupUnavailable { group } => args.set("group", *group),
            Self::ScheduledLinkChange { time_remaining } => {
                args.set("time_remaining", *time_remaining);
            }
            Self::DurationSeconds { seconds } => args.set("seconds", *seconds as i64),
        }
        args
    }
}

fn parse_bundle(locale: &str, source: &str) -> Result<Bundle, ()> {
    let locale: LanguageIdentifier = locale.parse().map_err(|_| ())?;
    let resource = FluentResource::try_new(source.to_owned()).map_err(|_| ())?;
    let mut bundle = FluentBundle::new_concurrent(vec![locale]);
    bundle.set_use_isolating(false);
    bundle.add_resource(resource).map_err(|_| ())?;
    Ok(bundle)
}

fn validate_english(bundle: &Bundle) -> Result<(), CatalogError> {
    for id in REQUIRED_IDS {
        let Some(message) = bundle.get_message(id) else {
            return Err(CatalogError::InvalidEnglish);
        };
        let args = required_sample_args(id);
        for attribute in ATTRIBUTES {
            let Some(pattern) = message.get_attribute(attribute).map(|value| value.value()) else {
                return Err(CatalogError::InvalidEnglish);
            };
            let mut errors = Vec::new();
            let output = bundle.format_pattern(pattern, Some(&args), &mut errors);
            if !errors.is_empty() || output.len() > MAX_MESSAGE_BYTES {
                return Err(CatalogError::InvalidEnglish);
            }
        }
    }
    Ok(())
}

fn validate_translation(bundle: &Bundle) -> Vec<String> {
    let mut warnings = Vec::new();
    for id in REQUIRED_IDS {
        let args = required_sample_args(id);
        for attribute in ATTRIBUTES {
            if format_attribute(bundle, id, attribute, &args)
                .is_none_or(|text| text.len() > MAX_MESSAGE_BYTES)
            {
                warnings.push(format!("{id}.{attribute} falls back to English"));
            }
        }
    }
    warnings
}

fn required_sample_args(id: &str) -> FluentArgs<'static> {
    let mut args = FluentArgs::new();
    match id {
        "link-status" => {
            args.set("count", 2);
            args.set("peer", "2000");
            args.set("mode", "transceive");
        }
        "last-keyed" | "peer-connected-local" | "peer-disconnected-local" => {
            args.set("peer", "2000");
        }
        "peer-connected-third-party" | "peer-disconnected-third-party" => {
            args.set("first", "1000");
            args.set("second", "2000");
        }
        "day-of-week" => args.set("day", "friday"),
        "time-announcement" => {
            args.set("greeting_text", "Good Morning");
            args.set("greeting_tts", "Good morning");
            args.set("time", "12:00 AM");
        }
        "loop-rejected" => args.set("node", "2000"),
        "priority-group-selected" => {
            args.set("group", "test group");
            args.set("peer", "2000");
        }
        "priority-group-unavailable" => args.set("group", "test group"),
        "scheduled-link-change" => args.set("time_remaining", "60 seconds"),
        "duration-seconds" => args.set("seconds", 60),
        _ => {}
    }
    args
}

fn format_attribute(
    bundle: &Bundle,
    id: &str,
    attribute: &str,
    args: &FluentArgs<'_>,
) -> Option<String> {
    let message = bundle.get_message(id)?;
    let pattern = message.get_attribute(attribute)?.value();
    let mut errors = Vec::new();
    let rendered = bundle.format_pattern(pattern, Some(args), &mut errors);
    errors.is_empty().then(|| rendered.into_owned())
}

fn catalog_path(directory: &Path, locale: &str) -> PathBuf {
    directory.join(format!("{locale}.ftl"))
}

fn read_optional(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

#[cfg(test)]
#[path = "messages_file_tests.rs"]
mod file_tests;
