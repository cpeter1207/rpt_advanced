//! Projection of the exact accepted WebRTC ingress and correlated egress observations.
use super::{
    model::{Owner, RecordKey, Stamp, meter_observation, meter_profile},
    peer::{self, EgressGraph},
    query::Topic,
};
use crate::{
    management::{Id, live::WebRtcObservation},
    status_meter::{Measurement, NATIVE_RATE, Quality},
};
use rpt_advanced_core::runtime::RuntimeClock;
use rptadv_peer_support::webrtc::channel::Reference;
use serde_json::{Value, json};

const STAGE: &str = "opus_encoder_input";

fn mapped_window(reference: &Reference, meter: Measurement) -> Option<Value> {
    if matches!(meter.quality, Quality::Fault | Quality::Unavailable) {
        return None;
    }
    let start = meter.end_sample.checked_sub(meter.valid_samples)?;
    if start < reference.source_sample.0 {
        return None;
    }
    let rtp = reference.positions().rtp_window(
        meter.source_generation,
        reference.epoch.0,
        start..meter.end_sample,
    )?;
    Some(json!({
        "first_sample":start.to_string(),"end_sample":meter.end_sample.to_string(),
        "first_rtp_timestamp":rtp.start.to_string(),"end_rtp_timestamp":rtp.end.to_string()
    }))
}

