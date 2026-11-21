//! Parse one authored pattern before resolving its text and structural references.

use fluent_syntax::{
    ast::{self, CallArguments, Expression, InlineExpression, PatternElement},
    parser,
};
use std::collections::BTreeSet;

/// A member ID or variable alias accepted by the modular Fluent contract.
pub fn identifier(id: &str) -> bool {
    id.len() <= 63
        && id.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// A syntax, identifier or configuration-boundary error; supplied text stays private.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternError {
    /// The supplied value cannot be parsed as exactly one native Fluent pattern.
    Syntax,
    /// Text has a NUL or exceeds its authoring size limit.
    InvalidText,
    /// Invalid name for a structural message or flat source binding.
    Identifier,
}

/// One native text expression, never reparsed after its arguments are substituted.
#[derive(Clone, Debug, PartialEq)]
pub struct NativePattern(pub(super) ast::Pattern<String>);

/// One top-level composition part, recognized before any Fluent formatting occurs.
#[derive(Clone, Debug, PartialEq)]
pub enum CompositionPart {
    /// A native text fragment evaluated by Fluent with the attempt's argument map.
    Text(NativePattern),
    /// The complete typed media/text definition under this exact registered message name.
    Named(String),
}

/// A native Fluent message or term reference at any depth, including every select variant.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NativeReference {
    /// Terms occupy their own Fluent namespace.
    pub term: bool,
    /// The referenced identifier without a term prefix.
    pub name: String,
    /// Selected attribute, or the entry's own value.
    pub attribute: Option<String>,
}

/// Dependencies shared by a single text-only format call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatternDependencies {
    /// References remain native even when nested in a selector, term or function.
    pub references: BTreeSet<NativeReference>,
    /// Flat variables referenced on any branch.
    pub arguments: BTreeSet<String>,
    /// Native functions requested by the text.
    pub functions: BTreeSet<String>,
}

impl NativePattern {
    /// Parse exactly one pattern. Every line is framed as a continuation and the
    /// resulting AST must contain one value and no additional attributes/entries.
    pub fn parse(source: &str) -> Result<Self, PatternError> {
        if source.contains('\0') || source.chars().count() > 65_536 {
            return Err(PatternError::InvalidText);
        }
        if source.trim().is_empty() {
            return Ok(Self(ast::Pattern {
                elements: Vec::new(),
            }));
        }
        let mut framed = String::from("pattern =");
        for line in source.split('\n') {
            framed.push_str("\n    ");
            framed.push_str(line);
        }
        let parsed = parser::parse(framed).map_err(|_| PatternError::Syntax)?;
        let mut body = parsed.body.into_iter();
        match (body.next(), body.next()) {
            (
                Some(ast::Entry::Message(ast::Message {
                    value: Some(pattern),
                    attributes,
                    ..
                })),
                None,
            ) if attributes.is_empty() => Ok(Self(pattern)),
            _ => Err(PatternError::Syntax),
        }
    }

    /// Lower only a direct, unqualified message placeable at the composition boundary.
    pub fn composition(self) -> Result<Vec<CompositionPart>, PatternError> {
        fn flush(parts: &mut Vec<CompositionPart>, elements: &mut Vec<PatternElement<String>>) {
            if elements.iter().any(
                |e| !matches!(e, PatternElement::TextElement { value } if value.trim().is_empty()),
            ) {
                parts.push(CompositionPart::Text(NativePattern(ast::Pattern {
                    elements: std::mem::take(elements),
                })));
            } else {
                elements.clear();
            }
        }
        let mut parts = Vec::new();
        let mut text = Vec::new();
        for element in self.0.elements {
            match element {
                PatternElement::Placeable {
                    expression:
                        Expression::Inline(InlineExpression::MessageReference {
                            id,
                            attribute: None,
                        }),
                } => {
                    if !identifier(&id.name) {
                        return Err(PatternError::Identifier);
                    }
                    flush(&mut parts, &mut text);
                    parts.push(CompositionPart::Named(id.name));
                }
                other => text.push(other),
            }
        }
        flush(&mut parts, &mut text);
        Ok(parts)
    }

