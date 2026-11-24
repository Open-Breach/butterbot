//! The `webhook` notifier: run events delivered as HTTP requests.
//!
//! Each configured instance (`notify.<name>` with `type = "webhook"`) becomes
//! a [`WebhookSink`] on the event bus. For every event the instance selects,
//! the sink sends one request to the instance's URL.
//!
//! The URL of a webhook carries its own credential, and a header may carry a
//! key. Neither is ever part of a log record or of an error: every failure is
//! described by this module in its own words, from the status the endpoint
//! answered with or from the kind of error, and never by the text of the
//! error the HTTP client returned, which names the URL.

use std::error::Error as _;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};

use crate::config::{
    Config, NotifierKind, Webhook, WebhookBody, WebhookMethod, defaults, env_name, vault_name,
};
use crate::events::{Event, EventBus, EventSink};
use crate::logging::{Level, Logger};
use crate::retry::{FIRST_PAUSE, retry_with_first_pause};
use crate::{Error, Result};

pub mod teams;
pub mod template;

use template::{Position, Template, Values};

/// Delivers events to one webhook instance.
pub struct WebhookSink {
    name: String,
    client: reqwest::Client,
    method: WebhookMethod,
    url: Template,
    headers: Vec<(HeaderName, Template)>,
    body: Body,
    /// The value of the `access_key` placeholder. Empty when no template of
    /// the instance uses it.
    access_key: String,
    /// The time limit of one attempt.
    timeout: Duration,
    /// The pause after the first failed attempt.
    first_pause: Duration,
    log: Logger,
}

/// What a request with a body carries.
enum Body {
    Json,
    Teams,
    Custom {
        template: Template,
        position: Position,
        content_type: HeaderValue,
    },
}

/// One request, ready to be sent as often as the retries need it.
struct Request {
    url: reqwest::Url,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
}

impl WebhookSink {
    /// Opens the webhook instance `name`. Retries are written to `log`.
    ///
    /// Fails when the instance cannot deliver anything: its URL or a key a
    /// template uses is supplied by no layer, its URL has another scheme than
    /// `https` or `http`, or its `ca_file` is not usable. The error names the
    /// configuration path and never a value.
    pub fn open(name: &str, webhook: &Webhook, log: &Logger) -> Result<Self> {
        let path = |setting: &str| format!("notify.{name}.{setting}");
        let not_set = |setting: &str, why: &str| {
            let path = path(setting);
            Error::permanent(format!(
                "{path} is not set{why}; supply it as the environment variable {} or as the Key \
                 Vault secret {}",
                env_name(&path),
                vault_name(&path)
            ))
        };

        let url = webhook.url.clone().ok_or_else(|| not_set("url", ""))?;
        if !has_scheme(&url, "https") && !has_scheme(&url, "http") {
            return Err(Error::permanent(format!(
                "{} has an unsupported scheme: the URL of a webhook starts with https:// or \
                 http://",
                path("url")
            )));
        }
        let access_key = if webhook.uses_access_key() {
            webhook
                .access_key
                .as_ref()
                .ok_or_else(|| {
                    not_set(
                        "access_key",
                        ", and a template of the notifier uses {{access_key}}",
                    )
                })?
                .expose()
                .to_owned()
        } else {
            String::new()
        };

        let mut headers = Vec::with_capacity(webhook.headers.len());
        for (header, value) in &webhook.headers {
            let header = HeaderName::from_bytes(header.as_bytes()).map_err(|_| {
                Error::permanent(format!(
                    "{}: {header:?} is not a valid header name",
                    path("headers")
                ))
            })?;
            headers.push((header, value.clone()));
        }
        let body = match &webhook.body {
            WebhookBody::Json => Body::Json,
            WebhookBody::Teams => Body::Teams,
            WebhookBody::Custom {
                template,
                content_type,
            } => Body::Custom {
                template: template.clone(),
                position: Position::of_body(content_type),
                content_type: HeaderValue::from_str(content_type).map_err(|_| {
                    Error::permanent(format!(
                        "{} is not a valid content type",
                        path("content_type")
                    ))
                })?,
            },
        };

        let mut extra_roots = Vec::new();
        if let Some(file) = &webhook.ca_file {
            let text = std::fs::read_to_string(file).map_err(|error| {
                Error::permanent(format!(
                    "the file {} that {} names cannot be read: {error}",
                    file.display(),
                    path("ca_file")
                ))
            })?;
            extra_roots.push(text);
        }
        // Redirects are not followed: a redirect would send the headers,
        // possibly with a key, to a host the configuration never named.
        let client = crate::http::client_without_redirects(&extra_roots)
            .map_err(|error| Error::permanent(format!("{}: {error}", path("ca_file"))))?;

        Ok(Self {
            name: name.to_owned(),
            client,
            method: webhook.method,
            url,
            headers,
            body,
            access_key,
            timeout: webhook.timeout,
            first_pause: FIRST_PAUSE,
            log: log.clone(),
        })
    }

