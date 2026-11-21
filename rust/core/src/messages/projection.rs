//! Resolve native JSON attributes in the same Fluent bundle as their dependencies.

use super::patterns::{NativePattern, NativeReference, PatternDependencies, identifier};
use super::{
    ATTRIBUTES, CatalogError, CatalogLayer, MessageCatalog, REQUIRED_IDS, parse_bundle,
    required_sample_args,
};
use crate::config::BindingSource;
use fluent_syntax::{ast, parser, serializer};
use std::collections::{BTreeMap, BTreeSet};
use unic_langid::LanguageIdentifier;

/// JSON-localized public entry after field-specific configuration inheritance.
#[derive(Clone, Debug, Default)]
pub struct LocalizedMessage {
    /// Canonical language IDs and their individually parsed attributes.
    pub locales: BTreeMap<LanguageIdentifier, BTreeMap<String, NativePattern>>,
    /// Checked flat aliases declared by this message.
    pub bindings: BTreeMap<String, BindingSource>,
}

/// Required sources collected through every native reference of a message.
pub type MessageBindings = BTreeMap<String, BTreeMap<String, BindingSource>>;

pub(crate) struct RejectedAttribute {
    pub(crate) name: String,
    pub(crate) language: LanguageIdentifier,
    pub(crate) form: String,
    pub(crate) inherited: bool,
}

type Forms = BTreeMap<NativeReference, NativePattern>;
pub(super) type Dependencies = BTreeMap<NativeReference, PatternDependencies>;

fn dependencies(forms: &Forms) -> Dependencies {
    forms
        .iter()
        .map(|(key, pattern)| (key.clone(), pattern.dependencies()))
        .collect()
}

fn key(name: &str, attribute: Option<&str>, term: bool) -> NativeReference {
    NativeReference {
        term,
        name: name.into(),
        attribute: attribute.map(str::to_owned),
    }
}

fn installed(layer: &CatalogLayer, forms: &mut Forms) -> Result<(), CatalogError> {
    let ast = parser::parse_runtime(layer.resource.source().to_owned())
        .map_err(|_| CatalogError::InvalidMessage)?;
    for entry in ast.body {
        let (term, id, value, attributes) = match entry {
            ast::Entry::Message(m) => (false, m.id.name, m.value, m.attributes),
            ast::Entry::Term(t) => (true, t.id.name, Some(t.value), t.attributes),
            _ => continue,
        };
        if let Some(value) = value {
            forms.insert(key(&id, None, term), NativePattern(value));
        }
        for attribute in attributes {
            forms.insert(
                key(&id, Some(&attribute.id.name), term),
                NativePattern(attribute.value),
            );
        }
    }
    Ok(())
}

fn bundle(locale: &LanguageIdentifier, forms: Forms) -> Result<CatalogLayer, CatalogError> {
    let mut entries = BTreeMap::<
        (bool, String),
        (Option<ast::Pattern<String>>, Vec<ast::Attribute<String>>),
    >::new();
    for (form, pattern) in forms {
        let pattern = if pattern.0.elements.is_empty() {
            ast::Pattern {
                elements: vec![ast::PatternElement::Placeable {
                    expression: ast::Expression::Inline(ast::InlineExpression::StringLiteral {
                        value: String::new(),
                    }),
                }],
            }
        } else {
            pattern.0
        };
        let (value, attributes) = entries.entry((form.term, form.name)).or_default();
        if let Some(name) = form.attribute {
            attributes.push(ast::Attribute {
                id: ast::Identifier { name },
                value: pattern,
            });
        } else {
            *value = Some(pattern);
        }
    }
    let mut body = Vec::new();
    for ((term, name), (value, attributes)) in entries {
        let id = ast::Identifier { name };
        body.push(if term {
            ast::Entry::Term(ast::Term {
                id,
                value: value.ok_or(CatalogError::InvalidMessage)?,
                attributes,
                comment: None,
            })
        } else {
            ast::Entry::Message(ast::Message {
                id,
                value,
                attributes,
                comment: None,
            })
        });
    }
    let source = serializer::serialize(&ast::Resource { body });
    parse_bundle(&locale.to_string(), &source).map_err(|_| CatalogError::InvalidMessage)
}

fn bind(
    bindings: &mut BTreeMap<String, BindingSource>,
    name: &str,
    source: &BindingSource,
) -> Result<(), CatalogError> {
    if !identifier(name)
        || BindingSource::standard_argument(name).is_some_and(|s| &s != source)
        || bindings
            .insert(name.to_owned(), source.clone())
            .is_some_and(|s| &s != source)
    {
        return Err(CatalogError::InvalidMessage);
    }
    Ok(())
}