    /// Facts about authored literal text in every select arm. Actual variable and
    /// native-reference expansions remain the Fluent formatter's responsibility.
    pub(super) fn static_forms_fit(&self, require_morse: bool) -> bool {
        #[derive(Default)]
        struct Facts {
            minimum: usize,
            longest: usize,
            may_be_silent: bool,
            always_silent: bool,
        }
        fn literal(value: &str) -> Facts {
            let silent = crate::runtime::morse_text(value).is_none();
            Facts {
                minimum: value.len(),
                longest: value.len(),
                may_be_silent: silent,
                always_silent: silent,
            }
        }
        fn check(pattern: &ast::Pattern<String>) -> Facts {
            let mut minimum = 0usize;
            let mut largest_extra = 0usize;
            let mut can_be_silent = true;
            let mut conditional_silences = 0;
            for element in &pattern.elements {
                let part = match element {
                    PatternElement::TextElement { value } => literal(value),
                    PatternElement::Placeable {
                        expression: Expression::Inline(InlineExpression::StringLiteral { value }),
                    } => literal(&fluent_syntax::unicode::unescape_unicode_to_string(value)),
                    PatternElement::Placeable {
                        expression: Expression::Select { variants, .. },
                    } => {
                        let mut choice = Facts {
                            minimum: usize::MAX,
                            always_silent: true,
                            ..Default::default()
                        };
                        for variant in variants {
                            let arm = check(&variant.value);
                            choice.minimum = choice.minimum.min(arm.minimum);
                            choice.longest = choice.longest.max(arm.longest);
                            choice.may_be_silent |= arm.may_be_silent;
                            choice.always_silent &= arm.always_silent;
                        }
                        choice
                    }
                    _ => Facts::default(),
                };
                minimum = minimum.saturating_add(part.minimum);
                largest_extra = largest_extra.max(part.longest - part.minimum);
                can_be_silent &= part.may_be_silent;
                if part.may_be_silent && !part.always_silent {
                    conditional_silences += 1;
                }
            }
            // Separate choices can be correlated. One witnessed arm plus guaranteed
            // literal minima has a provable bound; other combinations stay runtime-checked.
            Facts {
                minimum,
                longest: minimum.saturating_add(largest_extra),
                may_be_silent: can_be_silent && conditional_silences <= 1,
                always_silent: can_be_silent && conditional_silences == 0,
            }
        }
        let facts = check(&self.0);
        facts.longest <= super::MAX_MESSAGE_BYTES && (!require_morse || !facts.may_be_silent)
    }
    /// Inspect all native variants before accepting a configuration generation.
    pub fn dependencies(&self) -> PatternDependencies {
        let mut result = PatternDependencies::default();
        result.pattern(&self.0);
        result
    }
}

impl PatternDependencies {
    fn pattern(&mut self, pattern: &ast::Pattern<String>) {
        for element in &pattern.elements {
            if let PatternElement::Placeable { expression } = element {
                self.expression(expression);
            }
        }
    }

    fn expression(&mut self, expression: &Expression<String>) {
        match expression {
            Expression::Inline(value) => self.inline(value),
            Expression::Select { selector, variants } => {
                self.inline(selector);
                for variant in variants {
                    self.pattern(&variant.value);
                }
            }
        }
    }

    fn calls(&mut self, args: &CallArguments<String>) {
        for value in args
            .positional
            .iter()
            .chain(args.named.iter().map(|a| &a.value))
        {
            self.inline(value);
        }
    }

    fn inline(&mut self, expression: &InlineExpression<String>) {
        match expression {
            InlineExpression::VariableReference { id } => {
                self.arguments.insert(id.name.clone());
            }
            InlineExpression::FunctionReference { id, arguments } => {
                self.functions.insert(id.name.clone());
                self.calls(arguments);
            }
            InlineExpression::MessageReference { id, attribute }
            | InlineExpression::TermReference { id, attribute, .. } => {
                self.references.insert(NativeReference {
                    term: matches!(expression, InlineExpression::TermReference { .. }),
                    name: id.name.clone(),
                    attribute: attribute.as_ref().map(|a| a.name.clone()),
                });
                if let InlineExpression::TermReference {
                    arguments: Some(args),
                    ..
                } = expression
                {
                    self.calls(args);
                }
            }
            InlineExpression::Placeable { expression } => self.expression(expression),
            InlineExpression::StringLiteral { .. } | InlineExpression::NumberLiteral { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn top_level_parts_keep_every_nested_native_expression_as_text() {
        let parts=NativePattern::parse("{ siren } { $count ->\n [one] { label.tts }\n *[other] { NUMBER($count) }\n} { sign_off }").unwrap().composition().unwrap();
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[0],CompositionPart::Named(id) if id=="siren"));
        assert!(matches!(&parts[2],CompositionPart::Named(id) if id=="sign_off"));
        let CompositionPart::Text(pattern) = &parts[1] else {
            panic!()
        };
        let deps = pattern.dependencies();
        assert!(deps.arguments.contains("count"));
        assert!(deps.functions.contains("NUMBER"));
        assert!(deps.references.contains(&NativeReference {
            term: false,
            name: "label".into(),
            attribute: Some("tts".into())
        }));
        assert!(NativePattern::parse("x\n.other = injected").is_err());
        assert!(NativePattern::parse("\0").is_err());
    }
}
