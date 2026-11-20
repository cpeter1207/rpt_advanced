//! Translate resolved modular controls to the released USBRadioPlus and radio SDKs.
use super::{PreparationError, Result, abi};
use rpt_advanced_core::config::resources::profiles::radio::*;
use serde_json::json;
use std::{mem::size_of, ptr};

fn gain(db: f64) -> f32 {
    10.0f32.powf(db as f32 / 20.0)
}
fn route(r: OutputRoute) -> u32 {
    match r {
        OutputRoute::Disabled => 0,
        OutputRoute::Voice => 1,
        OutputRoute::Tone => 2,
        OutputRoute::Composite => 3,
        OutputRoute::AuxiliaryVoice => 4,
    }
}
fn rejected(field: &'static str) -> PreparationError {
    PreparationError::Unsupported(field)
}
pub(super) fn radio(p: &RadioPortProfile) -> Result<abi::rptadv_radio_session_config> {
    // SAFETY: this C record contains only integer and IEEE floating-point scalars and arrays.
    let mut r: abi::rptadv_radio_session_config = unsafe { std::mem::zeroed() };
    r.struct_size = size_of::<abi::rptadv_radio_session_config>() as u32;
    r.abi_version = 5;
    // The lifecycle replaces generation and frame limits with its admitted arguments.
    r.native_sample_rate_hz = 48000;
    r.interleaved_channels = 2;
    r.publication_interval_ms = p.general.status_snapshot_interval_ms;
    r.receive_input_gain = if p.rx_audio.enabled {
        gain(p.rx_audio.input_gain_db)
    } else {
        0.0
    };
    let rx = &p.signals.rx;
    let tx = &p.signals.tx;
    let sq = &rx.squelch;
    // The native provider reduces the raw port bits and maps an offline port to false.
    // Inverting afterward loses both per-pin polarity and the independent health gate.
    for (enabled, binding) in [
        (sq.enabled, &sq.binding),
        (rx.ctcss.enabled, &rx.ctcss.binding),
    ] {
        if enabled && binding.source == SignalSource::Parallel && binding.inverted {
            return Err(rejected("inverted_parallel_input"));
        }
    }
    r.receive.noise_filter_profile = u32::from(sq.noise_filter == NoiseFilter::Alternate);
    r.receive.squelch_open_level = (999 - sq.level) * 32767 / 1000;
    r.receive.squelch_hysteresis = sq.hysteresis;
    r.receive.ctcss_decoder_gain = gain(rx.ctcss.gain_db);
    r.receive.vad_mode = if !rx.vad_enabled {
        0
    } else {
        match rx.vad_mode {
            VadMode::Kerchunk => 1,
            VadMode::Continuous => 2,
        }
    };
    r.receive.vad_hang_ms = rx.vad_hang_ms;
    r.receive.vad_external_delay = 1;
    r.qualification.carrier_source = if !sq.enabled {
        8
    } else {
        match (sq.binding.source, sq.binding.inverted) {
            (SignalSource::Dsp, false) => 1,
            (SignalSource::SoundChipGpio, false) => 3,
            (SignalSource::SoundChipGpio, true) => 4,
            (SignalSource::Parallel, false) => 5,
            _ => return Err(rejected("carrier_source")),
        }
    };
    r.qualification.advanced_transport = 1;
    r.qualification.radio_duplex = u32::from(p.general.full_duplex());
    r.qualification.rx_on_delay_blocks = rx.on_delay_blocks;
    r.qualification.tx_off_delay_blocks = tx.off_delay_blocks;
    let tone = &rx.ctcss;
    r.qualification.subaudible_override = u32::from(tone.override_enabled);
    if tone.enabled {
        r.qualification.subaudible_source = match (tone.binding.source, tone.binding.inverted) {
            (SignalSource::Dsp, false) => 3,
            (SignalSource::SoundChipGpio, false) => 1,
            (SignalSource::SoundChipGpio, true) => 2,
            (SignalSource::Parallel, false) => 4,
            _ => return Err(rejected("receive_source")),
        };
        if tone.binding.source == SignalSource::Dsp {
            r.receive.ctcss_enabled = 1;
            r.receive.ctcss_relax = u32::from(tone.relaxed);
            for &hz in &tone.tones.ordered {
                r.receive.ctcss_tone_mask |=
                    1 << Tones::native_index(hz).ok_or_else(|| rejected("tones_hz"))?;
            }
        }
    } else if rx.dcs.enabled {
        if rx.dcs.source != SignalSource::Dsp {
            return Err(rejected("receive_source"));
        }
        r.qualification.subaudible_source = 3;
        r.receive.dcs_enabled = 1;
        r.receive.dcs_code = i32::from(rx.dcs.code.code);
        r.receive.dcs_inverted = u32::from(rx.dcs.code.inverted);
    }
    let t = &mut r.transmit;
    t.tx_settle_time_ms = tx.settle_ms as i32;
    t.receiver_blanking_ms = tx.rx_blanking_ms as i32;
    t.output_a_route = route(p.tx_audio.routes[0]);
    t.output_b_route = route(p.tx_audio.routes[1]);
    t.output_a_tone_gain = 1.0;
    t.output_b_tone_gain = 1.0;
    t.tone_off_mode = match tx.ctcss.turnoff {
        TurnoffMode::None => 0,
        TurnoffMode::PhaseShift => 1,
        TurnoffMode::ToneRemove => 2,
        TurnoffMode::TailTone => 3,
    };
    let tone = &tx.ctcss;
    if tone.enabled && tone.source == SignalSource::Dsp {
        t.ctcss_transmit_enabled = 1;
        t.default_ctcss_frequency_tenths_hz = i32::from(tone.tones.frequency);
        t.ctcss_peak = gain(tone.level_dbfs);
        t.ctcss_turnoff_duration_ms = tone.duration_ms as i32;
        t.ctcss_turnoff_phase_shift_degrees = tone.phase_shift_degrees;
        t.ctcss_turnoff_tail_tone_hz = tone.tail_tone_hz;
        for (&rx, &tx) in rx.ctcss.tones.ordered.iter().zip(&tone.tones.ordered) {
            t.mapped_ctcss_frequency_tenths_hz
                [Tones::native_index(rx).ok_or_else(|| rejected("tones_hz"))?] = i32::from(tx);
        }
    } else if tx.dcs.enabled && tx.dcs.source == SignalSource::Dsp {
        t.dcs_transmit_enabled = 1;
        t.dcs_code = i32::from(tx.dcs.code.code);
        t.dcs_inverted = u32::from(tx.dcs.code.inverted);
        t.dcs_peak = gain(tx.dcs.level_dbfs);
        t.dcs_turnoff_enabled = u32::from(tx.dcs.turnoff_enabled);
        t.dcs_turnoff_duration_ms = tx.dcs.duration_ms as i32;
    }
    Ok(r)
}
/// Construct the borrowed ABI record and consume all spans synchronously in the callback.
pub(super) fn with_request<T>(
    p: &RadioPortProfile,
    prefix: &str,
    call: impl FnOnce(&abi::UrpTypedPreparation) -> Result<T>,
) -> Result<T> {
    if p.interface.audio.backend == AudioBackend::Alsa && !p.remote.enabled {
        return Err(PreparationError::Unsupported("audio_backend"));
    }
    if !p.remote.enabled {
        // The ordinary product keys USB and parallel together. Exclusive parallel or rig PTT
        // needs the released remote owner until the ordinary physical capability adds selection.
        if p.signals.tx.ptt.source != SignalSource::SoundChipGpio {
            return Err(PreparationError::Unsupported("ptt_source"));
        }
        if (p.signals.tx.ctcss.enabled && p.signals.tx.ctcss.source != SignalSource::Dsp)
            || (p.signals.tx.dcs.enabled && p.signals.tx.dcs.source != SignalSource::Dsp)
        {
            return Err(PreparationError::Unsupported("encode_source"));
        }
    }
    let radio = radio(p)?;
    let a = &p.interface.audio;
    let g = &p.interface.gpio;
    // SAFETY: all-zero is valid for the C integer, float and pointer fields. Optional graph,
    // processing selector and unselected gain spans remain empty by ABI contract.
    let mut s: abi::UrpNativeStationConfig = unsafe { std::mem::zeroed() };
    s.struct_size = size_of::<abi::UrpNativeStationConfig>() as u32;
    s.abi_version = abi::URP_NATIVE_CONFIG_ABI_VERSION;
    s.radio = &radio;
    s.device_selection = u32::from(a.selection == DeviceSelection::Automatic);
    let span = |text: &str| -> Result<(*const u8, u32)> {
        Ok((
            if text.is_empty() {
                ptr::null()
            } else {
                text.as_ptr()
            },
            u32::try_from(text.len()).map_err(|_| PreparationError::Limit)?,
        ))
    };
    (s.device_identifier, s.device_identifier_length) = span(&a.device_identifier)?;
    (s.usb_serial, s.usb_serial_length) = span(&a.usb_serial)?;
    s.input_device_channels = a.input_device_channels;
    s.output_device_channels = a.output_device_channels;
    s.input_extra_buffer_ms = a.input_extra_buffer_ms;
    s.output_extra_buffer_ms = a.output_extra_buffer_ms;
    s.cm119_profile = match g.wiring_profile {
        WiringProfile::DudeUsb => 0,
        WiringProfile::SphUsb => 1,
        WiringProfile::Nhrc => 2,
        WiringProfile::Custom => 3,
    };
    s.ptt_inverted = u32::from(p.signals.tx.ptt.inverted);
    for (i, mode) in g.pins.iter().enumerate() {
        if *mode != GpioMode::Input {
            s.gpio_output_enable_mask |= 1 << i
        }
        if *mode == GpioMode::High {
            s.gpio_output_initial_mask |= 1 << i
        }
    }
    s.clip_led_mask = g.clip_led_pin.map_or(0, |n| 1 << (n - 1));
    s.hardware_gain_mask = u32::from(a.input_gain_db.is_some())
        | u32::from(a.output_a_gain_db.is_some()) << 1
        | u32::from(a.output_b_gain_db.is_some()) << 2;
    s.hardware_input_gain_db = a.input_gain_db.unwrap_or(0.0);
    s.hardware_output_a_gain_db = a.output_a_gain_db.unwrap_or(0.0);
    s.hardware_output_b_gain_db = a.output_b_gain_db.unwrap_or(0.0);
    s.receive_deemphasis = u32::from(p.rx_audio.deemphasis.enabled);
    s.receive_deemphasis_corner_hz = p.rx_audio.deemphasis.corner_hz;
    s.receive_output_gain_db = p.rx_audio.output_gain_db as i32;
    s.transmit_preemphasis = u32::from(p.tx_audio.preemphasis.enabled);
    s.transmit_preemphasis_corner_hz = p.tx_audio.preemphasis.corner_hz;
    let settings = settings(p, prefix)?;
    let req = abi::UrpTypedPreparation {
        struct_size: size_of::<abi::UrpTypedPreparation>() as u32,
        abi_version: 1,
        station: &s,
        settings: settings.as_ptr(),
        settings_length: settings.len() as u32,
    };
    call(&req)
}
fn settings(p: &RadioPortProfile, prefix: &str) -> Result<Vec<u8>> {
    let rx = &p.signals.rx;
    let tx = &p.signals.tx;
    let par = &p.interface.parallel;
    let output = par.outputs.map(|o| match o {
        ParallelOutput::Unassigned => None,
        ParallelOutput::High => Some("high"),
        ParallelOutput::Low => Some("low"),
        ParallelOutput::PushToTalk if p.signals.tx.ptt.source == SignalSource::Parallel => {
            Some("push_to_talk")
        }
        ParallelOutput::PushToTalk => None,
    });
    let input = par.inputs.map(|i| match i {
        ParallelInput::Unassigned => None,
        ParallelInput::Carrier => Some("carrier"),
        ParallelInput::Ctcss => Some("ctcss"),
        ParallelInput::Input => Some("input"),
    });
    let code =
        |c: DigitalCode| json!({"value":c.code,"polarity":if c.inverted{"inverted"}else{"normal"}});
    let mut root = serde_json::to_value(&p.processing).map_err(|_| PreparationError::Limit)?;
    root["station"] = json!({
        "channel_enabled":p.metadata.enabled,"eeprom_enabled":p.interface.audio.eeprom_enabled,
        "parallel_port":{"device":par.device,"base_address":par.base_address,"output_assignments":output,"input_assignments":input},
        "receive":{"squelch_delay_ms":rx.squelch_delay_ms},
        "ctcss":{"receive_frequencies":rx.ctcss.tones.ordered,"transmit_frequencies":tx.ctcss.tones.ordered,
            "receive_relax":u32::from(rx.ctcss.relaxed),"transmit_default":tx.ctcss.tones.frequency,
            "transmit_peak_dbfs":tx.ctcss.level_dbfs,"phase_shift_degrees":tx.ctcss.phase_shift_degrees,
            "tail_duration_ms":tx.ctcss.duration_ms,"tail_frequency_hz":tx.ctcss.tail_tone_hz},
        "dcs":{"receive_code":code(rx.dcs.code),"transmit_code":code(tx.dcs.code),"turnoff_code_enabled":tx.dcs.turnoff_enabled,
            "turnoff_duration_ms":tx.dcs.duration_ms,"peak_dbfs":tx.dcs.level_dbfs},
        "diagnostics":{"status_publication_interval_ms":p.general.status_snapshot_interval_ms}
    });
    if let Some(remote) = p.remote_settings(prefix) {
        root["remote"] = serde_json::to_value(remote).map_err(|_| PreparationError::Limit)?;
    }
    let bytes = serde_json::to_vec(&root).map_err(|_| PreparationError::Limit)?;
    if bytes.len() > 1_048_576 {
        Err(PreparationError::Limit)
    } else {
        Ok(bytes)
    }
}
