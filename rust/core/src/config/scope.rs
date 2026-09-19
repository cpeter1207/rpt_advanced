//! Shared section-name classification.

/// One documented section role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    General,
    Node,
    IdentifierDefault,
    IdentifierNode,
    IdentifierSet,
    AnnouncementDefault,
    AnnouncementNode,
    AnnouncementSet,
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
    MacroGlobal,
    MacroNode,
    EventNode,
    PermanentNode,
    ScheduleNode,
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
    if name.is_empty()
        || name
            .chars()
            .any(|ch| matches!(ch, '[' | ']' | '\t' | '\r' | '\n'))
    {
        return Err(());
    }
    let parts: Vec<_> = name.split(' ').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(());
    }
    let scope = match parts.as_slice() {
        ["general"] => Scope::flat(ScopeKind::General),
        ["identifier"] => Scope::flat(ScopeKind::IdentifierDefault),
        ["identifier", node] => Scope::node(ScopeKind::IdentifierNode, node),
        ["identifier", node, label] => Scope::named(ScopeKind::IdentifierSet, Some(node), label),
        ["announcement"] => Scope::flat(ScopeKind::AnnouncementDefault),
        ["announcement", node] => Scope::node(ScopeKind::AnnouncementNode, node),
        ["announcement", node, label] => {
            Scope::named(ScopeKind::AnnouncementSet, Some(node), label)
        }
        ["courtesy"] => Scope::flat(ScopeKind::CourtesyDefault),
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
        ["macro", label] => Scope::named(ScopeKind::MacroGlobal, None, label),
        ["macro", node, label] => Scope::named(ScopeKind::MacroNode, Some(node), label),
        ["event", node, label] => Scope::named(ScopeKind::EventNode, Some(node), label),
        ["permanent", node, label] => Scope::named(ScopeKind::PermanentNode, Some(node), label),
        ["schedule", node, label] => Scope::named(ScopeKind::ScheduleNode, Some(node), label),
        [known]
            if matches!(
                *known,
                "template" | "macro" | "event" | "permanent" | "schedule"
            ) =>
        {
            return Err(());
        }
        [single] => Scope::node(ScopeKind::Node, single),
        [known, ..]
            if matches!(
                *known,
                "general"
                    | "identifier"
                    | "announcement"
                    | "courtesy"
                    | "morse"
                    | "speech"
                    | "time"
                    | "template"
                    | "macro"
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
            | ScopeKind::AnnouncementSet
            | ScopeKind::CourtesySet
            | ScopeKind::TemplateGlobal
            | ScopeKind::TemplateNode
            | ScopeKind::MacroGlobal
            | ScopeKind::MacroNode
            | ScopeKind::EventNode
            | ScopeKind::PermanentNode
            | ScopeKind::ScheduleNode
    )
}

pub(crate) const fn is_node_scoped(kind: ScopeKind) -> bool {
    matches!(
        kind,
        ScopeKind::IdentifierNode
            | ScopeKind::IdentifierSet
            | ScopeKind::AnnouncementNode
            | ScopeKind::AnnouncementSet
            | ScopeKind::CourtesyNode
            | ScopeKind::CourtesySet
            | ScopeKind::MorseNode
            | ScopeKind::SpeechNode
            | ScopeKind::TimeNode
            | ScopeKind::TemplateNode
            | ScopeKind::MacroNode
            | ScopeKind::EventNode
            | ScopeKind::PermanentNode
            | ScopeKind::ScheduleNode
    )
}
