//! Canonical HAL fields from the admitted radio domain, independent of provider/SDK bodies.
use super::{Fault, Kind, Projections, Scope, Value, json};
use rpt_advanced_core::config::resources::profiles::radio::*;
use serde_json::Map;

fn decimal(value: u64, places: u32) -> Value {
    let scale = 10u64.pow(places);
    let whole = value / scale;
    if value.is_multiple_of(scale) {
        return json!(whole);
    }
    let text = format!("{whole}.{:0width$}", value % scale, width = places as usize);
    Value::Number(text.trim_end_matches('0').parse().expect("exact decimal"))
}
fn mhz(hz: u64) -> Value {
    decimal(hz, 6)
}
fn hz(tenths: u16) -> Value {
    decimal(u64::from(tenths), 1)
}
fn insert(obj: &mut Value, key: &str, value: Option<Value>) {
    if let Some(v) = value {
        obj[key] = v;
    }
}
fn digital(d: DigitalCode) -> String {
    format!("{:03o}{}", d.code, if d.inverted { 'I' } else { 'N' })
}
fn binding(b: &SignalBinding) -> Value {
    let mut obj = json!({"source":b.source.as_str(),"inverted":b.inverted});
    if !b.pins.is_empty() {
        obj["pin"] = json!(b.pins);
    }
    obj
}
fn rx_signals(s: &RxSignals) -> Value {
    let mut squelch = binding(&s.squelch.binding);
    squelch["enabled"] = json!(s.squelch.enabled);
    squelch["level"] = json!(s.squelch.level);
    squelch["hysteresis"] = json!(s.squelch.hysteresis);
    squelch["noise_filter"] = json!(s.squelch.noise_filter.as_str());
    let mut ctcss = binding(&s.ctcss.binding);
    ctcss["enabled"] = json!(s.ctcss.enabled);
    ctcss["frequency_hz"] = hz(s.ctcss.tones.frequency);
    ctcss["tones_hz"] = json!(
        s.ctcss
            .tones
            .ordered
            .iter()
            .copied()
            .map(hz)
            .collect::<Vec<_>>()
    );
    ctcss["gain_db"] = json!(s.ctcss.gain_db);
    ctcss["relaxed"] = json!(s.ctcss.relaxed);
    ctcss["override"] = json!(s.ctcss.override_enabled);
    json!({"squelch":squelch,"ctcss":ctcss,"vad":{"enabled":s.vad_enabled,"mode":s.vad_mode.as_str(),"timing":{"hang_ms":s.vad_hang_ms}},
        "dcs":{"enabled":s.dcs.enabled,"source":s.dcs.source.as_str(),"code":digital(s.dcs.code)},
        "timing":{"on_delay_ms":s.on_delay_ms,"squelch_delay_ms":s.squelch_delay_ms,"kerchunk_max_ms":s.kerchunk_max_ms}})
}
fn tx_signals(s: &TxSignals) -> Value {
    let mut ptt = binding(&s.ptt);
    ptt["timing"] = json!({"settle_ms":s.settle_ms,"off_delay_ms":s.off_delay_ms,"hang_ms":s.hang_ms,"timeout_ms":s.timeout_ms,"rx_blanking_ms":s.rx_blanking_ms});
    let c = &s.ctcss;
    let mut ctcss = json!({"enabled":c.enabled,"source":c.source.as_str(),"frequency_hz":hz(c.tones.frequency),
        "tones_hz":c.tones.ordered.iter().copied().map(hz).collect::<Vec<_>>(),"level_dbfs":c.level_dbfs,"on_input":c.on_input,"timing":{"hang_ms":c.hang_ms},
        "turnoff":{"mode":c.turnoff.as_str(),"phase_shift_degrees":c.phase_shift_degrees,"tail_tone_hz":c.tail_tone_hz,"timing":{"duration_ms":c.duration_ms}}});
    if c.source == SignalSource::External {
        insert(&mut ctcss, "output", c.output.as_ref().map(binding));
    }
    let d = &s.dcs;
    let mut dcs = json!({"enabled":d.enabled,"source":d.source.as_str(),"code":digital(d.code),"level_dbfs":d.level_dbfs,
        "turnoff":{"enabled":d.turnoff_enabled,"timing":{"duration_ms":d.duration_ms}}});
    if d.source == SignalSource::External {
        insert(&mut dcs, "output", d.output.as_ref().map(binding));
    }
    json!({"ptt":ptt,"ctcss":ctcss,"dcs":dcs})
}
fn interface(i: &Interface) -> Value {
    let a = &i.audio;
    let mut audio = json!({"backend":a.backend.as_str(),"device_selection":a.selection.as_str(),"device_identifier":a.device_identifier,"usb_serial":a.usb_serial,
        "input_device_channels":a.input_device_channels,"output_device_channels":a.output_device_channels,"eeprom_enabled":a.eeprom_enabled,
        "timing":{"input_extra_buffer_ms":a.input_extra_buffer_ms,"output_extra_buffer_ms":a.output_extra_buffer_ms}});
    for (name, value) in [
        ("input_gain_db", a.input_gain_db),
        ("output_a_gain_db", a.output_a_gain_db),
        ("output_b_gain_db", a.output_b_gain_db),
    ] {
        insert(&mut audio, name, value.map(Value::from));
    }
    let mut gpio = json!({"wiring_profile":i.gpio.wiring_profile.as_str(),"pins":i.gpio.pins.iter().enumerate().map(|(n,m)|((n+1).to_string(),json!(m.as_str()))).collect::<Map<_,_>>()});
    insert(
        &mut gpio,
        "clip_led_pin",
        i.gpio.clip_led_pin.map(Value::from),
    );
    let serial = &i.serial;
    let mut s = json!({"baud_rate":serial.baud_rate,"data_bits":serial.data_bits,"parity":serial.parity.as_str(),"stop_bits":serial.stop_bits,"rtscts":false,"dsrdtr":false,"xonxoff":false});
    insert(&mut s, "device", serial.device.as_deref().map(Value::from));
    let p = &i.parallel;
    let outputs = (2..=9)
        .zip(p.outputs)
        .map(|(k, v)| (k.to_string(), json!(v.as_str())))
        .collect::<Map<_, _>>();
    let inputs = [10, 12, 13, 15]
        .into_iter()
        .zip(p.inputs)
        .map(|(k, v)| (k.to_string(), json!(v.as_str())))
        .collect::<Map<_, _>>();
    json!({"audio":audio,"gpio":gpio,"serial":s,"parallel":{"device":p.device,"base_address":p.base_address,"outputs":outputs,"inputs":inputs}})
}
fn memory(m: &Memory) -> Value {
    let tone = |t: &MemoryTone| json!({"enabled":t.enabled,"frequency":hz(t.tenths_hz)});
    json!({"frequency":mhz(m.receive_hz),"offset":{"enabled":m.offset_enabled,"direction":if m.offset_minus{"minus"}else{"plus"},"frequency":mhz(m.offset_magnitude_hz)},
        "mode":match m.mode{MemoryMode::Am=>"AM",MemoryMode::Fm=>"FM",MemoryMode::Usb=>"USB",MemoryMode::Lsb=>"LSB"},
        "rx_ctcss":tone(&m.rx_ctcss),"tx_ctcss":tone(&m.tx_ctcss)})
}
fn scan(
    selector: Option<u32>,
    members: Option<&[u32]>,
    priority: Option<u32>,
    dwell: u64,
    settle: u64,
    resume: u64,
) -> Value {
    let mut s = json!({"timing":{"dwell_ms":dwell,"settle_ms":settle,"resume_delay_ms":resume}});
    insert(&mut s, "selector", selector.map(Value::from));
    insert(&mut s, "priority", priority.map(Value::from));
    insert(&mut s, "members", members.map(|v| json!(v)));
    s
}
fn general(s: &RadioGeneral) -> Value {
    let mut j = json!({"duplex_mode":s.duplex_mode.as_str(),"parrot":{"enabled":s.parrot_enabled},"timing":{"status_snapshot_interval_ms":s.status_snapshot_interval_ms,"tune_duration_ms":s.tune_duration_ms}});
    for (key, value) in ["scan_slow_ms", "scan_medium_ms", "scan_fast_ms"]
        .into_iter()
        .zip(s.scan_presets_ms)
    {
        insert(&mut j["timing"], key, value.map(Value::from));
    }
    insert(&mut j, "startup_memory", s.startup_memory.map(Value::from));
    insert(
        &mut j,
        "offset_magnitude_mhz",
        s.offset_magnitude_hz.map(mhz),
    );
    j
}
fn control(s: &RemotePolicy) -> Value {
    let mut hamlib = json!({"options":s.authored_hamlib_options().map(|(k,v)|(k.clone(),json!(v))).collect::<Map<_,_>>()});
    insert(&mut hamlib, "device", s.hamlib_device().map(Value::from));
    insert(
        &mut hamlib,
        "model",
        s.hamlib_model.as_deref().map(Value::from),
    );
    let mut j = json!({"enabled":s.enabled,"hamlib":hamlib,"access":s.access.as_str(),"retries":s.retries,"timeout_retries":s.timeout_retries,"timing":{"timeout_ms":s.timeout_ms,"poll_interval_ms":s.poll_interval_ms}});
    insert(&mut j, "backend", s.backend.map(|b| json!(b.as_str())));
    j
}
pub(super) fn scope(
    out: &mut Projections,
    scope: Scope,
    profiles: &ResolvedRadioProfiles,
) -> Result<(), Fault> {
    let settings = match scope {
        Scope::Site(_) => profiles.site(),
        Scope::Port(id) => &profiles.port(id).ok_or(Fault::Invalid)?.settings,
    };
    let rx = &settings.rx_audio;
    let tx = &settings.tx_audio;
    let mut rx_audio = json!({"enabled":rx.enabled,"input_gain_db":rx.input_gain_db,"output_gain_db":rx.output_gain_db,"deemphasis":{"enabled":rx.deemphasis.enabled,"corner_hz":rx.deemphasis.corner_hz}});
    insert(
        &mut rx_audio,
        "processing",
        settings
            .processing
            .local
            .as_ref()
            .map(ProcessingProfile::public_fields),
    );
    let mut tx_audio = json!({"enabled":tx.enabled,"preemphasis":{"enabled":tx.preemphasis.enabled,"corner_hz":tx.preemphasis.corner_hz},"output_a_assignment":tx.routes[0].as_str(),"output_b_assignment":tx.routes[1].as_str()});
    insert(
        &mut tx_audio,
        "processing",
        settings
            .processing
            .voice_telemetry
            .as_ref()
            .map(ProcessingProfile::public_fields),
    );
    let mut scans = settings
        .scan_lists
        .iter()
        .map(|(name, s)| {
            (
                name.clone(),
                scan(
                    Some(s.selector),
                    Some(&s.members),
                    s.priority,
                    s.dwell_ms,
                    s.settle_ms,
                    s.resume_delay_ms,
                ),
            )
        })
        .collect::<Map<_, _>>();
    if matches!(scope, Scope::Site(_)) {
        for (name, s) in profiles.partial_site_scans() {
            scans.insert(
                name.clone(),
                scan(
                    s.selector,
                    s.members.as_deref(),
                    s.priority,
                    s.dwell_ms,
                    s.settle_ms,
                    s.resume_delay_ms,
                ),
            );
        }
    }
    let r = &settings.remote;
    let ranges = |rows: &[[u64; 2]]| rows.iter().map(|row| row.map(mhz)).collect::<Vec<_>>();
    let mut power = json!({});
    for (name, v) in ["low_w", "medium_w", "high_w"]
        .into_iter()
        .zip(r.power_presets_mw)
    {
        insert(&mut power, name, v.map(|n| decimal(u64::from(n), 3)));
    }
    insert(
        &mut power,
        "max_w",
        r.maximum_power_mw.map(|n| decimal(u64::from(n), 3)),
    );
    for (kind, value) in [
        (Kind::Radio, general(&settings.general)),
        (Kind::Interface, interface(&settings.interface)),
        (Kind::RxAudio, rx_audio),
        (Kind::TxAudio, tx_audio),
        (
            Kind::PeerAudio,
            settings
                .processing
                .link
                .as_ref()
                .map_or_else(|| json!({}), ProcessingProfile::public_fields),
        ),
        (Kind::RxSignaling, rx_signals(&settings.signals.rx)),
        (Kind::TxSignaling, tx_signals(&settings.signals.tx)),
        (
            Kind::Memories,
            json!({"entries":settings.memories.iter().map(memory).collect::<Vec<_>>()}),
        ),
        (Kind::ScanLists, Value::Object(scans)),
        (Kind::Control, control(r)),
        (Kind::Power, power),
        (
            Kind::Limits,
            json!({"modes":r.mode_names().collect::<Vec<_>>(),"rx_ranges_mhz":ranges(&r.receive_ranges),
        "tx_ranges_mhz":ranges(&r.transmit_ranges),"rx_only_ranges_mhz":ranges(&r.receive_only_ranges)}),
        ),
    ] {
        for (path, author) in profiles.processing_band_sources(scope, kind) {
            if value.pointer(path).is_some() {
                out.sources
                    .entry((scope, kind, None))
                    .or_default()
                    .insert(path.into(), super::Origin::Resource(author));
            }
        }
        out.fixed_effective.insert((scope, kind), value);
    }
    Ok(())
}
