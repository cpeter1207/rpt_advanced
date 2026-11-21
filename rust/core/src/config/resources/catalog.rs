//! Format-1 public working-tree catalog. Exact local keys never enter paths.
use super::candidate::{ResourceBinding, ResourceId, ResourceKind, ResourceScope};

#[derive(Clone, Copy)]
pub(crate) enum Layout {
    File(&'static str),
    Collection(&'static str),
    Protocol,
}

const PROFILES: &[(ResourceKind, &str, Layout)] = &[
    (
        ResourceKind::Metadata,
        "metadata",
        Layout::File("metadata.json"),
    ),
    (ResourceKind::Radio, "radio", Layout::File("radio.json")),
    (
        ResourceKind::RxAudio,
        "rx-audio",
        Layout::File("rx/audio.json"),
    ),
    (
        ResourceKind::RxSignaling,
        "rx-signaling",
        Layout::File("rx/signaling.json"),
    ),
    (
        ResourceKind::TxAudio,
        "tx-audio",
        Layout::File("tx/audio.json"),
    ),
    (
        ResourceKind::TxSignaling,
        "tx-signaling",
        Layout::File("tx/signaling.json"),
    ),
    (
        ResourceKind::Interface,
        "interface",
        Layout::File("interface.json"),
    ),
    (
        ResourceKind::PeerAudio,
        "peer-audio",
        Layout::File("peer-audio.json"),
    ),
    (
        ResourceKind::Control,
        "control",
        Layout::File("control.json"),
    ),
    (
        ResourceKind::Memories,
        "memories",
        Layout::File("memories.json"),
    ),
    (
        ResourceKind::ScanLists,
        "scan-lists",
        Layout::File("scan-lists.json"),
    ),
    (
        ResourceKind::Command,
        "command",
        Layout::File("command/settings.json"),
    ),
    (ResourceKind::Limits, "limits", Layout::File("limits.json")),
    (ResourceKind::Power, "power", Layout::File("power.json")),
    (ResourceKind::Allstarlink, "allstarlink", Layout::Protocol),
    (ResourceKind::Echolink, "echolink", Layout::Protocol),
    (ResourceKind::PhonePatch, "phone-patch", Layout::Protocol),
    (
        ResourceKind::EcholinkProxy,
        "echolink-proxy",
        Layout::Protocol,
    ),
    (
        ResourceKind::Event,
        "event",
        Layout::Collection("schedule/events"),
    ),
    (
        ResourceKind::Macro,
        "macro",
        Layout::Collection("command/macros"),
    ),
    (
        ResourceKind::Message,
        "message",
        Layout::Collection("telemetry/messages"),
    ),
    (
        ResourceKind::Telemetry,
        "telemetry",
        Layout::Collection("telemetry/definitions"),
    ),
    (
        ResourceKind::Sensor,
        "sensor",
        Layout::Collection("sensors"),
    ),
    (ResourceKind::Input, "input", Layout::Collection("inputs")),
    (
        ResourceKind::Output,
        "output",
        Layout::Collection("outputs"),
    ),
    (ResourceKind::Alarm, "alarm", Layout::Collection("alarms")),
    (ResourceKind::Fan, "fan", Layout::Collection("fans")),
    (
        ResourceKind::Mailbox,
        "mailbox",
        Layout::Collection("mailboxes"),
    ),
    (
        ResourceKind::PermanentGroup,
        "permanent-group",
        Layout::Collection("permanent-groups"),
    ),
];

impl ResourceKind {
    /// Iterate the complete format-1 resource inventory in discovery order.
    pub fn catalog() -> impl Iterator<Item = Self> {
        PROFILES.iter().map(|(kind, _, _)| *kind)
    }

    pub(crate) fn profile(self) -> (&'static str, Layout) {
        let (_, name, layout) = PROFILES
            .iter()
            .find(|(kind, _, _)| *kind == self)
            .expect("every supported kind has one profile");
        (name, *layout)
    }
    /// Canonical envelope kind.
    pub fn as_str(self) -> &'static str {
        self.profile().0
    }
    pub(super) fn from_wire(name: &str) -> Option<Self> {
        PROFILES
            .iter()
            .find(|(_, n, _)| *n == name)
            .map(|(kind, _, _)| *kind)
    }
    /// Repeatable member whose stable local key belongs to the registry.
    pub fn is_named(self) -> bool {
        matches!(self.profile().1, Layout::Collection(_))
    }
    /// Port-local protocol and child presence does not create an ordinary slot.
    pub fn has_optional_presence(self) -> bool {
        self.is_protocol() || self == Self::EcholinkProxy || self.is_named()
    }
}

/// A dynamic path part is either a known canonical UUID or an explicit import token.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Member<'a> {
    /// Canonical existing member UUID.
    Existing(ResourceId),
    /// Explicit bounded request for a fresh identity.
    New(&'a str),
}

fn member(s: &str) -> Option<Member<'_>> {
    if let Some(token) = s.strip_prefix("new-") {
        return ((!token.is_empty() && token.len() <= 64)
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
        .then_some(Member::New(token));
    }
    let id: ResourceId = s.parse().ok()?;
    (id.to_string() == s).then_some(Member::Existing(id))
}

/// Decoded file identity independent of all key strings and object contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogFile<'a> {
    /// Owner directory, absent for the site.
    pub port: Option<Member<'a>>,
    /// Authoritative profile for this pathname.
    pub kind: ResourceKind,
    /// Member filename for repeatable resources.
    pub member: Option<Member<'a>>,
}

