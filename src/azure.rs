//! What every use of Azure has in common: the identity, the options of the
//! SDK clients and the mapping of SDK errors to error classes.

use std::ffi::OsStr;
use std::sync::Arc;

use azure_core::credentials::TokenCredential;
use azure_core::error::ErrorKind;
use azure_core::http::{ClientOptions, RetryOptions, Transport};
use azure_identity::{
    AzureCliCredential, ClientAssertionCredentialOptions, WorkloadIdentityCredential,
    WorkloadIdentityCredentialOptions,
};

use crate::{Error, Result};

/// The variable through which a cluster hands a workload its identity token.
pub const FEDERATED_TOKEN_FILE: &str = "AZURE_FEDERATED_TOKEN_FILE";

/// The identities Butterbot can run as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// The workload identity of a pod.
    Workload,
    /// The account signed in to the Azure CLI, for use from a terminal.
    AzureCli,
}

impl Identity {
    /// Chooses the identity: workload identity when the cluster provides a
    /// token file, the Azure CLI sign-in otherwise.
    pub fn detect(federated_token_file: Option<&OsStr>) -> Self {
        match federated_token_file {
            Some(path) if !path.is_empty() => Self::Workload,
            _ => Self::AzureCli,
        }
    }
}

/// The ambient Azure identity as a token credential.
pub fn credential(http: &reqwest::Client) -> Result<Arc<dyn TokenCredential>> {
    let identity = Identity::detect(std::env::var_os(FEDERATED_TOKEN_FILE).as_deref());
    let unavailable = |error: azure_core::Error| {
        Error::permanent(format!(
            "the Azure identity ({identity:?}) is not available: {error}"
        ))
    };
    Ok(match identity {
        Identity::Workload => {
            let options = WorkloadIdentityCredentialOptions {
                credential_options: ClientAssertionCredentialOptions {
                    client_options: client_options(http),
                },
                ..Default::default()
            };
            WorkloadIdentityCredential::new(Some(options)).map_err(unavailable)?
        }
        Identity::AzureCli => AzureCliCredential::new(None).map_err(unavailable)?,
    })
}

/// Options for an Azure SDK client: requests go through the shared HTTP
/// client, and the SDK does not retry, because the retry policy lives in
/// [`crate::retry`].
pub fn client_options(http: &reqwest::Client) -> ClientOptions {
    ClientOptions {
        transport: Some(Transport::new(Arc::new(http.clone()))),
        retry: RetryOptions::none(),
        ..Default::default()
    }
}

/// Maps an Azure SDK error to an error class. `action` says what was being
/// done, for the message.
pub fn classify(action: &str, error: &azure_core::Error) -> Error {
    match error.kind() {
        ErrorKind::HttpResponse { status, .. } => {
            let status = u16::from(*status);
            let message = format!("{action}: HTTP {status}: {error}");
            match status {
                408 | 429 | 500..=599 => Error::transient(message),
                _ => Error::permanent(message),
            }
        }
        ErrorKind::Connection | ErrorKind::Io => Error::transient(format!("{action}: {error}")),
        _ => Error::permanent(format!("{action}: {error}")),
    }
}

