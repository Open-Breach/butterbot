//! ACME issuance: accounts, orders, keys and profiles against any configured
//! ACME server.
//!
//! An ACME server is data: a directory URL and a few settings. One client
//! serves them all. The protocol itself (nonces, signing, the order state
//! machine) is `instant-acme`; this module adds what Butterbot needs around
//! it: accounts that are kept, error classes, and bounded waits.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::prelude::{BASE64_URL_SAFE, BASE64_URL_SAFE_NO_PAD};
use bytes::Bytes;
use instant_acme::{
    Account, AccountCredentials, BodyWrapper, BytesResponse, ExternalAccountKey, NewAccount,
};

use crate::config::Secret;
use crate::{Error, Result};

mod accounts;
pub mod keys;
mod order;

pub use accounts::Accounts;
pub use order::{Challenge, Issuer, OrderRequest, PendingOrder, Polling};

/// The ACME error type of a rate limit.
const RATE_LIMITED: &str = "urn:ietf:params:acme:error:rateLimited";

/// Sends the requests of the ACME client through the shared HTTP client, and
/// remembers the `Retry-After` of the last answer, which the ACME client does
/// not pass on with a rate-limit error.
struct Transport {
    http: reqwest::Client,
    retry_after: Arc<Mutex<Option<Duration>>>,
}

impl instant_acme::HttpClient for Transport {
    fn request(
        &self,
        request: http::Request<BodyWrapper<Bytes>>,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<BytesResponse, instant_acme::Error>> + Send>>
    {
        let http = self.http.clone();
        let retry_after = self.retry_after.clone();
        Box::pin(async move {
            let other =
                |error: reqwest::Error| instant_acme::Error::Other(Box::new(error.without_url()));
            let request =
                reqwest::Request::try_from(request.map(reqwest::Body::wrap)).map_err(other)?;
            let response = http.execute(request).await.map_err(other)?;
            let stated = response
                .headers()
                .get(http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after);
            if let Ok(mut slot) = retry_after.lock() {
                *slot = stated;
            }
            Ok(BytesResponse::from(http::Response::<reqwest::Body>::from(
                response,
            )))
        })
    }
}

/// Reads a `Retry-After` header: a number of seconds, or a date.
fn parse_retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(value.trim()).ok()?;
    Some(
        date.duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

/// An account at one ACME server, ready to place orders.
pub struct AcmeClient {
    /// The instance name of the server, for messages.
    server: String,
    account: Account,
    retry_after: Arc<Mutex<Option<Duration>>>,
    polling: Polling,
}

impl AcmeClient {
    /// Uses the account whose credentials are given. No account is registered.
    ///
    /// `credentials` is the value of `acme.<name>.access_key`. It names the
    /// server it belongs to, which has to be `directory_url`: credentials for
    /// a staging server must not be used at a production server, or the other
    /// way round.
    pub async fn with_account(
        server: &str,
        directory_url: &str,
        credentials: &Secret,
        http: &reqwest::Client,
    ) -> Result<Self> {
        let unusable = |problem: String| {
            Error::permanent(format!(
                "the account credentials in acme.{server}.access_key are not usable: {problem}"
            ))
        };
        let value: serde_json::Value = serde_json::from_str(credentials.expose())
            .map_err(|error| unusable(format!("not JSON ({})", error.classify_text())))?;
        if let Some(theirs) = value["directory"].as_str()
            && theirs != directory_url
        {
            return Err(unusable(format!(
                "they belong to the server {theirs}, not to {directory_url}"
            )));
        }
        let credentials: AccountCredentials = serde_json::from_value(value)
            .map_err(|_| unusable("they are not ACME account credentials".to_owned()))?;

        let (transport, retry_after) = transport(http);
        let account = Account::builder_with_http(transport)
            .from_credentials(credentials)
            .await
            .map_err(|error| classify(server, "loading the account", &error, &retry_after))?;
        Ok(Self {
            server: server.to_owned(),
            account,
            retry_after,
            polling: Polling::default(),
        })
    }

    /// Registers a new account and returns it with its credentials, serialised
    /// for `acme.<name>.access_key`.
    ///
    /// `external_account` is the key identifier and the key (base64url) of an
    /// external account binding, for servers that require one.
    pub async fn register(
        server: &str,
        directory_url: &str,
        external_account: Option<(&str, &Secret)>,
        http: &reqwest::Client,
    ) -> Result<(Self, Secret)> {
        let binding = match external_account {
            Some((kid, key)) => {
                let key = decode_base64url(key.expose()).ok_or_else(|| {
                    Error::permanent(format!(
                        "acme.{server}.eab_key is not usable: it is not base64url text"
                    ))
                })?;
                Some(ExternalAccountKey::new(kid.to_owned(), &key))
            }
            None => None,
        };
        let (transport, retry_after) = transport(http);
        let new_account = NewAccount {
            contact: &[],
            terms_of_service_agreed: true,
            only_return_existing: false,
        };
        let (account, credentials) = Account::builder_with_http(transport)
            .create(&new_account, directory_url.to_owned(), binding.as_ref())
            .await
            .map_err(|error| classify(server, "registering an account", &error, &retry_after))?;
        let serialised = serde_json::to_string(&credentials).map_err(|_| {
            Error::permanent(format!(
                "ACME server {server}: the new account's credentials cannot be serialised"
            ))
        })?;
        Ok((
            Self {
                server: server.to_owned(),
                account,
                retry_after,
                polling: Polling::default(),
            },
            Secret::new(serialised),
        ))
    }

    /// The instance name of the server.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The URL that identifies the account at the server.
    pub fn account_id(&self) -> &str {
        self.account.id()
    }

    /// The certificate profiles the server offers, by name.
    pub fn profiles(&self) -> Vec<String> {
        let mut profiles: Vec<String> = self
            .account
            .profiles()
            .map(|profile| profile.name.to_owned())
            .collect();
        profiles.sort();
        profiles
    }

    /// Changes how the server is polled for results. The default is every
    /// 5 seconds for at most 2 minutes.
    pub fn with_polling(mut self, polling: Polling) -> Self {
        self.polling = polling;
        self
    }

    /// Maps an error of the ACME client. `action` says what was being done.
    fn classify(&self, action: &str, error: &instant_acme::Error) -> Error {
        classify(&self.server, action, error, &self.retry_after)
    }
}

fn transport(http: &reqwest::Client) -> (Box<Transport>, Arc<Mutex<Option<Duration>>>) {
    let retry_after = Arc::new(Mutex::new(None));
    (
        Box::new(Transport {
            http: http.clone(),
            retry_after: retry_after.clone(),
        }),
        retry_after,
    )
}

/// Decodes base64url text, with or without padding.
fn decode_base64url(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    BASE64_URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| BASE64_URL_SAFE.decode(text))
        .ok()
}

/// Maps an error of the ACME client to an error class. The message names the
/// server and gives the reason the server stated.
fn classify(
    server: &str,
    action: &str,
    error: &instant_acme::Error,
    retry_after: &Mutex<Option<Duration>>,
) -> Error {
    use instant_acme::Error as Acme;

    let message = format!("ACME server {server}: {action}: {}", describe(error));
    match error {
        Acme::Api(problem) if problem.r#type.as_deref() == Some(RATE_LIMITED) => {
            let stated = retry_after.lock().ok().and_then(|slot| *slot);
            Error::rate_limited(message, stated)
        }
        Acme::Api(problem) => match problem.status {
            Some(408 | 429 | 500..=599) => Error::transient(message),
            _ => Error::permanent(message),
        },
        // No answer, or an answer that did not arrive whole.
        Acme::Other(_) | Acme::Http(_) => Error::transient(message),
        Acme::Timeout(_) => Error::timeout(message),
        _ => Error::permanent(message),
    }
}

