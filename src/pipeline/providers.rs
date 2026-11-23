//! The providers of a run as the configuration describes them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::Providers;
use crate::acme::{Accounts, Issuer, Polling};
use crate::config::{Config, Resolved, SecretVault};
use crate::dns::DnsProvider;
use crate::dns::propagation::{Confirmed, Published, Settings, wait_for_propagation};
use crate::logging::{Level, StepLog};
use crate::policy::RenewalInfoSource;
use crate::store::CertStore;
use crate::{Error, Result};

/// The ACME servers, DNS providers and stores of a configuration, opened
/// when first needed and then kept for the run.
pub struct ConfiguredProviders {
    config: Arc<Config>,
    http: reqwest::Client,
    accounts: Accounts,
    dns: Mutex<HashMap<String, Arc<dyn DnsProvider>>>,
    stores: Mutex<HashMap<String, Arc<dyn CertStore>>>,
}

impl ConfiguredProviders {
    /// `vault` is the vault behind the Key Vault layer, when it is enabled.
    pub fn new(config: Arc<Config>, vault: Option<Arc<dyn SecretVault>>) -> Result<Self> {
        let http = http_client(&config)?;
        Ok(Self {
            accounts: Accounts::new(http.clone(), vault),
            config,
            http,
            dns: Mutex::default(),
            stores: Mutex::default(),
        })
    }

    /// Changes how ACME servers are polled for results.
    pub fn with_polling(mut self, polling: Polling) -> Self {
        self.accounts = self.accounts.with_polling(polling);
        self
    }

    /// The HTTP client the providers share.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    fn unknown(kind: &str, name: &str) -> Error {
        Error::permanent(format!(
            "the {kind} {name} is not defined in the configuration"
        ))
    }
}

/// The shared HTTP client: the bundled roots, and the extra root of every
/// ACME server that has one, which only a test build can have.
fn http_client(config: &Config) -> Result<reqwest::Client> {
    let mut extra_roots = Vec::new();
    for (name, server) in &config.acme {
        if let Some(path) = &server.extra_root {
            let text = std::fs::read_to_string(path).map_err(|error| {
                Error::permanent(format!(
                    "the root certificate {} of the ACME server {name} cannot be read: {error}",
                    path.display()
                ))
            })?;
            extra_roots.push(text);
        }
    }
    crate::http::client_with_roots(&extra_roots)
}

#[async_trait]
impl Providers for ConfiguredProviders {
    fn check(&self, entry: &Resolved<'_>) -> Result<()> {
        // A DNS provider whose credential no layer supplies cannot be opened;
        // the error names the missing configuration path.
        self.dns(&entry.certificate.dns)?;
        if !self.accounts.available(entry.acme) {
            let name = &entry.certificate.acme;
            return Err(Error::permanent(format!(
                "no account is configured for the ACME server {name}: acme.{name}.access_key is \
                 not set. Register an account with `butterbot account new --acme {name}` and \
                 supply its credentials"
            )));
        }
        Ok(())
    }

    async fn renewal_info(&self, acme: &str, log: &StepLog) -> Option<Arc<dyn RenewalInfoSource>> {
        let server = self.config.acme.get(acme)?;
        // Asking needs the account. Without credentials that would mean
        // registering one, which a certificate that is not due must not cause.
        if !server.ari || server.access_key.is_none() {
            return None;
        }
        match self.accounts.client(acme, server, log).await {
            Ok(client) => Some(client),
            Err(error) => {
                log.record(
                    Level::Info,
                    "renewal information is not available; the renewal period alone decides",
                )
                .reason(error)
                .write();
                None
            }
        }
    }

    async fn issuer(&self, acme: &str, log: &StepLog) -> Result<Arc<dyn Issuer>> {
        let server = self
            .config
            .acme
            .get(acme)
            .ok_or_else(|| Self::unknown("ACME server", acme))?;
        let client = self.accounts.client(acme, server, log).await?;
        Ok(client)
    }

    fn dns(&self, dns: &str) -> Result<Arc<dyn DnsProvider>> {
        let mut opened = self
            .dns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(provider) = opened.get(dns) {
            return Ok(provider.clone());
        }
        let instance = self
            .config
            .dns
            .get(dns)
            .ok_or_else(|| Self::unknown("DNS provider", dns))?;
        let provider = crate::dns::open(dns, instance, &self.http)?;
        opened.insert(dns.to_owned(), provider.clone());
        Ok(provider)
    }

    fn store(&self, store: &str) -> Result<Arc<dyn CertStore>> {
        let mut opened = self
            .stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handle) = opened.get(store) {
            return Ok(handle.clone());
        }
        let instance = self
            .config
            .stores
            .get(store)
            .ok_or_else(|| Self::unknown("store", store))?;
        let handle = crate::store::open(instance, &self.http)?;
        opened.insert(store.to_owned(), handle.clone());
        Ok(handle)
    }

    async fn confirm_propagation(
        &self,
        dns: &str,
        timeout: Duration,
        records: &[Published],
        log: &StepLog,
    ) -> Result<Confirmed> {
        let instance = self
            .config
            .dns
            .get(dns)
            .ok_or_else(|| Self::unknown("DNS provider", dns))?;
        wait_for_propagation(&Settings::for_instance(instance, timeout), records, log).await
    }
}
