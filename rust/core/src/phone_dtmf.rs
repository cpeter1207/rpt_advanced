//! Inert per-call telephone DTMF grammar; emitted requests grant no authority.

use crate::command::{CommandMapError, DtmfCommandMap, LinkAction};
use std::{fmt, mem};

/// Prepared phone-only grammar choices, independent of call authentication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneDtmfOptions {
    /// A separate telephone egress exists; false never echoes reverse-call digits.
    pub telephone_egress: bool,
    /// Manual admission is configured; false reports manual actions unavailable.
    pub manual_admission: bool,
    /// Key prefix without the initiating asterisk.
    pub manual_key: String,
    /// Unkey prefix without the initiating asterisk.
    pub manual_unkey: String,
}

impl Default for PhoneDtmfOptions {
    fn default() -> Self {
        Self {
            telephone_egress: false,
            manual_admission: false,
            manual_key: "91".into(),
            manual_unkey: "90".into(),
        }
    }
}

/// A syntax result for the future call owner; none executes routing or policy.
#[derive(PartialEq, Eq)]
pub enum PhoneDtmfAction {
    /// One literal symbol for an existing telephone egress, never self-echo.
    TelephoneDigit(char),
    /// Request attachment to a decimal target or the configured default.
    Attach(Option<String>),
    /// Request an addressed command target without attaching media.
    SelectTarget(String),
    /// One symbol for the selected target's unchanged parser and authorization.
    NodeDigit {
        /// Selected decimal node; selection is not permission.
        target: String,
        /// Raw destination input; never log or regenerate credential symbols.
        digit: char,
    },
    /// A bare pound left addressed command mode locally.
    ExitCommandMode,
    /// Request call-local manual key admission.
    ManualKey,
    /// Request call-local manual unkey admission.
    ManualUnkey,
    /// Manual actions are unavailable under the configured VAD admission.
    ManualUnavailable,
    /// Discard incomplete or invalid collection privately without replay.
    CollectionDiscarded,
}

impl fmt::Debug for PhoneDtmfAction {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::TelephoneDigit(_) => "TelephoneDigit([redacted])",
            Self::Attach(_) => "Attach([redacted])",
            Self::SelectTarget(_) => "SelectTarget([redacted])",
            Self::NodeDigit { .. } => "NodeDigit([redacted])",
            Self::ExitCommandMode => "ExitCommandMode",
            Self::ManualKey => "ManualKey",
            Self::ManualUnkey => "ManualUnkey",
            Self::ManualUnavailable => "ManualUnavailable",
            Self::CollectionDiscarded => "CollectionDiscarded",
        })
    }
}

#[derive(Default)]
enum Collection {
    #[default]
    Idle,
    Prefix(String),
    Quote,
    Discard,
}

/// Pure collection state, owned by one future call generation.
///
/// Collection uses the existing three-second inactivity timeout and 127-symbol
/// bound (excluding the initiating star). Explicit node targets remain at most
/// 63 decimal digits. The call owner must authorize every emitted request.
pub struct PhoneDtmfGrammar {
    map: DtmfCommandMap,
    options: PhoneDtmfOptions,
    target: Option<String>,
    collection: Collection,
    last_ms: u64,
}

impl fmt::Debug for PhoneDtmfGrammar {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PhoneDtmfGrammar")
            .field("command_mode", &self.target.is_some())
            .field("collecting", &!matches!(self.collection, Collection::Idle))
            .finish_non_exhaustive()
    }
}

impl PhoneDtmfGrammar {
    /// Prepare routing and manual prefixes against the effective command namespace.
    pub fn new(map: &DtmfCommandMap, options: PhoneDtmfOptions) -> Result<Self, CommandMapError> {
        for prefix in [&options.manual_key, &options.manual_unkey] {
            if prefix.is_empty()
                || prefix.len() > 63
                || !prefix
                    .bytes()
                    .all(|digit| digit.is_ascii_digit() || matches!(digit, b'A'..=b'D'))
            {
                return Err(CommandMapError::InvalidPrefix);
            }
            if map.prefix_conflicts(prefix) {
                return Err(CommandMapError::AmbiguousPrefix);
            }
        }
        if options.manual_key.starts_with(&options.manual_unkey)
            || options.manual_unkey.starts_with(&options.manual_key)
        {
            return Err(CommandMapError::AmbiguousPrefix);
        }
        Ok(Self {
            map: map.clone(),
            options,
            target: None,
            collection: Collection::Idle,
            last_ms: 0,
        })
    }

