//! Typed remote-base DTMF payloads; link destinations stay in the linking command API.

#[path = "remote_base_command_wire.rs"]
mod wire;
pub use wire::RemoteOperation;

/// Per-node DTMF mutation policy; public status never requires an unlock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteDtmfAccess {
    /// Allow operating requests without a code.
    Open,
    /// Require the existing source-specific administrative grant.
    Locked,
    /// Disable operating requests while retaining public status queries.
    #[default]
    Disabled,
}

/// Direction of the familiar repeater-offset selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteOffset {
    /// Subtract the configured magnitude.
    Minus,
    /// Use the same receive/transmit base frequency.
    Simplex,
    /// Add the configured magnitude.
    Plus,
}

/// Public catalog queried in pages of at most five entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogKind {
    /// Declared typed settings and actions.
    Capabilities,
    /// Configuration-only memories.
    Memories,
    /// Configuration-only scan lists.
    ScanLists,
}

/// Exact signed decimal input. Explicit negative zero is retained for enum validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecimalValue {
    /// Entered sign: true negative, false nonnegative.
    pub negative: bool,
    /// Unsigned entered digits.
    pub magnitude: u64,
    /// Decimal digits after the point, zero through six.
    pub decimal_places: u32,
}

impl DecimalValue {
    /// Convert to the declared control's integer units without rounding or overflow.
    pub fn at_decimal_places(self, places: u32) -> Option<i64> {
        if self.decimal_places > 6 || places > 6 {
            return None;
        }
        let mut value = i128::from(self.magnitude);
        if self.negative {
            value = -value;
        }
        if places >= self.decimal_places {
            value = value.checked_mul(10_i128.pow(places - self.decimal_places))?;
        } else {
            let divisor = 10_i128.pow(self.decimal_places - places);
            if value % divisor != 0 {
                return None;
            }
            value /= divisor;
        }
        value.try_into().ok()
    }
}

/// Complete remote operating intent, independent of physical providers and link commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteCommand {
    /// Recall one configured memory.
    Recall(u32),
    /// Set integral-hertz base frequency and a configured offset direction.
    Frequency {
        /// Exact receive base frequency.
        hertz: u64,
        /// Offset selection; magnitude belongs to configuration.
        offset: RemoteOffset,
    },
    /// Set independent CTCSS encoder/decoder frequency.
    Tone {
        /// True transmit, false receive.
        encode: bool,
        /// Exact tenths of hertz.
        tenths_hz: u32,
    },
    /// Arm/disarm the selected tone direction.
    ToneEnabled {
        /// True transmit, false receive.
        encode: bool,
        /// Desired configured arm state.
        enabled: bool,
    },
    /// Low/medium/high configured power preset, indexed zero through two.
    PowerPreset(u8),
    /// Query short/long current radio status.
    Status {
        /// Include the complete operating and scan status.
        long: bool,
    },
    /// Set a shared mode selector.
    Mode(u32),
    /// Step the current base frequency by signed hertz.
    Step(i64),
    /// Run the selected list using a configured speed preset.
    ScanPreset {
        /// True forward, false reverse order.
        up: bool,
        /// Slow/medium/fast preset indexed zero through two.
        preset: u8,
    },
    /// Begin the admitted, configuration-bounded manual tune operation.
    Tune,
    /// Query one declared setting.
    GetSetting(u32),
    /// Set one declared scalar/enum/bool control; provider metadata validates the type.
    SetSetting {
        /// Stable public setting selector.
        selector: u32,
        /// Exact requested value, converted only after capability lookup.
        value: DecimalValue,
    },
    /// Start one configured scan list.
    StartScan(u32),
    /// Pause the scan on its current member.
    PauseScan,
    /// Resume a paused scan.
    ResumeScan,
    /// Stop the scan, retaining the applied member.
    StopScan,
    /// Set live scan direction: true forward, false reverse.
    ScanDirection(bool),
    /// Skip the current member for this scan run.
    Skip,
    /// Query one configured memory.
    Memory(u32),
    /// Query a bounded catalog page.
    Catalog {
        /// Catalog to query.
        kind: CatalogKind,
        /// Positive page number.
        page: u32,
    },
    /// Query a configured list and its current state.
    ScanStatus(u32),
    /// Set/clear administrative transmit inhibit; safety inhibition remains independent.
    Inhibit(bool),
    /// Set live scan dwell in positive milliseconds.
    Dwell(u64),
    /// Set live scan quiet hold in milliseconds, including zero.
    ResumeDelay(u64),
    /// Select a configured priority member, or disable priority excursions.
    Priority(Option<u32>),
    /// Query complete current radio/scan status.
    CurrentStatus,
    /// Invoke one declared argument-free operating action.
    Invoke(u32),
}

impl RemoteCommand {
    /// Status is public; every state-changing operating request needs the selected grant.
    pub fn administrative(self) -> bool {
        !matches!(
            self,
            Self::Status { .. }
                | Self::GetSetting(_)
                | Self::Memory(_)
                | Self::Catalog { .. }
                | Self::ScanStatus(_)
                | Self::CurrentStatus
        )
    }
}

fn number<T: std::str::FromStr>(text: &str) -> Option<T> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn selector(text: &str) -> Option<u32> {
    number::<u32>(text).filter(|value| *value != 0)
}

fn parts<const N: usize>(text: &str) -> Option<[&str; N]> {
    let mut input = text.split('*');
    let fields = std::array::from_fn(|_| input.next().unwrap_or_default());
    (!fields.iter().any(|field| field.is_empty()) && input.next().is_none()).then_some(fields)
}

