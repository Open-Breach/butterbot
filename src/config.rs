//! Configuration: layered sources, provider instances, certificate entries,
//! secrets addressed by path, and validation.

mod duration;
mod env;
mod error;
mod files;
mod layer;
mod load;
mod model;
mod names;
mod parse;
mod resolve;
mod secret;
mod secrets;
mod vault;

pub use duration::{format_duration, parse_duration};
pub use env::env_layer;
pub use error::ConfigError;
pub use files::{DEFAULT_PATH, PATH_VARIABLE, config_paths, read_files};
pub use layer::Layer;
pub use load::{Loaded, load};
pub use model::*;
pub use names::{ENV_PREFIX, env_name, vault_name};
pub use resolve::{Resolved, ResolvedStore};
pub use secret::{PLACEHOLDER, Secret};
pub use secrets::ExpectedSecret;
pub use vault::{AzureSecretVault, MemoryVault, SecretVault};

impl Config {
    /// Builds the configuration from layers, each overriding the one before.
    pub fn from_layers(layers: impl IntoIterator<Item = Layer>) -> Result<Self, ConfigError> {
        parse::parse(layer::merge(layers))
    }
}

#[cfg(test)]
mod tests;
