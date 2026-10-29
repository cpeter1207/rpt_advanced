//! Borrow one complete resolved provider record without retaining parser state in a host.
use rpt_advanced_core::config::{
    EchoLinkAnnouncement, EchoLinkSettings, EchoLinkTelemetry, EchoLinkTransport,
};
use rptadv_peer_support::echolink::abi as echo;

fn bytes(value: &str) -> echo::ByteSpan {
    echo::ByteSpan {
        data: value.as_ptr(),
        length: value.len(),
    }
}
fn list(value: &[echo::ByteSpan]) -> echo::StringList {
    echo::StringList {
        items: value.as_ptr(),
        count: value.len(),
    }
}

pub(super) fn visit<T>(
    local: &str,
    settings: &EchoLinkSettings,
    consume: impl FnOnce(&echo::AccountConfig) -> T,
) -> T {
    let servers: Vec<_> = settings
        .directory_servers
        .iter()
        .map(|value| bytes(value))
        .collect();
    let allow: Vec<_> = settings.allow.iter().map(|value| bytes(value)).collect();
    let deny: Vec<_> = settings.deny.iter().map(|value| bytes(value)).collect();
    let bind = settings.bind_address.to_string();
    consume(&echo::AccountConfig {
        local_node: bytes(local),
        enabled: u32::from(settings.enabled),
        node_number: settings.node_number,
        callsign: bytes(&settings.callsign),
        station: echo::StationMetadata {
            name: bytes(&settings.name),
            location: bytes(&settings.location),
            email: bytes(&settings.email),
            status_message: bytes(&settings.status_message),
            latitude: settings.latitude,
            longitude: settings.longitude,
            frequency_mhz: settings.frequency_mhz,
            tone_hz: settings.tone_hz,
            power_w: settings.power_w,
            antenna_height_m: settings.antenna_height_m,
            antenna_gain_db: settings.antenna_gain_db,
            antenna_direction_deg: settings.antenna_direction_deg,
        },
        transport: echo::TransportConfig {
            kind: match settings.transport {
                EchoLinkTransport::Direct => 0,
                EchoLinkTransport::Proxy => 1,
            },
            bind_address: bytes(&bind),
            audio_port: settings.audio_port,
            proxy_port: settings.proxy_port,
            proxy_host: bytes(&settings.proxy_host),
            directory_servers: list(&servers),
        },
        policy: echo::AccountPolicy {
            max_stations: settings.max_stations,
            max_pending_handshakes: settings.max_pending_handshakes,
            heartbeat_interval_ms: settings.heartbeat_interval_ms,
            heartbeat_timeout_ms: settings.heartbeat_timeout_ms,
            connect_timeout_ms: settings.connect_timeout_ms,
            directory_timeout_ms: settings.directory_timeout_ms,
            registration_refresh_ms: settings.registration_refresh_ms,
            directory_refresh_ms: settings.directory_refresh_ms,
            directory_retry_ms: settings.directory_retry_ms,
            directory_freshness_ms: settings.directory_freshness_ms,
            receive_gain_db: settings.receive_gain_db,
            transmit_gain_db: settings.transmit_gain_db,
            announcement: match settings.announcement {
                EchoLinkAnnouncement::Off => 0,
                EchoLinkAnnouncement::Node => 1,
                EchoLinkAnnouncement::Callsign => 2,
                EchoLinkAnnouncement::Both => 3,
            },
            telemetry: match settings.telemetry {
                EchoLinkTelemetry::Off => 0,
                EchoLinkTelemetry::On => 1,
                EchoLinkTelemetry::Follow => 2,
                EchoLinkTelemetry::Demand => 3,
            },
            telemetry_dynamic: u32::from(settings.telemetry_dynamic),
            allow: list(&allow),
            deny: list(&deny),
        },
        ..echo::AccountConfig::default()
    })
}