/// Share the authorized track binding on graph and meter records for this port.
pub(super) fn project(
    port: Id,
    observed: &WebRtcObservation,
    stamp: &Stamp,
    clock: Option<RuntimeClock>,
    mut put: impl FnMut(RecordKey, Value),
) {
    let owner = Owner::Port(port);
    let binding = observed.binding.to_string();
    let peer = format!("webrtc:{binding}");
    let chain = format!("link:{peer}:egress");
    let connection = observed.connection_generation;
    let graph = observed.graph_generation;
    if observed.transceive
        && let Some(ingress) = observed.ingress.as_deref()
    {
        peer::project(
            port,
            ("link", &peer, connection),
            ingress,
            EgressGraph::Separate,
            stamp,
            clock,
            &mut put,
        );
    }
    let entity = json!({"type":"link","id":peer,"generation":connection.to_string()});
    let anchor = observed
        .anchor
        .as_ref()
        .filter(|reference| reference.binding == binding);
    let epoch = anchor.map(|reference| reference.epoch.0);
    let correlation = json!({
        "binding":binding,"track":"audio","direction":"egress",
        "quality":if anchor.is_some() {"valid"} else {"unavailable"},
        "anchor":anchor,
    });
    put(
        RecordKey::new(
            owner,
            Some(Topic::Meters),
            &["graphs", &chain],
            "graph",
            chain.clone(),
            Some(graph),
        )
        .depends_on("link", &peer, connection)
        .with_media_epoch(epoch),
        json!({
            "chain_id":chain,"direction":"egress","entity":entity,
            "graph_generation":graph.to_string(),"sample_rate_hz":NATIVE_RATE,
            "sample_type":"float32","channels":["mono"],
            "stages":[
                {"id":STAGE,"purpose":"encoder_input","position":0,"inputs":[],
                    "state":"active","signals":["mono"]},
                {"id":"rtp_transport","purpose":"encoded_media_transport","position":1,
                    "inputs":[STAGE],"state":"unavailable","signals":["mono"],
                    "reason":"encoded_media"}
            ],
            "meter_profile":meter_profile(),"media_correlation":correlation,
        }),
    );
    let Some(meter) = observed
        .codec_input
        .filter(|meter| meter.graph_generation == graph)
    else {
        return;
    };
    let mut value = meter_observation(port, (&chain, STAGE, NATIVE_RATE), meter, stamp, clock);
    value["entity"] = entity;
    let mut correlation = correlation;
    if let Some(window) = anchor
        .filter(|_| value["quality"] != "stale")
        .and_then(|reference| mapped_window(reference, meter))
    {
        correlation["window"] = window;
    } else {
        correlation["quality"] = json!("unavailable");
    }
    value["media_correlation"] = correlation;
    let subject = format!("{chain}:{STAGE}:mono");
    put(
        RecordKey::new(
            owner,
            Some(Topic::Meters),
            &["meters", &subject],
            "meter",
            subject.clone(),
            Some(graph),
        )
        .depends_on("link", &peer, connection)
        .with_media_epoch(epoch),
        value,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::status_audio::{NativeIngressMeasurements, PeerMeasurements, StageReading};
    use rptadv_peer_support::webrtc::channel::{Coordinate, TimeBasis};

    #[test]
    fn codec_window_requires_a_current_matching_anchor_and_graph() {
        let binding: Id = "188a8e15-8cdf-4149-b532-fefb99f34270".parse().unwrap();
        let port: Id = "a7569549-8088-4382-8707-0be813bcc435".parse().unwrap();
        let mut observed = WebRtcObservation {
            binding,
            connection_generation: 2,
            sender_rate: None,
            decoded: None,
            receiver_quality: Vec::new(),
            ingress: None,
            transceive: false,
            graph_generation: 9,
            anchor: Some(Reference {
                binding: binding.to_string(),
                ssrc: 1234,
                epoch: Coordinate(3),
                source_generation: Coordinate(7),
                source_sample: Coordinate(1000),
                rtp_timestamp: Coordinate(4_294_967_000),
                time_basis: Some(TimeBasis::SessionMonotonic),
                time_ns: Some(Coordinate(12345)),
                uncertainty_ns: None,
            }),
            codec_input: Some(Measurement {
                graph_generation: 9,
                measurement_epoch: 4,
                source_generation: 7,
                end_sample: 4000,
                valid_samples: 2400,
                monotonic_ms: 800,
                quality: Quality::Valid,
                ..Measurement::default()
            }),
        };
        let stamp = Stamp {
            utc: None,
            monotonic_ms: 1000,
        };
        fn project_records(
            port: Id,
            stamp: &Stamp,
            observation: &WebRtcObservation,
        ) -> Vec<(RecordKey, Value)> {
            let mut records = Vec::new();
            project(port, observation, stamp, None, |key, value| {
                records.push((key, value))
            });
            records
        }
        let first = project_records(port, &stamp, &observed);
        assert_eq!(first.len(), 2);
        let meter = &first[1].1;
        assert_eq!(meter["audio_position"]["end_sample"], "4000");
        assert_eq!(
            meter["media_correlation"]["window"]["first_rtp_timestamp"],
            "4294967600"
        );
        assert_eq!(
            meter["media_correlation"]["window"]["end_rtp_timestamp"],
            "4294970000"
        );
        assert!(meter["media_correlation"]["anchor"]["uncertainty_ns"].is_null());
        assert_eq!(meter["entity"]["generation"], "2");

        observed.anchor.as_mut().unwrap().epoch.0 = 4;
        observed.anchor.as_mut().unwrap().source_generation.0 = 8;
        let next = project_records(port, &stamp, &observed);
        assert_ne!(first[0].0, next[0].0);
        assert_ne!(first[1].0, next[1].0);
        assert_eq!(next[1].1["media_correlation"]["quality"], "unavailable");
        assert!(next[1].1["media_correlation"].get("window").is_none());

        observed.graph_generation = 10;
        assert_eq!(project_records(port, &stamp, &observed).len(), 1);
        observed.anchor.as_mut().unwrap().binding = port.to_string();
        let graph = project_records(port, &stamp, &observed).pop().unwrap().1;
        assert!(graph["media_correlation"]["anchor"].is_null());
        assert_eq!(graph["stages"][1]["state"], "unavailable");

        let decoded = Measurement {
            graph_generation: 11,
            peak_dbfs: Some(-9.0),
            ..observed.codec_input.unwrap()
        };
        let stage = |available, measurement| StageReading {
            rate: NATIVE_RATE,
            available,
            measurement,
        };
        observed.ingress = Some(std::sync::Arc::new(PeerMeasurements {
            ingress_graph_generation: Some(11),
            decoded: stage(true, Some(decoded)),
            mix_minus: stage(false, None),
            codec_input: stage(false, None),
            native_ingress: Some(NativeIngressMeasurements {
                graph_generation: Some(10),
                ring_output: stage(
                    true,
                    Some(Measurement {
                        graph_generation: 10,
                        ..decoded
                    }),
                ),
                dsp_output: stage(false, None),
                station_mix_input: stage(true, None),
            }),
            provider_output: None,
        }));
        assert_eq!(project_records(port, &stamp, &observed).len(), 1);
        observed.transceive = true;
        let records = project_records(port, &stamp, &observed);
        let peer = format!("webrtc:{binding}");
        let native = format!("link:{peer}:native_ingress");
        let ingress = records
            .iter()
            .map(|(_, value)| value)
            .find(|value| {
                value["chain_id"] == format!("link:{peer}:ingress")
                    && value["direction"] == "ingress"
            })
            .unwrap();
        assert_eq!(ingress["stages"][1]["measurement_ref"]["chain_id"], native);
        assert_eq!(ingress["stages"][2]["state"], "bypassed");
        assert_eq!(ingress["stages"][3]["state"], "active");
        assert_eq!(
            ingress["stages"][3]["measurement_ref"]["stage_id"],
            "station_mix_input"
        );
        assert_eq!(
            records
                .iter()
                .filter(|(_, value)| value["direction"] == "egress")
                .count(),
            1,
            "the tailored WebRTC egress graph replaces the shared egress graph",
        );
        assert!(
            records
                .iter()
                .any(|(_, value)| value["stage_id"] == "decoded"
                    && value["peak_dbfs"] == -9.0
                    && value["entity"]["id"] == peer
                    && value["entity"]["generation"] == "2")
        );
    }
}
