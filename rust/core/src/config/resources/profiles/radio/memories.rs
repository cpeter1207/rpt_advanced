//! Ordered memory arrays and inherited, named scan definitions.
use super::super::normalize::{Fields, Rule, normalize, warn};
use super::{CandidateWarning, NormalizedProfileResource, RadioProfileError, ResourceBinding};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Operating modes supported by the public memory contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryMode {
    /// Frequency modulation.
    Fm,
    /// Amplitude modulation.
    Am,
    /// Upper sideband.
    Usb,
    /// Lower sideband.
    Lsb,
}

/// Tone programming retained independently from its enablement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryTone {
    /// Whether the memory requests this direction's CTCSS.
    pub enabled: bool,
    /// Exactly representable tenths of a hertz.
    pub tenths_hz: u16,
}

/// Complete radio memory with the derived transmitter frequency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Memory {
    /// One-based position, also used for the provider's internal display name.
    pub selector: u32,
    /// Exact receive/base frequency.
    pub receive_hz: u64,
    /// Exact transmit frequency after offset policy.
    pub transmit_hz: u64,
    /// Whether to apply the stored shift.
    pub offset_enabled: bool,
    /// Whether the stored direction is negative.
    pub offset_minus: bool,
    /// Stored shift magnitude, retained while disabled.
    pub offset_magnitude_hz: u64,
    /// Signed effective split.
    pub offset_hz: i64,
    /// Selected modulation.
    pub mode: MemoryMode,
    /// Receive decode definition.
    pub rx_ctcss: MemoryTone,
    /// Transmit encode definition.
    pub tx_ctcss: MemoryTone,
}

/// Effective scan definition; the containing key retains the exact scan name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanList<Selector = u32, Members = Vec<u32>> {
    /// Positive operation selector.
    pub selector: Selector,
    /// Ordered, distinct memory selectors.
    pub members: Members,
    /// Optional priority selector in this scan.
    pub priority: Option<u32>,
    /// Milliseconds after tune and settling.
    pub dwell_ms: u64,
    /// Delay after activity ends.
    pub resume_delay_ms: u64,
    /// Delay before scan dwell begins.
    pub settle_ms: u64,
}

/// Site scan defaults that a concrete port can complete by supplying a selector or member list.
pub type PartialSiteScan = ScanList<Option<u32>, Option<Vec<u32>>>;

type Body = Map<String, Value>;
type Sources<'a> = [Option<&'a NormalizedProfileResource>; 2];

fn scaled(value: &Value, places: i32) -> Option<i128> {
    crate::command_language::scaled(value.as_number()?.as_str(), places)
}
fn frequency(v: &Value) -> Option<Value> {
    u64::try_from(scaled(v, 6)?)
        .ok()
        .filter(|v| *v != 0)
        .map(|_| v.clone())
}
fn offset(v: &Value) -> Option<Value> {
    u64::try_from(scaled(v, 6)?).ok().map(|_| v.clone())
}
fn tone(v: &Value) -> Option<Value> {
    (500..=3000).contains(&scaled(v, 1)?).then(|| v.clone())
}
const TONE: Fields = &[("enabled", Rule::Bool), ("frequency", Rule::Value(tone))];
const MEMORY: Fields = &[
    ("frequency", Rule::Value(frequency)),
    ("mode", Rule::Choice(&["AM", "FM", "USB", "LSB"])),
    ("tx_ctcss", Rule::Object(TONE)),
    ("rx_ctcss", Rule::Object(TONE)),
    (
        "offset",
        Rule::Object(&[
            ("enabled", Rule::Bool),
            ("direction", Rule::Choice(&["plus", "minus"])),
            ("frequency", Rule::Value(offset)),
        ]),
    ),
];