/// A valid catalog subdirectory or a fully classified resource leaf.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogPath<'a> {
    /// A permitted directory in the resource catalog.
    Directory,
    /// A recognized resource leaf.
    File(CatalogFile<'a>),
}

/// Interpret the exact slash-separated catalog path, excluding working/ itself.
pub fn classify(path: &str) -> Option<CatalogPath<'_>> {
    if path.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return None;
    }
    if path == "ports" || path == "site" {
        return Some(CatalogPath::Directory);
    }
    let (port, tail) = if let Some(tail) = path.strip_prefix("site/") {
        (None, tail)
    } else {
        let rest = path.strip_prefix("ports/")?;
        let Some((part, tail)) = rest.split_once('/') else {
            member(rest)?;
            return Some(CatalogPath::Directory);
        };
        (Some(member(part)?), tail)
    };
    let protocols = if port.is_some() {
        "protocols"
    } else {
        "protocol-defaults"
    };
    for &(kind, name, layout) in PROFILES {
        let body = match layout {
            Layout::File(file) => file.to_string(),
            Layout::Protocol => format!("{protocols}/{name}.json"),
            Layout::Collection(collection) => {
                if let Some(leaf) = tail
                    .strip_prefix(collection)
                    .and_then(|rest| rest.strip_prefix('/'))
                {
                    let member = member(leaf.strip_suffix(".json")?)?;
                    return Some(CatalogPath::File(CatalogFile {
                        port,
                        kind,
                        member: Some(member),
                    }));
                }
                if collection == tail {
                    return Some(CatalogPath::Directory);
                }
                collection.to_owned()
            }
        };
        if body
            .strip_prefix(tail)
            .is_some_and(|suffix| suffix.starts_with('/'))
        {
            return Some(CatalogPath::Directory);
        }
        if body == tail {
            return Some(CatalogPath::File(CatalogFile {
                port,
                kind,
                member: None,
            }));
        }
    }
    None
}

/// Construct a canonical path using only fixed catalog names and assigned UUIDs.
pub fn relative_path(binding: ResourceBinding) -> String {
    let (scope, protocols) = match binding.scope {
        ResourceScope::Site(_) => ("site".to_owned(), "protocol-defaults"),
        ResourceScope::Port(id) => (format!("ports/{id}"), "protocols"),
    };
    match binding.kind.profile().1 {
        Layout::File(file) => format!("{scope}/{file}"),
        Layout::Protocol => format!("{scope}/{protocols}/{}.json", binding.kind.as_str()),
        Layout::Collection(dir) => format!("{scope}/{dir}/{}.json", binding.id),
    }
}