fn closure(
    root: &NativeReference,
    graph: &Dependencies,
    messages: &BTreeMap<String, LocalizedMessage>,
    inline: Option<(&PatternDependencies, &BTreeMap<String, BindingSource>)>,
) -> Result<BTreeMap<String, BindingSource>, CatalogError> {
    let mut bindings = BTreeMap::new();
    let mut arguments = BTreeSet::new();
    let mut complete = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    // Native terms resolve their own parameters. Traverse all branches for references/cycles
    // while keeping term-local variables out of the enclosing message's argument map.
    let mut work = vec![(root.clone(), false, false)];
    while let Some((form, term_scope, finish)) = work.pop() {
        let state = (form.clone(), term_scope);
        if finish {
            visiting.remove(&form);
            complete.insert(state);
            continue;
        }
        if complete.contains(&state) {
            continue;
        }
        if !visiting.insert(form.clone()) {
            return Err(CatalogError::InvalidMessage);
        }
        let fragment = inline.filter(|_| &form == root);
        let deps = fragment
            .map(|(deps, _)| deps)
            .or_else(|| graph.get(&form))
            .ok_or(CatalogError::InvalidMessage)?;
        if deps.functions.iter().any(|name| name != "NUMBER") {
            return Err(CatalogError::InvalidMessage);
        }
        if !term_scope {
            arguments.extend(deps.arguments.iter().cloned());
            if let Some(declared) = fragment
                .map(|(_, declared)| declared)
                .or_else(|| messages.get(&form.name).map(|m| &m.bindings))
            {
                for (name, source) in declared {
                    bind(&mut bindings, name, source)?;
                }
            }
        }
        work.push((form, term_scope, true));
        work.extend(
            deps.references
                .iter()
                .map(|next| (next.clone(), term_scope || next.term, false)),
        );
    }
    let builtin = REQUIRED_IDS
        .contains(&root.name.as_str())
        .then(|| required_sample_args(&root.name));
    for name in &arguments {
        if let Some(source) = BindingSource::standard_argument(name) {
            bind(&mut bindings, name, &source)?;
        } else if !bindings.contains_key(name)
            && builtin
                .as_ref()
                .is_none_or(|args| args.get(name.clone()).is_none())
        {
            return Err(CatalogError::InvalidMessage);
        }
    }
    bindings.retain(|name, _| arguments.contains(name));
    Ok(bindings)
}

// Isolate a failing dependency before invalidating roots which refer to it.
// Walk native helper values/terms too, since they need not be public roots themselves.
fn repair_first(invalid: &BTreeSet<NativeReference>, graph: &Dependencies) -> NativeReference {
    let mut current = invalid.iter().next().expect("nonempty failures").clone();
    let mut followed = BTreeSet::new();
    loop {
        followed.insert(current.clone());
        let mut seen = BTreeSet::from([current.clone()]);
        let mut work = vec![current.clone()];
        let mut dependency = None;
        while let Some(form) = work.pop() {
            for next in graph.get(&form).into_iter().flat_map(|d| &d.references) {
                if !seen.insert(next.clone()) {
                    continue;
                }
                if invalid.contains(next) && !followed.contains(next) {
                    dependency = Some(next.clone());
                    break;
                }
                work.push(next.clone());
            }
            if dependency.is_some() {
                break;
            }
        }
        let Some(next) = dependency else {
            return current;
        };
        current = next;
    }
}

fn cyclic(form: &NativeReference, graph: &Dependencies) -> bool {
    let mut seen = BTreeSet::new();
    let mut work = vec![form];
    while let Some(node) = work.pop() {
        for next in graph.get(node).into_iter().flat_map(|d| &d.references) {
            if next == form {
                return true;
            }
            if seen.insert(next) {
                work.push(next);
            }
        }
    }
    false
}

fn playable_attribute(attribute: &str, output: &str) -> bool {
    output.len() <= super::MAX_MESSAGE_BYTES
        && !output.contains('\0')
        && (attribute != "morse" || crate::runtime::morse_text(output).is_some())
}

impl MessageCatalog {
    /// Add effective JSON forms above installed assets, preparing each reference inside its
    /// actual locale bundle. Return the one argument map needed by each named format call.
    pub fn project_locales(
        self,
        locale: &LanguageIdentifier,
        messages: &BTreeMap<String, LocalizedMessage>,
    ) -> Result<(Self, MessageBindings), CatalogError> {
        self.project_message_sources(locale, messages, &BTreeSet::new(), &BTreeMap::new())
            .map(|(catalog, bindings, _)| (catalog, bindings))
    }

