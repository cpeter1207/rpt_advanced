//! Provider DTO assembled from effective policy and the command owner's chosen prefix.
use super::super::{
    AudioBackend, Memory, MemoryMode, MemoryTone, RadioPortProfile, RemoteAccess, RemoteBackend,
    ScanList, SignalSource,
};
use serde::Serialize;
use std::collections::BTreeMap;

/// Complete borrowed USBRadioPlus remote settings. Serialization preserves exact option values.
#[derive(Serialize)]
pub struct RemoteSettings<'a> {
    backend: &'static str,
    audio_source: &'static str,
    hamlib_model: &'a str,
    hamlib_options: &'a BTreeMap<String, String>,
    timeout_ms: u32,
    retries: u32,
    timeout_retries: u32,
    poll_interval_ms: u64,
    ptt_source: Option<&'static str>,
    cor_source: Option<&'static str>,
    external_encode_output: Option<EncoderOutput>,
    external_encode_inverted: bool,
    receive_ranges: &'a [[u64; 2]],
    transmit_ranges: &'a [[u64; 2]],
    receive_only_ranges: &'a [[u64; 2]],
    modes: &'a [u32],
    maximum_power_mw: Option<u32>,
    memories: Vec<MemoryRecord>,
    scan_lists: Vec<Scan<'a>>,
    startup_memory: u32,
    dtmf_access: &'static str,
    dtmf_prefix: &'a str,
    offset_magnitude_hz: Option<u64>,
    power_presets_mw: [Option<u32>; 3],
    scan_presets_ms: [Option<u64>; 3],
    tune_duration_ms: u32,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum EncoderOutput {
    Usb(u8),
    Parallel(u8),
}
#[derive(Serialize)]
struct Scan<'a> {
    selector: u32,
    name: &'a str,
    members: &'a [u32],
    dwell_ms: u64,
    resume_delay_ms: u64,
    settle_ms: u64,
    priority: Option<u32>,
}
impl<'a> Scan<'a> {
    fn from(key: &'a str, value: &'a ScanList) -> Self {
        Self {
            selector: value.selector,
            name: key,
            members: &value.members,
            dwell_ms: value.dwell_ms,
            resume_delay_ms: value.resume_delay_ms,
            settle_ms: value.settle_ms,
            priority: value.priority,
        }
    }
}
#[derive(Serialize)]
struct Signal {
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    control_source: Option<&'static str>,
    enabled: bool,
    tone_tenths_hz: u16,
    dcs_code: u16,
    inverted: bool,
}
impl Signal {
    fn from(tone: MemoryTone, source: SignalSource) -> Self {
        let source = match source {
            SignalSource::Dsp => "dsp",
            SignalSource::Hamlib => "rig",
            SignalSource::Auto => "automatic",
            SignalSource::SoundChipGpio | SignalSource::Parallel | SignalSource::External => {
                "external"
            }
            SignalSource::Serial | SignalSource::KeyDown => {
                unreachable!("unsupported physical binding normalized")
            }
        };
        Self {
            source,
            control_source: None,
            enabled: tone.enabled,
            tone_tenths_hz: tone.tenths_hz,
            dcs_code: 0,
            inverted: false,
        }
    }
}
#[derive(Serialize)]
struct State {
    receive_hz: u64,
    transmit_hz: Option<u64>,
    mode: u32,
    bandwidth_hz: u32,
    receive_vfo: u32,
    transmit_vfo: u32,
    split: bool,
    offset_hz: i64,
    rit_hz: i64,
    rit_enabled: bool,
    xit_hz: i64,
    xit_enabled: bool,
    power_mw: Option<u32>,
    encode: Signal,
    decode: Signal,
    channel: Option<u8>,
}
#[derive(Serialize)]
struct MemoryRecord {
    selector: u32,
    name: String,
    state: State,
}
impl MemoryRecord {
    fn from(m: &Memory, p: &RadioPortProfile) -> Self {
        let mode = match m.mode {
            MemoryMode::Fm => 1,
            MemoryMode::Am => 2,
            MemoryMode::Usb => 3,
            MemoryMode::Lsb => 4,
        };
        Self {
            selector: m.selector,
            name: m.selector.to_string(),
            state: State {
                receive_hz: m.receive_hz,
                transmit_hz: Some(m.receive_hz),
                offset_hz: m.offset_hz,
                mode,
                // The RF engine adds the signed shift once to the base TX VFO.
                bandwidth_hz: 0,
                receive_vfo: 0,
                transmit_vfo: 0,
                split: false,
                rit_hz: 0,
                rit_enabled: false,
                xit_hz: 0,
                xit_enabled: false,
                power_mw: None,
                channel: None,
                encode: Signal::from(m.tx_ctcss, p.signals.tx.ctcss.source),
                decode: Signal::from(m.rx_ctcss, p.signals.rx.ctcss.binding.source),
            },
        }
    }
}
impl RadioPortProfile {
    /// Project a complete, enabled remote policy using the command owner's selected DTMF prefix.
    pub fn remote_settings<'a>(&'a self, dtmf_prefix: &'a str) -> Option<RemoteSettings<'a>> {
        let r = &self.remote;
        if !r.enabled {
            return None;
        }
        let backend = match r.backend? {
            RemoteBackend::Hamlib => "hamlib",
            RemoteBackend::ParallelChannel => "parallel_channel",
            RemoteBackend::ParallelRtx => "parallel_rtx",
        };
        let ptt_source = match self.signals.tx.ptt.source {
            SignalSource::Hamlib => "hamlib",
            SignalSource::Parallel => "parallel",
            SignalSource::SoundChipGpio => "usb",
            _ => unreachable!("normalized PTT"),
        };
        let squelch = &self.signals.rx.squelch;
        let cor_source = if squelch.enabled {
            match squelch.binding.source {
                SignalSource::Dsp => "dsp",
                SignalSource::Parallel => "parallel",
                SignalSource::SoundChipGpio => "usb",
                _ => unreachable!("normalized carrier"),
            }
        } else {
            "open"
        };
        let tx = &self.signals.tx;
        let external = if tx.ctcss.enabled && tx.ctcss.source == SignalSource::External {
            tx.ctcss.output.as_ref()
        } else if tx.dcs.enabled && tx.dcs.source == SignalSource::External {
            tx.dcs.output.as_ref()
        } else {
            None
        };
        let external_encode_output = external.and_then(|b| match (b.source, b.pins.as_slice()) {
            (SignalSource::SoundChipGpio, [n]) => Some(EncoderOutput::Usb(*n)),
            (SignalSource::Parallel, [n]) => Some(EncoderOutput::Parallel(*n)),
            _ => None,
        });
        Some(RemoteSettings {
            backend,
            audio_source: match self.interface.audio.backend {
                AudioBackend::SoundChip => "cm119",
                AudioBackend::Alsa => "alsa",
            },
            hamlib_model: r.hamlib_model.as_deref().unwrap_or(""),
            hamlib_options: &r.hamlib_options,
            timeout_ms: r.timeout_ms,
            retries: r.retries,
            timeout_retries: r.timeout_retries,
            poll_interval_ms: r.poll_interval_ms,
            ptt_source: Some(ptt_source),
            cor_source: Some(cor_source),
            external_encode_output,
            external_encode_inverted: external.is_some_and(|b| b.inverted),
            receive_ranges: &r.receive_ranges,
            transmit_ranges: &r.transmit_ranges,
            receive_only_ranges: &r.receive_only_ranges,
            modes: &r.modes,
            maximum_power_mw: r.maximum_power_mw,
            memories: self
                .memories
                .iter()
                .map(|m| MemoryRecord::from(m, self))
                .collect(),
            scan_lists: self
                .scan_lists
                .iter()
                .map(|(n, x)| Scan::from(n, x))
                .collect(),
            startup_memory: self.general.startup_memory?,
            dtmf_access: match r.access {
                RemoteAccess::Open => "open",
                RemoteAccess::Locked => "locked",
                RemoteAccess::Disabled => "disabled",
            },
            dtmf_prefix,
            offset_magnitude_hz: self.general.offset_magnitude_hz,
            power_presets_mw: r.power_presets_mw,
            scan_presets_ms: self.general.scan_presets_ms,
            tune_duration_ms: self.general.tune_duration_ms,
        })
    }
}
