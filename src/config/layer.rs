//! Configuration layers and how a TOML document becomes one.
//!
//! A layer is a tree of values from one source. Every value remembers where it
//! was set, so that a problem can be reported together with its origin.

use std::fmt;

use config::{Map, Source, Value, ValueKind};

/// The values of one configuration source.
#[derive(Clone)]
pub struct Layer {
    values: Map<String, Value>,
}

impl Layer {
    /// Reads a TOML document. `origin` says where the document comes from and
    /// is attached to every value.
    ///
    /// The error message gives the position of a syntax error but never quotes
    /// the document, because the offending line may hold a secret.
    pub fn from_toml(origin: &str, text: &str) -> Result<Self, String> {
        let table: toml::Table = toml::from_str(text).map_err(|error| {
            let position = error
                .span()
                .map(|span| {
                    let before = &text[..span.start.min(text.len())];
                    let line = before.matches('\n').count() + 1;
                    let column = before
                        .rsplit('\n')
                        .next()
                        .map_or(0, |last| last.chars().count())
                        + 1;
                    format!("line {line}, column {column}: ")
                })
                .unwrap_or_default();
            format!("{position}{}", error.message())
        })?;
        let origin = origin.to_owned();
        let values = table
            .into_iter()
            .map(|(key, value)| (key, from_toml(&origin, value)))
            .collect();
        Ok(Self { values })
    }
}

/// A layer is not printed with its values: they may include secrets.
impl fmt::Debug for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Layer").finish_non_exhaustive()
    }
}

impl Source for Layer {
    fn clone_into_box(&self) -> Box<dyn Source + Send + Sync> {
        Box::new(self.clone())
    }

    fn collect(&self) -> Result<Map<String, Value>, config::ConfigError> {
        Ok(self.values.clone())
    }
}

fn from_toml(origin: &String, value: toml::Value) -> Value {
    let kind = match value {
        toml::Value::String(text) => ValueKind::String(text),
        toml::Value::Integer(number) => ValueKind::I64(number),
        toml::Value::Float(number) => ValueKind::Float(number),
        toml::Value::Boolean(flag) => ValueKind::Boolean(flag),
        toml::Value::Datetime(datetime) => ValueKind::String(datetime.to_string()),
        toml::Value::Array(items) => ValueKind::Array(
            items
                .into_iter()
                .map(|item| from_toml(origin, item))
                .collect(),
        ),
        toml::Value::Table(table) => ValueKind::Table(
            table
                .into_iter()
                .map(|(key, item)| (key, from_toml(origin, item)))
                .collect(),
        ),
    };
    Value::new(Some(origin), kind)
}

/// Merges layers, each overriding the one before, into one tree.
pub(super) fn merge(layers: impl IntoIterator<Item = Layer>) -> Value {
    let mut builder = config::Config::builder();
    for layer in layers {
        builder = builder.add_source(layer);
    }
    // A `Layer` cannot fail to collect, and nothing else is in the builder.
    builder
        .build()
        .map(|merged| merged.cache)
        .unwrap_or_else(|_| Map::<String, Value>::new().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_error_gives_the_position_and_not_the_text() {
        let text = "[dns.cloudflare]\ntype = \"cloudflare\"\naccess_key = \"hunter2\n";
        let error = Layer::from_toml("test.toml", text).expect_err("unterminated string");
        assert!(error.starts_with("line 3, column "), "{error}");
        assert!(!error.contains("hunter2"), "{error}");
    }

    #[test]
    fn debug_output_has_no_values() {
        let layer = Layer::from_toml("test.toml", "[dns.cloudflare]\naccess_key = \"hunter2\"\n")
            .expect("valid TOML");
        assert!(!format!("{layer:?}").contains("hunter2"));
    }
}
