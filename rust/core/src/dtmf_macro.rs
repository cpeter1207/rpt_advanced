//! Bounded, explicitly terminated control streams; no human timeout parsing.

use crate::{
    command::{DtmfCommandMap, LinkAction},
    config::ResolvedDtmfMacroSettings,
};

/// Prepared expansion bounds, independent of civil time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DtmfMacroLimits {
    /// Duration added by each comma.
    pub pause_ms: u64,
    /// Maximum significant symbols, including separators and terminators.
    pub max_symbols: usize,
    /// Maximum total pause and actual elapsed expansion duration.
    pub max_duration_ms: u64,
}

impl Default for DtmfMacroLimits {
    fn default() -> Self {
        Self {
            pause_ms: 2000,
            max_symbols: 4096,
            max_duration_ms: 120_000,
        }
    }
}

/// One prepared command boundary or monotonic delay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MacroStep {
    /// Expansion-local destination selection; never changes human selection.
    SelectTarget(String),
    /// Complete mapped command, excluding its outer star and terminating hash.
    Command(String),
    /// Coalesced comma duration.
    Pause(u64),
}

/// Validated bounded stream, prepared before configuration publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledDtmfMacro {
    /// Operator label; never a second DTMF mapping.
    pub name: String,
    /// Full star-prefixed invocation.
    pub trigger: String,
    /// Require an existing source grant on admission and every command.
    pub protected: bool,
    /// Explicit parser boundaries and pauses.
    pub steps: Vec<MacroStep>,
}

/// Credential-free diagnostic for an unavailable expansion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacroCompileError(&'static str);

impl MacroCompileError {
    /// Safe diagnostic text, containing no expansion or credential digits.
    pub fn message(self) -> &'static str {
        self.0
    }
}

/// Compile a stream against the effective target command namespace.
/// Remote/cross-hosted targets stay unavailable until their namespace and
/// source-grant capability can be verified; ordinary human routing is unchanged.
pub fn compile(
    settings: &ResolvedDtmfMacroSettings,
    limits: DtmfMacroLimits,
    map: &DtmfCommandMap,
    local: &str,
    triggers: &[String],
) -> Result<CompiledDtmfMacro, MacroCompileError> {
    let invalid = || MacroCompileError("invalid or unterminated macro command stream");
    let trigger = settings
        .trigger
        .strip_prefix('*')
        .filter(|digits| {
            !digits.is_empty()
                && digits.len() <= 63
                && digits
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'A'..=b'D'))
        })
        .ok_or_else(invalid)?;
    if map.mappings().iter().any(|mapping| {
        !mapping.digits.is_empty()
            && (trigger.starts_with(&mapping.digits) || mapping.digits.starts_with(trigger))
    }) || triggers
        .iter()
        .filter(|other| *other == &settings.trigger)
        .count()
        > 1
        || triggers.iter().any(|other| {
            other != &settings.trigger
                && (other.starts_with(&settings.trigger) || settings.trigger.starts_with(other))
        })
    {
        return Err(MacroCompileError(
            "ambiguous macro trigger; macro unavailable",
        ));
    }
    let mut steps = Vec::new();
    let mut command = String::new();
    let mut symbols = 0_usize;
    let mut total_pause = 0_u64;
    // Only a terminator sets complete, after clearing the buffer and adding a step.
    let mut complete = false;
    let mut trailing_pause = false;
    for symbol in settings
        .commands
        .chars()
        .filter(|symbol| !symbol.is_whitespace())
    {
        symbols += 1;
        if symbols > limits.max_symbols || limits.max_duration_ms == 0 {
            return Err(MacroCompileError("macro expansion exceeds prepared bounds"));
        }
        match symbol {
            ',' => {
                if !complete || limits.pause_ms == 0 {
                    return Err(invalid());
                }
                total_pause = total_pause
                    .checked_add(limits.pause_ms)
                    .filter(|duration| *duration <= limits.max_duration_ms)
                    .ok_or(MacroCompileError(
                        "macro pause duration exceeds prepared bounds",
                    ))?;
                if let Some(MacroStep::Pause(duration)) = steps.last_mut() {
                    *duration += limits.pause_ms;
                } else {
                    steps.push(MacroStep::Pause(limits.pause_ms));
                }
                trailing_pause = true;
            }
            '#' => {
                let text = command
                    .strip_prefix('*')
                    .filter(|text| !text.is_empty())
                    .ok_or_else(invalid)?;
                if triggers
                    .iter()
                    .any(|trigger| trigger.strip_prefix('*') == Some(text))
                {
                    return Err(MacroCompileError("nested macro invocation is unavailable"));
                }
                let lock = map.mappings().iter().any(|mapping| {
                    mapping.action == LinkAction::AdminLock && mapping.digits == text
                });
                if lock || map.parse_remote(text).is_some() {
                    steps.push(MacroStep::Command(text.into()));
                } else {
                    let parsed = map.parse(text).ok_or_else(invalid)?;
                    match parsed.action {
                        LinkAction::AdminUnlock | LinkAction::AdminLock => {
                            return Err(MacroCompileError(
                                "credential operations are forbidden in macro expansions",
                            ));
                        }
                        LinkAction::PadTest => {
                            return Err(MacroCompileError(
                                "interactive capture is unavailable in macro expansions",
                            ));
                        }
                        LinkAction::Command => {
                            if parsed.node != local {
                                return Err(MacroCompileError(
                                    "macro target namespace and source grants are not verified",
                                ));
                            }
                            steps.push(MacroStep::SelectTarget(parsed.node));
                        }
                        _ => steps.push(MacroStep::Command(text.into())),
                    }
                }
                command.clear();
                complete = true;
                trailing_pause = false;
            }
            '0'..='9' | 'A'..='D' | '*' => {
                command.push(symbol);
                complete = false;
            }
            _ => return Err(invalid()),
        }
    }
    if !complete || trailing_pause {
        return Err(invalid());
    }
    Ok(CompiledDtmfMacro {
        name: settings.name.clone(),
        trigger: settings.trigger.clone(),
        protected: settings.protected,
        steps,
    })
}

#[cfg(test)]
#[path = "dtmf_macro_tests.rs"]
mod tests;
