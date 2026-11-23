//! The Key Vault layer.
//!
//! The layer exists only when `secrets.key_vault_url` is set by a lower layer.
//! It reads the names of the vault's secrets, which is metadata only, keeps
//! the names that map to a configuration path, and fetches the value of those
//! and of no other secret.

use async_trait::async_trait;
use config::{Map, Value, ValueKind};

use super::env::insert;
use super::names::path_from_vault_name;
use super::{Layer, Secret};
use crate::Result;
use crate::logging::Logger;
use crate::retry::retry;

/// The operations Butterbot needs from the secret store behind the Key Vault
/// layer. Implemented for Azure Key Vault and, in tests, by a fake.
#[async_trait]
pub trait SecretVault: Send + Sync {
    /// The names of the secrets that can be read. Must not fetch any value.
    async fn list_names(&self) -> Result<Vec<String>>;

    /// The current value of the secret `name`.
    async fn get(&self, name: &str) -> Result<Secret>;

    /// Stores `value` as the current value of the secret `name`.
    async fn set(&self, name: &str, value: &Secret) -> Result<()>;
}

/// Reads the Key Vault layer. Transient failures are retried.
pub(super) async fn vault_layer(vault: &dyn SecretVault, log: &Logger) -> Result<Layer> {
    let mut names = retry(log, "listing the secrets of the Key Vault", || {
        vault.list_names()
    })
    .await?;
    names.sort();
    let mut values = Map::new();
    for name in names {
        let Some(path) = path_from_vault_name(&name) else {
            continue;
        };
        let action = format!("reading the Key Vault secret {name}");
        let secret = retry(log, &action, || vault.get(&name)).await?;
        let origin = format!("Key Vault secret {name}");
        let value = Value::new(Some(&origin), ValueKind::String(secret.expose().to_owned()));
        insert(&mut values, &path, value);
    }
    Ok(Layer::new(values))
}

/// Azure Key Vault as the secret store behind the Key Vault layer.
pub struct AzureSecretVault {
    url: String,
    client: azure_security_keyvault_secrets::SecretClient,
}

impl AzureSecretVault {
    /// Opens the vault at `url` with the ambient Azure identity. Nothing is
    /// sent until the first operation.
    pub fn connect(url: &str, http: &reqwest::Client) -> Result<Self> {
        let credential = crate::azure::credential(http)?;
        Self::with(url, credential, crate::azure::client_options(http))
    }

    fn with(
        url: &str,
        credential: std::sync::Arc<dyn azure_core::credentials::TokenCredential>,
        client_options: azure_core::http::ClientOptions,
    ) -> Result<Self> {
        use azure_security_keyvault_secrets::{SecretClient, SecretClientOptions};

        let options = SecretClientOptions {
            client_options,
            ..Default::default()
        };
        let client = SecretClient::new(url, credential, Some(options)).map_err(|error| {
            crate::Error::permanent(format!("{url} is not usable as a Key Vault URL: {error}"))
        })?;
        Ok(Self {
            url: url.to_owned(),
            client,
        })
    }

    fn classify(&self, action: &str, error: &azure_core::Error) -> crate::Error {
        crate::azure::classify(&format!("{action} in Key Vault {}", self.url), error)
    }
}

#[async_trait]
impl SecretVault for AzureSecretVault {
    /// Lists the enabled secrets. The listing carries properties only.
    async fn list_names(&self) -> Result<Vec<String>> {
        use azure_security_keyvault_secrets::ResourceExt;
        use futures_util::TryStreamExt;

        let failed = |error: azure_core::Error| self.classify("listing the secret names", &error);
        let mut pager = self.client.list_secret_properties(None).map_err(failed)?;
        let mut names = Vec::new();
        while let Some(properties) = pager.try_next().await.map_err(failed)? {
            let enabled = properties
                .attributes
                .as_ref()
                .and_then(|attributes| attributes.enabled);
            if enabled == Some(false) {
                continue;
            }
            names.push(properties.resource_id().map_err(failed)?.name);
        }
        Ok(names)
    }

    async fn get(&self, name: &str) -> Result<Secret> {
        let failed =
            |error: azure_core::Error| self.classify(&format!("reading the secret {name}"), &error);
        let secret = self
            .client
            .get_secret(name, None)
            .await
            .map_err(failed)?
            .into_model()
            .map_err(failed)?;
        secret.value.map(Secret::new).ok_or_else(|| {
            crate::Error::permanent(format!(
                "the secret {name} in Key Vault {} has no value",
                self.url
            ))
        })
    }

    async fn set(&self, name: &str, value: &Secret) -> Result<()> {
        use azure_security_keyvault_secrets::models::SetSecretParameters;

        let failed =
            |error: azure_core::Error| self.classify(&format!("writing the secret {name}"), &error);
        let parameters = SetSecretParameters {
            value: Some(value.expose().to_owned()),
            ..Default::default()
        };
        let content = parameters.try_into().map_err(failed)?;
        self.client
            .set_secret(name, content, None)
            .await
            .map_err(failed)?;
        Ok(())
    }
}

/// The Azure client against constructed HTTP exchanges.
///
/// The responses are built from the Key Vault REST API reference (operations
/// "Get Secrets", "Get Secret" and "Set Secret", and the authentication
/// challenge described under "Authentication, requests and responses"). They
/// are constructed, not recorded from a real vault.
#[cfg(test)]
mod azure_tests {
    use super::*;
    use crate::azure::testing::{Exchange, MockHttp};

