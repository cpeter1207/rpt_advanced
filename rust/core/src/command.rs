//! Configurable DTMF linking-command recognition.

use std::fmt;

/// Linking operations represented by the legacy command table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkAction {
    /// Disconnect one nonpermanent link.
    Disconnect,
    /// Monitor one remote node.
    Monitor,
    /// Transceive with one remote node.
    Transceive,
    /// Forward subsequent commands to one remote node.
    Command,
    /// Report this node's links.
    Status,
    /// Disconnect all links while retaining reconnect information.
    DisconnectAll,
    /// Report the last transmitting node.
    LastKeyed,
    /// Monitor one remote node locally without forwarding audio.
    LocalMonitor,
    /// Disconnect one permanent link.
    DisconnectPermanent,
    /// Maintain one permanent monitor link.
    PermanentMonitor,
    /// Maintain one permanent transceive link.
    PermanentTransceive,
    /// Report connected-network topology.
    FullStatus,
    /// Restore links saved by disconnect-all.
    ReconnectAll,
    /// Maintain one permanent local-monitor link.
    PermanentLocalMonitor,
    /// Disconnect every active nonpermanent link.
    DisconnectNonPermanentAll,
    /// Announce the local time.
    Time,
}

/// Number of configurable linking actions.
pub const ACTION_COUNT: usize = 16;

/// A DTMF prefix and the operation it selects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandMapping {
    /// Prefix digits, excluding the initiating asterisk.
    pub digits: String,
    /// Selected operation.
    pub action: LinkAction,
}

impl CommandMapping {
    /// Construct an owned command mapping.
    pub fn new(digits: impl Into<String>, action: LinkAction) -> Self {
        Self {
            digits: digits.into(),
            action,
        }
    }
}

/// A parsed command with an owned decimal destination, if required.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// Selected operation.
    pub action: LinkAction,
    /// Decimal destination, or the empty string for node-free operations.
    pub node: String,
}

/// A configurable, validated DTMF command map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DtmfCommandMap {
    mappings: Vec<CommandMapping>,
}

/// A command-prefix validation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandMapError {
    /// A caller-supplied configuration option does not name a supported action.
    UnknownAction,
    /// A prefix contains unsupported characters or exceeds 63 bytes.
    InvalidPrefix,
    /// Two enabled mappings cannot be distinguished safely.
    AmbiguousPrefix,
}

impl fmt::Display for CommandMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownAction => "unknown command action",
            Self::InvalidPrefix => "invalid command prefix",
            Self::AmbiguousPrefix => "ambiguous command prefix",
        })
    }
}

impl std::error::Error for CommandMapError {}

impl DtmfCommandMap {
    /// Construct and validate an owned mapping table.
    pub fn new(mappings: Vec<CommandMapping>) -> Result<Self, CommandMapError> {
        Self::validate(&mappings)?;
        Ok(Self { mappings })
    }