impl Memory {
    fn read(row: &Value, index: usize) -> Option<Self> {
        let selector = u32::try_from(index).ok()?.checked_add(1)?;
        let receive_hz = u64::try_from(scaled(row.get("frequency")?, 6)?).ok()?;
        let split = row.get("offset")?;
        let magnitude = scaled(split.get("frequency")?, 6)?;
        let negative = split.get("direction")?.as_str()? == "minus";
        let signed = if split.get("enabled")?.as_bool()? {
            if magnitude == 0 {
                return None;
            }
            if negative { -magnitude } else { magnitude }
        } else {
            0
        };
        let offset_hz = i64::try_from(signed).ok()?;
        let transmit_hz = receive_hz
            .checked_add_signed(offset_hz)
            .filter(|v| *v > 0)?;
        let read_tone = |name| {
            let t = row.get(name)?;
            Some(MemoryTone {
                enabled: t.get("enabled")?.as_bool()?,
                tenths_hz: u16::try_from(scaled(t.get("frequency")?, 1)?).ok()?,
            })
        };
        let mode = match row.get("mode")?.as_str()? {
            "FM" => MemoryMode::Fm,
            "AM" => MemoryMode::Am,
            "USB" => MemoryMode::Usb,
            "LSB" => MemoryMode::Lsb,
            _ => return None,
        };
        Some(Self {
            selector,
            receive_hz,
            transmit_hz,
            offset_hz,
            offset_enabled: split.get("enabled")?.as_bool()?,
            offset_minus: negative,
            offset_magnitude_hz: u64::try_from(magnitude).ok()?,
            mode,
            rx_ctcss: read_tone("rx_ctcss")?,
            tx_ctcss: read_tone("tx_ctcss")?,
        })
    }
}
pub(super) fn normalize_memories(
    b: ResourceBinding,
    body: &Body,
    w: &mut Vec<CandidateWarning>,
) -> Body {
    let mut output = Body::new();
    for (key, value) in body {
        if key != "entries" {
            w.push(CandidateWarning {
                resource: b,
                warning: crate::config::resources::ResourceWarning::UnknownField,
            });
            continue;
        }
        let rows = value.as_array().and_then(|rows| {
            rows.iter()
                .enumerate()
                .map(|(i, raw)| {
                    let row = Value::Object(normalize(b, raw.as_object()?, MEMORY, w));
                    Memory::read(&row, i).map(|_| row)
                })
                .collect::<Option<Vec<_>>>()
        });
        if let Some(rows) = rows {
            output.insert(key.clone(), Value::Array(rows));
        } else {
            warn(w, b, "entries")
        }
    }
    output
}
pub(super) fn effective(layers: Sources<'_>) -> Vec<Memory> {
    layers
        .into_iter()
        .flatten()
        .find_map(|r| r.configuration.get("entries"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, row)| Memory::read(row, i).expect("normalized complete memory"))
        .collect()
}

