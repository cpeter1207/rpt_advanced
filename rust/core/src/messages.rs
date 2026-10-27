//! Fluent-backed, bounded built-in RF messages, prepared only on the control plane.

use crate::schedule::{CivilTime, Weekday};
use fluent_bundle::{FluentArgs, FluentResource, concurrent::FluentBundle};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use unic_langid::LanguageIdentifier;

const ENGLISH: &str = include_str!("../../../messages/en-US.ftl");
const PACKAGE_DIRS: [&str; 2] = [
    "/usr/share/rpt-advanced/messages",
    "/usr/share/asterisk/rpt_advanced/messages",
];
const ADMIN_DIRS: [&str; 2] = [
    "/etc/rpt_advanced/messages",
    "/etc/asterisk/rpt_advanced/messages",
];
pub(crate) const MAX_MESSAGE_BYTES: usize = 127;
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
    "repeater-timed-out",
    "repeater-timeout-cleared",
    "duration-unit",
    "duration-join",
    "parrot-levels",
    "pad-test-empty",
    "pad-test-overflow",
    "dtmf-symbol",
    "incoming-text",
];

/// A typed built-in RF message request; callers cannot select arbitrary Fluent IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message<'a> {
    /// No keypad symbols were received during a local pad test.
    PadTestEmpty,
    /// Report discarded symbols before reading back the retained prefix.
    PadTestOverflow,
    /// One keypad symbol, localized independently for each output form.
    DtmfSymbol {
        /// Conventional DTMF symbol.
        symbol: char,
    },
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
        /// Effective group name or exact remote node identity.
        target: &'a str,
        /// True before connection, false before disconnection.
        connecting: bool,
        /// Localized remaining-time phrase.
        time_remaining: &'a str,
    },
    /// Report the elapsed transmitter watchdog duration.
    RepeaterTimedOut {
        /// Localized elapsed-duration phrase for this output form.
        duration: &'a str,
    },
    /// Announce that every receive input has cleared after a transmitter timeout.
    RepeaterTimeoutCleared,
    /// Format one nonzero duration component, or zero seconds.
    DurationPart {
        /// Whole number of the selected units.
        count: u64,
        /// Unit selected by the duration formatter.
        unit: DurationUnit,
    },
    /// Report the retained receive burst's peak and RMS levels.
    ParrotLevels {
        /// Peak dBFS rounded to the nearest integer.
        peak_dbfs: i32,
        /// RMS dBFS rounded to the nearest integer.
        rms_dbfs: i32,
    },
}

/// Ordered units used by localized compound durations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurationUnit {
    /// The current local month's Gregorian day count.
    Month,
    /// Seven days.
    Week,
    /// Twenty-four hours.
    Day,
    /// Sixty minutes.
    Hour,
    /// Sixty seconds.
    Minute,
    /// One second.
    Second,
}

impl DurationUnit {
    /// Stable Fluent selection key, independent of translated unit wording.
    fn key(self) -> &'static str {
        match self {
            Self::Month => "month",
            Self::Week => "week",
            Self::Day => "day",
            Self::Hour => "hour",
            Self::Minute => "minute",
            Self::Second => "second",
        }
    }
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

impl MessageForms {
    /// Read the matching audience form while composing localized fragments.
    fn attribute(&self, attribute: &str) -> &str {
        match attribute {
            "text" => &self.text,
            "tts" => &self.tts,
            _ => &self.morse,
        }
    }
}

/// A configuration-selected argument, formatted independently for each audience.
#[derive(Clone, Debug, PartialEq)]
pub enum MessageArgument {
    /// Natural numeric value, retaining Fluent plural/number formatting.
    Number(f64),
    /// Already localized identity, time, status, or unavailable-value wording.
    Forms(MessageForms),
}

/// Catalog loading, validation, or formatting failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogError {
    /// A pad report contains symbols outside the DTMF alphabet.
    InvalidSymbol,
    /// The required English catalog is invalid or incomplete.
    InvalidEnglish,
    /// A formatted RF message exceeds the 127-byte telemetry bound.
    OutputTooLong,
    /// A configured message, attribute, or required argument could not be resolved.
    InvalidMessage,
}

type Bundle = FluentBundle<FluentResource>;