    /// Whether the requests are sent without encryption: the URL uses `http`.
    pub fn is_unencrypted(&self) -> bool {
        has_scheme(&self.url, "http")
    }

    /// Changes the pause after the first failed attempt, so that a test of
    /// the retries does not wait for seconds.
    #[cfg(test)]
    pub(crate) fn with_first_pause(mut self, first_pause: Duration) -> Self {
        self.first_pause = first_pause;
        self
    }

    /// Builds the request for `event` from the method, the URL, the headers
    /// and the body of the instance.
    fn request(&self, event: &Event) -> Result<Request> {
        let values = Values::of(event, &self.access_key);

        // The message says nothing about the URL, not even what is wrong
        // with it.
        let url = reqwest::Url::parse(&self.url.render(Position::Url, &values)).map_err(|_| {
            Error::permanent(format!("notify.{}.url is not a valid URL", self.name))
        })?;

        let mut headers = HeaderMap::new();
        let body = match self.method {
            WebhookMethod::Get => None,
            WebhookMethod::Post => {
                let (content_type, bytes) = match &self.body {
                    Body::Json => (
                        HeaderValue::from_static(defaults::WEBHOOK_CONTENT_TYPE),
                        serde_json::Value::Object(event.to_json())
                            .to_string()
                            .into_bytes(),
                    ),
                    Body::Teams => (
                        HeaderValue::from_static(defaults::WEBHOOK_CONTENT_TYPE),
                        teams::message(event).to_string().into_bytes(),
                    ),
                    Body::Custom {
                        template,
                        position,
                        content_type,
                    } => (
                        content_type.clone(),
                        template.render(*position, &values).into_bytes(),
                    ),
                };
                headers.insert(CONTENT_TYPE, content_type);
                Some(bytes)
            }
        };
        for (header, template) in &self.headers {
            let rendered = template.render(Position::Header, &values);
            let mut value = HeaderValue::from_bytes(rendered.as_bytes()).map_err(|_| {
                Error::permanent(format!(
                    "the value of the header {header} cannot be sent: it contains a line break \
                     or another control character"
                ))
            })?;
            value.set_sensitive(true);
            headers.insert(header.clone(), value);
        }
        Ok(Request { url, headers, body })
    }

    /// Sends the request once. A 2xx answer is a success. A network error, a
    /// 5xx answer and status 429 are transient; every other answer, a
    /// redirect included, is permanent.
    async fn attempt(&self, request: &Request) -> Result<()> {
        let method = match self.method {
            WebhookMethod::Post => reqwest::Method::POST,
            WebhookMethod::Get => reqwest::Method::GET,
        };
        let mut builder = self
            .client
            .request(method, request.url.clone())
            .headers(request.headers.clone())
            .timeout(self.timeout);
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        let status = match builder.send().await {
            Ok(response) => response.status(),
            Err(error) => return Err(transport_error(&error, self.timeout)),
        };
        if status.is_success() {
            Ok(())
        } else if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            Err(Error::transient(answered(status)))
        } else if status.is_redirection() {
            Err(Error::permanent(format!(
                "{}; redirects are not followed",
                answered(status)
            )))
        } else {
            Err(Error::permanent(answered(status)))
        }
    }
}

