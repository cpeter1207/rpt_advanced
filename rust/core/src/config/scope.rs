//! Shared section-name classification.

use super::site::SiteKind;

/// One documented section role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    General,
    Node,
    RadioDefault,
    RadioNode,
    RemoteDefault,
    RemoteNode,
    ProcessingDefault,
    ProcessingNode,
    EchoLinkDefault,
    EchoLinkNode,
    IdentifierDefault,
    IdentifierNode,
    IdentifierSet,
    AnnouncementDefault,
    AnnouncementNode,
    AnnouncementSet,
    TailDefault,
    TailNode,
    TailSet,
    CourtesyDefault,
    CourtesyNode,
    CourtesySet,
    MorseDefault,
    MorseNode,
    SpeechDefault,
    SpeechNode,
    TimeDefault,
    TimeNode,
    TemplateGlobal,
    TemplateNode,
    MessageGlobal,
    MessageNode,
    MacroGlobal,
    MacroNode,
    DtmfMacroGlobal,
    DtmfMacroNode,
    EventNode,
    PermanentNode,
    ScheduleNode,
    SiteDefault(SiteKind),
    SiteNode(SiteKind),
    SiteSet(SiteKind),
    Unknown,
}

/// Borrowed interpretation of a section name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scope<'a> {
    pub(crate) kind: ScopeKind,
    pub(crate) node: Option<&'a str>,
    pub(crate) label: Option<&'a str>,
}

impl<'a> Scope<'a> {
    const fn flat(kind: ScopeKind) -> Self {
        Self {
            kind,
            node: None,
            label: None,
        }
    }

    const fn node(kind: ScopeKind, node: &'a str) -> Self {
        Self {
            kind,
            node: Some(node),
            label: None,
        }
    }

    const fn named(kind: ScopeKind, node: Option<&'a str>, label: &'a str) -> Self {
        Self {
            kind,
            node,
            label: Some(label),
        }
    }
}

