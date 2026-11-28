//! Project lifecycle edges without retaining producer inputs or private media.
use super::Host;
use crate::management::status::edges::{Edge, Payload};
use rpt_advanced_core::config::resources::candidate::ResourceId as Id;
use rpt_advanced_core::controller::TelemetryPhase;
use serde_json::json;

impl Host {
    /// Timestamp transmit-owner edges in the controller's observed monotonic clock.
    pub(super) fn collect_telemetry_status(&mut self, local: &str, now_ms: u64) {
        let Some(node) = self.runtime.node(local) else {
            return;
        };
        let owner = local
            .parse::<Id>()
            .ok()
            .zip(node.command_work().map(|work| work.id()));
        let (events, lost, sample) = node.drain_telemetry_observations();
        if lost {
            self.status_edges.mark_lost();
        }
        let Some((port, generation)) = owner else {
            return;
        };
        for event in events {
            let (kind, phase, terminal) = match event.phase {
                TelemetryPhase::Queued => ("telemetry.queued", "queued", false),
                TelemetryPhase::Started => ("telemetry.started", "playing", false),
                TelemetryPhase::Finished => ("telemetry.finished", "finished", true),
                TelemetryPhase::Cancelled => ("telemetry.cancelled", "cancelled", true),
                TelemetryPhase::Failed => ("telemetry.failed", "failed", true),
            };
            let public = json!({
                "id":event.id.to_string(),
                "quality":if event.phase == TelemetryPhase::Failed {"fault"} else {"valid"},
                "category":event.category,
                "audience":event.audience,
                "phase":phase,
                "sample_position":event.sample_position.to_string(),
                "deadline_sample":event.deadline_sample.map(|value| value.to_string()),
                "reason":event.reason,
            });
            let (value, terminal) = if terminal {
                (None, Some(public))
            } else {
                (Some(public), None)
            };
            self.status_edges.push(Edge {
                port,
                generation,
                at_ms: now_ms.saturating_sub(sample.saturating_sub(event.sample_position) / 48),
                payload: Payload::Telemetry {
                    id: event.id.to_string(),
                    playback_generation: generation,
                    kind,
                    value,
                    terminal,
                },
            });
        }
    }
}