/// One configuration generation's validated Fluent catalog.
pub struct MessageCatalog {
    english: Bundle,
    // Low-to-high precedence; lookup falls back separately for each attribute.
    overlays: Vec<Bundle>,
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
        let admin_dirs = ADMIN_DIRS.map(PathBuf::from);
        let package_dirs = PACKAGE_DIRS.map(PathBuf::from);
        Self::load_from_catalog_dirs(locale, &admin_dirs, &package_dirs)
    }

    fn load_from_catalog_dirs(
        locale: &str,
        admin_dirs: &[PathBuf],
        package_dirs: &[PathBuf],
    ) -> Result<Self, CatalogError> {
        let mut catalog = Self::from_sources("en-US", ENGLISH, None)?;
        for language in std::iter::once("en-US").chain((locale != "en-US").then_some(locale)) {
            let before = catalog.overlays.len();
            for directory in package_dirs.iter().rev().chain(admin_dirs.iter().rev()) {
                if let Some(source) = read_optional(&catalog_path(directory, language)) {
                    catalog.add_overlay(language, &source);
                }
            }
            if language != "en-US"
                && before == catalog.overlays.len()
                && catalog.translation_warnings.is_empty()
            {
                catalog
                    .translation_warnings
                    .push("catalog unavailable".into());
            }
        }
        Ok(catalog)
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
                        let warnings = validate_translation(&bundle, true);
                        (Some(bundle), warnings)
                    }
                },
            }
        };
        Ok(Self {
            english,
            overlays: translated.into_iter().collect(),
            translation_warnings,
        })
    }

    fn add_overlay(&mut self, locale: &str, source: &str) {
        match parse_bundle(locale, source) {
            Ok(bundle) => {
                self.translation_warnings
                    .extend(validate_translation(&bundle, false));
                self.overlays.push(bundle);
            }
            Err(()) => self
                .translation_warnings
                .push("catalog is malformed".into()),
        }
    }

    /// Format three RF variants, falling back independently to English per attribute.
    pub fn format(&self, message: &Message<'_>) -> Result<MessageForms, CatalogError> {
        self.format_attributes(message.id(), |_| message.args())
    }

    /// Attribute incoming human text without interpreting its body as a template.
    /// Sender characters are separated only for speech; the total bound includes
    /// the already-bounded source/body and one translated catalog literal.
    pub fn format_incoming_text(
        &self,
        source: &str,
        body: &str,
    ) -> Result<MessageForms, CatalogError> {
        let spoken = source
            .chars()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.format_bounded_attributes(
            "incoming-text",
            |attribute| {
                let mut args = FluentArgs::new();
                args.set(
                    "source",
                    if attribute == "tts" {
                        spoken.as_str()
                    } else {
                        source
                    },
                );
                args.set(
                    "kind",
                    if source.bytes().all(|byte| byte.is_ascii_digit()) {
                        "node"
                    } else {
                        "callsign"
                    },
                );
                args.set("body", body);
                args
            },
            |_| MAX_MESSAGE_BYTES + spoken.len() + body.len(),
        )
    }

    /// Format a validated configuration-selected ID; arguments are data, never content tokens.
    pub fn format_named(
        &self,
        id: &str,
        args: &BTreeMap<String, MessageArgument>,
    ) -> Result<MessageForms, CatalogError> {
        self.format_attributes(id, |attribute| {
            let mut values = FluentArgs::new();
            for (name, value) in args {
                match value {
                    MessageArgument::Number(value) => values.set(name, *value),
                    MessageArgument::Forms(forms) => values.set(
                        name,
                        match attribute {
                            "text" => forms.text.as_str(),
                            "tts" => forms.tts.as_str(),
                            _ => forms.morse.as_str(),
                        },
                    ),
                }
            }
            values
        })
    }

    fn format_attributes<'a>(
        &self,
        id: &str,
        arguments: impl Fn(&str) -> FluentArgs<'a>,
    ) -> Result<MessageForms, CatalogError> {
        self.format_bounded_attributes(id, arguments, |_| MAX_MESSAGE_BYTES)
    }

    /// Compose already-bounded fragments without applying the single-fragment limit twice.
    /// Each audience's budget includes its inputs and at most one catalog-sized literal.
    fn format_bounded_attributes<'a>(
        &self,
        id: &str,
        arguments: impl Fn(&str) -> FluentArgs<'a>,
        maximum: impl Fn(&str) -> usize,
    ) -> Result<MessageForms, CatalogError> {
        let mut output = Vec::with_capacity(3);
        for attribute in ATTRIBUTES {
            let args = arguments(attribute);
            let mut too_long = false;
            let text = self
                .overlays
                .iter()
                .rev()
                .chain(std::iter::once(&self.english))
                .find_map(|bundle| {
                    let text = format_attribute(bundle, id, attribute, &args)?;
                    if text.len() > maximum(attribute) {
                        too_long = true;
                        None
                    } else {
                        Some(text)
                    }
                })
                .ok_or(if too_long {
                    CatalogError::OutputTooLong
                } else {
                    CatalogError::InvalidMessage
                })?;
            output.push(text);
        }
        let mut output = output.into_iter();
        Ok(MessageForms {
            text: output.next().expect("three requested attributes"),
            tts: output.next().expect("three requested attributes"),
            morse: output.next().expect("three requested attributes"),
        })
    }

    /// Format a compound duration from largest to smallest unit, omitting zero parts.
    /// A month uses the current local month's length, not a fixed thirty-day approximation.
    /// Zero is spoken as zero seconds. Fluent owns plurals and list punctuation for each form.
    /// Each unit/literal keeps the catalog fragment bound; the composed total may be longer.
    pub fn format_duration(
        &self,
        seconds: u64,
        local: CivilTime,
    ) -> Result<MessageForms, CatalogError> {
        let (year, month, ..) = local.components();
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let month_days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        let mut remaining = seconds;
        let mut parts = Vec::with_capacity(6);
        for (unit, size) in [
            (DurationUnit::Month, month_days * 86400),
            (DurationUnit::Week, 604800),
            (DurationUnit::Day, 86400),
            (DurationUnit::Hour, 3600),
            (DurationUnit::Minute, 60),
            (DurationUnit::Second, 1),
        ] {
            let count = remaining / size;
            remaining %= size;
            if count != 0 || (seconds == 0 && unit == DurationUnit::Second) {
                parts.push(self.format(&Message::DurationPart { count, unit })?);
            }
        }
        let mut forms = parts.remove(0);
        for (index, part) in parts.iter().enumerate() {
            forms = self.format_bounded_attributes(
                "duration-join",
                |attribute| {
                    let mut args = FluentArgs::new();
                    args.set("head", forms.attribute(attribute));
                    args.set("tail", part.attribute(attribute));
                    args.set(
                        "position",
                        if index + 1 == parts.len() {
                            "last"
                        } else {
                            "more"
                        },
                    );
                    args
                },
                |attribute| {
                    MAX_MESSAGE_BYTES
                        + forms.attribute(attribute).len()
                        + part.attribute(attribute).len()
                },
            )?;
        }
        Ok(forms)
    }

    /// Format a watchdog notice with matching localized duration forms.
    /// Each duration fragment keeps its existing bound within the composed notice.
    pub fn format_timeout(
        &self,
        seconds: u64,
        local: CivilTime,
    ) -> Result<MessageForms, CatalogError> {
        let duration = self.format_duration(seconds, local)?;
        self.format_bounded_attributes(
            "repeater-timed-out",
            |attribute| {
                let mut args = FluentArgs::new();
                args.set("duration", duration.attribute(attribute));
                args
            },
            |attribute| MAX_MESSAGE_BYTES + duration.attribute(attribute).len(),
        )
    }

    /// Format a named connect/disconnect warning with matching localized duration forms.
    /// Bounded target and duration fragments feed the existing composed telemetry stream.
    pub fn format_schedule_warning(
        &self,
        seconds: u64,
        local: CivilTime,
        target: &MessageForms,
        connecting: bool,
    ) -> Result<MessageForms, CatalogError> {
        if ATTRIBUTES
            .iter()
            .any(|attribute| target.attribute(attribute).len() > MAX_MESSAGE_BYTES)
        {
            return Err(CatalogError::OutputTooLong);
        }
        let duration = self.format_duration(seconds, local)?;
        let warning = Message::ScheduledLinkChange {
            target: &target.text,
            connecting,
            time_remaining: &duration.text,
        };
        self.format_bounded_attributes(
            "scheduled-link-change",
            |attribute| {
                let mut args = warning.args();
                args.set("target", target.attribute(attribute));
                args.set("time_remaining", duration.attribute(attribute));
                args
            },
            |attribute| {
                MAX_MESSAGE_BYTES
                    + target.attribute(attribute).len()
                    + duration.attribute(attribute).len()
            },
        )
    }

    /// Compose one local pad reply from at most 128 independently bounded fragments.
    /// Ordinary catalog messages retain their 127-byte bound.
    pub fn format_pad_test(
        &self,
        digits: &str,
        overflowed: bool,
    ) -> Result<MessageForms, CatalogError> {
        if digits.len() > 127 {
            return Err(CatalogError::OutputTooLong);
        }
        if !digits
            .bytes()
            .all(|digit| b"0123456789*#ABCD".contains(&digit))
        {
            return Err(CatalogError::InvalidSymbol);
        }
        if digits.is_empty() {
            return self.format(&Message::PadTestEmpty);
        }
        let mut forms = MessageForms {
            text: String::new(),
            tts: String::new(),
            morse: String::new(),
        };
        let fragments = overflowed
            .then_some(Message::PadTestOverflow)
            .into_iter()
            .chain(digits.chars().map(|symbol| Message::DtmfSymbol { symbol }));
        for message in fragments {
            let fragment = self.format(&message)?;
            for (target, part) in [
                (&mut forms.text, fragment.text),
                (&mut forms.tts, fragment.tts),
                (&mut forms.morse, fragment.morse),
            ] {
                if !target.is_empty() {
                    target.push(' ');
                }
                target.push_str(&part);
            }
        }
        Ok(forms)
    }
}

