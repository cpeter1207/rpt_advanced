//! Translate resolved node policy to the released native radio-session ABI.

use crate::abi;
use rpt_advanced_core::config::{
    CtcssTurnoffMode, RadioCarrierSource, RadioDuplexMode, RadioNoiseFilter, RadioOutputAssignment,
    RadioReceiveAudioSource, RadioSignalingMode, RadioSubaudibleSource, ResolvedRadioSettings,
};
use std::mem::size_of;

const CTCSS_TONES_TENTHS_HZ: [u16; 38] = [
    670, 719, 744, 770, 797, 825, 854, 885, 915, 948, 974, 1000, 1035, 1072, 1109, 1148, 1188,
    1230, 1273, 1318, 1365, 1413, 1462, 1514, 1567, 1622, 1679, 1738, 1799, 1862, 1928, 2035, 2107,
    2181, 2257, 2336, 2418, 2503,
];

/// Build a native radio-core configuration from one resolved channel.
pub fn radio_session_config(
    radio: &ResolvedRadioSettings,
    generation_id: u64,
    maximum_frames: u32,
    publication_interval_ms: u32,
) -> Result<abi::rptadv_radio_session_config, RadioConfigError> {
    if generation_id == 0 || maximum_frames == 0 {
        return Err(RadioConfigError::InvalidBounds);
    }
    let settings = &radio.signaling;
    let receive_ctcss = settings.receive_mode == RadioSignalingMode::Ctcss
        && settings.receive_subaudible_source == RadioSubaudibleSource::Dsp;
    let receive_dcs = settings.receive_mode == RadioSignalingMode::Dcs
        && settings.receive_subaudible_source == RadioSubaudibleSource::Dsp;
    let mut ctcss_tone_mask = 0_u64;
    if receive_ctcss {
        for tone in &settings.receive_ctcss_tones_tenths_hz {
            ctcss_tone_mask |= 1_u64 << tone_index(*tone)?;
        }
    }
    let mut mapped_ctcss = [0_i32; 38];
    if settings.transmit_mode == RadioSignalingMode::Ctcss {
        for tone in &settings.transmit_ctcss_tones_tenths_hz {
            mapped_ctcss[tone_index(*tone)?] = i32::from(*tone);
        }
    }
    Ok(abi::rptadv_radio_session_config {
        struct_size: size_of::<abi::rptadv_radio_session_config>() as u32,
        abi_version: 4,
        generation_id,
        native_sample_rate_hz: 48_000,
        interleaved_channels: 2,
        maximum_receive_frame_count: maximum_frames,
        maximum_transmit_frame_count: maximum_frames,
        publication_interval_ms,
        receive_channel: 0,
        receive_input_gain: if settings.receive_audio_source == RadioReceiveAudioSource::Disabled {
            0.0
        } else {
            db_to_linear(radio.receive_input_gain_db)
        },
        receive: abi::rptadv_radio_receive_config {
            noise_filter_profile: match settings.noise_filter {
                RadioNoiseFilter::Standard => 0,
                RadioNoiseFilter::Alternate => 1,
            },
            squelch_open_level: (u32::from(999 - settings.squelch_level) * 32_767) / 1_000,
            squelch_hysteresis: u32::from(settings.squelch_hysteresis),
            ctcss_decoder_gain: db_to_linear(settings.receive_ctcss_decoder_gain_db),
            vox_threshold: i32::from(settings.vox_threshold),
            vox_hang_ms: settings.vox_hang_ms as i32,
            ctcss_enabled: u32::from(receive_ctcss),
            ctcss_tone_mask,
            ctcss_relax: u32::from(settings.receive_ctcss_relaxed),
            dcs_enabled: u32::from(receive_dcs),
            dcs_code: i32::from(settings.receive_dcs_code.value),
            dcs_inverted: u32::from(settings.receive_dcs_code.inverted),
            cpu_saver_enabled: 0,
            native_squelch_delay_frames: 0,
        },
        qualification: abi::rptadv_radio_qualification_config {
            carrier_source: carrier_source(settings.carrier_source),
            subaudible_source: if settings.receive_mode == RadioSignalingMode::Carrier {
                0
            } else {
                subaudible_source(settings.receive_subaudible_source)
            },
            subaudible_override: u32::from(settings.receive_ctcss_override),
            advanced_transport: 1,
            radio_duplex: u32::from(settings.radio_duplex_mode == RadioDuplexMode::Full),
            rx_on_delay_blocks: settings.receive_on_delay_ms.div_ceil(20),
            tx_off_delay_blocks: settings.transmit_off_delay_ms.div_ceil(20),
        },
        transmit: abi::rptadv_radio_transmit_config {
            ctcss_transmit_enabled: u32::from(settings.transmit_mode == RadioSignalingMode::Ctcss),
            default_ctcss_frequency_tenths_hz: i32::from(settings.transmit_ctcss_default_tenths_hz),
            mapped_ctcss_frequency_tenths_hz: mapped_ctcss,
            tone_off_mode: tone_off_mode(settings.transmit_ctcss_turnoff_mode),
            ctcss_turnoff_duration_ms: settings.transmit_ctcss_turnoff_duration_ms as i32,
            ctcss_turnoff_phase_shift_degrees: f64::from(
                settings.transmit_ctcss_phase_shift_degrees,
            ),
            ctcss_turnoff_tail_tone_hz: f64::from(settings.transmit_ctcss_tail_tone_hz),
            dcs_transmit_enabled: u32::from(settings.transmit_mode == RadioSignalingMode::Dcs),
            dcs_turnoff_enabled: u32::from(settings.transmit_dcs_turnoff_enabled),
            dcs_turnoff_duration_ms: settings.transmit_dcs_turnoff_duration_ms as i32,
            receiver_blanking_ms: settings.transmit_receive_blanking_ms as i32,
            tx_settle_time_ms: settings.transmit_settle_ms as i32,
            cpu_saver_enabled: 0,
            dcs_code: i32::from(settings.transmit_dcs_code.value),
            dcs_inverted: u32::from(settings.transmit_dcs_code.inverted),
            dcs_peak: db_to_linear(settings.transmit_dcs_level_dbfs),
            ctcss_peak: db_to_linear(settings.transmit_ctcss_level_dbfs),
            output_a_route: output_assignment(settings.transmit_output_a_assignment),
            output_b_route: output_assignment(settings.transmit_output_b_assignment),
            output_a_tone_gain: 1.0,
            output_a_tone_bias: 0.0,
            output_b_tone_gain: 1.0,
            output_b_tone_bias: 0.0,
        },
    })
}