    /// Remove replaced media/composition IDs from the native text namespace before link checks.
    pub(crate) fn project_message_sources(
        mut self,
        locale: &LanguageIdentifier,
        messages: &BTreeMap<String, LocalizedMessage>,
        media: &BTreeSet<String>,
        previous: &BTreeMap<String, LocalizedMessage>,
    ) -> Result<(Self, MessageBindings, Vec<RejectedAttribute>), CatalogError> {
        let english: LanguageIdentifier = "en-US".parse().expect("English locale");
        let mut bindings = MessageBindings::new();
        let mut layers = Vec::new();
        let mut rejected = Vec::new();
        // Check optional locales even when a different port language is currently selected.
        let languages = messages
            .values()
            .flat_map(|m| m.locales.keys().cloned())
            .chain([locale.clone()])
            .filter(|lang| lang != &english)
            .collect::<BTreeSet<_>>();
        let roots = messages
            .keys()
            .map(String::as_str)
            .chain(REQUIRED_IDS.iter().copied())
            .collect::<BTreeSet<_>>();
        for lang in std::iter::once(&english).chain(languages.iter()) {
            let required = lang == &english;
            let mut forms = Forms::new();
            let mut fallback = BTreeMap::<NativeReference, Vec<NativePattern>>::new();
            let mut lower = Vec::new();
            let mut authored = BTreeSet::new();
            let mut dropped = BTreeSet::new();
            for layer in std::iter::once(&self.english)
                .chain(&self.overlays)
                .filter(|layer| layer.bundle.locales.first() == Some(lang))
            {
                if !forms.is_empty() {
                    lower.push((forms.clone(), false));
                }
                let mut add = Forms::new();
                installed(layer, &mut add)?;
                for (form, pattern) in add {
                    if !form.term && media.contains(&form.name) {
                        continue;
                    }
                    if let Some(previous) = forms.insert(form.clone(), pattern) {
                        fallback.entry(form).or_default().push(previous);
                    }
                }
            }
            if !forms.is_empty() {
                lower.push((forms.clone(), false));
            }
            let mut inherited_forms = forms.clone();
            for (name, message) in previous {
                if let Some(translations) = message.locales.get(lang) {
                    for (attribute, pattern) in translations {
                        inherited_forms.insert(key(name, Some(attribute), false), pattern.clone());
                    }
                }
            }
            if !previous.is_empty() {
                lower.push((inherited_forms, true));
            }
            for (id, message) in messages {
                if !identifier(id) {
                    return Err(CatalogError::InvalidMessage);
                }
                for (name, source) in &message.bindings {
                    bind(&mut BTreeMap::new(), name, source)?;
                }
                if let Some(translations) = message.locales.get(lang) {
                    for (attribute, pattern) in translations {
                        if !ATTRIBUTES.contains(&attribute.as_str()) {
                            return Err(CatalogError::InvalidMessage);
                        }
                        let form = key(id, Some(attribute), false);
                        authored.insert(form.clone());
                        if let Some(old) = previous
                            .get(id)
                            .and_then(|m| m.locales.get(lang))
                            .and_then(|m| m.get(attribute))
                            && old != pattern
                        {
                            if let Some(asset) = forms.insert(form.clone(), old.clone()) {
                                fallback.entry(form.clone()).or_default().push(asset);
                            }
                        }
                        if let Some(previous) = forms.insert(form.clone(), pattern.clone()) {
                            fallback.entry(form).or_default().push(previous);
                        }
                    }
                }
            }
            let mut graph = dependencies(&forms);
            loop {
                let checked_layer = bundle(lang, forms.clone())?;
                let mut invalid = BTreeSet::new();
                let mut attempt = bindings.clone();
                for &id in &roots {
                    for attribute in ATTRIBUTES {
                        let root = key(id, Some(attribute), false);
                        if !required && !forms.contains_key(&root) {
                            continue;
                        }
                        let checked = closure(&root, &graph, messages, None).and_then(|sources| {
                            let mut args = required_sample_args(id);
                            for (name, source) in &sources {
                                if matches!(
                                    source,
                                    BindingSource::Sensor(_) | BindingSource::Input(_)
                                ) {
                                    args.set(name.clone(), 1.0);
                                } else {
                                    args.set(name.clone(), "X");
                                }
                            }
                            if forms.get(&root).is_none_or(|pattern| {
                                // The shipped unit suffix intentionally yields an empty fragment for no units.
                                !pattern.static_forms_fit(
                                    attribute == "morse"
                                        && (id != "remote-unit" || authored.contains(&root)),
                                )
                            }) || super::format_attribute(
                                &checked_layer.bundle,
                                id,
                                attribute,
                                &args,
                            )
                            .is_none_or(|output| {
                                let empty_suffix = id == "remote-unit"
                                    && output.is_empty()
                                    && !authored.contains(&root);
                                !(empty_suffix || playable_attribute(attribute, &output))
                            }) {
                                return Err(CatalogError::InvalidMessage);
                            }
                            if required || lang == locale {
                                let target = attempt.entry(id.to_owned()).or_default();
                                for (name, source) in sources {
                                    bind(target, &name, &source)?;
                                }
                            }
                            Ok(())
                        });
                        if checked.is_err() {
                            invalid.insert(root);
                        }
                    }
                }
                if invalid.is_empty() {
                    bindings = attempt;
                    break;
                }
                // Non-public native helpers can themselves be optional admin overrides.
                // A missing attribute in such a helper is repaired at its defining form.
                let mut pending = invalid.iter().cloned().collect::<Vec<_>>();
                let mut seen = BTreeSet::new();
                while let Some(form) = pending.pop() {
                    if !seen.insert(form.clone()) {
                        continue;
                    }
                    let Some(deps) = graph.get(&form) else {
                        continue;
                    };
                    if fallback.get(&form).is_some_and(|rows| !rows.is_empty())
                        && (deps.references.iter().any(|next| !graph.contains_key(next))
                            || deps.functions.iter().any(|name| name != "NUMBER")
                            || cyclic(&form, &graph))
                    {
                        invalid.insert(form.clone());
                    }
                    pending.extend(deps.references.iter().cloned());
                }
                // Required authored English may depend on an optional package/admin overlay.
                // Repair the latter first and validate the original English source again.
                if required {
                    invalid.retain(|form| {
                        !authored.contains(form)
                            && fallback.get(form).is_some_and(|lower| !lower.is_empty())
                    });
                    if invalid.is_empty() {
                        return Err(CatalogError::InvalidEnglish);
                    }
                }
                {
                    let form = repair_first(&invalid, &graph);
                    if let Some(previous) = fallback.get_mut(&form).and_then(Vec::pop) {
                        graph.insert(form.clone(), previous.dependencies());
                        forms.insert(form.clone(), previous);
                    } else {
                        forms.remove(&form);
                        graph.remove(&form);
                    }
                    if authored.remove(&form) {
                        dropped.insert(form);
                    }
                    self.translation_warnings
                        .push("invalid optional Fluent attribute; using catalog fallback".into());
                }
            }
            for form in dropped {
                let name = form.name.clone();
                let attribute = form.attribute.as_ref().expect("JSON attribute").clone();
                let inherited = previous
                    .get(&name)
                    .and_then(|m| m.locales.get(lang))
                    .and_then(|m| m.get(&attribute))
                    .is_some_and(|p| Some(p) == forms.get(&form));
                rejected.push(RejectedAttribute {
                    name,
                    language: lang.clone(),
                    form: attribute,
                    inherited,
                });
            }
            if required || lang == locale {
                // Installed ordinary entries may use approved aliases without a JSON member.
                // Other installed helpers keep their caller-specific argument contract.
                for form in forms.keys().filter(|f| {
                    !f.term
                        && f.attribute
                            .as_deref()
                            .is_some_and(|a| ATTRIBUTES.contains(&a))
                        && !roots.contains(f.name.as_str())
                }) {
                    if let Ok(sources) = closure(form, &graph, messages, None) {
                        for (name, source) in sources {
                            bind(
                                bindings.entry(form.name.clone()).or_default(),
                                &name,
                                &source,
                            )?;
                        }
                    }
                }
                let mut layer = bundle(lang, forms.clone())?;
                let _ = layer.dependencies.set(graph);
                for (state, inherited) in lower
                    .into_iter()
                    .filter(|(state, inherited)| *inherited || state != &forms)
                {
                    let graph = dependencies(&state);
                    for form in state.keys().filter(|form| {
                        !form.term
                            && form
                                .attribute
                                .as_deref()
                                .is_some_and(|attr| ATTRIBUTES.contains(&attr))
                    }) {
                        if let Ok(sources) = closure(
                            form,
                            &graph,
                            if inherited { previous } else { messages },
                            None,
                        ) {
                            let fallback = (if inherited {
                                &mut self.inherited_bindings
                            } else {
                                &mut self.fallback_bindings
                            })
                            .entry(form.name.clone())
                            .or_default();
                            for (name, source) in sources {
                                bind(fallback, &name, &source)?;
                            }
                        }
                    }
                    let mut lower = bundle(lang, state)?;
                    lower.inherited = inherited;
                    let _ = lower.dependencies.set(graph);
                    layer.lower.push(lower);
                }
                layers.push(layer);
            }
        }
        self.inherited_definitions = previous
            .iter()
            .map(|(id, m)| {
                (
                    id.clone(),
                    LocalizedMessage {
                        bindings: m.bindings.clone(),
                        ..Default::default()
                    },
                )
            })
            .collect();
        self.english = layers.remove(0);
        self.overlays = layers;
        Ok((self, bindings, rejected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{MessageArgument, MessageForms};

    fn localized(rows: &[(&str, &str, &str)]) -> LocalizedMessage {
        let mut def = LocalizedMessage::default();
        for (lang, form, pattern) in rows {
            def.locales
                .entry(lang.parse().unwrap())
                .or_default()
                .insert((*form).into(), NativePattern::parse(pattern).unwrap());
        }
        def
    }

    fn projected(
        defs: &BTreeMap<String, LocalizedMessage>,
        lang: &str,
    ) -> Result<(MessageCatalog, MessageBindings), CatalogError> {
        MessageCatalog::from_sources("en-US", super::super::ENGLISH, None)?
            .project_locales(&lang.parse().unwrap(), defs)
    }
    #[test]
    fn review35_morse_fallback_checks_english_and_optional_independently() {
        for (english, french, safe) in [("OK", "你好", true), ("你好", "OK", false)] {
            let defs = BTreeMap::from([(
                "custom".into(),
                localized(&[
                    ("en-US", "text", "Text"),
                    ("en-US", "tts", "Spoken"),
                    ("en-US", "morse", english),
                    ("fr", "morse", french),
                ]),
            )]);
            let result = projected(&defs, "fr");
            if safe {
                assert_eq!(
                    result
                        .unwrap()
                        .0
                        .format_named("custom", &Default::default())
                        .unwrap()
                        .morse,
                    "OK"
                );
            } else {
                assert!(result.is_err());
            }
        }
    }
    #[test]
    fn review35_invalid_optional_leaf_is_repaired_before_valid_dependent() {
        let part = localized(&[
            ("en-US", "text", "EN PART"),
            ("en-US", "tts", "Part"),
            ("en-US", "morse", "PART"),
            ("fr", "text", "{ absent.text }"),
        ]);
        let notice = localized(&[
            ("en-US", "text", "NOTICE"),
            ("en-US", "tts", "Notice"),
            ("en-US", "morse", "NOTICE"),
            ("fr", "text", "Avis { part.text }"),
        ]);
        let defs = BTreeMap::from([("part".into(), part), ("notice".into(), notice)]);
        let previous = BTreeMap::from([("part".into(), localized(&[("fr", "text", "BON")]))]);
        let (catalog, _, rejected) =
            MessageCatalog::from_sources("en-US", super::super::ENGLISH, None)
                .unwrap()
                .project_message_sources(
                    &"fr".parse().unwrap(),
                    &defs,
                    &Default::default(),
                    &previous,
                )
                .unwrap();
        assert_eq!(
            catalog
                .format_named("notice", &Default::default())
                .unwrap()
                .text,
            "Avis BON"
        );
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].name, "part");
        assert!(rejected[0].inherited);
    }
    #[test]
    fn round_two_cyclic_installed_helper_restores_its_package() {
        let source = format!("{}\nhelper =\n    .text = valid", super::super::ENGLISH);
        let mut installed = MessageCatalog::from_sources("en-US", &source, None).unwrap();
        installed.add_overlay("en-US", "helper =\n    .text = { helper.text }");
        let authored = localized(&[
            ("en-US", "text", "notice { helper.text }"),
            ("en-US", "tts", "Notice"),
            ("en-US", "morse", "NOTICE"),
        ]);
        let (catalog, _) = installed
            .project_locales(
                &"en-US".parse().unwrap(),
                &BTreeMap::from([("notice".into(), authored)]),
            )
            .unwrap();
        assert_eq!(
            catalog
                .format_named("notice", &BTreeMap::new())
                .unwrap()
                .text,
            "notice valid"
        );
    }
    #[test]
    fn installed_helper_is_repaired_before_authored_required_english_is_judged() {
        let source = format!("{}\nhelper =\n    .text = valid", super::super::ENGLISH);
        let mut installed = MessageCatalog::from_sources("en-US", &source, None).unwrap();
        installed.add_overlay("en-US", "helper =\n    .text = { unknown.text }");
        let authored = localized(&[
            ("en-US", "text", "notice { helper.text }"),
            ("en-US", "tts", "Notice"),
            ("en-US", "morse", "NOTICE"),
        ]);
        let (catalog, _) = installed
            .project_locales(
                &"en-US".parse().unwrap(),
                &BTreeMap::from([("notice".into(), authored)]),
            )
            .unwrap();
        assert_eq!(
            catalog
                .format_named("notice", &BTreeMap::new())
                .unwrap()
                .text,
            "notice valid"
        );
    }
    #[test]
    fn review35_installed_native_standard_bindings_are_addressable() {
        let (_, bindings) = projected(&BTreeMap::new(), "en-US").unwrap();
        assert_eq!(
            bindings.get("site-time").and_then(|b| b.get("time")),
            Some(&BindingSource::Time)
        );
    }
    #[test]
    fn review35_runtime_long_value_recovers_lower_same_locale() {
        let defs = BTreeMap::from([(
            "no-links".into(),
            localized(&[
                ("en-US", "text", "en { $node-name }"),
                ("fr", "text", "fr { $node-name }"),
            ]),
        )]);
        for lang in ["fr", "en-US"] {
            let mut catalog =
                MessageCatalog::from_sources("en-US", super::super::ENGLISH, None).unwrap();
            catalog.add_overlay(lang, "no-links =\n    .text = SHORT\n");
            let (catalog, _) = catalog
                .project_locales(&lang.parse().unwrap(), &defs)
                .unwrap();
            let long = MessageForms {
                text: "Z".repeat(126),
                tts: "Z".into(),
                morse: "Z".into(),
            };
            assert_eq!(
                catalog
                    .format_named(
                        "no-links",
                        &BTreeMap::from([("node-name".into(), MessageArgument::Forms(long))])
                    )
                    .unwrap()
                    .text,
                "SHORT"
            );
        }
    }
    #[test]
    fn round_two_correlated_selects_preserve_feasible_variants() {
        for (form, x, y) in [
            ("text", "X".repeat(70), "Y".to_owned()),
            ("morse", "E".into(), "你好".into()),
        ] {
            let pattern = format!(
                "{{ $node-name ->\n [station] {x}\n *[other] {y}\n}}{{ $node-name ->\n [station] {y}\n *[other] {x}\n}}"
            );
            let custom = localized(&[
                ("en-US", "text", "OK"),
                ("en-US", "tts", "OK"),
                ("en-US", "morse", "OK"),
                ("en-US", form, &pattern),
            ]);
            let (catalog, _) =
                projected(&BTreeMap::from([("notice".into(), custom)]), "en-US").unwrap();
            for name in ["station", "other"] {
                let args = BTreeMap::from([(
                    "node-name".into(),
                    MessageArgument::Forms(MessageForms {
                        text: name.into(),
                        tts: name.into(),
                        morse: name.into(),
                    }),
                )]);
                let result = catalog.format_named("notice", &args).unwrap();
                assert_eq!(result.attribute(form).len(), x.len() + y.len());
            }
        }
    }

    #[test]
    fn review35_static_nondefault_selector_arm_is_checked() {
        for (attr, bad) in [("text", "X".repeat(128)), ("morse", "你好".to_owned())] {
            let pattern = format!("{{ $node-name ->\n [station] {bad}\n *[other] OK\n}}");
            let mut entry = localized(&[
                ("en-US", "text", "OK"),
                ("en-US", "tts", "OK"),
                ("en-US", "morse", "OK"),
            ]);
            entry
                .locales
                .get_mut(&"en-US".parse().unwrap())
                .unwrap()
                .insert(attr.into(), NativePattern::parse(&pattern).unwrap());
            assert!(projected(&BTreeMap::from([("custom".into(), entry)]), "en-US").is_err());
        }
    }
    #[test]
    fn json_locale_native_links_resolve_in_the_effective_bundle_per_form() {
        let sub = localized(&[
            ("en-US", "text", "{ $peer-callsign }"),
            ("en-US", "tts", "{ $peer-callsign }"),
            ("en-US", "morse", "{ $peer-callsign }"),
            ("fr", "text", "FR { $peer-callsign }"),
        ]);
        let root = localized(&[
            ("en-US", "text", "EN { identity.text }"),
            ("en-US", "tts", "Say { identity.tts }"),
            ("en-US", "morse", "{ identity.morse }"),
            ("fr", "text", "{ identity.text }"),
            ("fr", "tts", "{ absent.tts }"),
        ]);
        let defs = BTreeMap::from([("identity".into(), sub), ("report".into(), root)]);
        let locale = "fr".parse().unwrap();
        let (catalog, bindings) =
            MessageCatalog::from_sources("en-US", super::super::ENGLISH, None)
                .unwrap()
                .project_locales(&locale, &defs)
                .unwrap();
        assert_eq!(
            bindings["report"]["peer-callsign"],
            BindingSource::standard_argument("peer-callsign").unwrap()
        );
        let values = BTreeMap::from([(
            "peer-callsign".into(),
            MessageArgument::Forms(MessageForms {
                text: "{ tone }".into(),
                tts: "one".into(),
                morse: "ONE".into(),
            }),
        )]);
        let forms = catalog.format_named("report", &values).unwrap();
        assert_eq!(forms.text, "FR { tone }");
        assert_eq!(forms.tts, "Say one");
        assert_eq!(forms.morse, "ONE");
        assert!(!catalog.translation_warnings().is_empty());
    }

    #[test]
    fn json_english_rejects_cycles_conflicts_and_unreachable_branch_errors() {
        let def = localized(&[
            ("en-US", "text", "ok"),
            ("en-US", "tts", "ok"),
            ("en-US", "morse", "ok"),
        ]);
        for text in [
            "{ report.text }",
            "{ $node-name ->\n [hidden] { missing.morse }\n *[other] ok\n}",
            "{ $unknown }",
        ] {
            let mut root = def.clone();
            root.locales
                .get_mut(&"en-US".parse::<LanguageIdentifier>().unwrap())
                .unwrap()
                .insert("text".into(), NativePattern::parse(text).unwrap());
            let defs = BTreeMap::from([("report".into(), root)]);
            assert!(
                MessageCatalog::from_sources("en-US", super::super::ENGLISH, None)
                    .unwrap()
                    .project_locales(&"en-US".parse().unwrap(), &defs)
                    .is_err()
            );
        }
        let mut root = def;
        root.bindings
            .insert("node-name".into(), BindingSource::Time);
        assert!(
            MessageCatalog::from_sources("en-US", super::super::ENGLISH, None)
                .unwrap()
                .project_locales(
                    &"en-US".parse().unwrap(),
                    &BTreeMap::from([("report".into(), root)])
                )
                .is_err()
        );
    }
}

/// A separately compiled text-only fragment and its checked per-attempt argument sources.
#[derive(Clone)]
pub struct CompiledText {
    resource: std::sync::Arc<fluent_bundle::FluentResource>,
    languages: BTreeSet<LanguageIdentifier>,
    pub(crate) static_morse: bool,
    pub(crate) fallback_bindings: BTreeMap<String, BindingSource>,
    pub(crate) inherited_bindings: BTreeMap<String, BindingSource>,
    /// Sources reached by this fragment's native references.
    pub bindings: BTreeMap<String, BindingSource>,
}

impl std::fmt::Debug for CompiledText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompiledText")
    }
}
impl PartialEq for CompiledText {
    fn eq(&self, other: &Self) -> bool {
        self.resource.source() == other.resource.source()
            && self.languages == other.languages
            && self.bindings == other.bindings
            && self.fallback_bindings == other.fallback_bindings
            && self.inherited_bindings == other.inherited_bindings
    }
}
impl Eq for CompiledText {}

