use crate::management::{Fault, PreparedConfiguration, ProfileInputs};
use rpt_advanced_core::{
    audio::{LinkAudioQueue, PcmRead, PcmStreamReader},
    config::resources::{
        PortKey,
        candidate::{
            CapturedResource, ModularCandidate, ResourceBinding, ResourceId, ResourceKind,
            ResourceRegistry, ResourceScope,
        },
    },
    controller::PreparedMedia,
    link::{
        AudioPeer, LinkAudio, Mode, PeerInput, PeerSignals, PhoneAccountId, PhoneCallId,
        PhoneSource, SourcePosition,
    },
    messages::InstalledMessageSources,
};
use serde_json::json;
use std::sync::Arc;

fn station(phone: bool) -> Result<(PreparedConfiguration, ResourceId), Fault> {
    let id = |n: u8| format!("00000000-0000-4000-8000-{n:012x}").parse().unwrap();
    let site = id(1);
    let port = id(2);
    let row = |n, kind| ResourceBinding {
        id: id(n),
        kind,
        scope: ResourceScope::Port(port),
    };
    let interface = row(3, ResourceKind::Interface);
    let settings = row(4, ResourceKind::TelemetrySettings);
    let link = row(5, ResourceKind::Courtesy);
    let handset = row(6, ResourceKind::Courtesy);
    let mut members = vec![(link, PortKey::new("link"))];
    if phone {
        members.push((handset, PortKey::new("phone")));
    }
    let registry = ResourceRegistry::with_members(
        site,
        &[(port, PortKey::new("station"))],
        &[interface, settings],
        &members,
    )
    .unwrap();
    let rows: Vec<_> = registry
        .bindings()
        .map(|binding| {
            let mut doc = json!({"id":binding.id.to_string(),"kind":binding.kind.as_str()});
            doc["configuration"] = if binding.id == site {
                json!({"name":"Site","inherit_from":null})
            } else if binding.id == port {
                doc["key"] = json!("station");
                json!({"name":"Station","inherit_from":"site","enabled":true})
            } else if binding == interface {
                json!({"audio":{"device_identifier":"3-1"}})
            } else if binding == settings {
                json!({"timing":{"courtesy_delay_ms":1}})
            } else {
                let (input, text) = if binding == link {
                    ("link", "L")
                } else {
                    assert_eq!(binding, handset);
                    ("phone", "P")
                };
                doc["key"] = json!(input);
                json!({"input":input,"morse":{"text":text}})
            };
            (binding, serde_json::to_vec(&doc).unwrap())
        })
        .collect();
    let captured: Vec<_> = rows
        .iter()
        .map(|(binding, json)| CapturedResource {
            binding: *binding,
            json,
        })
        .collect();
    let candidate = ModularCandidate::assemble(&registry, &captured).unwrap();
    let inputs = Arc::new(ProfileInputs {
        messages: InstalledMessageSources::capture(),
        interfaces: Default::default(),
    });
    PreparedConfiguration::prepare(&candidate, Some("UTC"), &inputs)
        .map(|prepared| (prepared, port))
}

struct Voice(Arc<PeerSignals>);
impl PeerInput for Voice {
    fn source_rate(&self) -> u32 {
        48_000
    }
    fn signals(&self) -> &PeerSignals {
        &self.0
    }
    fn available(&self) -> u64 {
        if self.0.radio_keyed() { 48 } else { 0 }
    }
    fn finite_playout(&self) -> Option<bool> {
        Some(self.0.radio_keyed())
    }
    fn render(&mut self, output: &mut [f32]) -> Option<usize> {
        output.fill(0.25);
        Some(output.len())
    }
}

struct Marker(f32);
impl PcmStreamReader for Marker {
    fn render(&mut self, output: &mut [f32]) -> PcmRead {
        output.fill(self.0);
        PcmRead::FinalSamples(output.len())
    }
}

fn release(phone: bool) -> Result<(Option<usize>, bool), Fault> {
    let (prepared, port) = station(phone)?;
    let (mut node, mut control) = prepared
        .runtime()
        .prepare_station_controller(port, 1, |_, name, _| {
            Ok(match name {
                "link" => 1,
                "phone" => 2,
                _ => panic!("unexpected recipe"),
            })
        })
        .unwrap();
    let (writer, _reader) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
    let (peer, signals) = if phone {
        let source = PhoneSource::new(
            PhoneAccountId::new("fixture").unwrap(),
            1,
            PhoneCallId::new([1; 16]).unwrap(),
            1,
            1,
        )
        .unwrap();
        let signals = Arc::new(PeerSignals::for_phone(source.clone()));
        assert!(signals.authenticate_phone());
        let peer = AudioPeer::for_phone(
            &source,
            &signals.reply_target(),
            Mode::phone(false),
            Voice(signals.clone()),
            writer,
            48,
        )
        .unwrap();
        (peer, signals)
    } else {
        let signals = Arc::new(PeerSignals::new());
        let peer = AudioPeer::new(
            "100",
            Mode::TRANSCEIVE,
            Voice(signals.clone()),
            writer,
            48,
            0,
        )
        .unwrap();
        (peer, signals)
    };
    let (mut links, _dispatch) = LinkAudio::new(vec![peer], 48).unwrap();
    let mut tick = || {
        let mut rf = [0.0; 48];
        links
            .process(&mut node, false, &mut rf, SourcePosition::default())
            .unwrap();
        rf
    };
    signals.publish_pcm();
    signals.set_radio_keyed(true);
    assert_eq!(tick(), [0.25; 48], "admitted voice reaches station");
    assert!(control.take_message_request().is_none());
    signals.set_radio_keyed(false);
    // Each tick advances 1 ms. The asynchronous reader requests its recipe only when due.
    let mut recipe = None;
    let mut audible = false;
    for _ in 0..6 {
        let rf = tick();
        assert!(rf.iter().all(|sample| sample.is_finite()));
        // Measure actual RF only after recipe completion. Output processing changes gain.
        audible |= match recipe {
            Some(1) => rf.iter().any(|sample| *sample > 0.01),
            Some(2) => rf.iter().any(|sample| *sample < -0.01),
            _ => false,
        };
        if let Some(request) = control.take_message_request() {
            let marker = match request.content_index {
                1 => 0.5,
                2 => -0.75,
                _ => panic!("invalid recipe"),
            };
            let stream = PreparedMedia::new_stream(Box::new(Marker(marker))).unwrap();
            assert!(
                control
                    .complete_message(request.attempt, Ok(stream))
                    .is_ok()
            );
            assert!(recipe.replace(request.content_index).is_none());
        }
    }
    Ok((recipe, audible))
}

#[test]
fn admitted_phone_voice_release_plays_its_own_courtesy() {
    assert_eq!(
        release(false),
        Ok((Some(1), true)),
        "ordinary direct-link release"
    );
    assert_eq!(
        release(true),
        Ok((Some(2), true)),
        "admitted phone release must select phone courtesy"
    );
}