fn output_assignment(assignment: RadioOutputAssignment) -> u32 {
    match assignment {
        RadioOutputAssignment::Disabled => 0,
        RadioOutputAssignment::Voice => 1,
        RadioOutputAssignment::Tone => 2,
        RadioOutputAssignment::Composite => 3,
        RadioOutputAssignment::AuxiliaryVoice => 4,
    }
}

/// Resolve the released radio core's CTCSS table index for configuration and processor ports.
pub(crate) fn tone_index(tenths_hz: u16) -> Result<usize, RadioConfigError> {
    CTCSS_TONES_TENTHS_HZ
        .iter()
        .position(|tone| *tone == tenths_hz)
        .ok_or(RadioConfigError::UnsupportedTone)
}

fn db_to_linear(db: i64) -> f32 {
    10.0_f32.powf(db as f32 / 20.0)
}

fn carrier_source(source: RadioCarrierSource) -> u32 {
    match source {
        RadioCarrierSource::Disabled => 0,
        RadioCarrierSource::Dsp => 1,
        RadioCarrierSource::Vox => 2,
        RadioCarrierSource::Cm119 => 3,
        RadioCarrierSource::Cm119Inverted => 4,
        RadioCarrierSource::Parallel => 5,
        RadioCarrierSource::ParallelInverted => 6,
    }
}

