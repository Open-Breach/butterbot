//! Tests of the configuration module as a whole.

use std::time::Duration;

use super::*;

mod env;
mod files;
mod model;
mod validation;
mod vault;

/// A configuration that sets every key the model has.
pub(super) const FULL: &str = r#"
[run]
parallel = 2
min_reissue = "3d"
retry_wait_max = "90s"

[secrets]
key_vault_url = "https://butterbot-config.vault.example.com/"

[acme.letsencrypt]
directory_url = "https://acme.example.com/directory"
access_key = '{"id":"https://acme.example.com/acct/1","key_pkcs8":"..."}'
eab_kid = "kid-1"
eab_key = "eab-secret"
profile = "tlsserver"
renew_before = "20d"
key_type = "ecdsa-p256"
ari = false

[acme.letsencrypt-staging]
directory_url = "https://acme-staging.example.com/directory"

[dns.azure]
type = "azure-dns"
subscription_id = "00000000-0000-0000-0000-000000000000"
resource_group = "dns-zones"
ttl = "120s"
propagation_check = "resolver"
propagation_resolvers = ["192.0.2.1", "192.0.2.2:5353", "2001:db8::1"]
propagation_interval = "5s"
propagation_timeout = "10m"
settle = "45s"

[dns.cloudflare]
type = "cloudflare"
access_key = "cloudflare-token"

[store.vault]
type = "azure-key-vault"
vault_url = "https://butterbot-certs.vault.example.com/"
format = "pem"

[store.local]
type = "directory"
path = "/var/lib/butterbot/certs"

[[certificate]]
name = "wildcard-example-com"
names = ["*.example.com", "example.com"]
acme = "letsencrypt"
dns = "azure"
stores = ["vault", "local"]
renew_before = "15d"
key_type = "rsa4096"
profile = "shortlived"
format = "pfx"
propagation_timeout = "3m"
settle = "1m"

[[certificate]]
name = "api-example-org"
names = ["api.example.org"]
acme = "letsencrypt-staging"
dns = "cloudflare"
stores = ["local"]
"#;

/// Builds the configuration from one TOML document.
pub(super) fn from_toml(text: &str) -> Result<Config, ConfigError> {
    let layer = Layer::from_toml("test.toml", text).expect("the test document is valid TOML");
    Config::from_layers([layer])
}

/// The problems of a configuration that is expected to be rejected.
pub(super) fn problems(text: &str) -> Vec<String> {
    from_toml(text)
        .expect_err("the configuration is rejected")
        .problems
}

pub(super) fn days(days: u64) -> Duration {
    Duration::from_secs(days * 86_400)
}

/// Environment variables as the process would see them.
pub(super) fn variables(pairs: &[(&str, &str)]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    pairs
        .iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect()
}

/// Builds the configuration from one TOML document and environment variables.
pub(super) fn from_toml_and_env(text: &str, pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
    let file = Layer::from_toml("test.toml", text).expect("the test document is valid TOML");
    Config::from_layers([file, env_layer(variables(pairs))?])
}
