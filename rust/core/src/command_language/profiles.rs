//! Prepare immutable per-invoking-scope mappings and macro sequences after complete authoring edits.
use super::{
    Action, ActionError, Completed, Mapping, PreparedAction, binding::Decoder,
    prepare_macro_action, shape,
};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};
type Obj = Map<String, Value>;
static DEFAULTS: LazyLock<Obj> = LazyLock::new(|| {
    serde_json::from_str(include_str!("defaults.json")).expect("canonical DTMF defaults")
});

/// Value-free effective-profile validation failure.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LanguageError {
    /// Unusable required fields after inherited fallback.
    Incomplete,
    /// An enabled code duplicates another complete code or includes the terminator.
    CodeCollision,
    /// A macro key has no effective definition.
    MissingMacro,
    /// A static call graph is recursive.
    Cycle,
    /// Authored input attempts to persist credentials or fabricate an ingress owner.
    SourcePolicy,
    /// Effective domain resource referenced by an action does not exist.
    MissingResource,
}
/// Recoverable authored field diagnostics without supplied keys, identifiers or values.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LanguageWarning {
    /// Unknown or stale variant field was omitted.
    Unknown,
    /// An invalid field retained its inherited setting.
    Invalid(&'static str),
}
/// Whole invocation limits, shared across all nested calls and isolated DTMF collectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacroLimits {
    /// Default pause duration for an action without explicit timing.
    pub pause_ms: u64,
    /// Monotonic elapsed-time budget.
    pub max_duration_ms: u64,
    /// Injected symbol budget.
    pub max_symbols: usize,
    /// Expanded action count, including nested calls and pauses.
    pub max_actions: usize,
    /// Initial macro has depth one.
    pub max_depth: usize,
}
impl Default for MacroLimits {
    fn default() -> Self {
        Self {
            pause_ms: 2000,
            max_duration_ms: 120000,
            max_symbols: 4096,
            max_actions: 4096,
            max_depth: 8,
        }
    }
}
/// Prepared mappings, named macros and collector settings for the invoking scope.
pub struct CommandLanguage {
    /// Immutable mapping inventory, including disabled masks.
    pub mappings: BTreeMap<String, Mapping>,
    /// Prepared nonempty ordered action lists keyed by exact resource name.
    pub macros: BTreeMap<String, Vec<PreparedAction>>,
    /// Literal completion symbol.
    pub terminator: char,
    /// Positive interval measured from the last buffered symbol.
    pub inactivity_timeout_ms: u64,
    /// Buffer bound, excluding the literal terminator.
    pub maximum_digits: usize,
    /// Frozen budget configuration for new invocations.
    pub limits: MacroLimits,
    /// Value-free recoverable diagnostics accumulated while resolving both layers.
    pub warnings: Vec<LanguageWarning>,
    /// Accepted recognized sparse overrides in site, then invoking-port order.
    pub local: [Value; 2],
}
fn checked_uint(
    v: Option<&Value>,
    key: &'static str,
    current: u64,
    max: u64,
    w: &mut Vec<LanguageWarning>,
) -> u64 {
    let Some(v) = v else { return current };
    v.as_u64()
        .filter(|n| (1..=max).contains(n))
        .unwrap_or_else(|| {
            w.push(LanguageWarning::Invalid(key));
            current
        })
}
fn object<'a>(
    v: Option<&'a Value>,
    key: &'static str,
    w: &mut Vec<LanguageWarning>,
) -> Option<&'a Obj> {
    v.and_then(|v| {
        v.as_object().or_else(|| {
            w.push(LanguageWarning::Invalid(key));
            None
        })
    })
}
fn named(key: &str) -> bool {
    !key.is_empty() && key.chars().count() <= 63
}
fn selected(m: &Obj) -> Option<&'static [&'static str]> {
    let action = m.get("action")?.as_str()?;
    let operation = match m.get("operation") {
        Some(v) if action == "radio" => Some(v.as_str()?),
        v => v.and_then(Value::as_str),
    };
    shape::fields(action, operation, m.get("target").and_then(Value::as_str))
}
fn merge(base: &mut Obj, overlay: &Obj) {
    for (k, v) in overlay {
        if let Some(Value::Object(old)) = base.get_mut(k)
            && let Value::Object(new) = v
        {
            merge(old, new);
        } else {
            base.insert(k.clone(), v.clone());
        }
    }
}
fn normalize_nested(
    sparse: &mut Obj,
    action: Option<&str>,
    operation: Option<&str>,
    w: &mut Vec<LanguageWarning>,
) {
    if action == Some("radio")
        && operation == Some("set")
        && let Some(v) = sparse.get_mut("state")
        && !normalize_radio(v, &mut Vec::new(), w)
    {
        sparse.remove("state");
        w.push(LanguageWarning::Invalid("state"));
    }
    if action == Some("interface_control")
        && operation == Some("write_pin")
        && let Some(Value::Object(pin)) = sparse.get_mut("pin")
    {
        pin.retain(|k, v| match k.as_str() {
            "domain" if matches!(v.as_str(), Some("sound_gpio" | "parallel")) => true,
            "pin" if v.as_u64().is_some_and(|n| (1..=9).contains(&n)) => true,
            "pin" | "domain" => {
                w.push(LanguageWarning::Invalid("pin"));
                false
            }
            _ => {
                w.push(LanguageWarning::Unknown);
                false
            }
        });
    }
    if (action == Some("pause")
        || matches!(
            (action, operation),
            (
                Some("radio"),
                Some("set_scan_dwell" | "set_scan_resume_delay")
            ) | (Some("interface_control"), Some("write_pin"))
        ))
        && let Some(t) = sparse.get_mut("timing")
    {
        if let Some(obj) = t.as_object_mut() {
            obj.retain(|k, _| {
                if k == "duration_ms" {
                    true
                } else {
                    w.push(LanguageWarning::Unknown);
                    false
                }
            });
            let range = match (action, operation) {
                (Some("pause"), _) => 1..=120000,
                (_, Some("set_scan_resume_delay")) => 0..=u64::MAX,
                (_, Some("write_pin")) => 100..=429496729400,
                _ => 1..=u64::MAX,
            };
            if obj.get("duration_ms").is_some_and(|v| {
                v.as_u64().is_none_or(|n| {
                    !range.contains(&n) || operation == Some("write_pin") && n % 100 != 0
                })
            }) {
                obj.remove("duration_ms");
                w.push(LanguageWarning::Invalid("duration_ms"));
            }
        } else {
            sparse.remove("timing");
            w.push(LanguageWarning::Invalid("timing"));
        }
    }
}