fn subaudible_source(source: RadioSubaudibleSource) -> u32 {
    match source {
        RadioSubaudibleSource::Disabled => 0,
        RadioSubaudibleSource::Cm119 => 1,
        RadioSubaudibleSource::Cm119Inverted => 2,
        RadioSubaudibleSource::Dsp => 3,
        RadioSubaudibleSource::Parallel => 4,
        RadioSubaudibleSource::ParallelInverted => 5,
    }
}

fn tone_off_mode(mode: CtcssTurnoffMode) -> u32 {
    match mode {
        CtcssTurnoffMode::None => 0,
        CtcssTurnoffMode::PhaseShift => 1,
        CtcssTurnoffMode::ToneRemove => 2,
        CtcssTurnoffMode::TailTone => 3,
    }
}

/// The native session cannot be prepared from the supplied channel policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioConfigError {
    /// A runtime generation and nonzero callback maximum are required.
    InvalidBounds,
    /// A configured CTCSS tone is not in the radio core's 38-tone table.
    UnsupportedTone,
}

impl std::fmt::Display for RadioConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBounds => "radio generation and callback bounds must be nonzero",
            Self::UnsupportedTone => "configured CTCSS tone is not supported by the radio core",
        })
    }
}

impl std::error::Error for RadioConfigError {}

#[cfg(test)]
mod tests {
    use super::{RadioConfigError, radio_session_config};
    use rpt_advanced_core::config::{ConfigDocument, NodeId, ResolvedNodeSettings};

    fn resolve(document: &ConfigDocument) -> super::ResolvedRadioSettings {
        ResolvedNodeSettings::resolve(document, &NodeId::new("1000").unwrap())
            .unwrap()
            .value
            .radio
    }

    fn map(overrides: &str) -> Result<crate::abi::rptadv_radio_session_config, RadioConfigError> {
        let document = ConfigDocument::parse(&format!("[1000]\n[radio]\n{overrides}"))
            .expect("valid radio configuration");
        let radio = resolve(&document);
        radio_session_config(&radio, 1, 960, 50)
    }

    #[test]
    fn maps_receive_input_gain_without_changing_detector_calibration() {
        for (options, expected) in [
            ("", 1.0),
            ("receive_input_gain_db=-2\n", 0.794_328_2),
            ("receive_input_gain_db=6\n", 1.995_262_3),
            ("receive_audio=disabled\nreceive_input_gain_db=6\n", 0.0),
        ] {
            let config = map(options).unwrap();
            assert!((config.receive_input_gain - expected).abs() < 0.000_001);
            assert_eq!(config.receive.ctcss_decoder_gain, 1.0);
        }
    }

    #[test]
    fn maps_every_carrier_source() {
        for (source, expected) in [
            ("disabled", 0),
            ("dsp", 1),
            ("vox", 2),
            ("cm119", 3),
            ("cm119_inverted", 4),
            ("parallel", 5),
            ("parallel_inverted", 6),
        ] {
            let config = map(&format!("carrier_source={source}\n")).unwrap();
            assert_eq!(config.qualification.carrier_source, expected);
        }
    }

    #[test]
    fn maps_every_external_subaudible_source() {
        for (source, expected) in [
            ("disabled", 0),
            ("cm119", 1),
            ("cm119_inverted", 2),
            ("dsp", 3),
            ("parallel", 4),
            ("parallel_inverted", 5),
        ] {
            let config = map(&format!(
                "receive_signaling=ctcss\nctcss_source={source}\nreceive_ctcss_tones_hz=100.0\n"
            ))
            .unwrap();
            assert_eq!(config.qualification.subaudible_source, expected);
            assert_eq!(config.receive.ctcss_enabled, u32::from(source == "dsp"));
        }
    }

