//! Attribute effective public leaves to the declaration admitted by their profile.
use super::*;
use rpt_advanced_core::config::resources::candidate::ResourceRegistry;
pub(in crate::management) enum Origin {
    BuiltIn,
    Resource(Binding),
    Derived(Vec<Binding>),
}
impl From<Option<Binding>> for Origin {
    fn from(source: Option<Binding>) -> Self {
        source.map_or(Self::BuiltIn, Self::Resource)
    }
}
pub(super) type Origins = BTreeMap<String, Origin>;

fn visit<'a>(
    value: &'a Value,
    path: &mut Vec<&'a str>,
    visit_leaf: &mut impl FnMut(&[&str], &Value),
) {
    if let Value::Object(map) = value {
        for (key, child) in map {
            path.push(key);
            visit(child, path, visit_leaf);
            path.pop();
        }
    } else {
        visit_leaf(path, value);
    }
}
fn pointer(path: &[&str]) -> String {
    path.iter()
        .map(|s| format!("/{}", s.replace('~', "~0").replace('/', "~1")))
        .collect()
}
pub(super) fn selected(fields: &Fields, source: impl Fn(&[&str]) -> Option<Binding>) -> Origins {
    let mut out = Origins::new();
    visit(
        &Value::Object(fields.clone()),
        &mut Vec::new(),
        &mut |path, _| {
            out.insert(pointer(path), source(path).into());
        },
    );
    out
}
fn accepted(authored: &Value, effective: &Value) -> bool {
    match (authored, effective) {
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| accepted(a, b))
        }
        (Value::Number(a), Value::Number(b)) if a.is_f64() || b.is_f64() => {
            a == b
                || (0..=18)
                    .find_map(|places| {
                        let scale = rpt_advanced_core::command_language::scaled;
                        Some(scale(a.as_str(), places)? == scale(b.as_str(), places)?)
                    })
                    .unwrap_or_else(|| a.as_f64().is_some() && a.as_f64() == b.as_f64())
        }
        _ => authored == effective,
    }
}
impl Projections {
    pub(super) fn add_sources(&mut self, registry: &ResourceRegistry) {
        let site = Scope::Site(registry.site_id());
        for ((scope, id), value) in &self.effective {
            let Some(binding) = registry.by_id(*id) else {
                continue;
            };
            let key = (*scope, binding.kind, binding.kind.is_named().then_some(*id));
            let sources = self.sources.entry(key).or_default();
            let name = registry
                .members()
                .iter()
                .find_map(|(b, n)| (b.id == *id).then_some(n));
            let layers = [
                Some(binding),
                (*scope != site && binding.scope != site)
                    .then(|| {
                        if let Some(name) = name {
                            registry.named_binding(site, binding.kind, name)
                        } else {
                            registry.binding(site, binding.kind)
                        }
                    })
                    .flatten(),
            ];
            fallbacks(sources, value, binding.kind, layers, &self.local);
        }
        for (&(scope, kind), value) in &self.fixed_effective {
            let sources = self.sources.entry((scope, kind, None)).or_default();
            let layers = [
                registry.binding(scope, kind),
                (scope != site)
                    .then(|| registry.binding(site, kind))
                    .flatten(),
            ];
            fallbacks(sources, value, kind, layers, &self.local);
        }
        let scopes =
            std::iter::once(site).chain(registry.port_keys().map(|(_, id)| Scope::Port(id)));
        for scope in scopes {
            self.physical_sources(scope, registry);
        }
    }