/// Normalize a sparse radio subtree using the same exact native decoder for each leaf.
fn normalize_radio(
    v: &mut Value,
    path: &mut Vec<&'static str>,
    w: &mut Vec<LanguageWarning>,
) -> bool {
    let fields: &[&'static str] = match path.as_slice() {
        [] => &[
            "mode",
            "rx_vfo",
            "tx_vfo",
            "split",
            "rit_enabled",
            "xit_enabled",
            "power_mw",
            "channel",
            "rx_frequency_mhz",
            "tx_frequency_mhz",
            "bandwidth_mhz",
            "offset_mhz",
            "rit_mhz",
            "xit_mhz",
            "encode",
            "decode",
        ],
        ["encode" | "decode"] => &["source", "control_source", "enabled", "ctcss", "dcs"],
        ["encode" | "decode", "ctcss"] => &["frequency_hz"],
        ["encode" | "decode", "dcs"] => &["bits", "inverted"],
        _ => {
            let leaf = path.iter().rev().fold(v.clone(), |child, &k| {
                Value::Object(Obj::from_iter([(k.to_owned(), child)]))
            });
            return super::radio::operating(&leaf).is_ok();
        }
    };
    let Some(obj) = v.as_object_mut() else {
        return false;
    };
    obj.retain(|k, child| {
        let Some(&field) = fields.iter().find(|&&f| f == k) else {
            w.push(LanguageWarning::Unknown);
            return false;
        };
        path.push(field);
        let valid = normalize_radio(child, path, w);
        path.pop();
        if !valid {
            w.push(LanguageWarning::Invalid(field));
        }
        valid
    });
    true
}
fn mapping_layer(
    key: &str,
    parent: &Obj,
    incoming: &Obj,
    w: &mut Vec<LanguageWarning>,
) -> Result<(Obj, Obj), LanguageError> {
    if incoming.contains_key("parameter")
        || incoming.contains_key("code")
        || incoming.get("pin").is_some_and(|v| !v.is_object())
    {
        return Err(LanguageError::SourcePolicy);
    }
    let mut sparse = incoming.clone();
    for key in ["command", "protected"] {
        let valid = |v: &Value| {
            if key == "command" {
                v.as_str()
                    .is_some_and(|s| s.len() <= 63 && s.chars().all(crate::link::valid_digit))
            } else {
                v.is_boolean()
            }
        };
        if sparse.get(key).is_some_and(|v| !valid(v)) {
            sparse.remove(key);
            w.push(LanguageWarning::Invalid(key));
        }
    }
    let mut inherited = parent.clone();
    let changed_action = sparse
        .get("action")
        .is_some_and(|a| Some(a) != parent.get("action"));
    let action = sparse
        .get("action")
        .or_else(|| parent.get("action"))
        .and_then(Value::as_str);
    let selector = match action {
        Some("rx_mute_toggle") => Some("target"),
        Some(
            "radio"
            | "controller_query"
            | "radio_maintenance"
            | "controller_management"
            | "controller_lifecycle"
            | "radio_host_query"
            | "phone_session"
            | "controller_admission"
            | "interface_control"
            | "controller_input",
        ) => Some("operation"),
        _ => None,
    };
    let changed = changed_action
        || selector.is_some_and(|k| sparse.get(k).is_some_and(|v| Some(v) != parent.get(k)));
    if changed {
        inherited.retain(|k, _| {
            matches!(k.as_str(), "command" | "protected") || !changed_action && k == "action"
        });
    }
    let mut candidate = inherited.clone();
    merge(&mut candidate, &sparse);
    if selected(&candidate).is_none() && changed && selected(parent).is_some() {
        // A bad discriminator cannot reinterpret dependent fields under the old variant.
        sparse.retain(|k, _| matches!(k.as_str(), "command" | "protected"));
        inherited = parent.clone();
        candidate = inherited.clone();
        merge(&mut candidate, &sparse);
        w.push(LanguageWarning::Invalid(if changed_action {
            "action"
        } else {
            selector.unwrap_or("action")
        }));
    }
    if let Some(fields) = selected(&candidate) {
        normalize_nested(
            &mut sparse,
            candidate.get("action").and_then(Value::as_str),
            candidate.get("operation").and_then(Value::as_str),
            w,
        );
        sparse.retain(|k, _| {
            let ok = matches!(k.as_str(), "action" | "command" | "protected")
                || fields.contains(&k.as_str());
            if !ok {
                w.push(LanguageWarning::Unknown);
            }
            ok
        });
        loop {
            candidate = inherited.clone();
            merge(&mut candidate, &sparse);
            match Mapping::prepare(key, &Value::Object(candidate.clone())) {
                Err(ActionError::Field(field))
                    if sparse.contains_key(field)
                        && field != "action"
                        && Some(field) != selector =>
                {
                    sparse.remove(field);
                    w.push(LanguageWarning::Invalid(field));
                }
                Err(ActionError::SourceOnly) => return Err(LanguageError::SourcePolicy),
                _ => break,
            }
        }
    }
    Ok((candidate, sparse))
}
fn macro_actions(
    v: &Value,
    w: &mut Vec<LanguageWarning>,
) -> Result<(Vec<PreparedAction>, Value), LanguageError> {
    let array = v
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 4096)
        .ok_or(LanguageError::Incomplete)?;
    let mut out = Vec::with_capacity(array.len());
    let mut accepted = Vec::with_capacity(array.len());
    for action in array {
        let mut obj = action.as_object().ok_or(LanguageError::Incomplete)?.clone();
        let fields = selected(&obj).ok_or(LanguageError::Incomplete)?;
        let kind = obj.get("action").and_then(Value::as_str).map(str::to_owned);
        let op = obj
            .get("operation")
            .and_then(Value::as_str)
            .map(str::to_owned);
        normalize_nested(&mut obj, kind.as_deref(), op.as_deref(), w);
        obj.retain(|k, _| {
            if matches!(k.as_str(), "action" | "parameter" | "code" | "pin")
                || fields.contains(&k.as_str())
                || shape::known_field(k)
            {
                return true;
            }
            w.push(LanguageWarning::Unknown);
            false
        });
        let mut body = Value::Object(obj);
        loop {
            match prepare_macro_action(&body) {
                Ok(action) => {
                    out.push(action);
                    break;
                }
                Err(ActionError::Field(field))
                    if fields.contains(&field) && body.get(field).is_some() =>
                {
                    // An optional invalid field can use the parser's own default. Removing a
                    // required field still leaves an incomplete action on the next pass.
                    body.as_object_mut().expect("action object").remove(field);
                    w.push(LanguageWarning::Invalid(field));
                }
                Err(ActionError::SourceOnly) => return Err(LanguageError::SourcePolicy),
                Err(_) => return Err(LanguageError::Incomplete),
            }
        }
        accepted.push(body);
    }
    Ok((out, Value::Array(accepted)))
}
impl CommandLanguage {
    /// All mandatory compatibility bindings with built-in completion and budget settings.
    pub fn standard() -> Self {
        Self::resolve(&serde_json::json!({}), &serde_json::json!({}))
            .expect("valid canonical command defaults")
    }
    /// Resolve two command projections after all JSON Patch operations. Named resources
    /// are supplied under macros by the in-memory registry adapter only.
    pub(crate) fn resolve(site: &Value, port: &Value) -> Result<Self, LanguageError> {
        let mut result = Self {
            mappings: BTreeMap::new(),
            macros: BTreeMap::new(),
            terminator: '#',
            inactivity_timeout_ms: 3000,
            maximum_digits: 128,
            limits: MacroLimits::default(),
            warnings: vec![],
            local: Default::default(),
        };
        let mut mappings = DEFAULTS.clone();
        let mut keys = BTreeSet::new();
        for (i, layer) in [site, port].into_iter().enumerate() {
            let o = layer.as_object().ok_or(LanguageError::Incomplete)?;
            let mut local = Obj::new();
            result.settings(o, &mut local);
            let w = &mut result.warnings;
            if let Some(ms) = object(o.get("mappings"), "mappings", w) {
                let mut clean = Obj::new();
                for (key, v) in ms {
                    if !named(key) {
                        w.push(LanguageWarning::Invalid("mappings"));
                        continue;
                    }
                    let Some(o) = object(Some(v), "mappings", w) else {
                        continue;
                    };
                    let empty = Obj::new();
                    let base = mappings
                        .get(key)
                        .and_then(Value::as_object)
                        .unwrap_or(&empty);
                    let (merged, sparse) = mapping_layer(key, base, o, w)?;
                    mappings.insert(key.clone(), Value::Object(merged));
                    clean.insert(key.clone(), Value::Object(sparse));
                }
                local.insert("mappings".into(), Value::Object(clean));
            }
            if let Some(macros) = object(o.get("macros"), "macros", w) {
                let mut clean = Obj::new();
                for (key, v) in macros {
                    if !named(key) {
                        return Err(LanguageError::Incomplete);
                    }
                    keys.insert(key.clone());
                    let mut sparse = Obj::new();
                    if let Some(body) = object(Some(v), "macros", w) {
                        for k in body.keys().filter(|k| k.as_str() != "actions") {
                            let _ = k;
                            w.push(LanguageWarning::Unknown);
                        }
                        if let Some(actions) = body.get("actions") {
                            match macro_actions(actions, w) {
                                Ok((prepared, accepted)) => {
                                    result.macros.insert(key.clone(), prepared);
                                    sparse.insert("actions".into(), accepted);
                                }
                                Err(LanguageError::SourcePolicy) => {
                                    return Err(LanguageError::SourcePolicy);
                                }
                                Err(_) => w.push(LanguageWarning::Invalid("actions")),
                            }
                        }
                    }
                    clean.insert(key.clone(), Value::Object(sparse));
                }
                local.insert("macros".into(), Value::Object(clean));
            }
            result.local[i] = Value::Object(local);
        }
        if keys.iter().any(|key| !result.macros.contains_key(key)) {
            return Err(LanguageError::Incomplete);
        }
        let mut codes = BTreeSet::new();
        for (key, obj) in mappings {
            let m = Mapping::prepare(&key, &obj).map_err(|e| {
                if e == ActionError::SourceOnly {
                    LanguageError::SourcePolicy
                } else {
                    LanguageError::Incomplete
                }
            })?;
            if !m.command.is_empty()
                && (m.command.contains(result.terminator) || !codes.insert(m.command.clone()))
            {
                return Err(LanguageError::CodeCollision);
            }
            if let Decoder::Fixed(Action::CallMacro(key)) = &m.decoder
                && !result.macros.contains_key(key)
            {
                return Err(LanguageError::MissingMacro);
            }
            result.mappings.insert(key, m);
        }
        result.validate_calls()?;
        Ok(result)
    }
    /// Choose one longest enabled code. A rejected match never falls back to a shorter code.
    pub fn match_completed<'a>(&'a self, digits: &'a str) -> Option<Completed<'a>> {
        self.mappings
            .values()
            .filter(|m| !m.command.is_empty() && digits.starts_with(&m.command))
            .max_by_key(|m| m.command.len())
            .map(|m| Completed {
                mapping: m,
                parameter: &digits[m.command.len()..],
            })
    }
    pub(crate) fn static_actions(&self) -> impl Iterator<Item = &Action> {
        self.macros
            .values()
            .flatten()
            .map(|p| &p.action)
            .chain(self.mappings.values().filter_map(|m| match &m.decoder {
                Decoder::Fixed(a) => Some(a),
                _ => None,
            }))
    }
    fn validate_calls(&self) -> Result<(), LanguageError> {
        #[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
        enum Node<'a> {
            Macro(&'a str),
            Injection(&'a str),
        }
        fn edge(action: &Action) -> Option<Node<'_>> {
            match action {
                Action::CallMacro(key) => Some(Node::Macro(key)),
                Action::ExecuteDtmf(digits) => Some(Node::Injection(digits)),
                _ => None,
            }
        }

        let mut children: BTreeMap<Node<'_>, BTreeSet<Node<'_>>> = BTreeMap::new();
        for (key, actions) in &self.macros {
            let outgoing = children.entry(Node::Macro(key)).or_default();
            for child in actions.iter().filter_map(|p| edge(&p.action)) {
                if let Node::Macro(key) = child
                    && !self.macros.contains_key(key)
                {
                    return Err(LanguageError::MissingMacro);
                }
                outgoing.insert(child);
            }
        }
        // Check even shadowed public literals: both accepted sparse layers can be published.
        let mut literals = BTreeSet::new();
        for layer in &self.local {
            if let Some(mappings) = layer["mappings"].as_object() {
                literals.extend(mappings.values().filter_map(|m| m["digits"].as_str()));
            }
            if let Some(macros) = layer["macros"].as_object() {
                literals.extend(
                    macros
                        .values()
                        .filter_map(|m| m["actions"].as_array())
                        .flatten()
                        .filter_map(|a| a["digits"].as_str()),
                );
            }
        }
        literals.extend(self.static_actions().filter_map(|action| match action {
            Action::ExecuteDtmf(d) => Some(d.as_str()),
            _ => None,
        }));
        for digits in literals {
            let outgoing = children.entry(Node::Injection(digits)).or_default();
            for candidate in digits.split(self.terminator) {
                let Some(m) = self.match_completed(candidate) else {
                    continue;
                };
                match &m.mapping.decoder {
                    Decoder::Administration(true) | Decoder::Fixed(Action::PadTest) => {
                        return Err(LanguageError::SourcePolicy);
                    }
                    Decoder::Administration(false) if !m.parameter.is_empty() => {
                        return Err(LanguageError::SourcePolicy);
                    }
                    // Only complete, non-overflowing fixed matches accept this tail.
                    Decoder::Fixed(a)
                        if candidate.len() <= self.maximum_digits && m.parameter.is_empty() =>
                    {
                        if let Some(child) = edge(a) {
                            outgoing.insert(child);
                        }
                    }
                    _ => (),
                }
            }
        }
        // Kahn's algorithm is iterative even for untrusted, deeply nested graphs.
        let mut parents: BTreeMap<_, Vec<_>> = BTreeMap::new();
        let mut remaining = BTreeMap::new();
        for (node, deps) in children {
            remaining.insert(node, deps.len());
            for child in deps {
                parents.entry(child).or_default().push(node);
            }
        }
        let mut ready: Vec<_> = remaining
            .iter()
            .filter_map(|(&k, &n)| (n == 0).then_some(k))
            .collect();
        let mut done = 0;
        while let Some(k) = ready.pop() {
            done += 1;
            for parent in parents.get(&k).into_iter().flatten() {
                let n = remaining.get_mut(parent).expect("existing parent");
                *n -= 1;
                if *n == 0 {
                    ready.push(*parent);
                }
            }
        }
        if done == remaining.len() {
            Ok(())
        } else {
            Err(LanguageError::Cycle)
        }
    }
    fn settings(&mut self, o: &Obj, local: &mut Obj) {
        let w = &mut self.warnings;
        if let Some(t) = o.get("terminator") {
            if let Some(c) = t
                .as_str()
                .filter(|s| s.len() == 1)
                .and_then(|s| s.chars().next())
                .filter(|c| crate::link::valid_digit(*c))
            {
                self.terminator = c;
                local.insert("terminator".into(), t.clone());
            } else {
                w.push(LanguageWarning::Invalid("terminator"));
            }
        }
        for (key, current, max) in [
            ("maximum_digits", self.maximum_digits as u64, 4096),
            (
                "inactivity_timeout_ms",
                self.inactivity_timeout_ms,
                u64::MAX,
            ),
        ] {
            let before = w.len();
            let n = checked_uint(o.get(key), key, current, max, w);
            if key == "maximum_digits" {
                self.maximum_digits = n as usize
            } else {
                self.inactivity_timeout_ms = n;
            }
            if let Some(v) = o.get(key)
                && w.len() == before
            {
                local.insert(key.into(), v.clone());
            }
        }
        if let Some(l) = object(o.get("macro_limits"), "macro_limits", w) {
            let mut clean = Obj::new();
            for (k, current, max) in [
                ("max_symbols", &mut self.limits.max_symbols, 4096),
                ("max_actions", &mut self.limits.max_actions, 4096),
                ("max_depth", &mut self.limits.max_depth, 32),
            ] {
                let before = w.len();
                *current = checked_uint(l.get(k), k, *current as u64, max, w) as usize;
                if l.contains_key(k) && before == w.len() {
                    clean.insert(k.into(), Value::from(*current));
                }
            }
            if let Some(t) = object(l.get("timing"), "timing", w) {
                let mut timing = Obj::new();
                for (k, current) in [
                    ("pause_ms", &mut self.limits.pause_ms),
                    ("max_duration_ms", &mut self.limits.max_duration_ms),
                ] {
                    let before = w.len();
                    *current = checked_uint(t.get(k), k, *current, 120000, w);
                    if t.contains_key(k) && before == w.len() {
                        timing.insert(k.into(), Value::from(*current));
                    }
                }
                w.extend(
                    t.keys()
                        .filter(|k| !matches!(k.as_str(), "pause_ms" | "max_duration_ms"))
                        .map(|_| LanguageWarning::Unknown),
                );
                clean.insert("timing".into(), Value::Object(timing));
            }
            w.extend(
                l.keys()
                    .filter(|k| {
                        !matches!(
                            k.as_str(),
                            "max_symbols" | "max_actions" | "max_depth" | "timing"
                        )
                    })
                    .map(|_| LanguageWarning::Unknown),
            );
            local.insert("macro_limits".into(), Value::Object(clean));
        }
        w.extend(
            o.keys()
                .filter(|k| {
                    !matches!(
                        k.as_str(),
                        "macro_limits"
                            | "mappings"
                            | "macros"
                            | "terminator"
                            | "maximum_digits"
                            | "inactivity_timeout_ms"
                            | "muting"
                            | "admin"
                            | "regeneration"
                    )
                })
                .map(|_| LanguageWarning::Unknown),
        );
    }
}
