//! Configuration: layered sources, provider instances, certificate entries,
//! secrets addressed by path, and validation.

mod duration;
mod error;
mod layer;
mod model;
mod parse;
mod secret;

pub use duration::{format_duration, parse_duration};
pub use error::ConfigError;
pub use layer::Layer;
pub use model::*;
pub use secret::Secret;

impl Config {
    /// Builds the configuration from layers, each overriding the one before.
    pub fn from_layers(layers: impl IntoIterator<Item = Layer>) -> Result<Self, ConfigError> {
        parse::parse(layer::merge(layers))
    }
}

#[cfg(test)]
mod tests;
