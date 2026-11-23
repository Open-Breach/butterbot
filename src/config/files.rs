//! Configuration files: which ones are read, and reading them.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use super::{ConfigError, Layer};

/// The file read when no other is named.
pub const DEFAULT_PATH: &str = "/etc/butterbot/config.toml";

/// The environment variable that replaces [`DEFAULT_PATH`].
pub const PATH_VARIABLE: &str = "BUTTERBOT_CONFIG";

/// The configuration files to read, in the order in which they are merged.
///
/// `options` are the `--config` options in the order given; they take
/// precedence. Without them the value of `BUTTERBOT_CONFIG` names the one file,
/// and without that the default path does. An empty variable counts as unset.
pub fn config_paths(options: &[PathBuf], variable: Option<&OsStr>) -> Vec<PathBuf> {
    if !options.is_empty() {
        return options.to_vec();
    }
    match variable {
        Some(path) if !path.is_empty() => vec![PathBuf::from(path)],
        _ => vec![PathBuf::from(DEFAULT_PATH)],
    }
}

/// Reads the files as layers, in order. A file that is missing, unreadable or
/// not valid TOML is a configuration error; every such file is reported.
pub fn read_files(paths: &[PathBuf]) -> Result<Vec<Layer>, ConfigError> {
    let mut layers = Vec::with_capacity(paths.len());
    let mut problems = Vec::new();
    for path in paths {
        match read_file(path) {
            Ok(layer) => layers.push(layer),
            Err(problem) => problems.push(problem),
        }
    }
    if problems.is_empty() {
        Ok(layers)
    } else {
        Err(ConfigError::new(problems))
    }
}

fn read_file(path: &Path) -> Result<Layer, String> {
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => format!("configuration file {shown} does not exist"),
        _ => format!("configuration file {shown} cannot be read: {error}"),
    })?;
    Layer::from_toml(&shown.to_string(), &text)
        .map_err(|problem| format!("configuration file {shown} is not valid TOML: {problem}"))
}