    /// Validate mappings using the legacy prefix-collision rule.
    pub fn validate(mappings: &[CommandMapping]) -> Result<(), CommandMapError> {
        for mapping in mappings {
            if mapping.digits.len() > 63
                || (!mapping.digits.is_empty()
                    && !mapping
                        .digits
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'A'..=b'D')))
            {
                return Err(CommandMapError::InvalidPrefix);
            }
        }
        for (index, left) in mappings.iter().enumerate() {
            for right in mappings.iter().skip(index + 1) {
                if left.digits.is_empty()
                    || right.digits.is_empty()
                    || (!left.digits.starts_with(&right.digits)
                        && !right.digits.starts_with(&left.digits))
                {
                    continue;
                }
                let equal = left.digits == right.digits;
                let longer = if left.digits.len() > right.digits.len() {
                    left
                } else {
                    right
                };
                if equal || takes_node(longer.action) {
                    return Err(CommandMapError::AmbiguousPrefix);
                }
            }
        }
        Ok(())
    }

    /// Borrow the validated mapping table.
    pub fn mappings(&self) -> &[CommandMapping] {
        &self.mappings
    }

    /// Parse a complete command without its initiating asterisk.
    pub fn parse(&self, digits: &str) -> Option<Command> {
        self.mappings
            .iter()
            .find(|mapping| {
                !mapping.digits.is_empty()
                    && !takes_node(mapping.action)
                    && mapping.digits == digits
            })
            .map(|mapping| Command {
                action: mapping.action,
                node: String::new(),
            })
            .or_else(|| {
                self.mappings.iter().find_map(|mapping| {
                    if mapping.digits.is_empty()
                        || !digits.starts_with(&mapping.digits)
                        || !takes_node(mapping.action)
                    {
                        return None;
                    }
                    let node = &digits[mapping.digits.len()..];
                    if node.is_empty() || !node.bytes().all(|byte| byte.is_ascii_digit()) {
                        return None;
                    }
                    Some(Command {
                        action: mapping.action,
                        node: node.to_owned(),
                    })
                })
            })
    }

    /// Start bounded command collection using this map.
    pub fn collector(&self) -> CommandCollector {
        CommandCollector {
            mappings: self.mappings.clone(),
            digits: String::new(),
            last_ms: 0,
            active: false,
        }
    }

    /// Build the standard 16-action command map.
    pub fn standard() -> Self {
        let prefixes = [
            "1", "2", "3", "4", "70", "806", "72", "75", "811", "812", "813", "73", "816", "818",
            "10", "722",
        ];
        let mappings = prefixes
            .into_iter()
            .zip(all_actions())
            .map(|(digits, action)| CommandMapping::new(digits, action))
            .collect();
        Self::new(mappings).expect("standard command map is valid")
    }
}

impl Default for DtmfCommandMap {
    fn default() -> Self {
        Self::standard()
    }
}

/// Bounded DTMF collection state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandCollector {
    mappings: Vec<CommandMapping>,
    digits: String,
    last_ms: u64,
    active: bool,
}

impl CommandCollector {
    /// Feed one DTMF character or `\0` as the timeout tick.
    pub fn feed(&mut self, digit: char, now_ms: u64) -> Option<String> {
        if digit == '*' {
            self.digits.clear();
            self.active = true;
            self.last_ms = now_ms;
            return None;
        }
        if !self.active {
            return None;
        }
        let mut finish =
            digit == '#' || (digit == '\0' && now_ms.wrapping_sub(self.last_ms) >= 3000);
        if digit != '\0' && digit != '#' {
            if !digit.is_ascii_digit() && !matches!(digit, 'A'..='D') || self.digits.len() == 127 {
                self.active = false;
                return None;
            }
            self.digits.push(digit);
            self.last_ms = now_ms;
            let map = DtmfCommandMap {
                mappings: self.mappings.clone(),
            };
            finish = map.parse(&self.digits).is_some_and(|command| {
                command.node.is_empty() && !may_extend(&self.mappings, &self.digits)
            });
        }
        if !finish {
            return None;
        }
        self.active = false;
        (!self.digits.is_empty()).then(|| self.digits.clone())
    }

    /// Return whether collection is currently active.
    pub fn active(&self) -> bool {
        self.active
    }
}

fn all_actions() -> [LinkAction; ACTION_COUNT] {
    [
        LinkAction::Disconnect,
        LinkAction::Monitor,
        LinkAction::Transceive,
        LinkAction::Command,
        LinkAction::Status,
        LinkAction::DisconnectAll,
        LinkAction::LastKeyed,
        LinkAction::LocalMonitor,
        LinkAction::DisconnectPermanent,
        LinkAction::PermanentMonitor,
        LinkAction::PermanentTransceive,
        LinkAction::FullStatus,
        LinkAction::ReconnectAll,
        LinkAction::PermanentLocalMonitor,
        LinkAction::DisconnectNonPermanentAll,
        LinkAction::Time,
    ]
}

fn takes_node(action: LinkAction) -> bool {
    !matches!(
        action,
        LinkAction::Status
            | LinkAction::DisconnectAll
            | LinkAction::LastKeyed
            | LinkAction::FullStatus
            | LinkAction::ReconnectAll
            | LinkAction::DisconnectNonPermanentAll
            | LinkAction::Time
    )
}

fn may_extend(mappings: &[CommandMapping], digits: &str) -> bool {
    mappings.iter().any(|mapping| {
        !takes_node(mapping.action)
            && mapping.digits.len() > digits.len()
            && mapping.digits.starts_with(digits)
    })
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
