//! The secrets a configuration expects, with their name in each source.

use super::model::*;
use super::names::{env_name, vault_name};

/// A secret the configuration requires, and where each source looks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedSecret {
    /// The configuration path, for example `dns.cloudflare.access_key`.
    pub path: String,
    /// The environment variable that sets it.
    pub env_name: String,
    /// The Key Vault secret that sets it.
    pub vault_name: String,
    /// Whether any layer supplied a value.
    pub found: bool,
}

impl ExpectedSecret {
    fn new(path: String, found: bool) -> Self {
        Self {
            env_name: env_name(&path),
            vault_name: vault_name(&path),
            path,
            found,
        }
    }
}

impl Config {
    /// Every secret the configuration requires: the account credentials of
    /// each ACME server, the binding key of each ACME server that has an
    /// `eab_kid`, and the API token of each Cloudflare DNS provider. Providers
    /// that use the ambient Azure identity need none.
    pub fn expected_secrets(&self) -> Vec<ExpectedSecret> {
        let mut secrets = Vec::new();
        for (name, server) in &self.acme {
            secrets.push(ExpectedSecret::new(
                format!("acme.{name}.access_key"),
                server.access_key.is_some(),
            ));
            if server.eab_kid.is_some() || server.eab_key.is_some() {
                secrets.push(ExpectedSecret::new(
                    format!("acme.{name}.eab_key"),
                    server.eab_key.is_some(),
                ));
            }
        }
        for (name, instance) in &self.dns {
            match &instance.provider {
                DnsProviderConfig::Cloudflare { access_key } => secrets.push(ExpectedSecret::new(
                    format!("dns.{name}.access_key"),
                    access_key.is_some(),
                )),
                DnsProviderConfig::AzureDns { .. } | DnsProviderConfig::ChallTestSrv { .. } => {}
            }
        }
        secrets
    }
}
