//! Exact scalar requests for the shared remote owner. No RF policy lives here.

use super::{CatalogKind, RemoteCommand, RemoteOffset};

/// Pointer-free ABI-one operation, matching the shared SDK's `UrpRemoteOperation`.
/// Hosts copy these fields across their adapter boundary; the physical owner
/// validates values against actual capabilities and the configured RF policy.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteOperation {
    /// Complete record byte count, 40 on supported native platforms.
    pub struct_size: u32,
    /// Operation contract version, one.
    pub abi_version: u32,
    /// Shared operation identifier, one through 27.
    pub operation: u32,
    /// Memory, setting, list, preset, or catalog selector.
    pub selector: u32,
    /// Signed step in hertz.
    pub signed_value: i64,
    /// Exact unsigned operand or duration in declared units.
    pub magnitude: u64,
    /// Entered decimal precision; conversion occurs after capability lookup.
    pub decimal_places: u32,
    /// Operation-specific direction, sign, or enable bits.
    pub flags: u32,
}

impl RemoteCommand {
    /// Copy command intent without rounding, capability guesses or device I/O.
    /// Numeric identifiers follow the shared SDK operation contract, not DTMF.
    pub fn wire(self) -> RemoteOperation {
        let base = RemoteOperation {
            struct_size: std::mem::size_of::<RemoteOperation>() as u32,
            abi_version: 1,
            operation: 0,
            selector: 0,
            signed_value: 0,
            magnitude: 0,
            decimal_places: 0,
            flags: 0,
        };
        match self {
            Self::Recall(selector) => RemoteOperation {
                operation: 1,
                selector,
                ..base
            },
            Self::Frequency { hertz, offset } => RemoteOperation {
                operation: 2,
                magnitude: hertz,
                flags: match offset {
                    RemoteOffset::Minus => 1,
                    RemoteOffset::Simplex => 2,
                    RemoteOffset::Plus => 3,
                },
                ..base
            },
            Self::Tone { encode, tenths_hz } => RemoteOperation {
                operation: 3,
                magnitude: u64::from(tenths_hz),
                flags: u32::from(encode),
                ..base
            },
            Self::ToneEnabled { encode, enabled } => RemoteOperation {
                operation: 4,
                flags: u32::from(encode) | (u32::from(enabled) << 1),
                ..base
            },
            Self::PowerPreset(selector) => RemoteOperation {
                operation: 5,
                selector: u32::from(selector),
                ..base
            },
            Self::Status { long } => RemoteOperation {
                operation: 6,
                flags: u32::from(long),
                ..base
            },
            Self::Mode(selector) => RemoteOperation {
                operation: 7,
                selector,
                ..base
            },
            Self::Step(signed_value) => RemoteOperation {
                operation: 8,
                signed_value,
                ..base
            },
            Self::ScanPreset { up, preset } => RemoteOperation {
                operation: 9,
                selector: u32::from(preset),
                flags: u32::from(up),
                ..base
            },
            Self::Tune => RemoteOperation {
                operation: 10,
                ..base
            },
            Self::GetSetting(selector) => RemoteOperation {
                operation: 11,
                selector,
                ..base
            },
            Self::SetSetting { selector, value } => RemoteOperation {
                operation: 12,
                selector,
                magnitude: value.magnitude,
                decimal_places: value.decimal_places,
                flags: u32::from(value.negative),
                ..base
            },
            Self::StartScan(selector) => RemoteOperation {
                operation: 13,
                selector,
                ..base
            },
            Self::PauseScan => RemoteOperation {
                operation: 14,
                ..base
            },
            Self::ResumeScan => RemoteOperation {
                operation: 15,
                ..base
            },
            Self::StopScan => RemoteOperation {
                operation: 16,
                ..base
            },
            Self::ScanDirection(up) => RemoteOperation {
                operation: 17,
                flags: u32::from(up),
                ..base
            },
            Self::Skip => RemoteOperation {
                operation: 18,
                ..base
            },
            Self::Memory(selector) => RemoteOperation {
                operation: 19,
                selector,
                ..base
            },
            Self::Catalog { kind, page } => RemoteOperation {
                operation: 20,
                magnitude: u64::from(page),
                selector: match kind {
                    CatalogKind::Capabilities => 1,
                    CatalogKind::Memories => 2,
                    CatalogKind::ScanLists => 3,
                },
                ..base
            },
            Self::ScanStatus(selector) => RemoteOperation {
                operation: 21,
                selector,
                ..base
            },
            Self::Inhibit(enabled) => RemoteOperation {
                operation: 22,
                flags: u32::from(enabled),
                ..base
            },
            Self::Dwell(magnitude) => RemoteOperation {
                operation: 23,
                magnitude,
                ..base
            },
            Self::ResumeDelay(magnitude) => RemoteOperation {
                operation: 24,
                magnitude,
                ..base
            },
            Self::Priority(selector) => RemoteOperation {
                operation: 25,
                selector: selector.unwrap_or(0),
                ..base
            },
            Self::CurrentStatus => RemoteOperation {
                operation: 26,
                ..base
            },
            Self::Invoke(selector) => RemoteOperation {
                operation: 27,
                selector,
                ..base
            },
        }
    }
}