/// Stand-ins for Azure in tests: a transport that replays constructed HTTP
/// exchanges and a credential that needs no sign-in.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use azure_core::credentials::{AccessToken, TokenRequestOptions};
    use azure_core::http::headers::Headers;
    use azure_core::http::{AsyncRawResponse, Body, HttpClient, Request, StatusCode};
    use azure_core::time::{Duration, OffsetDateTime};

    use super::*;

    /// One response to give, in the order the requests arrive.
    #[derive(Debug)]
    pub(crate) struct Exchange {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: String,
    }

    impl Exchange {
        pub(crate) fn json(status: u16, body: &str) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: body.to_owned(),
            }
        }

        /// The answer of Key Vault to a request without a token: a challenge
        /// that names the authority to sign in at and the resource to ask a
        /// token for. A Key Vault client sends its first request without a
        /// token and without a body to obtain it. In this constructed
        /// challenge the resource is the test vaults' parent domain, where a
        /// real vault names `https://vault.azure.net`.
        pub(crate) fn key_vault_challenge() -> Self {
            Self {
                status: 401,
                headers: vec![(
                    "www-authenticate",
                    "Bearer authorization=\"https://login.example.com/00000000-0000-0000-0000-000000000000\", \
                     resource=\"https://vault.example.com\""
                        .to_owned(),
                )],
                body: r#"{"error":{"code":"Unauthorized","message":"AKV10000: Request is missing a Bearer or PoP token."}}"#
                    .to_owned(),
            }
        }
    }

    /// What was sent in one request.
    #[derive(Debug, Clone)]
    pub(crate) struct Sent {
        pub(crate) method: String,
        pub(crate) path: String,
        pub(crate) query: String,
        pub(crate) body: String,
        /// Whether the request carried the test credential's token.
        pub(crate) authorized: bool,
    }

    /// A transport that answers from a list of constructed exchanges.
    #[derive(Debug, Clone)]
    pub(crate) struct MockHttp {
        inner: Arc<Inner>,
    }

    #[derive(Debug)]
    struct Inner {
        exchanges: Mutex<VecDeque<Exchange>>,
        requests: Mutex<Vec<Sent>>,
    }

    impl MockHttp {
        pub(crate) fn new(exchanges: impl IntoIterator<Item = Exchange>) -> Self {
            let inner = Inner {
                exchanges: Mutex::new(exchanges.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            };
            Self {
                inner: Arc::new(inner),
            }
        }

        pub(crate) fn client_options(&self) -> ClientOptions {
            ClientOptions {
                transport: Some(Transport::new(self.inner.clone())),
                retry: RetryOptions::none(),
                ..Default::default()
            }
        }

        /// A transport for a Key Vault client: the challenge, then `exchanges`.
        pub(crate) fn key_vault(exchanges: impl IntoIterator<Item = Exchange>) -> Self {
            Self::new(std::iter::once(Exchange::key_vault_challenge()).chain(exchanges))
        }

        pub(crate) fn requests(&self) -> Vec<Sent> {
            self.inner.requests.lock().unwrap().clone()
        }

        /// The requests that carried the token, which leaves out the request
        /// that obtained the challenge.
        pub(crate) fn authorized_requests(&self) -> Vec<Sent> {
            self.requests()
                .into_iter()
                .filter(|request| request.authorized)
                .collect()
        }
    }

    #[async_trait]
    impl HttpClient for Inner {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            let body = match request.body() {
                Body::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                _ => String::from("<stream>"),
            };
            let authorized = request.headers().iter().any(|(name, value)| {
                name.as_str() == "authorization" && value.as_str() == "Bearer test-token"
            });
            self.requests.lock().unwrap().push(Sent {
                method: request.method().to_string(),
                path: request.url().path().to_owned(),
                query: request.url().query().unwrap_or_default().to_owned(),
                body,
                authorized,
            });
            let exchange = self
                .exchanges
                .lock()
                .unwrap()
                .pop_front()
                .expect("more requests were sent than exchanges were constructed");
            let status = StatusCode::from(exchange.status);
            let mut headers = Headers::new();
            headers.insert("content-type", "application/json; charset=utf-8");
            for (name, value) in exchange.headers {
                headers.insert(name, value);
            }
            Ok(AsyncRawResponse::from_bytes(status, headers, exchange.body))
        }
    }

    #[derive(Debug)]
    struct TestCredential;

    #[async_trait]
    impl TokenCredential for TestCredential {
        async fn get_token(
            &self,
            _scopes: &[&str],
            _options: Option<TokenRequestOptions<'_>>,
        ) -> azure_core::Result<AccessToken> {
            Ok(AccessToken::new(
                "test-token",
                OffsetDateTime::now_utc() + Duration::hours(1),
            ))
        }
    }

    /// A credential that hands out a fixed token.
    pub(crate) fn credential() -> Arc<dyn TokenCredential> {
        Arc::new(TestCredential)
    }
}

#[cfg(test)]
mod tests {
    use azure_core::http::StatusCode;

    use super::*;

    #[test]
    fn workload_identity_is_used_when_the_cluster_provides_a_token_file() {
        let path = OsStr::new("/var/run/secrets/azure/tokens/azure-identity-token");
        assert_eq!(Identity::detect(Some(path)), Identity::Workload);
    }

    #[test]
    fn azure_cli_sign_in_is_used_otherwise() {
        assert_eq!(Identity::detect(None), Identity::AzureCli);
        assert_eq!(Identity::detect(Some(OsStr::new(""))), Identity::AzureCli);
    }

    fn http_error(status: StatusCode) -> azure_core::Error {
        ErrorKind::HttpResponse {
            status,
            error_code: None,
            raw_response: None,
        }
        .into_error()
    }

    #[test]
    fn server_and_network_failures_are_transient() {
        for status in [
            StatusCode::RequestTimeout,
            StatusCode::TooManyRequests,
            StatusCode::InternalServerError,
            StatusCode::BadGateway,
            StatusCode::ServiceUnavailable,
            StatusCode::GatewayTimeout,
        ] {
            assert!(
                classify("reading", &http_error(status)).is_transient(),
                "{status:?}"
            );
        }
        assert!(classify("reading", &ErrorKind::Io.into_error()).is_transient());
        assert!(classify("reading", &ErrorKind::Connection.into_error()).is_transient());
    }

    #[test]
    fn refusals_are_permanent() {
        for status in [
            StatusCode::BadRequest,
            StatusCode::Unauthorized,
            StatusCode::Forbidden,
            StatusCode::NotFound,
            StatusCode::Conflict,
        ] {
            let error = classify("reading", &http_error(status));
            assert_eq!(error.class(), "permanent", "{status:?}");
        }
        let error = classify("reading", &ErrorKind::Credential.into_error());
        assert_eq!(error.class(), "permanent");
    }
}
