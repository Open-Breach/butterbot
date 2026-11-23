//! The accounts of a run: one per ACME server, set up when first needed.
//!
//! An account's credentials are a secret like any other and come from
//! configuration. When they are missing:
//!
//! - with the Key Vault layer enabled, an account is registered and its
//!   credentials are saved to the vault before anything is ordered. If saving
//!   fails, the account is not used: an account that cannot be found again
//!   would be registered anew on every run.
//! - without it, no account is registered during a run. The operator
//!   registers one with `butterbot account new`.
//!
//! Whatever the outcome for a server is, it holds for the whole run: every
//! certificate on that server gets the same account or the same failure, and
//! nothing is attempted twice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::OnceCell;

use super::AcmeClient;
use crate::config::{AcmeServer, SecretVault, env_name, vault_name};
use crate::logging::{Level, Log};
use crate::retry::retry;
use crate::{Error, Result};

type Outcome = Arc<OnceCell<Result<Arc<AcmeClient>>>>;

/// The ACME accounts of a run.
pub struct Accounts {
    http: reqwest::Client,
    /// The vault behind the Key Vault layer, when it is enabled.
    vault: Option<Arc<dyn SecretVault>>,
    outcomes: Mutex<HashMap<String, Outcome>>,
}

impl Accounts {
    pub fn new(http: reqwest::Client, vault: Option<Arc<dyn SecretVault>>) -> Self {
        Self {
            http,
            vault,
            outcomes: Mutex::new(HashMap::new()),
        }
    }

    /// The client for the ACME server instance `name`, with its account.
    ///
    /// The first call for a server sets the account up; later calls, also
    /// concurrent ones, get the result of the first.
    pub async fn client(
        &self,
        name: &str,
        server: &AcmeServer,
        log: &dyn Log,
    ) -> Result<Arc<AcmeClient>> {
        let outcome = self
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(name.to_owned())
            .or_default()
            .clone();
        outcome
            .get_or_init(|| async { self.set_up(name, server, log).await.map(Arc::new) })
            .await
            .clone()
    }