    #[test]
    fn maps_every_transmit_tone_off_mode() {
        for (mode, expected) in [
            ("none", 0),
            ("phase_shift", 1),
            ("tone_remove", 2),
            ("tail_tone", 3),
        ] {
            let config = map(&format!("transmit_ctcss_turnoff_mode={mode}\n")).unwrap();
            assert_eq!(config.transmit.tone_off_mode, expected);
        }
    }

    #[test]
    fn maps_each_transmit_ctcss_tone_to_its_native_table_slot() {
        let config = map(
            "transmit_signaling=ctcss\ntransmit_ctcss_tones_hz=100.0,103.5\ntransmit_ctcss_default_hz=100.0\n",
        )
        .unwrap();

        assert_eq!(config.transmit.mapped_ctcss_frequency_tenths_hz[11], 1000);
        assert_eq!(config.transmit.mapped_ctcss_frequency_tenths_hz[12], 1035);
    }

    #[test]
    fn maps_configured_transmit_output_assignments() {
        let document = ConfigDocument::parse(
            "[radio]\ntransmit_output_b_assignment=voice\n[1000]\n[radio 1000]\ntransmit_output_a_assignment=disabled\ntransmit_output_b_assignment=composite\n",
        )
        .expect("valid radio configuration");
        let radio = resolve(&document);
        let config = radio_session_config(&radio, 1, 960, 50).unwrap();

        assert_eq!(config.transmit.output_a_route, 0);
        assert_eq!(config.transmit.output_b_route, 3);
    }

    #[test]
    fn maps_dcs_and_disabled_receive_audio() {
        let config = map(
            "receive_audio=disabled\nreceive_signaling=dcs\ndcs_receive_code=023N\
             \ntransmit_signaling=dcs\ntransmit_dcs_code=047I\nnoise_filter=alternate\nradio_duplex_mode=full\n",
        )
        .unwrap();
        assert_eq!(config.receive_input_gain, 0.0);
        assert_eq!(config.receive.dcs_enabled, 1);
        assert_eq!(config.receive.dcs_code, 0o23);
        assert_eq!(config.receive.dcs_inverted, 0);
        assert_eq!(config.receive.noise_filter_profile, 1);
        assert_eq!(config.qualification.radio_duplex, 1);
        assert_eq!(config.transmit.dcs_transmit_enabled, 1);
        assert_eq!(config.transmit.dcs_code, 0o47);
        assert_eq!(config.transmit.dcs_inverted, 1);
    }

    #[test]
    fn rejects_zero_generation_or_frame_bound() {
        let document = ConfigDocument::parse("[1000]\n[radio]\n").unwrap();
        let radio = resolve(&document);
        assert!(matches!(
            radio_session_config(&radio, 0, 960, 50),
            Err(RadioConfigError::InvalidBounds)
        ));
        assert!(matches!(
            radio_session_config(&radio, 1, 0, 50),
            Err(RadioConfigError::InvalidBounds)
        ));
    }

    #[test]
    fn rejects_tones_outside_the_radio_core_table() {
        let document = ConfigDocument::parse("[1000]\n[radio]\nreceive_signaling=ctcss\n").unwrap();
        let mut radio = resolve(&document);
        radio.signaling.receive_ctcss_tones_tenths_hz.push(1010);
        assert!(matches!(
            radio_session_config(&radio, 1, 960, 50),
            Err(RadioConfigError::UnsupportedTone)
        ));
    }

    #[test]
    fn configuration_errors_have_actionable_messages() {
        assert_eq!(
            RadioConfigError::InvalidBounds.to_string(),
            "radio generation and callback bounds must be nonzero"
        );
        assert_eq!(
            RadioConfigError::UnsupportedTone.to_string(),
            "configured CTCSS tone is not supported by the radio core"
        );
    }
}
