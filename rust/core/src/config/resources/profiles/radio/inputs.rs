//! Field-specific radio inheritance with identified source candidates.
use super::super::normalize::warn;
use super::{CandidateWarning, NormalizedProfileResource, ResourceBinding, ResourceScope};
use serde_json::{Map, Value};
pub(super) struct Inputs<'a>(pub(super) [Option<(&'a Map<String, Value>, ResourceBinding)>; 2]);
impl Inputs<'_> {
    pub(super) fn values<'a>(
        &'a self,
        path: &'a [&'a str],
    ) -> impl Iterator<Item = (&'a Value, ResourceBinding)> + 'a {
        self.0.into_iter().flatten().filter_map(move |(body, b)| {
            let (first, rest) = path.split_first()?;
            Some((
                rest.iter()
                    .try_fold(body.get(*first)?, |v, key| v.get(*key))?,
                b,
            ))
        })
    }
    pub(super) fn number(&self, path: &[&str], default: f64) -> f64 {
        self.values(path)
            .find_map(|(v, _)| v.as_f64())
            .unwrap_or(default)
    }
    pub(super) fn flag(&self, path: &[&str], default: bool) -> bool {
        self.values(path)
            .find_map(|(v, _)| v.as_bool())
            .unwrap_or(default)
    }
    pub(super) fn uint(&self, path: &[&str], default: u32) -> u32 {
        self.values(path)
            .find_map(|(v, _)| v.as_u64())
            .map_or(default, |n| n as u32)
    }
    pub(super) fn ordered(
        &self,
        low: &[&'static str],
        high: &[&'static str],
        defaults: [f64; 2],
        w: &mut Vec<CandidateWarning>,
    ) -> [f64; 2] {
        let choices = [low, high].map(|path| {
            self.values(path)
                .map(|(v, b)| (v.as_f64().expect("normalized frequency"), Some(b)))
                .collect::<Vec<_>>()
        });
        choose_pair(
            choices,
            defaults,
            [low[low.len() - 1], high[high.len() - 1]],
            |l, h| l < h,
            w,
        )
    }
}

/// Preserve the largest number of compatible authored values, preferring the nearest scope.
pub(super) fn choose_pair<T: Copy>(
    choices: [Vec<(T, Option<ResourceBinding>)>; 2],
    defaults: [T; 2],
    names: [&'static str; 2],
    valid: impl Fn(T, T) -> bool + Copy,
    w: &mut Vec<CandidateWarning>,
) -> [T; 2] {
    let [mut lows, mut highs] = choices;
    lows.push((defaults[0], None));
    highs.push((defaults[1], None));
    let rank = |source: Option<ResourceBinding>| match source.map(|b| b.scope) {
        Some(ResourceScope::Port(_)) => 2,
        Some(ResourceScope::Site(_)) => 1,
        None => 0,
    };
    let (li, hi) = lows
        .iter()
        .enumerate()
        .flat_map(|(li, l)| {
            highs
                .iter()
                .enumerate()
                .filter(move |(_, h)| valid(l.0, h.0))
                .map(move |(hi, h)| {
                    let ranks = [rank(l.1), rank(h.1)];
                    (
                        (
                            ranks.iter().filter(|&&r| r > 0).count(),
                            ranks.iter().filter(|&&r| r == 2).count(),
                            ranks,
                        ),
                        (li, hi),
                    )
                })
        })
        .max_by_key(|(score, _)| *score)
        .expect("built-in pair satisfies the relationship")
        .1;
    for (candidates, cut, field) in [(&lows, li, names[0]), (&highs, hi, names[1])] {
        for &(_, binding) in &candidates[..cut] {
            if let Some(b) = binding {
                warn(w, b, field);
            }
        }
    }
    [lows[li].0, highs[hi].0]
}

impl<'a> Inputs<'a> {
    pub(super) fn from_resources(resources: [Option<&'a NormalizedProfileResource>; 2]) -> Self {
        Self(resources.map(|r| r.map(|r| (&r.configuration, r.binding))))
    }
    pub(super) fn get(&self, path: &[&str]) -> Option<&'a Value> {
        self.0.iter().flatten().find_map(|(body, _)| {
            let (first, rest) = path.split_first()?;
            rest.iter()
                .try_fold(body.get(*first)?, |value, key| value.get(*key))
        })
    }
    pub(super) fn text<'b>(&self, path: &[&str], default: &'b str) -> &'b str
    where
        'a: 'b,
    {
        self.get(path).and_then(Value::as_str).unwrap_or(default)
    }
}