    async fn set_up(&self, name: &str, server: &AcmeServer, log: &dyn Log) -> Result<AcmeClient> {
        let url = server.directory_url.as_str();
        if let Some(credentials) = &server.access_key {
            let action = format!("loading the account at the ACME server {name}");
            return retry(log, &action, || {
                AcmeClient::with_account(name, url, credentials, &self.http)
            })
            .await;
        }

        let path = format!("acme.{name}.access_key");
        let Some(vault) = &self.vault else {
            return Err(Error::permanent(format!(
                "no account is configured for the ACME server {name}: {path} is not set. Register \
                 an account with `butterbot account new --acme {name}` and supply its credentials"
            )));
        };

        let binding = match (&server.eab_kid, &server.eab_key) {
            (Some(kid), Some(key)) => Some((kid.as_str(), key)),
            (Some(_), None) => {
                let path = format!("acme.{name}.eab_key");
                return Err(Error::permanent(format!(
                    "the ACME server {name} has an eab_kid but the secret {path} is not set; \
                     supply it as the environment variable {} or as the Key Vault secret {}",
                    env_name(&path),
                    vault_name(&path)
                )));
            }
            (None, _) => None,
        };
        let action = format!("registering an account at the ACME server {name}");
        let (client, credentials) = retry(log, &action, || {
            AcmeClient::register(name, url, binding, &self.http)
        })
        .await?;

        let secret = vault_name(&path);
        let action = format!("saving the account credentials as the Key Vault secret {secret}");
        retry(log, &action, || vault.set(&secret, &credentials)).await.map_err(|error| {
            let message = format!(
                "an account was registered at the ACME server {name}, but its credentials could \
                 not be saved as the Key Vault secret {secret} ({error}). Nothing is ordered with \
                 an account that would be lost"
            );
            match error {
                Error::Transient(_) => Error::transient(message),
                _ => Error::permanent(message),
            }
        })?;
        log.record(
            Level::Info,
            "registered an ACME account and saved its credentials to the Key Vault".to_owned(),
        )
        .field("acme", name)
        .field("account", client.account_id())
        .field("secret", secret)
        .write();
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;
    use crate::config::{MemoryVault, Secret};
    use crate::logging::{Capture, LogFormat, Logger};
    use crate::testing::AcmeTestServer;

    const SECRET: &str = "acme--letsencrypt--access-key";

    fn server(directory_url: &str, access_key: Option<Secret>) -> AcmeServer {
        AcmeServer {
            directory_url: directory_url.to_owned(),
            access_key,
            eab_kid: None,
            eab_key: None,
            profile: None,
            renew_before: None,
            key_type: None,
            ari: true,
        }
    }

    fn log() -> (Capture, Logger) {
        let capture = Capture::new();
        let logger = capture.logger(
            LogFormat::Json,
            Level::Info,
            datetime!(2026-11-23 02:00:00 UTC),
        );
        (capture, logger)
    }

    // acme-issuance, "Account registration on first use", scenarios "First run with a
    // vault" and "Second run with a vault".
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn with_a_vault_an_account_is_registered_and_saved_and_the_next_run_reuses_it() {
        let acme = AcmeTestServer::start();
        let url = acme.pebble.directory_url.as_str();
        let vault = Arc::new(MemoryVault::new());
        let (capture, log) = log();

        // The first run: no credentials anywhere.
        let first_run = Accounts::new(acme.pebble.http(), Some(vault.clone()));
        let registered = first_run
            .client("letsencrypt", &server(url, None), &log)
            .await
            .expect("an account");

        let saved = vault
            .value(SECRET)
            .expect("the credentials are saved under the derived name");
        let credentials: serde_json::Value = serde_json::from_str(saved.expose()).expect("JSON");
        assert_eq!(credentials["id"], registered.account_id());
        assert_eq!(vault.set_calls(), [SECRET]);
        let records = capture.json();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["secret"], SECRET);
        assert!(
            !capture.text().contains("key_pkcs8"),
            "the credentials are not logged"
        );

        // The second run: the configuration now has the credentials, from the vault layer.
        let second_run = Accounts::new(acme.pebble.http(), Some(vault.clone()));
        let reused = second_run
            .client("letsencrypt", &server(url, Some(saved)), &log)
            .await
            .expect("the saved account");

        assert_eq!(
            reused.account_id(),
            registered.account_id(),
            "the saved account is reused"
        );
        assert_eq!(
            vault.set_calls(),
            [SECRET],
            "nothing is registered or saved a second time"
        );
    }

    // acme-issuance, "Account registration on first use", scenario "First run without a vault".
    #[tokio::test]
    async fn without_a_vault_no_account_is_registered_and_the_message_points_to_account_new() {
        // Nothing listens at this address: a registration attempt would fail
        // with a network error, not with the message below.
        let url = format!("http://{}/directory", crate::testing::free_address());
        let accounts = Accounts::new(crate::http::client().expect("an HTTP client"), None);
        let (_, log) = log();

        let error = accounts
            .client("letsencrypt", &server(&url, None), &log)
            .await
            .err()
            .expect("no account is available");

        assert_eq!(error.class(), "permanent");
        assert_eq!(
            error.to_string(),
            "no account is configured for the ACME server letsencrypt: acme.letsencrypt.access_key \
             is not set. Register an account with `butterbot account new --acme letsencrypt` and \
             supply its credentials"
        );
    }