/// Parse the payload after the configured remote family prefix, excluding termination.
/// Ordinary linking commands and their initiating/reset stars are handled elsewhere.
pub fn parse_remote_command(text: &str) -> Option<RemoteCommand> {
    use RemoteCommand as C;
    let fixed = match text {
        "40" => Some(C::ToneEnabled {
            encode: false,
            enabled: false,
        }),
        "41" => Some(C::ToneEnabled {
            encode: false,
            enabled: true,
        }),
        "42" => Some(C::ToneEnabled {
            encode: true,
            enabled: false,
        }),
        "43" => Some(C::ToneEnabled {
            encode: true,
            enabled: true,
        }),
        "44" => Some(C::PowerPreset(0)),
        "45" => Some(C::PowerPreset(1)),
        "46" => Some(C::PowerPreset(2)),
        "51" => Some(C::Status { long: false }),
        "52" => Some(C::Status { long: true }),
        "61" => Some(C::Mode(1)),
        "62" => Some(C::Mode(3)),
        "63" => Some(C::Mode(4)),
        "64" => Some(C::Mode(2)),
        "711" => Some(C::Step(-20)),
        "714" => Some(C::Step(-100)),
        "717" => Some(C::Step(-500)),
        "713" => Some(C::Step(20)),
        "716" => Some(C::Step(100)),
        "719" => Some(C::Step(500)),
        "721" => Some(C::ScanPreset {
            up: false,
            preset: 0,
        }),
        "724" => Some(C::ScanPreset {
            up: false,
            preset: 1,
        }),
        "727" => Some(C::ScanPreset {
            up: false,
            preset: 2,
        }),
        "723" => Some(C::ScanPreset {
            up: true,
            preset: 0,
        }),
        "726" => Some(C::ScanPreset {
            up: true,
            preset: 1,
        }),
        "729" => Some(C::ScanPreset {
            up: true,
            preset: 2,
        }),
        "79" => Some(C::Tune),
        "93" => Some(C::PauseScan),
        "94" => Some(C::ResumeScan),
        "95" => Some(C::StopScan),
        "960" => Some(C::ScanDirection(false)),
        "961" => Some(C::ScanDirection(true)),
        "97" => Some(C::Skip),
        "9940" => Some(C::Inhibit(false)),
        "9941" => Some(C::Inhibit(true)),
        "998" => Some(C::CurrentStatus),
        _ => None,
    };
    if fixed.is_some() {
        return fixed;
    }
    if let Some((prefix, rest)) = text.split_at_checked(3) {
        match prefix {
            "990" => {
                return Some(C::Catalog {
                    kind: CatalogKind::Capabilities,
                    page: selector(rest)?,
                });
            }
            "991" => {
                return Some(C::Catalog {
                    kind: CatalogKind::Memories,
                    page: selector(rest)?,
                });
            }
            "992" => {
                return Some(C::Catalog {
                    kind: CatalogKind::ScanLists,
                    page: selector(rest)?,
                });
            }
            "993" => return Some(C::ScanStatus(selector(rest)?)),
            "995" => return Some(C::Dwell(number::<u64>(rest).filter(|value| *value != 0)?)),
            "996" => return Some(C::ResumeDelay(number(rest)?)),
            "997" => {
                return Some(C::Priority(
                    number::<u32>(rest).map(|value| (value != 0).then_some(value))?,
                ));
            }
            "999" => return Some(C::Invoke(selector(rest)?)),
            _ => (),
        }
    }
    if let Some((prefix, rest)) = text.split_at_checked(2) {
        match prefix {
            "90" => return Some(C::GetSetting(selector(rest)?)),
            "91" => {
                let [setting, sign, digits, places] = parts(rest)?;
                let value = DecimalValue {
                    negative: match sign {
                        "0" => false,
                        "1" => true,
                        _ => return None,
                    },
                    magnitude: number(digits)?,
                    decimal_places: number(places)?,
                };
                value.at_decimal_places(value.decimal_places)?;
                return Some(C::SetSetting {
                    selector: selector(setting)?,
                    value,
                });
            }
            "92" => return Some(C::StartScan(selector(rest)?)),
            "98" => return Some(C::Memory(selector(rest)?)),
            _ => (),
        }
    }
    let (prefix, rest) = text.split_at_checked(1)?;
    match prefix {
        "0" => Some(C::Recall(selector(rest)?)),
        "1" => {
            let [whole, fraction, offset] = parts(rest)?;
            if !(1..=5).contains(&fraction.len()) {
                return None;
            }
            let hertz = number::<u64>(whole)?.checked_mul(1_000_000)?.checked_add(
                number::<u64>(fraction)?.checked_mul(10_u64.pow(6 - fraction.len() as u32))?,
            )?;
            if hertz == 0 {
                return None;
            }
            Some(C::Frequency {
                hertz,
                offset: match offset {
                    "1" => RemoteOffset::Minus,
                    "2" => RemoteOffset::Simplex,
                    "3" => RemoteOffset::Plus,
                    _ => return None,
                },
            })
        }
        "2" | "3" => {
            let [whole, tenth] = parts(rest)?;
            if tenth.len() != 1 {
                return None;
            }
            let tone = number::<u32>(whole)?
                .checked_mul(10)?
                .checked_add(number::<u32>(tenth)?)?;
            (500..=3000).contains(&tone).then_some(C::Tone {
                encode: prefix == "3",
                tenths_hz: tone,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "remote_base_command_tests.rs"]
mod tests;
