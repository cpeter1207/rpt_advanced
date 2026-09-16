//! Strict, bounded scheduled-message templates.

use std::fmt;

/// Maximum rendered payload size, excluding C's equivalent trailing NUL byte.
const MAX_OUTPUT_BYTES: usize = 127;

/// Maximum replacement widths guaranteed by the current controller inputs.
const WEEKDAY_MAX: usize = 9;
const DATE_MAX: usize = 10;
const TIME_MAX: usize = 8;
const GREETING_MAX: usize = 14;
const LINK_STATUS_MAX: usize = 106;
const NODE_NAME_MAX: usize = 63;

/// A validated scheduled-message template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageTemplate(String);

/// Values available to a scheduled-message template.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemplateValues<'a> {
    /// Local weekday name.
    pub day_of_week: &'a str,
    /// Local date text.
    pub date: &'a str,
    /// Local clock text.
    pub time: &'a str,
    /// Time-of-day greeting.
    pub greeting: &'a str,
    /// Current direct-link status summary.
    pub link_status: &'a str,
    /// Local node identifier.
    pub node: &'a str,
    /// Local station callsign.
    pub callsign: &'a str,
}

/// A scheduled-message template validation or rendering failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateError {
    /// A substitution is malformed or is not one of the documented names.
    InvalidSyntax,
    /// The complete rendered payload would exceed 127 bytes.
    OutputTooLong,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidSyntax => "invalid message template syntax",
            Self::OutputTooLong => "message template output is too long",
        })
    }
}

impl std::error::Error for TemplateError {}

impl MessageTemplate {
    /// Parse a template and reject malformed, unknown, or conservatively oversized output.
    pub fn parse(text: &str) -> Result<Self, TemplateError> {
        let mut maximum = 0;
        visit_template(text, |part| {
            maximum = checked_total(maximum, part.maximum_length())?;
            Ok(())
        })?;
        Ok(Self(text.to_owned()))
    }

    /// Render the template completely, returning an error instead of truncated output.
    pub fn render(&self, values: &TemplateValues<'_>) -> Result<String, TemplateError> {
        let mut output = String::new();
        visit_template(&self.0, |part| {
            let text = match part {
                TemplatePart::Literal(text) => text,
                TemplatePart::Substitution(name) => replacement(name, values),
            };
            checked_total(output.len(), text.len())?;
            output.push_str(text);
            Ok(())
        })?;
        Ok(output)
    }
}

#[derive(Clone, Copy)]
enum TemplatePart<'a> {
    Literal(&'a str),
    Substitution(Substitution),
}

impl TemplatePart<'_> {
    fn maximum_length(self) -> usize {
        match self {
            Self::Literal(text) => text.len(),
            Self::Substitution(substitution) => substitution.maximum_length(),
        }
    }
}

#[derive(Clone, Copy)]
enum Substitution {
    DayOfWeek,
    Date,
    Time,
    Greeting,
    LinkStatus,
    Node,
    Callsign,
}

impl Substitution {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "day_of_week" => Some(Self::DayOfWeek),
            "date" => Some(Self::Date),
            "time" => Some(Self::Time),
            "greeting" => Some(Self::Greeting),
            "link_status" => Some(Self::LinkStatus),
            "node" => Some(Self::Node),
            "callsign" => Some(Self::Callsign),
            _ => None,
        }
    }

    fn maximum_length(self) -> usize {
        match self {
            Self::DayOfWeek => WEEKDAY_MAX,
            Self::Date => DATE_MAX,
            Self::Time => TIME_MAX,
            Self::Greeting => GREETING_MAX,
            Self::LinkStatus => LINK_STATUS_MAX,
            Self::Node | Self::Callsign => NODE_NAME_MAX,
        }
    }
}

fn visit_template<'a>(
    text: &'a str,
    mut visit: impl FnMut(TemplatePart<'a>) -> Result<(), TemplateError>,
) -> Result<(), TemplateError> {
    let mut remaining = text;
    while let Some(start) = remaining.find("${") {
        visit(TemplatePart::Literal(&remaining[..start]))?;
        let substitution = &remaining[start + 2..];
        let end = substitution.find('}').ok_or(TemplateError::InvalidSyntax)?;
        let name = &substitution[..end];
        let parsed = Substitution::parse(name).ok_or(TemplateError::InvalidSyntax)?;
        visit(TemplatePart::Substitution(parsed))?;
        remaining = &substitution[end + 1..];
    }
    visit(TemplatePart::Literal(remaining))
}

fn replacement<'a>(substitution: Substitution, values: &TemplateValues<'a>) -> &'a str {
    match substitution {
        Substitution::DayOfWeek => values.day_of_week,
        Substitution::Date => values.date,
        Substitution::Time => values.time,
        Substitution::Greeting => values.greeting,
        Substitution::LinkStatus => values.link_status,
        Substitution::Node => values.node,
        Substitution::Callsign => values.callsign,
    }
}

fn checked_total(used: usize, added: usize) -> Result<usize, TemplateError> {
    if added > MAX_OUTPUT_BYTES - used {
        return Err(TemplateError::OutputTooLong);
    }
    Ok(used + added)
}
