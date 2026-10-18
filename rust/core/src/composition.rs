//! Control-only compilation of ordered, acyclic message content.
//!
//! References are sized before expansion. No untrusted nesting consumes the call
//! stack, and runtime values are never reparsed as content. A `None` section is
//! the invoking ID/tail/message; named sections are resolved for the same node.

use crate::{config::ResolvedMessageSettings, messages::MAX_MESSAGE_BYTES};
use std::collections::{BTreeMap, BTreeSet};

/// One explicit sequence point, retaining its media/binding owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// Operator-authored text with a Morse fallback.
    Literal(String),
    /// Catalog message and bindings from the named or invoking section.
    Message(Option<String>),
    /// Existing file/speech/Morse fallback chain.
    Media(Option<String>),
    /// Attention tones; omitted in receive-driven Morse playback.
    Tone(Option<String>),
}

/// A validated sequence prepared as one logical telemetry item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composition {
    segments: Vec<Segment>,
}

/// Configuration cannot safely produce this composition; callers disable it with a warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositionError {
    /// Malformed braces or an unknown structural token.
    Syntax,
    /// A referenced macro is missing or lacks its required content source.
    MissingDefinition,
    /// Macro references are cyclic.
    Cycle,
    /// A segment exceeds its byte bound, expansion overflows, or allocation fails.
    TooLarge,
}

#[derive(Debug)]
enum Token {
    Segment(Segment),
    Reference(String),
}

#[derive(Clone, Copy, Default)]
struct Size {
    segments: usize,
    bytes: usize,
}

impl Composition {
    /// Compile content on control. Caller validates invoking-section media and bindings.
    ///
    /// Expanded segment count and bytes are checked before allocation; repeated
    /// references use cached sizes, avoiding exponential work during validation.
    pub fn compile(
        content: &str,
        definitions: &BTreeMap<String, ResolvedMessageSettings>,
    ) -> Result<Self, CompositionError> {
        let root = parse(content, None)?;
        let mut tokens: BTreeMap<String, Vec<Token>> = BTreeMap::new();
        let mut sizes = BTreeMap::new();
        let mut visiting = BTreeSet::new();
        let mut work: Vec<_> = references(&root)
            .map(|name| (name.to_owned(), false))
            .collect();
        while let Some((name, finish)) = work.pop() {
            if finish {
                sizes.insert(name.clone(), measure(&tokens[&name], &sizes)?);
                visiting.remove(&name);
                continue;
            }
            if sizes.contains_key(&name) {
                continue;
            }
            if !visiting.insert(name.clone()) {
                return Err(CompositionError::Cycle);
            }
            let definition = definitions
                .get(&name)
                .ok_or(CompositionError::MissingDefinition)?;
            let parsed = parse(&definition.content, Some(&name))?;
            for token in &parsed {
                let valid = match token {
                    Token::Segment(Segment::Message(_)) => definition.message_id.is_some(),
                    Token::Segment(Segment::Tone(_)) => {
                        definition.media.tone.as_ref().is_some_and(|tone| {
                            crate::audio::ToneSequence::new(&tone.sequence, tone.level_db).is_ok()
                        })
                    }
                    Token::Segment(Segment::Media(_)) => {
                        definition.media.file.is_some()
                            || definition.media.speech.is_some()
                            || definition.media.morse.is_some()
                    }
                    _ => true,
                };
                if !valid {
                    return Err(CompositionError::MissingDefinition);
                }
            }
            work.push((name.clone(), true));
            work.extend(references(&parsed).map(|name| (name.to_owned(), false)));
            tokens.insert(name, parsed);
        }
        let size = measure(&root, &sizes)?;
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(size.segments)
            .map_err(|_| CompositionError::TooLarge)?;
        let mut stack = vec![root.iter()];
        while let Some(current) = stack.last_mut() {
            match current.next() {
                Some(Token::Reference(name)) => stack.push(tokens[name].iter()),
                Some(Token::Segment(segment)) => segments.push(copy_segment(segment)?),
                None => {
                    stack.pop();
                }
            }
        }
        Ok(Self { segments })
    }

