//! Templates for the URL, the header values and a custom body of a webhook.
//!
//! A template is plain text with placeholders written as `{{name}}`, from a
//! fixed list. There are no conditions, loops or filters. A template is
//! parsed when the configuration is loaded, so that an unknown placeholder is
//! a configuration error. It is rendered for one event, and rendering knows
//! where the result is used: a value is inserted so that it cannot change the
//! structure of what surrounds it.

use std::fmt;

use serde_json::Value;

use crate::events::{CompletedOutcome, Event};

/// The placeholders a template may contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    /// The event kind, for example `failure`.
    Event,
    /// The event in one sentence.
    Summary,
    /// The event's fields as lines of name and value.
    Details,
    /// For `run_completed`, the exit code its outcome maps to. Empty for
    /// every other event.
    ExitCode,
    /// The event as a JSON object.
    EventJson,
    /// The secret `notify.<name>.access_key`.
    AccessKey,
}

impl Placeholder {
    pub const ALL: [Self; 6] = [
        Self::Event,
        Self::Summary,
        Self::Details,
        Self::ExitCode,
        Self::EventJson,
        Self::AccessKey,
    ];

    /// The name as it is written between the braces.
    pub fn name(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Summary => "summary",
            Self::Details => "details",
            Self::ExitCode => "exit_code",
            Self::EventJson => "event_json",
            Self::AccessKey => "access_key",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|placeholder| placeholder.name() == name)
    }

    /// The names of all placeholders, for a message.
    pub fn names() -> String {
        Self::ALL.map(Self::name).join(", ")
    }
}

/// Why a text is not a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    /// `{{name}}` with a name that is not a placeholder.
    UnknownPlaceholder(String),
    /// `{{` without a `}}` after it.
    Unclosed,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlaceholder(name) => write!(f, "unknown placeholder `{{{{{name}}}}}`"),
            Self::Unclosed => f.write_str("`{{` without a closing `}}`"),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Placeholder(Placeholder),
}

/// A parsed template.
///
/// `Debug` does not print the text: the template of a URL or of a header
/// value is a secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
}

impl fmt::Debug for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[template]")
    }
}

/// Where a rendered template is used, which decides how a value is inserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// In a URL: percent-encoded.
    Url,
    /// In a header value: as it is.
    Header,
    /// In a body whose content type is JSON: escaped as the content of a JSON
    /// string, without the quotation marks. `event_json` is inserted as a
    /// JSON object.
    JsonBody,
    /// In a body of another content type: as it is.
    OtherBody,
}

impl Position {
    /// The position of a body with the given content type.
    pub fn of_body(content_type: &str) -> Self {
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if media_type == "application/json" || media_type.ends_with("+json") {
            Self::JsonBody
        } else {
            Self::OtherBody
        }
    }
}

impl Template {
    /// Reads `text` as a template.
    pub fn parse(text: &str) -> Result<Self, TemplateError> {
        let mut parts = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("{{") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                return Err(TemplateError::Unclosed);
            };
            let name = &after[..end];
            let placeholder = Placeholder::from_name(name)
                .ok_or_else(|| TemplateError::UnknownPlaceholder(name.to_owned()))?;
            if start > 0 {
                parts.push(Part::Text(rest[..start].to_owned()));
            }
            parts.push(Part::Placeholder(placeholder));
            rest = &after[end + 2..];
        }
        if !rest.is_empty() {
            parts.push(Part::Text(rest.to_owned()));
        }
        Ok(Self { parts })
    }

    /// Whether the template contains `placeholder`.
    pub fn uses(&self, placeholder: Placeholder) -> bool {
        self.parts.contains(&Part::Placeholder(placeholder))
    }

    /// The text before the first placeholder.
    pub fn literal_start(&self) -> &str {
        match self.parts.first() {
            Some(Part::Text(text)) => text,
            _ => "",
        }
    }

    /// The template with every placeholder replaced by its value for one
    /// event. A placeholder without a value for the event is replaced by
    /// nothing.
    pub fn render(&self, position: Position, values: &Values) -> String {
        let mut rendered = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => rendered.push_str(text),
                Part::Placeholder(placeholder) => {
                    let value = values.get(*placeholder);
                    match position {
                        Position::Url => percent_encode(value, &mut rendered),
                        Position::Header | Position::OtherBody => rendered.push_str(value),
                        Position::JsonBody if *placeholder == Placeholder::EventJson => {
                            rendered.push_str(value);
                        }
                        Position::JsonBody => escape_json(value, &mut rendered),
                    }
                }
            }
        }
        rendered
    }
}

/// The value of every placeholder for one event.
pub struct Values {
    event: &'static str,
    summary: String,
    details: String,
    exit_code: &'static str,
    event_json: String,
    access_key: String,
}

impl Values {
    /// The values for `event`. `access_key` is the value of the placeholder
    /// of that name, empty when the instance has no key.
    pub fn of(event: &Event, access_key: &str) -> Self {
        Self {
            event: event.kind().as_str(),
            summary: event.summary(),
            details: details(event),
            exit_code: exit_code(event),
            event_json: Value::Object(event.to_json()).to_string(),
            access_key: access_key.to_owned(),
        }
    }

    fn get(&self, placeholder: Placeholder) -> &str {
        match placeholder {
            Placeholder::Event => self.event,
            Placeholder::Summary => &self.summary,
            Placeholder::Details => &self.details,
            Placeholder::ExitCode => self.exit_code,
            Placeholder::EventJson => &self.event_json,
            Placeholder::AccessKey => &self.access_key,
        }
    }
}

/// The exit code the outcome of a `run_completed` event maps to: `0` or `1`.
/// Empty for every other event.
pub fn exit_code(event: &Event) -> &'static str {
    match event {
        Event::RunCompleted(completed) => match completed.outcome {
            CompletedOutcome::Success => "0",
            CompletedOutcome::Failed => "1",
        },
        Event::Failure(_) | Event::Renewed(_) | Event::Expiring(_) => "",
    }
}

/// The fields of an event as `name: value` lines, in the order of the event's
/// JSON, without `event`.
pub fn details(event: &Event) -> String {
    fields(event)
        .into_iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The fields of an event as text, in the order of the event's JSON, without
/// `event`.
pub fn fields(event: &Event) -> Vec<(String, String)> {
    event
        .to_json()
        .into_iter()
        .filter(|(name, _)| name != "event")
        .map(|(name, value)| {
            let text = field_text(&value);
            (name, text)
        })
        .collect()
}

/// The value of a field as a person reads it: text without quotation marks, a
/// list as its items separated by commas, and `none` for a field without a
/// value.
fn field_text(value: &Value) -> String {
    match value {
        Value::Null => "none".to_owned(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(field_text).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// Appends `value` so that it is one piece of a URL: everything except
/// letters, digits and `-._~` is percent-encoded.
fn percent_encode(value: &str, to: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            to.push(char::from(byte));
        } else {
            to.push('%');
            to.push(char::from(HEX[usize::from(byte >> 4)]));
            to.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
}

/// Appends `value` as the content of a JSON string, without the surrounding
/// quotation marks.
fn escape_json(value: &str, to: &mut String) {
    let quoted = Value::String(value.to_owned()).to_string();
    to.push_str(&quoted[1..quoted.len() - 1]);
}

#[cfg(test)]
mod tests;