fn answered(status: reqwest::StatusCode) -> String {
    format!("the endpoint answered with status {}", status.as_u16())
}

/// Whether the URL is written with `scheme`. The scheme is part of the text
/// before the first placeholder.
fn has_scheme(url: &Template, scheme: &str) -> bool {
    let start = url.literal_start();
    start
        .get(..scheme.len() + 3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&format!("{scheme}://")))
}

/// What went wrong when no answer was received, by its kind. The text of the
/// HTTP client's error is not used: it names the URL, and the errors below it
/// name the host.
fn transport_error(error: &reqwest::Error, timeout: Duration) -> Error {
    if is_certificate_error(error) {
        // Trying again does not make a certificate acceptable.
        Error::permanent("the connection failed: the certificate of the endpoint was not accepted")
    } else if error.is_timeout() {
        Error::transient(format!(
            "the endpoint did not answer within {}",
            crate::config::format_duration(timeout)
        ))
    } else if error.is_connect() {
        Error::transient("the connection failed")
    } else {
        Error::transient("the request failed before an answer was received")
    }
}

/// Whether the connection failed because the endpoint's certificate was
/// refused: issued by an authority that is not trusted, expired, or for
/// another name.
fn is_certificate_error(error: &reqwest::Error) -> bool {
    let mut cause = error.source();
    while let Some(current) = cause {
        if is_refused_certificate(current) {
            return true;
        }
        cause = current.source();
    }
    false
}

/// Whether `error` is the TLS library's refusal of a certificate. On its way
/// up the TLS error is wrapped in I/O errors, which do not present what they
/// wrap as their source.
fn is_refused_certificate(error: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(tls) = error.downcast_ref::<rustls::Error>() {
        return matches!(tls, rustls::Error::InvalidCertificate(_));
    }
    error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::get_ref)
        .is_some_and(|inner| is_refused_certificate(inner))
}

#[async_trait]
impl EventSink for WebhookSink {
    /// Sends `event`, repeating the request after a transient failure. The
    /// event bus limits the whole delivery, the pauses included.
    async fn deliver(&self, event: &Event) -> Result<()> {
        let request = self.request(event)?;
        let action = format!(
            "delivering the {} event to the notifier {}",
            event.kind(),
            self.name
        );
        retry_with_first_pause(&self.log, &action, self.first_pause, || {
            self.attempt(&request)
        })
        .await
    }
}

/// Registers a sink on `bus` for every webhook instance of the configuration.
///
/// An instance that cannot deliver anything is reported with one error and
/// receives no events; the run is otherwise not affected. An instance whose
/// URL uses `http` is registered, with a warning.
pub fn register(config: &Config, bus: &mut EventBus, log: &Logger) {
    for (name, instance) in &config.notify {
        let NotifierKind::Webhook(webhook) = &instance.kind;
        match WebhookSink::open(name, webhook, log) {
            Ok(sink) => {
                if sink.is_unencrypted() {
                    warn_unencrypted(log, name);
                }
                bus.register(name, instance.events.clone(), Arc::new(sink));
            }
            Err(error) => log
                .record(
                    Level::Error,
                    "a notifier is misconfigured and receives no events",
                )
                .field("notifier", name.as_str())
                .reason(error)
                .write(),
        }
    }
}

/// Says that the requests of the notifier `name` are not encrypted.
pub fn warn_unencrypted(log: &Logger, name: &str) {
    log.record(
        Level::Warn,
        "the URL of a notifier uses http: its requests are not encrypted",
    )
    .field("notifier", name)
    .write();
}

#[cfg(test)]
mod tests;