    /// Feed one decoded DTMF symbol with a monotonic timestamp in milliseconds.
    pub fn digit(&mut self, digit: char, now_ms: u64) -> Vec<PhoneDtmfAction> {
        let mut output = self.tick(now_ms);
        self.last_ms = self.last_ms.max(now_ms);
        if !"0123456789ABCD*#".contains(digit) {
            self.collection = Collection::Discard;
            output.push(PhoneDtmfAction::CollectionDiscarded);
            return output;
        }
        if digit == '#' && self.target.is_some() && !matches!(self.collection, Collection::Quote) {
            self.target = None;
            self.collection = Collection::Idle;
            output.push(PhoneDtmfAction::ExitCommandMode);
            return output;
        }
        match mem::take(&mut self.collection) {
            Collection::Idle => {
                if digit == '*' {
                    self.collection = Collection::Prefix(String::new());
                } else {
                    self.emit(digit, &mut output);
                }
            }
            Collection::Quote => self.emit(digit, &mut output),
            Collection::Discard => {
                self.collection = match digit {
                    '#' => Collection::Idle,
                    '*' => Collection::Prefix(String::new()),
                    _ => Collection::Discard,
                };
            }
            Collection::Prefix(mut text) => {
                if digit == '*' && text.is_empty() {
                    self.collection = Collection::Quote;
                } else if digit == '#' {
                    self.collection = Collection::Prefix(text);
                    output.extend(self.finish());
                } else if digit == '*' {
                    if self.target.is_some() {
                        self.forward_prefix(&text, &mut output);
                    } else {
                        output.push(PhoneDtmfAction::CollectionDiscarded);
                    }
                    self.collection = Collection::Prefix(String::new());
                } else if text.len() == 127 {
                    // Retire the entire oversized collection; never leak its tail to an IVR.
                    self.collection = Collection::Discard;
                    output.push(PhoneDtmfAction::CollectionDiscarded);
                } else {
                    text.push(digit);
                    self.prefix(text, &mut output);
                }
            }
        }
        output
    }

    /// Complete collection at an explicit boundary, not at each symbol's release.
    pub fn finish(&mut self) -> Vec<PhoneDtmfAction> {
        let mut output = Vec::new();
        match mem::take(&mut self.collection) {
            Collection::Prefix(text) => {
                if self.target.is_some() {
                    self.forward_prefix(&text, &mut output);
                } else if self.map.mappings().iter().any(|mapping| {
                    mapping.action == LinkAction::Transceive
                        && !mapping.digits.is_empty()
                        && mapping.digits == text
                }) {
                    output.push(PhoneDtmfAction::Attach(None));
                } else if let Some(command) = self
                    .map
                    .parse(&text)
                    .filter(|command| command.node.len() < 64)
                {
                    match command.action {
                        LinkAction::Transceive => {
                            output.push(PhoneDtmfAction::Attach(Some(command.node)))
                        }
                        LinkAction::Command => {
                            self.target = Some(command.node.clone());
                            output.push(PhoneDtmfAction::SelectTarget(command.node));
                        }
                        _ => output.push(PhoneDtmfAction::CollectionDiscarded),
                    }
                } else {
                    output.push(PhoneDtmfAction::CollectionDiscarded);
                }
            }
            Collection::Quote => output.push(PhoneDtmfAction::CollectionDiscarded),
            Collection::Idle | Collection::Discard => {}
        }
        output
    }

    /// Apply the existing three-second collection timeout without replaying quotes.
    pub fn tick(&mut self, now_ms: u64) -> Vec<PhoneDtmfAction> {
        if !matches!(self.collection, Collection::Idle)
            && now_ms.saturating_sub(self.last_ms) >= 3000
        {
            self.finish()
        } else {
            Vec::new()
        }
    }

    /// Revoke collection and selected target without emitting stored symbols.
    pub fn cancel(&mut self) {
        self.collection = Collection::Idle;
        self.target = None;
        self.last_ms = 0;
    }

    /// Inspect syntactic command selection; this is never an authorization grant.
    pub fn selected_target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    fn emit(&self, digit: char, output: &mut Vec<PhoneDtmfAction>) {
        if let Some(target) = &self.target {
            output.push(PhoneDtmfAction::NodeDigit {
                target: target.clone(),
                digit,
            });
        } else if self.options.telephone_egress {
            output.push(PhoneDtmfAction::TelephoneDigit(digit));
        }
    }

    fn forward_prefix(&self, text: &str, output: &mut Vec<PhoneDtmfAction>) {
        for digit in std::iter::once('*').chain(text.chars()) {
            self.emit(digit, output);
        }
    }

    fn prefix(&mut self, text: String, output: &mut Vec<PhoneDtmfAction>) {
        if self.target.is_some() {
            let key = text == self.options.manual_key;
            if key || text == self.options.manual_unkey {
                output.push(if !self.options.manual_admission {
                    PhoneDtmfAction::ManualUnavailable
                } else if key {
                    PhoneDtmfAction::ManualKey
                } else {
                    PhoneDtmfAction::ManualUnkey
                });
            } else if self.options.manual_key.starts_with(&text)
                || self.options.manual_unkey.starts_with(&text)
            {
                self.collection = Collection::Prefix(text);
            } else {
                self.forward_prefix(&text, output);
            }
        } else if self.map.mappings().iter().any(|mapping| {
            matches!(mapping.action, LinkAction::Transceive | LinkAction::Command)
                && !mapping.digits.is_empty()
                && (mapping.digits.starts_with(&text) || text.starts_with(&mapping.digits))
        }) {
            self.collection = Collection::Prefix(text);
        } else {
            self.forward_prefix(&text, output);
        }
    }
}