    const URL: &str = "https://butterbot-config.vault.example.com";

    fn vault(http: &MockHttp) -> AzureSecretVault {
        AzureSecretVault::with(
            URL,
            crate::azure::testing::credential(),
            http.client_options(),
        )
        .expect("the URL is usable")
    }

    #[tokio::test]
    async fn list_follows_pages_and_leaves_out_disabled_secrets() {
        let http = MockHttp::key_vault([
            Exchange::json(
                200,
                r#"{
                  "value": [
                    {
                      "contentType": "plainText",
                      "id": "https://butterbot-config.vault.example.com/secrets/dns--cloudflare--access-key",
                      "attributes": { "enabled": true, "created": 1482189047, "updated": 1482189047 }
                    },
                    {
                      "id": "https://butterbot-config.vault.example.com/secrets/retired",
                      "attributes": { "enabled": false, "created": 1482189047, "updated": 1482189047 }
                    }
                  ],
                  "nextLink": "https://butterbot-config.vault.example.com:443/secrets?api-version=7.6&$skiptoken=page2"
                }"#,
            ),
            Exchange::json(
                200,
                r#"{
                  "value": [
                    {
                      "id": "https://butterbot-config.vault.example.com/secrets/database-password",
                      "attributes": { "enabled": true, "created": 1482189047, "updated": 1482189047 }
                    }
                  ],
                  "nextLink": null
                }"#,
            ),
        ]);

        let names = vault(&http)
            .list_names()
            .await
            .expect("both pages are read");

        assert_eq!(names, ["dns--cloudflare--access-key", "database-password"]);
        let requests = http.authorized_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/secrets");
        assert!(
            requests[1].query.contains("skiptoken=page2"),
            "{:?}",
            requests[1]
        );
    }

    #[tokio::test]
    async fn get_returns_the_value() {
        let http = MockHttp::key_vault([Exchange::json(
            200,
            r#"{
              "value": "token-from-the-vault",
              "id": "https://butterbot-config.vault.example.com/secrets/dns--cloudflare--access-key/4387e9f3d6e14c459867679a90fd0f79",
              "attributes": {
                "enabled": true,
                "created": 1493938410,
                "updated": 1493938410,
                "recoveryLevel": "Recoverable+Purgeable"
              }
            }"#,
        )]);

        let secret = vault(&http)
            .get("dns--cloudflare--access-key")
            .await
            .expect("the secret is read");

        assert_eq!(secret.expose(), "token-from-the-vault");
        let requests = http.authorized_requests();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/secrets/dns--cloudflare--access-key/");
    }

    #[tokio::test]
    async fn set_sends_the_value() {
        let http = MockHttp::key_vault([Exchange::json(
            200,
            r#"{
              "value": "{\"id\":\"https://acme.example.com/acct/1\"}",
              "id": "https://butterbot-config.vault.example.com/secrets/acme--letsencrypt--access-key/4387e9f3d6e14c459867679a90fd0f79",
              "attributes": {
                "enabled": true,
                "created": 1493938410,
                "updated": 1493938410,
                "recoveryLevel": "Recoverable+Purgeable"
              }
            }"#,
        )]);

        let credentials = Secret::new(r#"{"id":"https://acme.example.com/acct/1"}"#);
        vault(&http)
            .set("acme--letsencrypt--access-key", &credentials)
            .await
            .expect("stored");

        let requests = http.requests();
        assert_eq!(requests.len(), 2);
        assert!(
            !requests[0].authorized && requests[0].body.is_empty(),
            "{:?}",
            requests[0]
        );
        assert!(
            requests[1].authorized,
            "the value is sent only with the token"
        );
        assert_eq!(requests[1].method, "PUT");
        assert_eq!(requests[1].path, "/secrets/acme--letsencrypt--access-key");
        assert_eq!(
            requests[1].body,
            r#"{"value":"{\"id\":\"https://acme.example.com/acct/1\"}"}"#
        );
    }

    #[tokio::test]
    async fn refusal_is_permanent_and_names_the_vault() {
        let http = MockHttp::key_vault([Exchange::json(
            403,
            r#"{
              "error": {
                "code": "Forbidden",
                "message": "Caller is not authorized to perform action on resource.",
                "innererror": { "code": "ForbiddenByRbac" }
              }
            }"#,
        )]);

        let error = vault(&http)
            .list_names()
            .await
            .expect_err("access is refused");

        assert_eq!(error.class(), "permanent");
        let message = error.to_string();
        assert_eq!(
            message,
            format!(
                "listing the secret names in Key Vault {URL}: HTTP 403: Caller is not authorized \
                 to perform action on resource."
            )
        );
        assert_eq!(
            http.authorized_requests().len(),
            1,
            "the SDK does not retry by itself"
        );
    }

    #[tokio::test]
    async fn server_failure_is_transient_and_left_to_the_retry_helper() {
        let http = MockHttp::key_vault([Exchange::json(
            503,
            r#"{ "error": { "code": "ServiceUnavailable", "message": "The service is unavailable." } }"#,
        )]);

        let error = vault(&http)
            .get("dns--cloudflare--access-key")
            .await
            .expect_err("unavailable");

        assert!(error.is_transient(), "{error:?}");
        assert_eq!(
            http.authorized_requests().len(),
            1,
            "the SDK does not retry by itself"
        );
    }
}