impl MessageCatalog {
    /// Materialized English messages with all three native attributes.
    pub(crate) fn named_ids(&self) -> impl Iterator<Item = &str> {
        self.english.resource.entries().filter_map(|entry| {
            let ast::Entry::Message(message) = entry else {
                return None;
            };
            ATTRIBUTES
                .iter()
                .all(|attr| message.attributes.iter().any(|a| a.id.name == *attr))
                .then_some(message.id.name)
        })
    }

    /// Compile an ordinary textual part against the projected English and selected-locale
    /// bundles. A missing optional native reference can use the whole English form.
    pub fn compile_text(
        &self,
        pattern: NativePattern,
        declared: &BTreeMap<String, BindingSource>,
        messages: &BTreeMap<String, LocalizedMessage>,
    ) -> Result<CompiledText, CatalogError> {
        if !pattern.static_forms_fit(false) {
            return Err(CatalogError::InvalidMessage);
        }
        let mut bindings = BTreeMap::new();
        let deps = pattern.dependencies();
        let mut fallback_bindings = BTreeMap::new();
        let mut inherited_bindings = BTreeMap::new();
        let mut languages = BTreeSet::new();
        let root = key("", None, false);
        for (index, layer) in std::iter::once(&self.english)
            .chain(&self.overlays)
            .enumerate()
        {
            let graph = layer
                .dependencies
                .get()
                .ok_or(CatalogError::InvalidMessage)?;
            for previous in &layer.lower {
                let old = previous.dependencies.get().expect("projected fallback");
                if let Ok(sources) = closure(
                    &root,
                    old,
                    if previous.inherited {
                        &self.inherited_definitions
                    } else {
                        messages
                    },
                    Some((&deps, declared)),
                ) {
                    for (name, source) in sources {
                        bind(
                            if previous.inherited {
                                &mut inherited_bindings
                            } else {
                                &mut fallback_bindings
                            },
                            &name,
                            &source,
                        )?;
                    }
                }
            }
            match closure(&root, graph, messages, Some((&deps, declared))) {
                Ok(sources) => {
                    for (name, source) in &sources {
                        bind(&mut bindings, name, source)?;
                    }
                    languages.extend(layer.bundle.locales.first().cloned());
                }
                Err(e) if index == 0 => return Err(e),
                Err(_) => {}
            }
        }
        let static_morse = pattern.static_forms_fit(true);
        let mut value = pattern.0;
        if value.elements.is_empty() {
            value.elements.push(ast::PatternElement::Placeable {
                expression: ast::Expression::Inline(ast::InlineExpression::StringLiteral {
                    value: String::new(),
                }),
            });
        }
        let source = serializer::serialize(&ast::Resource {
            body: vec![ast::Entry::Message(ast::Message {
                id: ast::Identifier {
                    name: "fragment".into(),
                },
                value: Some(value),
                attributes: Vec::new(),
                comment: None,
            })],
        });
        let resource = fluent_bundle::FluentResource::try_new(source)
            .map_err(|_| CatalogError::InvalidMessage)?;
        Ok(CompiledText {
            resource: std::sync::Arc::new(resource),
            languages,
            static_morse,
            fallback_bindings,
            inherited_bindings,
            bindings,
        })
    }