    // acme-issuance, "Account registration on first use", scenario "Credentials cannot be saved".
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn account_whose_credentials_cannot_be_saved_is_not_used() {
        let acme = AcmeTestServer::start();
        let url = acme.pebble.directory_url.as_str();
        let vault = Arc::new(MemoryVault::new());
        vault.fail_writes(Error::permanent("HTTP 403: Caller is not authorized"));
        let accounts = Accounts::new(acme.pebble.http(), Some(vault.clone()));
        let (_, log) = log();

        let error = accounts
            .client("letsencrypt", &server(url, None), &log)
            .await
            .err()
            .expect("the credentials cannot be saved");

        assert_eq!(
            error.to_string(),
            "an account was registered at the ACME server letsencrypt, but its credentials could \
             not be saved as the Key Vault secret acme--letsencrypt--access-key (HTTP 403: Caller \
             is not authorized). Nothing is ordered with an account that would be lost"
        );
        assert!(vault.value(SECRET).is_none());

        // Every further certificate on that server fails the same way, and no
        // second account is registered: the outcome holds for the run. There
        // is no client, so nothing can be ordered with the unsaved account.
        let again = accounts
            .client("letsencrypt", &server(url, None), &log)
            .await
            .err()
            .expect("the same failure");
        assert_eq!(again, error);
        assert_eq!(
            vault.set_calls().len(),
            1,
            "saving was attempted once, for one registration"
        );
    }

    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn certificates_processed_at_the_same_time_share_one_account() {
        let acme = AcmeTestServer::start();
        let url = acme.pebble.directory_url.as_str();
        let vault = Arc::new(MemoryVault::new());
        let accounts = Accounts::new(acme.pebble.http(), Some(vault.clone()));
        let (_, log) = log();
        let server = server(url, None);

        let (first, second, third) = tokio::join!(
            accounts.client("letsencrypt", &server, &log),
            accounts.client("letsencrypt", &server, &log),
            accounts.client("letsencrypt", &server, &log),
        );

        let id = first.expect("an account").account_id().to_owned();
        assert_eq!(second.expect("an account").account_id(), id);
        assert_eq!(third.expect("an account").account_id(), id);
        assert_eq!(vault.set_calls(), [SECRET], "one registration");
    }

    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn each_server_has_its_own_account() {
        let (production, staging) = (AcmeTestServer::start(), AcmeTestServer::start());
        let vault = Arc::new(MemoryVault::new());
        // One HTTP client that trusts both test servers.
        let http = crate::http::client_with_roots(&[
            production.pebble.root_pem.clone(),
            staging.pebble.root_pem.clone(),
        ])
        .expect("an HTTP client");
        let accounts = Accounts::new(http, Some(vault.clone()));
        let (_, log) = log();

        accounts
            .client(
                "letsencrypt",
                &server(&production.pebble.directory_url, None),
                &log,
            )
            .await
            .expect("an account");
        accounts
            .client(
                "letsencrypt-staging",
                &server(&staging.pebble.directory_url, None),
                &log,
            )
            .await
            .expect("an account");

        assert_eq!(
            vault.set_calls(),
            [
                "acme--letsencrypt--access-key",
                "acme--letsencrypt-staging--access-key"
            ]
        );
    }

    #[tokio::test]
    async fn binding_key_that_is_not_supplied_is_named() {
        let url = format!("http://{}/directory", crate::testing::free_address());
        let vault = Arc::new(MemoryVault::new());
        let accounts = Accounts::new(crate::http::client().expect("an HTTP client"), Some(vault));
        let (_, log) = log();
        let bound = AcmeServer {
            eab_kid: Some("kid-1".into()),
            ..server(&url, None)
        };

        let error = accounts
            .client("bound", &bound, &log)
            .await
            .err()
            .expect("the key is missing");

        assert_eq!(
            error.to_string(),
            "the ACME server bound has an eab_kid but the secret acme.bound.eab_key is not set; \
             supply it as the environment variable BUTTERBOT__ACME__BOUND__EAB_KEY or as the Key \
             Vault secret acme--bound--eab-key"
        );
    }

    // acme-issuance, "Any ACME server by configuration", scenario "Directory unreachable".
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn unreachable_server_fails_after_the_allowed_retries_and_is_named() {
        // Real credentials of a server that is then stopped.
        let acme = AcmeTestServer::start();
        let url = acme.pebble.directory_url.clone();
        let http = acme.pebble.http();
        let (_, credentials) = AcmeClient::register("letsencrypt", &url, None, &http)
            .await
            .expect("registered");
        drop(acme);
        // From here on the pauses between attempts are skipped.
        tokio::time::pause();

        let accounts = Accounts::new(http, None);
        let (capture, log) = log();
        let error = accounts
            .client("letsencrypt", &server(&url, Some(credentials)), &log)
            .await
            .err()
            .expect("the server cannot be reached");

        assert!(error.is_transient(), "{error:?}");
        assert!(
            error
                .to_string()
                .starts_with("ACME server letsencrypt: loading the account: "),
            "{error}"
        );
        let retried = capture
            .json()
            .iter()
            .filter(|record| record["level"] == "warn")
            .count();
        assert_eq!(retried, 2, "three attempts in total");
    }
}