/// Classify one exact section name. A malformed documented shape is an error;
/// an unrelated future shape remains safely ignorable.
pub(crate) fn parse_scope(name: &str) -> Result<Scope<'_>, ()> {
    if name.is_empty() || name.chars().any(|ch| matches!(ch, '[' | ']' | '\r' | '\n')) {
        return Err(());
    }
    let (family, suffix) = name
        .split_once(' ')
        .map_or((name, None), |(family, suffix)| (family, Some(suffix)));
    if [
        "local",
        "link",
        "voice_telemetry",
        "hardware",
        "receive",
        "transmit",
        "ctcss",
        "dcs",
        "duplex",
        "diagnostics",
    ]
    .iter()
    .any(|known| family.eq_ignore_ascii_case(known))
    {
        return match suffix {
            None => Ok(Scope::flat(ScopeKind::ProcessingDefault)),
            Some(profile) if !profile.is_empty() => {
                Ok(Scope::node(ScopeKind::ProcessingNode, profile))
            }
            Some(_) => Err(()),
        };
    }
    if family.eq_ignore_ascii_case("radio") || family.eq_ignore_ascii_case("remote_base") {
        let (global, scoped) = if family.eq_ignore_ascii_case("radio") {
            (ScopeKind::RadioDefault, ScopeKind::RadioNode)
        } else {
            (ScopeKind::RemoteDefault, ScopeKind::RemoteNode)
        };
        return match suffix {
            None => Ok(Scope::flat(global)),
            Some(node) if !node.is_empty() && !node.chars().any(char::is_whitespace) => {
                Ok(Scope::node(scoped, node))
            }
            Some(_) => Err(()),
        };
    }
    if family.eq_ignore_ascii_case("remote_memory") || family.eq_ignore_ascii_case("remote_scan") {
        return match suffix.and_then(|value| value.split_once(' ')) {
            Some((node, label)) if !node.is_empty() && !label.trim().is_empty() => {
                Ok(Scope::named(ScopeKind::RemoteNode, Some(node), label))
            }
            _ => Err(()),
        };
    }
    if name.contains('\t') {
        return Err(());
    }
    let parts: Vec<_> = name.split(' ').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(());
    }
    let scope = match parts.as_slice() {
        [family] if SiteKind::parse(family).is_some() => {
            Scope::flat(ScopeKind::SiteDefault(SiteKind::parse(family).unwrap()))
        }
        [family, node] if SiteKind::parse(family).is_some() => {
            Scope::node(ScopeKind::SiteNode(SiteKind::parse(family).unwrap()), node)
        }
        [family, node, label] if SiteKind::parse(family).is_some() => Scope::named(
            ScopeKind::SiteSet(SiteKind::parse(family).unwrap()),
            Some(node),
            label,
        ),
        [family, ..] if SiteKind::parse(family).is_some() => return Err(()),
        ["general"] => Scope::flat(ScopeKind::General),
        ["echolink"] => Scope::flat(ScopeKind::EchoLinkDefault),
        ["echolink", node] => Scope::node(ScopeKind::EchoLinkNode, node),
        ["identifier"] => Scope::flat(ScopeKind::IdentifierDefault),
        ["identifier", node] => Scope::node(ScopeKind::IdentifierNode, node),
        ["identifier", node, label] => Scope::named(ScopeKind::IdentifierSet, Some(node), label),
        ["announcement"] => Scope::flat(ScopeKind::AnnouncementDefault),
        ["announcement", node] => Scope::node(ScopeKind::AnnouncementNode, node),
        ["announcement", node, label] => {
            Scope::named(ScopeKind::AnnouncementSet, Some(node), label)
        }
        ["courtesy"] => Scope::flat(ScopeKind::CourtesyDefault),
        ["tail"] => Scope::flat(ScopeKind::TailDefault),
        ["tail", node] => Scope::node(ScopeKind::TailNode, node),
        ["tail", node, label] => Scope::named(ScopeKind::TailSet, Some(node), label),
        ["courtesy", node] => Scope::node(ScopeKind::CourtesyNode, node),
        ["courtesy", node, label] => Scope::named(ScopeKind::CourtesySet, Some(node), label),
        ["morse"] => Scope::flat(ScopeKind::MorseDefault),
        ["morse", node] => Scope::node(ScopeKind::MorseNode, node),
        ["speech"] => Scope::flat(ScopeKind::SpeechDefault),
        ["speech", node] => Scope::node(ScopeKind::SpeechNode, node),
        ["time"] => Scope::flat(ScopeKind::TimeDefault),
        ["time", node] => Scope::node(ScopeKind::TimeNode, node),
        ["template", label] => Scope::named(ScopeKind::TemplateGlobal, None, label),
        ["template", node, label] => Scope::named(ScopeKind::TemplateNode, Some(node), label),
        ["message", label] => Scope::named(ScopeKind::MessageGlobal, None, label),
        ["message", node, label] => Scope::named(ScopeKind::MessageNode, Some(node), label),
        ["macro", label] => Scope::named(ScopeKind::MacroGlobal, None, label),
        ["macro", node, label] => Scope::named(ScopeKind::MacroNode, Some(node), label),
        ["dtmf_macro", label] => Scope::named(ScopeKind::DtmfMacroGlobal, None, label),
        ["dtmf_macro", node, label] => Scope::named(ScopeKind::DtmfMacroNode, Some(node), label),
        ["event", node, label] => Scope::named(ScopeKind::EventNode, Some(node), label),
        ["permanent", node, label] => Scope::named(ScopeKind::PermanentNode, Some(node), label),
        ["schedule", node, label] => Scope::named(ScopeKind::ScheduleNode, Some(node), label),
        [known]
            if matches!(
                *known,
                "template"
                    | "message"
                    | "macro"
                    | "dtmf_macro"
                    | "event"
                    | "permanent"
                    | "schedule"
            ) =>
        {
            return Err(());
        }
        [single] => Scope::node(ScopeKind::Node, single),
        [known, ..]
            if matches!(
                *known,
                "general"
                    | "echolink"
                    | "radio"
                    | "local"
                    | "link"
                    | "voice_telemetry"
                    | "identifier"
                    | "announcement"
                    | "tail"
                    | "courtesy"
                    | "morse"
                    | "speech"
                    | "time"
                    | "template"
                    | "message"
                    | "macro"
                    | "dtmf_macro"
                    | "event"
                    | "permanent"
                    | "schedule"
            ) =>
        {
            return Err(());
        }
        _ => Scope::flat(ScopeKind::Unknown),
    };
    Ok(scope)
}

pub(crate) const fn is_named(kind: ScopeKind) -> bool {
    matches!(
        kind,
        ScopeKind::IdentifierSet
            | ScopeKind::SiteSet(_)
            | ScopeKind::AnnouncementSet
            | ScopeKind::TailSet
            | ScopeKind::CourtesySet
            | ScopeKind::TemplateGlobal
            | ScopeKind::TemplateNode
            | ScopeKind::MessageGlobal
            | ScopeKind::MessageNode
            | ScopeKind::MacroGlobal
            | ScopeKind::MacroNode
            | ScopeKind::DtmfMacroGlobal
            | ScopeKind::DtmfMacroNode
            | ScopeKind::EventNode
            | ScopeKind::PermanentNode
            | ScopeKind::ScheduleNode
    )
}

pub(crate) const fn is_node_scoped(kind: ScopeKind) -> bool {
    matches!(
        kind,
        ScopeKind::IdentifierNode
            | ScopeKind::SiteNode(_)
            | ScopeKind::SiteSet(_)
            | ScopeKind::RadioNode
            | ScopeKind::RemoteNode
            | ScopeKind::EchoLinkNode
            | ScopeKind::IdentifierSet
            | ScopeKind::AnnouncementNode
            | ScopeKind::TailNode
            | ScopeKind::TailSet
            | ScopeKind::AnnouncementSet
            | ScopeKind::CourtesyNode
            | ScopeKind::CourtesySet
            | ScopeKind::MorseNode
            | ScopeKind::SpeechNode
            | ScopeKind::TimeNode
            | ScopeKind::TemplateNode
            | ScopeKind::MessageNode
            | ScopeKind::MacroNode
            | ScopeKind::DtmfMacroNode
            | ScopeKind::EventNode
            | ScopeKind::PermanentNode
            | ScopeKind::ScheduleNode
    )
}
