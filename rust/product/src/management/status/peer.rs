//! PCM observations from an established network or attached phone peer.
use super::{
    model::{Owner, RecordKey, Stamp, meter_observation, meter_profile},
    query::Topic,
};
use crate::{
    configuration_radio::ProviderOutputState, link::status_audio::PeerMeasurements, management::Id,
};
use rpt_advanced_core::runtime::RuntimeClock;
use serde_json::{Value, json};

const RING_OUTPUT: &str = "native_ring_output";
const DSP_OUTPUT: &str = "provider_dsp_pcm_output";

fn measurement_ref(chain: &str, graph: Option<u64>, stage: &str) -> Value {
    json!({
        "chain_id":chain,"graph_generation":graph.map(|generation|generation.to_string()),
        "stage_id":stage,"signal":"mono"
    })
}

/// The shared egress graph or a separately projected transport-specific graph.
pub(super) enum EgressGraph {
    Shared(Option<u64>),
    Separate,
}

pub(super) fn project(
    port: Id,
    identity: (&'static str, &str, u64),
    meters: &PeerMeasurements,
    egress: EgressGraph,
    stamp: &Stamp,
    clock: Option<RuntimeClock>,
    mut put: impl FnMut(RecordKey, Value),
) {
    let (family, entity_id, connection) = identity;
    let owner = Owner::Port(port);
    let entity = json!({"type":family,"id":entity_id,"generation":connection.to_string()});
    let native = meters.native_ingress.as_ref();
    let native_chain = native.map(|_| format!("{family}:{entity_id}:native_ingress"));
    if let Some((native, chain)) = native.zip(native_chain.as_deref()) {
        let graph = native.graph_generation;
        let stages = [
            (
                RING_OUTPUT,
                "resampled_native_pcm",
                None,
                &native.ring_output,
            ),
            (
                DSP_OUTPUT,
                "processed_peer_pcm",
                Some(RING_OUTPUT),
                &native.dsp_output,
            ),
        ];
        let descriptors = stages
            .iter()
            .enumerate()
            .map(|(position, (stage, purpose, input, reading))| {
                let mut descriptor = json!({
                    "id":stage,"purpose":purpose,"position":position,
                    "inputs":input.iter().collect::<Vec<_>>(),
                    "state":if reading.available {"active"} else {"bypassed"},
                    "signals":["mono"]
                });
                if !reading.available {
                    descriptor["measurement_ref"] = measurement_ref(chain, graph, RING_OUTPUT);
                }
                descriptor
            })
            .collect::<Vec<_>>();
        put(
            RecordKey::new(
                owner,
                Some(Topic::Meters),
                &["graphs", chain],
                "graph",
                chain.to_owned(),
                graph,
            )
            .depends_on(family, entity_id, connection),
            json!({
                "chain_id":chain,"direction":"ingress","entity":entity,
                "graph_generation":graph.map(|generation|generation.to_string()),
                "sample_type":"float32","sample_rate_hz":48000,"channels":["mono"],
                "stages":descriptors,"meter_profile":meter_profile()
            }),
        );
        for (stage, _, _, reading) in stages {
            let Some(meter) = reading
                .measurement
                .filter(|meter| Some(meter.graph_generation) == graph)
            else {
                continue;
            };
            let subject = format!("{chain}:{stage}:mono");
            let mut value =
                meter_observation(port, (chain, stage, reading.rate), meter, stamp, clock);
            value["entity"] = entity.clone();
            put(
                RecordKey::new(
                    owner,
                    Some(Topic::Meters),
                    &["meters", &subject],
                    "meter",
                    subject.clone(),
                    graph,
                )
                .depends_on(family, entity_id, connection),
                value,
            );
        }
    }
    let ingress = (
        "ingress",
        meters.ingress_graph_generation,
        [
            (
                "decoded",
                "accepted_source_pcm",
                None,
                Some(&meters.decoded),
            ),
            (
                "jitter_resample",
                "elastic_48khz_conversion",
                Some("decoded"),
                None,
            ),
            (
                "provider_dsp_output",
                "provider_or_controller_processing",
                Some("jitter_resample"),
                None,
            ),
            (
                "station_mix_input",
                "admitted_programme",
                Some("provider_dsp_output"),
                None,
            ),
        ],
    );
    let egress = match egress {
        EgressGraph::Shared(graph) => Some((
            "egress",
            graph,
            [
                (
                    "mix_minus",
                    "source_excluded_programme",
                    None,
                    Some(&meters.mix_minus),
                ),
                ("egress_dsp", "egress_processing", Some("mix_minus"), None),
                (
                    "codec_input",
                    "negotiated_pcm",
                    Some("egress_dsp"),
                    Some(&meters.codec_input),
                ),
                (
                    "encoder",
                    "codec_or_transport_internals",
                    Some("codec_input"),
                    None,
                ),
            ],
        )),
        EgressGraph::Separate => None,
    };
    for (side, graph, stages) in std::iter::once(ingress).chain(egress) {
        let chain = format!("{family}:{entity_id}:{side}");
        let descriptor = stages
            .iter()
            .enumerate()
            .map(|(position, (stage, purpose, input, reading))| {
                let provider = (*stage == "provider_dsp_output")
                    .then_some(meters.provider_output.as_ref())
                    .flatten();
                let alias = native.zip(native_chain.as_deref()).and_then(|(native, chain)| {
                    let (point, reading, state) = match *stage {
                        "jitter_resample" => (RING_OUTPUT, &native.ring_output, "active"),
                        "provider_dsp_output" if native.dsp_output.available => {
                            (DSP_OUTPUT, &native.dsp_output, "active")
                        }
                        "provider_dsp_output" => (RING_OUTPUT, &native.ring_output, "bypassed"),
                        _ => return None,
                    };
                    Some((measurement_ref(chain, native.graph_generation, point), reading.rate, state))
                });
                let provider_reason = provider.and_then(|provider| match &provider.state {
                    ProviderOutputState::Observed(_) => alias.is_none().then_some("aggregate_only"),
                    ProviderOutputState::RateMismatch => Some("provider_rate_mismatch"),
                    ProviderOutputState::ObserverUnavailable => Some("provider_observation_unavailable"),
                    ProviderOutputState::ReadFailed => Some("provider_observation_failed"),
                });
                let available = reading.is_some_and(|reading| reading.available)
                    || alias.is_some()
                    || provider_reason == Some("aggregate_only");
                let state = alias.as_ref().map_or(
                    if available {"active"} else {"unavailable"},
                    |(_, _, state)| *state,
                );
                let rate = reading
                    .map(|reading| reading.rate)
                    .or_else(|| alias.as_ref().map(|(_, rate, _)| *rate))
                    .or_else(|| provider.map(|provider| provider.rate));
                let mut descriptor = json!({
                    "id":stage,"purpose":purpose,"position":position,"inputs":input.iter().collect::<Vec<_>>(),
                    "state":state,"reason":provider_reason,"sample_rate_hz":rate,
                    "signals":if available {json!(["mono"])} else {json!([])}
                });
                if let Some((output, _, _)) = alias {
                    descriptor["measurement_ref"] = output;
                }
                descriptor
            })
            .collect::<Vec<_>>();
        put(
            RecordKey::new(
                owner,
                Some(Topic::Meters),
                &["graphs", &chain],
                "graph",
                chain.clone(),
                graph,
            )
            .depends_on(family, entity_id, connection),
            json!({
                    "chain_id":chain,"direction":side,"entity":entity,"graph_generation":graph.map(|generation|generation.to_string()),
                    "sample_type":"float32","channels":["mono"],"stages":descriptor,"meter_profile":meter_profile()
            }),
        );
        if side == "ingress"
            && let (Some(graph), Some(provider)) = (graph, meters.provider_output.as_ref())
            && let ProviderOutputState::Observed(levels) = &provider.state
        {
            let stage = "provider_dsp_output";
            let quality = if levels.integrated_lufs.is_some() || levels.true_peak_dbtp.is_some() {
                "valid"
            } else if levels.valid_samples == 0 {
                "warming"
            } else {
                "unavailable"
            };
            let subject = format!("{chain}:{stage}:mono");
            put(
                RecordKey::new(
                    owner,
                    Some(Topic::Meters),
                    &["meters", &subject],
                    "meter",
                    subject.clone(),
                    Some(graph),
                )
                .depends_on(family, entity_id, connection),
                json!({
                    "port_id":port.to_string(),"chain_id":chain,"stage_id":stage,"signal":"mono",
                    "entity":entity,"graph_generation":graph.to_string(),
                    "measurement_epoch":levels.loudness_epoch.to_string(),
                    "observed_at":null,"quality":quality,"signal_state":"unknown","reason":"provider_aggregate_only",
                    "valid_samples":levels.valid_samples.to_string(),
                    "peak_dbfs":null,"rms_dbfs":null,"lufs":null,"peak_hold_dbfs":null,
                    "integrated_lufs":levels.integrated_lufs,"true_peak_dbtp":levels.true_peak_dbtp,
                    "processed_blocks":levels.processed_blocks.to_string(),"bypassed_blocks":levels.bypassed_blocks.to_string(),
                    "failed_blocks":levels.failed_blocks.to_string()
                }),
            );
        }
        for (stage, _, _, reading) in stages {
            let Some(reading) = reading else {
                continue;
            };
            let Some(meter) = reading
                .measurement
                .filter(|meter| Some(meter.graph_generation) == graph)
            else {
                continue;
            };
            let subject = format!("{chain}:{stage}:mono");
            let mut value =
                meter_observation(port, (&chain, stage, reading.rate), meter, stamp, clock);
            value["entity"] = entity.clone();
            put(
                RecordKey::new(
                    owner,
                    Some(Topic::Meters),
                    &["meters", &subject],
                    "meter",
                    subject.clone(),
                    graph,
                )
                .depends_on(family, entity_id, connection),
                value,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        configuration_radio::ProviderOutput,
        link::status_audio::{NativeIngressMeasurements, StageReading},
    };

    #[test]
    fn peer_ingress_links_to_native_taps_and_identifies_provider_rate_mismatch() {
        let stage = |rate, available| StageReading {
            rate,
            available,
            measurement: None,
        };
        let mut measurements = PeerMeasurements {
            ingress_graph_generation: Some(11),
            decoded: stage(48_000, true),
            mix_minus: stage(48_000, true),
            codec_input: stage(48_000, true),
            native_ingress: Some(NativeIngressMeasurements {
                graph_generation: Some(7),
                ring_output: stage(48_000, true),
                dsp_output: stage(48_000, true),
            }),
            provider_output: Some(ProviderOutput {
                rate: 8_000,
                state: ProviderOutputState::RateMismatch,
            }),
        };
        let stamp = Stamp {
            utc: Some("2026-11-26T00:02:15.000Z".into()),
            monotonic_ms: 5_000,
        };
        let project_graph = |measurements: &PeerMeasurements| {
            let mut records = Vec::new();
            project(
                "361c602c-7664-4600-990d-6bd6cae4b32c".parse().unwrap(),
                ("link", "allstarlink:1234", 12),
                measurements,
                EgressGraph::Shared(Some(7)),
                &stamp,
                None,
                |_, document| records.push(document),
            );
            records
        };
        let docs = project_graph(&measurements);
        let ingress = docs
            .iter()
            .find(|doc| {
                doc.get("stages").is_some() && doc["chain_id"] == "link:allstarlink:1234:ingress"
            })
            .unwrap();
        assert_eq!(
            ingress["stages"][1]["measurement_ref"]["stage_id"],
            "native_ring_output"
        );
        assert_eq!(
            ingress["stages"][2]["measurement_ref"]["stage_id"],
            "provider_dsp_pcm_output"
        );
        assert_eq!(
            ingress["stages"][2]["measurement_ref"]["graph_generation"],
            "7"
        );
        assert_eq!(ingress["stages"][2]["state"], "active");
        assert_eq!(ingress["stages"][2]["sample_rate_hz"], 48_000);
        assert_eq!(ingress["stages"][2]["reason"], "provider_rate_mismatch");
        assert!(!docs.iter().any(|doc| doc.get("integrated_lufs").is_some()));

        measurements.native_ingress.as_mut().unwrap().dsp_output = stage(48_000, false);
        measurements.provider_output = None;
        let bypass = project_graph(&measurements);
        let ingress = bypass
            .iter()
            .find(|doc| {
                doc.get("stages").is_some() && doc["chain_id"] == "link:allstarlink:1234:ingress"
            })
            .unwrap();
        assert_eq!(
            ingress["stages"][2]["measurement_ref"]["stage_id"],
            "native_ring_output"
        );
        assert_eq!(ingress["stages"][2]["state"], "bypassed");
    }
}