impl Message<'_> {
    fn id(&self) -> &'static str {
        match self {
            Self::PadTestEmpty => "pad-test-empty",
            Self::PadTestOverflow => "pad-test-overflow",
            Self::DtmfSymbol { .. } => "dtmf-symbol",
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
            Self::RepeaterTimedOut { .. } => "repeater-timed-out",
            Self::RepeaterTimeoutCleared => "repeater-timeout-cleared",
            Self::DurationPart { .. } => "duration-unit",
            Self::ParrotLevels { .. } => "parrot-levels",
        }
    }

    fn args(&self) -> FluentArgs<'_> {
        let mut args = FluentArgs::new();
        match self {
            Self::NoLinks
            | Self::LastKeyedNone
            | Self::Greeting { .. }
            | Self::RepeaterTimeoutCleared
            | Self::PadTestEmpty
            | Self::PadTestOverflow => {}
            Self::DtmfSymbol { symbol } => {
                args.set("symbol", symbol.to_string());
                args.set(
                    "kind",
                    match symbol {
                        '*' => "star",
                        '#' => "pound",
                        _ => "other",
                    },
                );
            }
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
            Self::ScheduledLinkChange {
                target,
                connecting,
                time_remaining,
            } => {
                args.set("target", *target);
                args.set(
                    "direction",
                    if *connecting { "connect" } else { "disconnect" },
                );
                args.set("time_remaining", *time_remaining);
            }
            Self::DurationPart { count, unit } => {
                args.set("count", *count as i64);
                args.set("unit", unit.key());
            }
            Self::RepeaterTimedOut { duration } => args.set("duration", *duration),
            Self::ParrotLevels {
                peak_dbfs,
                rms_dbfs,
            } => {
                args.set("peak_dbfs", i64::from(*peak_dbfs));
                args.set("rms_dbfs", i64::from(*rms_dbfs));
            }
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

fn validate_translation(bundle: &Bundle, require_builtin_attributes: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    for id in REQUIRED_IDS {
        let args = required_sample_args(id);
        for attribute in ATTRIBUTES {
            if !require_builtin_attributes
                && bundle
                    .get_message(id)
                    .and_then(|message| message.get_attribute(attribute))
                    .is_none()
            {
                continue;
            }
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
        "incoming-text" => {
            args.set("source", "2000");
            args.set("kind", "node");
            args.set("body", "test message");
        }
        "dtmf-symbol" => {
            args.set("symbol", "*");
            args.set("kind", "star");
        }
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
        "scheduled-link-change" => {
            args.set("target", "test group");
            args.set("direction", "connect");
            args.set("time_remaining", "1 minute");
        }
        "repeater-timed-out" => args.set("duration", "3 minutes"),
        "duration-unit" => {
            args.set("unit", "second");
            args.set("count", 60);
        }
        "duration-join" => {
            args.set("head", "1 minute");
            args.set("tail", "30 seconds");
            args.set("position", "last");
        }
        "parrot-levels" => {
            args.set("peak_dbfs", -3);
            args.set("rms_dbfs", -18);
        }
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