/// The reason of an ACME error, with the server's own words where it gave any.
fn describe(error: &instant_acme::Error) -> String {
    match error {
        instant_acme::Error::Api(problem) => describe_problem(problem),
        instant_acme::Error::Other(source) => {
            // The chain of causes says more than the top of it.
            let mut text = source.to_string();
            let mut cause = source.source();
            while let Some(next) = cause {
                text.push_str(&format!(": {next}"));
                cause = next.source();
            }
            text
        }
        other => other.to_string(),
    }
}

fn describe_problem(problem: &instant_acme::Problem) -> String {
    let kind = problem
        .r#type
        .as_deref()
        .map(|kind| kind.rsplit(':').next().unwrap_or(kind))
        .unwrap_or("error");
    let mut text = match &problem.detail {
        Some(detail) => format!("{kind}: {detail}"),
        None => kind.to_owned(),
    };
    for sub in &problem.subproblems {
        let name = sub.identifier.as_ref().map(identifier_name);
        let detail = sub.detail.as_deref().unwrap_or("no detail");
        match name {
            Some(name) => text.push_str(&format!("; {name}: {detail}")),
            None => text.push_str(&format!("; {detail}")),
        }
    }
    text
}

/// The name in an ACME identifier.
fn identifier_name(identifier: &instant_acme::Identifier) -> String {
    match identifier {
        instant_acme::Identifier::Dns(name) => name.clone(),
        instant_acme::Identifier::Ip(address) => address.to_string(),
        other => format!("{other:?}"),
    }
}

/// A short description of a JSON error that cannot contain any of the text.
trait ClassifyText {
    fn classify_text(&self) -> &'static str;
}

impl ClassifyText for serde_json::Error {
    fn classify_text(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Eof => "it ends early",
            serde_json::error::Category::Syntax => "syntax error",
            serde_json::error::Category::Data => "unexpected content",
            serde_json::error::Category::Io => "unreadable",
        }
    }
}

#[cfg(test)]
mod tests;
