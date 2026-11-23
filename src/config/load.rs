//! Loading: every layer, in order, into one validated configuration.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use config::{Value, ValueKind};

use super::layer::merge;
use super::vault::vault_layer;
use super::{Config, ConfigError, Layer, SecretVault, env_layer, parse, read_files};
use crate::logging::Logger;

/// A loaded configuration together with the vault behind its Key Vault layer.
pub struct Loaded {
    pub config: Config,
    /// The vault named by `secrets.key_vault_url`, or `None` when the Key
    /// Vault layer is not enabled. Kept so that the credentials of a newly
    /// registered ACME account can be saved to it.
    pub vault: Option<Arc<dyn SecretVault>>,
}

/// Loads the configuration: the files in order, then the environment, then,
/// when `secrets.key_vault_url` is set by those, Key Vault.
///
/// `connect` opens the vault at the given URL. It is not called when the Key
/// Vault layer is not enabled. Retries of the vault are written to `log`.
pub async fn load<C>(
    log: &Logger,
    files: &[PathBuf],
    variables: impl IntoIterator<Item = (OsString, OsString)>,
    connect: C,
) -> Result<Loaded, ConfigError>
where
    C: FnOnce(&str) -> crate::Result<Arc<dyn SecretVault>>,
{
    let mut layers: Vec<Layer> = Vec::new();
    let mut problems = Vec::new();
    match read_files(files) {
        Ok(file_layers) => layers.extend(file_layers),
        Err(error) => problems.extend(error.problems),
    }
    match env_layer(variables) {
        Ok(layer) => layers.push(layer),
        Err(error) => problems.extend(error.problems),
    }
    if !problems.is_empty() {
        return Err(ConfigError::new(problems));
    }

    let mut vault = None;
    if let Some(url) = key_vault_url(&merge(layers.iter().cloned())) {
        let unreadable = |error: crate::Error| {
            ConfigError::single(format!("Key Vault {url} cannot be read: {error}"))
        };
        let connected = connect(&url).map_err(unreadable)?;
        layers.push(
            vault_layer(connected.as_ref(), log)
                .await
                .map_err(unreadable)?,
        );
        vault = Some(connected);
    }

    let config = parse::parse(merge(layers))?;
    Ok(Loaded { config, vault })
}

/// The value of `secrets.key_vault_url` in a merged tree, if it is usable.
/// A value of the wrong type is left for validation to report.
fn key_vault_url(root: &Value) -> Option<String> {
    let ValueKind::Table(root) = &root.kind else {
        return None;
    };
    let ValueKind::Table(secrets) = &root.get("secrets")?.kind else {
        return None;
    };
    match &secrets.get("key_vault_url")?.kind {
        ValueKind::String(url) if !url.is_empty() => Some(url.clone()),
        _ => None,
    }
}
