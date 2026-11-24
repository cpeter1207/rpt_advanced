use rpt_advanced_core::{
    audio::LinkAudioQueue,
    config::resources::{
        PortKey,
        candidate::{
            CapturedResource, ModularCandidate, ResourceBinding, ResourceId, ResourceKind,
            ResourceRegistry, ResourceScope,
        },
    },
    link::{AudioPeer, LinkAudio, Mode, PeerInput, PeerSignals, SourcePosition},
    messages::InstalledMessageSources,
};
use rptadv_product::management::{PreparedConfiguration, ProfileInputs};
use serde_json::json;
use std::sync::Arc;

fn configured(rx: bool, tx: bool) -> (PreparedConfiguration, ResourceId) {
    let id = |n: u8| format!("00000000-0000-4000-8000-{n:012x}").parse().unwrap();
    let site = id(1);
    let port = id(2);
    let sections = [
        (ResourceKind::Radio, json!({"duplex_mode":"full"})),
        (
            ResourceKind::Interface,
            json!({"audio":{"device_identifier":"3-1"}}),
        ),
        (ResourceKind::RxAudio, json!({"enabled":rx})),
        (ResourceKind::TxAudio, json!({"enabled":tx})),
    ];
    let bindings: Vec<_> = sections
        .iter()
        .enumerate()
        .map(|(i, (kind, _))| ResourceBinding {
            id: id(3 + i as u8),
            scope: ResourceScope::Port(port),
            kind: *kind,
        })
        .collect();
    let registry =
        ResourceRegistry::new(site, &[(port, PortKey::new("station"))], &bindings).unwrap();
    let rows: Vec<_> = registry
        .bindings()
        .map(|binding| {
            let mut doc = json!({"id":binding.id.to_string(),"kind":binding.kind.as_str()});
            doc["configuration"] = if binding.id == site {
                json!({"name":"Site","inherit_from":null})
            } else if binding.id == port {
                doc["key"] = json!("station");
                json!({"name":"Station","inherit_from":"site","enabled":true})
            } else {
                sections
                    .iter()
                    .find(|(kind, _)| *kind == binding.kind)
                    .unwrap()
                    .1
                    .clone()
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
    (
        PreparedConfiguration::prepare(&candidate, Some("UTC"), &inputs).unwrap(),
        port,
    )
}

struct Wave {
    signals: PeerSignals,
    playing: bool,
}
impl PeerInput for Wave {
    fn signals(&self) -> &PeerSignals {
        &self.signals
    }
    fn source_rate(&self) -> u32 {
        48_000
    }
    fn available(&self) -> u64 {
        if self.playing { 4 } else { 0 }
    }
    fn finite_playout(&self) -> Option<bool> {
        Some(self.playing)
    }
    fn render(&mut self, audio: &mut [f32]) -> Option<usize> {
        audio.fill(0.25);
        Some(audio.len())
    }
}

#[derive(Debug, PartialEq)]
struct Output {
    keyed: bool,
    rf: [f32; 4],
    other_peer: [f32; 4],
}

fn observe(rx: bool, tx: bool, from_receiver: bool) -> Output {
    let (configured, port) = configured(rx, tx);
    let mut next_recipe = 0;
    let (mut node, _control) = configured
        .runtime()
        .prepare_station_controller(port, 1, |_, _, _| {
            let index = next_recipe;
            next_recipe += 1;
            Ok(index)
        })
        .unwrap();
    let peer = |name, playing| {
        let (writer, reader) = LinkAudioQueue::new(960, 64).unwrap().into_endpoints();
        let signals = PeerSignals::new();
        if playing {
            signals.publish_pcm();
            signals.set_radio_keyed(true);
        }
        let input = Wave { signals, playing };
        (
            AudioPeer::new(name, Mode::TRANSCEIVE, input, writer, 4, 0).unwrap(),
            reader,
        )
    };
    let (source, _source_output) = peer("100", !from_receiver);
    let (other, mut other_output) = peer("200", false);
    let (mut links, mut egress) = LinkAudio::new(vec![source, other], 4).unwrap();
    let mut rf = [if from_receiver { 0.5 } else { 0.0 }; 4];
    let keyed = links
        .process(&mut node, from_receiver, &mut rf, SourcePosition::default())
        .unwrap();
    assert_eq!(
        node.ctcss_enabled(),
        tx,
        "unscoped CTCSS follows TX admission"
    );
    assert_eq!(egress.dispatch(1), 1);
    let mut other_peer = [0.0; 4];
    other_output.read(&mut other_peer);
    Output {
        keyed,
        rf,
        other_peer,
    }
}

fn expected(keyed: bool, rf: f32, peer: f32) -> Output {
    Output {
        keyed,
        rf: [rf; 4],
        other_peer: [peer; 4],
    }
}

#[test]
fn transmit_disabled_preserves_local_and_peer_forwarding() {
    assert_eq!(observe(true, true, true), expected(true, 0.5, 0.5));
    assert_eq!(observe(true, true, false), expected(true, 0.25, 0.25));
    let local = observe(true, false, true);
    let remote = observe(true, false, false);
    eprintln!("TX_ADMISSION local={local:?} remote={remote:?}");
    assert_eq!(
        (local, remote),
        (expected(false, 0.0, 0.5), expected(false, 0.0, 0.25)),
        "disabled station TX must be quiet and dekeyed while both peer routes continue"
    );
}

#[test]
fn receive_disabled_preserves_peer_to_peer_and_rf_audio() {
    assert_eq!(observe(true, true, true), expected(true, 0.5, 0.5));
    let peer = observe(false, true, false);
    assert_eq!(peer, expected(true, 0.25, 0.25));
    let local = observe(false, true, true);
    eprintln!("RX_ADMISSION peer={peer:?} local={local:?}");
    assert_eq!(
        local,
        expected(false, 0.0, 0.0),
        "disabled station RX must reject its qualifier and PCM on RF and peer output"
    );
}