    /// Format a native part in the existing catalog. Its bytes never enter a parser.
    pub fn format_text(
        &self,
        fragment: &CompiledText,
        args: &BTreeMap<String, super::MessageArgument>,
    ) -> Result<super::MessageForms, CatalogError> {
        self.format_text_for(fragment, args, false)
    }

    pub(crate) fn format_text_for(
        &self,
        fragment: &CompiledText,
        args: &BTreeMap<String, super::MessageArgument>,
        spoken: bool,
    ) -> Result<super::MessageForms, CatalogError> {
        self.format_text_scoped(fragment, args, None, spoken, false)
    }

    pub(crate) fn validate_fragment(
        &self,
        fragment: &CompiledText,
        args: &BTreeMap<String, super::MessageArgument>,
        spoken: bool,
    ) -> Result<super::MessageForms, CatalogError> {
        self.format_text_scoped(fragment, args, None, spoken, true)?;
        self.format_text_for(fragment, args, spoken)
    }

    pub(crate) fn format_text_scoped(
        &self,
        fragment: &CompiledText,
        current: &BTreeMap<String, super::MessageArgument>,
        inherited: Option<&super::InheritedArguments>,
        spoken: bool,
        english_only: bool,
    ) -> Result<super::MessageForms, CatalogError> {
        let Some(ast::Entry::Message(message)) = fragment.resource.get_entry(0) else {
            return Err(CatalogError::InvalidMessage);
        };
        let pattern = message.value.as_ref().ok_or(CatalogError::InvalidMessage)?;
        self.format_candidates(
            english_only,
            |_| super::MAX_MESSAGE_BYTES,
            |layer, attribute| {
                let args = super::named_arguments(
                    if layer.inherited {
                        inherited.map_or(current, |site| &site.available)
                    } else {
                        current
                    },
                    attribute,
                );
                let bundle = &layer.bundle;
                if !fragment.languages.contains(bundle.locales.first()?) {
                    return None;
                }
                super::format_candidate_pattern(
                    bundle,
                    pattern,
                    &args,
                    inherited.filter(|_| layer.inherited),
                    |text| {
                        !spoken
                            || attribute != "morse"
                            || crate::runtime::morse_text(text).is_some()
                    },
                )
            },
        )
    }
}