fn members(v: &Value) -> Option<Value> {
    let a = v.as_array()?;
    let mut unique = BTreeSet::new();
    (!a.is_empty()
        && a.iter().all(|value| {
            value
                .as_u64()
                .is_some_and(|n| n != 0 && n <= u32::MAX.into() && unique.insert(n))
        }))
    .then(|| v.clone())
}
const SCAN: Fields = &[
    ("selector", Rule::Unsigned(1, u32::MAX as u64)),
    ("priority", Rule::Unsigned(1, u32::MAX as u64)),
    ("members", Rule::Value(members)),
    (
        "timing",
        Rule::Object(&[
            ("dwell_ms", Rule::Unsigned(1, u64::MAX)),
            ("resume_delay_ms", Rule::Unsigned(0, u64::MAX)),
            ("settle_ms", Rule::Unsigned(0, u64::MAX)),
        ]),
    ),
];
pub(super) fn normalize_scans(
    b: ResourceBinding,
    body: &Body,
    w: &mut Vec<CandidateWarning>,
) -> Body {
    body.iter()
        .filter_map(|(name, v)| {
            let valid = !name.is_empty()
                && name.chars().count() <= 63
                && !name
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '[' | ']' | ':' | '\0'));
            if let Some(obj) = v.as_object().filter(|_| valid) {
                Some((name.clone(), Value::Object(normalize(b, obj, SCAN, w))))
            } else {
                warn(w, b, "scan_lists");
                None
            }
        })
        .collect()
}
pub(super) fn scans(
    layers: Sources<'_>,
    memory_count: usize,
    mut partial_site_scans: Option<&mut BTreeMap<String, PartialSiteScan>>,
    w: &mut Vec<CandidateWarning>,
    diagnostics: &mut Vec<RadioProfileError>,
) -> BTreeMap<String, ScanList> {
    let retain_site_references = partial_site_scans.is_some();
    let mut scans = BTreeMap::new();
    let mut selectors = BTreeSet::new();
    let names = layers
        .into_iter()
        .flatten()
        .flat_map(|r| r.configuration.keys())
        .collect::<BTreeSet<_>>();
    for name in names {
        let sources =
            layers.map(|r| r.and_then(|r| r.configuration.get(name).map(|body| (body, r.binding))));
        let source = sources
            .into_iter()
            .flatten()
            .next()
            .expect("named definition")
            .1;
        let field = |key| {
            sources
                .into_iter()
                .flatten()
                .find_map(|(row, b)| row.get(key).map(|v| (v, b)))
        };
        let timing = |key, default| {
            sources
                .into_iter()
                .flatten()
                .find_map(|(row, _)| row.get("timing")?.get(key)?.as_u64())
                .unwrap_or(default)
        };
        let selector =
            field("selector").map(|(v, b)| (v.as_u64().expect("normalized selector") as u32, b));
        if selector.is_none() {
            if retain_site_references {
                diagnostics.push(RadioProfileError {
                    resource: source,
                    conflicting: None,
                    field: "selector",
                });
            } else {
                warn(w, source, "selector");
                continue;
            }
        }
        let mut members = None;
        for (row, b) in sources.into_iter().flatten() {
            if let Some(values) = row.get("members").and_then(Value::as_array) {
                let values = values
                    .iter()
                    .map(|v| v.as_u64().expect("normalized member") as u32)
                    .collect::<Vec<_>>();
                if values.iter().all(|&n| n as usize <= memory_count) {
                    members = Some(values);
                    break;
                }
                if retain_site_references {
                    diagnostics.push(RadioProfileError {
                        resource: b,
                        conflicting: None,
                        field: "members",
                    });
                    members = Some(values);
                    break;
                }
                warn(w, b, "members");
            }
        }
        if members.is_none() {
            if let Some((_, resource)) = field("members") {
                diagnostics.push(RadioProfileError {
                    conflicting: None,
                    resource,
                    field: "members",
                });
            }
            if retain_site_references {
                diagnostics.push(RadioProfileError {
                    resource: source,
                    conflicting: None,
                    field: "members",
                });
            } else {
                warn(w, source, "members");
                continue;
            }
        }
        let priority = sources.into_iter().flatten().find_map(|(row, b)| {
            let n = row.get("priority")?.as_u64()? as u32;
            if members.as_ref().is_some_and(|values| values.contains(&n)) {
                Some(n)
            } else {
                if retain_site_references {
                    diagnostics.push(RadioProfileError {
                        resource: b,
                        conflicting: None,
                        field: "priority",
                    });
                    return Some(n);
                }
                warn(w, b, "priority");
                None
            }
        });
        if let Some((selector, selector_source)) = selector
            && !selectors.insert(selector)
        {
            diagnostics.push(RadioProfileError {
                conflicting: None,
                resource: selector_source,
                field: "selector",
            });
        }
        let names = ["dwell_ms", "settle_ms"];
        let candidates = names.map(|key| {
            sources
                .into_iter()
                .flatten()
                .filter_map(|(row, b)| Some((row.get("timing")?.get(key)?.as_u64()?, Some(b))))
                .collect()
        });
        let [dwell_ms, settle_ms] = super::inputs::choose_pair(
            candidates,
            [500, 0],
            names,
            |d, s| d.checked_add(s).is_some(),
            w,
        );
        let resume_delay_ms = timing("resume_delay_ms", 2000);
        match (selector, members) {
            (Some((selector, _)), Some(members)) => {
                scans.insert(
                    name.clone(),
                    ScanList {
                        selector,
                        members,
                        priority,
                        dwell_ms,
                        settle_ms,
                        resume_delay_ms,
                    },
                );
            }
            (selector, members) => {
                if let Some(partial) = partial_site_scans.as_deref_mut() {
                    partial.insert(
                        name.clone(),
                        PartialSiteScan {
                            selector: selector.map(|(s, _)| s),
                            members,
                            priority,
                            dwell_ms,
                            settle_ms,
                            resume_delay_ms,
                        },
                    );
                }
            }
        }
    }
    scans
}
