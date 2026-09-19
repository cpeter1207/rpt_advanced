//! Per-recipient bounded topology publication on change and at thirty seconds.
use super::LinkManager;
use std::collections::HashSet;

/// Tracks the last advertised control revision and refresh deadline.
#[derive(Default)]
pub struct TopologyManager {
    revision: u64,
    due_ms: u64,
}
impl TopologyManager {
    /// Prepare ordinary channel-owner messages outside radio/audio callbacks.
    pub fn publish(&mut self, hub: &LinkManager, now_ms: u64) -> Vec<(String, String)> {
        if self.revision == hub.revision && now_ms < self.due_ms {
            return Vec::new();
        }
        self.revision = hub.revision;
        self.due_ms = now_ms.saturating_add(30000);
        hub.peers
            .iter()
            .filter(|peer| !peer.ended)
            .map(|recipient| {
                (
                    recipient.name.clone(),
                    format!("L {}", hub.topology_for(Some(&recipient.name))),
                )
            })
            .collect()
    }
}
impl LinkManager {
    /// Complete bounded forwarding topology for status, using the same wire mode rules.
    pub fn full_topology(&self) -> String {
        self.topology_for(None)
    }

    fn topology_for(&self, recipient: Option<&str>) -> String {
        let mut tokens = Vec::new();
        let mut seen = HashSet::new();
        let mut length = 2;
        'peers: for peer in self.peers.iter().filter(|peer| {
            !peer.ended && Some(peer.name.as_str()) != recipient && peer.mode.forwards()
        }) {
            let direct = if peer.mode.transmits() { 'T' } else { 'R' };
            for (mode, node) in std::iter::once((direct, peer.name.as_str())).chain(
                peer.routes.iter().map(|route| {
                    (
                        if direct == 'R' && route.mode == 'T' {
                            'R'
                        } else {
                            route.mode
                        },
                        route.node.as_str(),
                    )
                }),
            ) {
                if Some(node) == recipient || node == self.local || !seen.insert(node) {
                    continue;
                }
                if length + node.len() + 2 + 8 > 10000 {
                    tokens.push("R000000".into());
                    break 'peers;
                }
                length += node.len() + 2;
                tokens.push(format!("{mode}{node}"));
            }
        }
        tokens.join(",")
    }
}
