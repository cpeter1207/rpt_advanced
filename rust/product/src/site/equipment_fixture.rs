//! Resolved physical endpoints used by acquisition, backend and radio owner tests.
use rpt_advanced_core::config::{
    Calibration, InputSettings, OutputSettings, SensorSettings, SiteBackend, SiteEndpoint,
};

pub(super) fn endpoint(backend: SiteBackend, device: &str, channel: &str) -> SiteEndpoint {
    SiteEndpoint {
        backend,
        device: device.into(),
        channel: channel.into(),
    }
}

pub(super) fn input(name: &str, endpoint: SiteEndpoint) -> InputSettings {
    InputSettings {
        name: name.into(),
        enabled: true,
        endpoint: Some(endpoint),
        interface: None,
        active_high: true,
        debounce_ms: 50,
        dtmf_id: None,
        on_active: vec![],
        on_inactive: vec![],
    }
}

pub(super) fn output(name: &str, endpoint: SiteEndpoint) -> OutputSettings {
    OutputSettings {
        name: name.into(),
        enabled: true,
        endpoint: Some(endpoint),
        interface: None,
        active_high: true,
        startup_on: false,
        safe_on: false,
        pulse_ms: 500,
    }
}

pub(super) fn sensor(name: &str, endpoint: SiteEndpoint) -> SensorSettings {
    SensorSettings {
        name: name.into(),
        enabled: true,
        endpoint: Some(endpoint),
        interface: None,
        label_message_id: String::new(),
        unit: String::new(),
        precision: 1,
        poll_interval_ms: 1000,
        stale_after_ms: 5000,
        smoothing_ms: 0,
        calibration: Calibration::None,
        dtmf_id: None,
    }
}