    /// Ordered segments; sequence boundaries do not imply independent completion or PTT.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }
}

fn references(tokens: &[Token]) -> impl Iterator<Item = &str> {
    tokens.iter().filter_map(|token| match token {
        Token::Reference(name) => Some(name.as_str()),
        _ => None,
    })
}

fn measure(tokens: &[Token], sizes: &BTreeMap<String, Size>) -> Result<Size, CompositionError> {
    let mut total = Size::default();
    for token in tokens {
        let size = match token {
            Token::Reference(name) => sizes[name],
            Token::Segment(segment) => {
                let text = match segment {
                    Segment::Literal(text) => text.as_str(),
                    Segment::Message(name) | Segment::Media(name) | Segment::Tone(name) => {
                        name.as_deref().unwrap_or_default()
                    }
                };
                Size {
                    segments: 1,
                    bytes: size_of::<Segment>() + text.len(),
                }
            }
        };
        total.segments = total
            .segments
            .checked_add(size.segments)
            .ok_or(CompositionError::TooLarge)?;
        total.bytes = total
            .bytes
            .checked_add(size.bytes)
            .filter(|n| *n <= isize::MAX as usize)
            .ok_or(CompositionError::TooLarge)?;
    }
    Ok(total)
}

fn copy_text(text: &str) -> Result<String, CompositionError> {
    let mut result = String::new();
    result
        .try_reserve_exact(text.len())
        .map_err(|_| CompositionError::TooLarge)?;
    result.push_str(text);
    Ok(result)
}

fn copy_segment(segment: &Segment) -> Result<Segment, CompositionError> {
    Ok(match segment {
        Segment::Literal(text) => Segment::Literal(copy_text(text)?),
        Segment::Message(name) => Segment::Message(name.as_deref().map(copy_text).transpose()?),
        Segment::Media(name) => Segment::Media(name.as_deref().map(copy_text).transpose()?),
        Segment::Tone(name) => Segment::Tone(name.as_deref().map(copy_text).transpose()?),
    })
}

fn parse(content: &str, owner: Option<&str>) -> Result<Vec<Token>, CompositionError> {
    let mut result = Vec::new();
    let mut literal = String::new();
    let mut chars = content.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' | '}' if chars.peek() == Some(&ch) => {
                chars.next();
                literal.push(ch);
            }
            '}' => return Err(CompositionError::Syntax),
            '{' => {
                flush_literal(&mut result, &mut literal)?;
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') | None => return Err(CompositionError::Syntax),
                        Some(ch) => name.push(ch),
                    }
                }
                let token = match name.as_str() {
                    "message" => Token::Segment(Segment::Message(owner.map(str::to_owned))),
                    "media" => Token::Segment(Segment::Media(owner.map(str::to_owned))),
                    "tone_sequence" => Token::Segment(Segment::Tone(owner.map(str::to_owned))),
                    name => Token::Reference(
                        name.strip_prefix("message_macro_")
                            .filter(|name| !name.is_empty())
                            .ok_or(CompositionError::Syntax)?
                            .into(),
                    ),
                };
                result.push(token);
            }
            _ => literal.push(ch),
        }
    }
    flush_literal(&mut result, &mut literal)?;
    Ok(result)
}

/// Schema and resolver share grammar validation; reference resolution occurs after inheritance.
pub(crate) fn syntax_valid(content: &str) -> bool {
    parse(content, None).is_ok()
}

fn flush_literal(tokens: &mut Vec<Token>, text: &mut String) -> Result<(), CompositionError> {
    let value = text.trim();
    if value.len() > MAX_MESSAGE_BYTES {
        return Err(CompositionError::TooLarge);
    }
    if !value.is_empty() {
        tokens.push(Token::Segment(Segment::Literal(value.into())));
    }
    text.clear();
    Ok(())
}
