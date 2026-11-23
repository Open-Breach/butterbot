//! The environment layer.

use std::ffi::OsString;

use config::{Map, Value, ValueKind};

use super::names::path_from_env_name;
use super::{ConfigError, Layer};

/// Builds the environment layer from environment variables.
///
/// Only variables that start with `BUTTERBOT__` are looked at. Every value is
/// text. A variable with the prefix that does not name a configuration key, or
/// whose value is not valid UTF-8, is a configuration error.
pub fn env_layer(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Layer, ConfigError> {
    // The variables of a process come in no particular order. Sorting makes
    // the outcome and the order of reported problems repeatable.
    let mut ours: Vec<(String, Vec<String>, OsString)> = Vec::new();
    let mut problems = Vec::new();
    for (name, value) in variables {
        let Some(name) = name.to_str() else { continue };
        match path_from_env_name(name) {
            None => {}
            Some(Ok(path)) => ours.push((name.to_owned(), path, value)),
            Some(Err(problem)) => problems.push(problem),
        }
    }
    ours.sort();
    problems.sort();

    let mut values = Map::new();
    for (name, path, value) in ours {
        match value.into_string() {
            Ok(text) => {
                let origin = format!("environment variable {name}");
                insert(
                    &mut values,
                    &path,
                    Value::new(Some(&origin), ValueKind::String(text)),
                );
            }
            Err(_) => problems.push(format!("environment variable {name} is not valid UTF-8")),
        }
    }
    if problems.is_empty() {
        Ok(Layer::new(values))
    } else {
        Err(ConfigError::new(problems))
    }
}

/// Sets `value` at `path`, creating the tables on the way.
pub(super) fn insert(values: &mut Map<String, Value>, path: &[String], value: Value) {
    let Some((key, rest)) = path.split_first() else {
        return;
    };
    if rest.is_empty() {
        values.insert(key.clone(), value);
        return;
    }
    let child = values
        .entry(key.clone())
        .or_insert_with(|| Value::new(None, ValueKind::Table(Map::new())));
    if !matches!(child.kind, ValueKind::Table(_)) {
        *child = Value::new(None, ValueKind::Table(Map::new()));
    }
    if let ValueKind::Table(table) = &mut child.kind {
        insert(table, rest, value);
    }
}
