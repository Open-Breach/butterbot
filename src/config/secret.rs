//! The wrapper type for secret configuration values.

use std::fmt;

/// What is shown in place of a secret value.
pub const PLACEHOLDER: &str = "[secret]";

/// A secret configuration value, such as an API token or ACME account
/// credentials.
///
/// `Debug` and `Display` print [`PLACEHOLDER`], so that a secret cannot reach a
/// log or an error message by being formatted. The value is only available
/// through [`Secret::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret value. Call this only where the value is handed to the
    /// service it authenticates to.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PLACEHOLDER)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PLACEHOLDER)
    }
}
