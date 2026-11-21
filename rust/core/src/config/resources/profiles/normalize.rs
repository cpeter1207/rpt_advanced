//! Check sparse bodies and resolve field-specific inheritance off the audio path.

use super::{CandidateWarning, NormalizedProfileResource, ResourceBinding, ResourceScope};
use crate::config::resources::ResourceWarning;
use serde_json::{Map, Value};

pub(super) type Fields = &'static [(&'static str, Rule)];
pub(super) type Path = &'static [&'static str];

/// Field-specific normalization with recursive handling of section objects.
pub(super) enum Rule {
    Object(Fields),
    Bool,
    Number(f64, f64),
    Unsigned(u64, u64),
    Choice(&'static [&'static str]),
    Value(fn(&Value) -> Option<Value>),
}

pub(super) fn normalize(
    binding: ResourceBinding,
    fields: &Map<String, Value>,
    schema: Fields,
    warnings: &mut Vec<CandidateWarning>,
) -> Map<String, Value> {
    normalize_fields(binding, fields.iter(), schema, warnings)
}

/// Normalize the fields owned by one decoder of a shared configuration section.
pub(super) fn normalize_fields<'a>(
    binding: ResourceBinding,
    fields: impl Iterator<Item = (&'a String, &'a Value)>,
    schema: Fields,
    warnings: &mut Vec<CandidateWarning>,
) -> Map<String, Value> {
    let mut normalized = Map::new();
    for (key, value) in fields {
        let Some((field, rule)) = schema.iter().find(|(field, _)| key.as_str() == *field) else {
            warnings.push(CandidateWarning {
                resource: binding,
                warning: ResourceWarning::UnknownField,
            });
            continue;
        };
        let accepted = match rule {
            Rule::Object(children) => value
                .as_object()
                .map(|body| Value::Object(normalize(binding, body, children, warnings))),
            Rule::Number(min, max) => value
                .as_f64()
                .filter(|n| (*min..=*max).contains(n))
                .map(|_| value.clone()),
            Rule::Bool => value.as_bool().map(Value::Bool),
            Rule::Unsigned(min, max) => value
                .as_u64()
                .filter(|number| (*min..=*max).contains(number))
                .map(Value::from),
            Rule::Choice(choices) => value
                .as_str()
                .filter(|value| choices.contains(value))
                .map(Value::from),
            Rule::Value(check) => check(value),
        };
        if let Some(accepted) = accepted {
            normalized.insert(key.clone(), accepted);
        } else {
            warn(warnings, binding, field);
        }
    }
    normalized
}

pub(super) fn warn(
    warnings: &mut Vec<CandidateWarning>,
    binding: ResourceBinding,
    field: &'static str,
) {
    warnings.push(CandidateWarning {
        resource: binding,
        warning: ResourceWarning::InvalidValue(field),
    });
}

/// Look up normalized leaves in local, site, then built-in order. A normalized
/// object supplies only its present children, while an array is one leaf.
pub(super) struct Layers<'a> {
    pub(super) local: &'a NormalizedProfileResource,
    pub(super) site: Option<&'a NormalizedProfileResource>,
}

impl Layers<'_> {
    pub(super) fn values(&self, path: Path) -> impl Iterator<Item = (&Value, ResourceBinding)> {
        [Some(self.local), self.site]
            .into_iter()
            .flatten()
            .filter_map(move |resource| {
                let (field, children) = path.split_first().expect("nonempty profile field path");
                let value = children
                    .iter()
                    .try_fold(resource.configuration.get(*field)?, |value, child| {
                        value.get(*child)
                    })?;
                Some((value, resource.binding))
            })
    }

    pub(super) fn get(&self, path: Path) -> Option<&Value> {
        self.values(path).next().map(|(value, _)| value)
    }

    pub(super) fn text(&self, path: Path) -> Option<&str> {
        self.get(path).and_then(Value::as_str)
    }

    pub(super) fn number(&self, path: Path) -> Option<u64> {
        self.get(path).and_then(Value::as_u64)
    }

    pub(super) fn flag(&self, path: Path) -> Option<bool> {
        self.get(path).and_then(Value::as_bool)
    }
}

/// One usable field candidate and its authored source, or a built-in value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimingSource {
    /// Candidate value in milliseconds.
    pub milliseconds: u32,
    /// Source resource or None for a built-in value.
    pub resource: Option<ResourceBinding>,
}

impl TimingSource {
    fn rank(self) -> u8 {
        match self.resource.map(|resource| resource.scope) {
            Some(ResourceScope::Port(_)) => 2,
            Some(ResourceScope::Site(_)) => 1,
            None => 0,
        }
    }
}

pub(super) fn timings<const N: usize>(
    layers: &Layers<'_>,
    paths: [Path; N],
    defaults: [u32; N],
    valid: impl Fn([u32; N]) -> bool,
    warnings: &mut Vec<CandidateWarning>,
) -> ([Vec<TimingSource>; N], [usize; N]) {
    let sources: [Vec<_>; N] = std::array::from_fn(|index| {
        layers
            .values(paths[index])
            .map(|(value, resource)| TimingSource {
                milliseconds: value.as_u64().expect("normalized timing") as u32,
                resource: Some(resource),
            })
            .chain([TimingSource {
                milliseconds: defaults[index],
                resource: None,
            }])
            .collect()
    });

    // At most three fields and three options per field. Keep the most authored
    // fields, then the most port fields, then prefer earlier listed fields.
    let mut combinations = vec![[0; N]];
    for (field, options) in sources.iter().enumerate() {
        combinations = combinations
            .into_iter()
            .flat_map(|selection| {
                (0..options.len()).map(move |index| {
                    let mut selection = selection;
                    selection[field] = index;
                    selection
                })
            })
            .collect();
    }
    let selected = combinations
        .into_iter()
        .filter(|choice| valid(std::array::from_fn(|i| sources[i][choice[i]].milliseconds)))
        .max_by_key(|choice| {
            let ranks: [u8; N] = std::array::from_fn(|i| sources[i][choice[i]].rank());
            (
                ranks.iter().filter(|&&rank| rank > 0).count(),
                ranks.iter().filter(|&&rank| rank == 2).count(),
                ranks,
            )
        })
        .expect("built-in settings satisfy the relationship");
    for (i, &choice) in selected.iter().enumerate() {
        for source in &sources[i][..choice] {
            if let Some(binding) = source.resource {
                warn(
                    warnings,
                    binding,
                    paths[i].last().expect("nonempty field path"),
                );
            }
        }
    }
    (sources, selected)
}