    fn physical_sources(&mut self, scope: Scope, registry: &ResourceRegistry) {
        // A pin array can be selected explicitly or derived from the connector role map.
        for (kind, prefix, direction, role) in [
            (Kind::RxSignaling, "/squelch", "inputs", "cor"),
            (Kind::RxSignaling, "/ctcss", "inputs", "ctcss"),
            (Kind::TxSignaling, "/ptt", "outputs", "ptt"),
        ] {
            let fields = &self.fixed_effective[&(scope, kind)];
            if fields
                .pointer(&format!("{prefix}/source"))
                .and_then(Value::as_str)
                != Some("parallel")
            {
                continue;
            }
            let path = format!("{prefix}/pin");
            let Some(pins) = fields.pointer(&path).and_then(Value::as_array) else {
                continue;
            };
            let pin_origin = self.sources[&(scope, kind, None)].get(&path);
            let explicit = match pin_origin {
                Some(Origin::Resource(b)) => Some(*b),
                _ => None,
            };
            let interface = (scope, Kind::Interface, None);
            let mut derived = Vec::new();
            for pin in pins.iter().filter_map(Value::as_u64) {
                let connector = format!("/parallel/{direction}/{pin}");
                if self.fixed_effective[&(scope, Kind::Interface)]
                    .pointer(&connector)
                    .and_then(Value::as_str)
                    != Some(role)
                {
                    continue;
                }
                if let Some(Origin::Resource(b)) = self.sources[&interface].get(&connector) {
                    if !derived.contains(b) {
                        derived.push(*b);
                    }
                } else if let Some(binding) = explicit {
                    self.sources
                        .get_mut(&interface)
                        .expect("interface sources")
                        .insert(connector, Origin::Derived(vec![binding]));
                }
            }
            if let (Scope::Port(_), Some(pin_owner)) = (scope, explicit) {
                let iface = &self.fixed_effective[&(scope, Kind::Interface)];
                if let Some(roles) = iface["parallel"][direction].as_object() {
                    for (pin, current) in roles {
                        if current != "" {
                            continue;
                        }
                        let candidates = [
                            registry.binding(scope, Kind::Interface),
                            registry.binding(Scope::Site(registry.site_id()), Kind::Interface),
                        ];
                        let authored = candidates.into_iter().flatten().find_map(|b| {
                            let v = self
                                .local
                                .get(&b.id)?
                                .get("parallel")?
                                .get(direction)?
                                .get(pin)?
                                .as_str()?;
                            Some((b, v))
                        });
                        if let Some((declaration, prior)) = authored
                            && prior == role
                        {
                            self.sources
                                .get_mut(&interface)
                                .expect("interface sources")
                                .insert(
                                    format!("/parallel/{direction}/{pin}"),
                                    Origin::Derived(vec![declaration, pin_owner]),
                                );
                        }
                    }
                }
            }
            if explicit.is_none() && !derived.is_empty() {
                self.sources
                    .get_mut(&(scope, kind, None))
                    .expect("signal sources")
                    .insert(path, Origin::Derived(derived));
            }
        }
        if matches!(scope, Scope::Port(_)) {
            let tx = (scope, Kind::TxSignaling, None);
            let iface = (scope, Kind::Interface, None);
            let fields = &self.fixed_effective[&(scope, Kind::TxSignaling)];
            for encoder in ["ctcss", "dcs"] {
                let binding = &fields[encoder];
                if binding["enabled"] != true || binding["source"] != "external" {
                    continue;
                }
                let out = &binding["output"];
                let base = match out["source"].as_str() {
                    Some("parallel") => "/parallel/outputs",
                    Some("sound_chip_gpio") => "/gpio/pins",
                    _ => continue,
                };
                let Some(pin) = out["pin"]
                    .as_array()
                    .and_then(|v| v.first())
                    .and_then(Value::as_u64)
                else {
                    continue;
                };
                let mut authors = Vec::new();
                for field in [
                    "enabled",
                    "source",
                    "output/source",
                    "output/pin",
                    "output/inverted",
                ] {
                    if let Some(Origin::Resource(b)) =
                        self.sources[&tx].get(&format!("/{encoder}/{field}"))
                        && !authors.contains(b)
                    {
                        authors.push(*b);
                    }
                }
                if !authors.is_empty() {
                    self.sources
                        .get_mut(&iface)
                        .expect("interface sources")
                        .insert(format!("{base}/{pin}"), Origin::Derived(authors));
                }
            }
        }
        let key = (scope, Kind::RxSignaling, None);
        let rx = &self.fixed_effective[&(scope, Kind::RxSignaling)];
        if rx.pointer("/ctcss/source").and_then(Value::as_str) == Some("dsp")
            && let Some(Origin::Resource(b)) = self.sources[&key].get("/ctcss/frequency_hz")
            && matches!(
                self.sources[&key].get("/ctcss/tones_hz"),
                Some(Origin::BuiltIn)
            )
            && rx
                .pointer("/ctcss/tones_hz")
                .and_then(Value::as_array)
                .is_some_and(|tones| {
                    tones.len() == 1 && Some(&tones[0]) == rx.pointer("/ctcss/frequency_hz")
                })
        {
            let binding = *b;
            self.sources
                .get_mut(&key)
                .expect("RX sources")
                .insert("/ctcss/tones_hz".into(), Origin::Derived(vec![binding]));
        }
    }
}
fn fallbacks(
    sources: &mut Origins,
    effective: &Value,
    kind: Kind,
    layers: [Option<Binding>; 2],
    locals: &BTreeMap<Id, Fields>,
) {
    visit(effective, &mut Vec::new(), &mut |path, v| {
        let entry = sources.entry(pointer(path));
        entry.or_insert_with(|| {
            layers
                .iter()
                .flatten()
                .find(|binding| {
                    let Some((head, rest)) = path.split_first() else {
                        return false;
                    };
                    let from = locals
                        .get(&binding.id)
                        .and_then(|m| m.get(*head))
                        .and_then(|val| rest.iter().try_fold(val, |cur, part| cur.get(*part)));
                    // Memory rows acquire built-in fields after choosing the complete replacement array.
                    from.is_some_and(|a| {
                        (kind == Kind::Memories && path == ["entries"]) || accepted(a, v)
                    })
                })
                .copied()
                .into()
        });
    });
}
