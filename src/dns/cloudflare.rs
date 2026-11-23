//! The `cloudflare` DNS provider.
//!
//! Cloudflare keeps one DNS record per TXT value. Publishing creates one
//! record for each value and remembers the identifiers; cleanup deletes
//! exactly those records. Other records at the same name are never listed,
//! changed or deleted.

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{DnsProvider, RecordHandle, Zone};
use crate::config::Secret;
use crate::{Error, Result};

/// The address of the Cloudflare API.
pub const API_URL: &str = "https://api.cloudflare.com/client/v4";

/// How many zones are asked for per page.
const ZONES_PER_PAGE: u32 = 50;

/// The error code with which Cloudflare refuses a record that already exists
/// with the same name, type and content.
const IDENTICAL_RECORD_EXISTS: i64 = 81058;

/// A DNS provider instance of type `cloudflare`.
pub struct CloudflareProvider {
    http: reqwest::Client,
    api_url: String,
    token: Secret,
}

/// The envelope every answer of the Cloudflare API comes in.
#[derive(Debug, Deserialize)]
struct Envelope {
    /// Absent in some answers; only an explicit `false` is a failure.
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    errors: Vec<ApiError>,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    result_info: Option<ResultInfo>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct ResultInfo {
    #[serde(default)]
    total_pages: u32,
}

/// A request that failed, with what is needed to decide how to go on.
struct Failure {
    error: Error,
    codes: Vec<i64>,
    status: Option<StatusCode>,
}

impl CloudflareProvider {
    /// A provider that authenticates with `token`.
    pub fn new(http: reqwest::Client, token: Secret) -> Self {
        Self::with_api_url(http, token, API_URL)
    }

    /// A provider that talks to the API at another address.
    pub fn with_api_url(http: reqwest::Client, token: Secret, api_url: &str) -> Self {
        Self {
            http,
            api_url: api_url.trim_end_matches('/').to_owned(),
            token,
        }
    }

    /// Sends one request and returns the answer's envelope. `action` says
    /// what is being done, for the error message.
    async fn call(
        &self,
        action: &str,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> std::result::Result<Envelope, Failure> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.api_url))
            .bearer_auth(self.token.expose())
            .query(query);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| Failure {
            // A request that did not get an answer: the network, or a timeout.
            // `without_url` keeps the message free of anything but the cause.
            error: Error::transient(format!(
                "{action}: no answer from Cloudflare: {}",
                error.without_url()
            )),
            codes: Vec::new(),
            status: None,
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|error| Failure {
            error: Error::transient(format!(
                "{action}: the answer from Cloudflare could not be read: {}",
                error.without_url()
            )),
            codes: Vec::new(),
            status: Some(status),
        })?;
        let envelope: Option<Envelope> = serde_json::from_str(&text).ok();
        match envelope {
            Some(envelope) if status.is_success() && envelope.success != Some(false) => {
                Ok(envelope)
            }
            envelope => {
                let errors = envelope.map(|envelope| envelope.errors).unwrap_or_default();
                let reasons: Vec<String> = errors
                    .iter()
                    .map(|error| format!("{} (code {})", error.message, error.code))
                    .collect();
                let reasons = if reasons.is_empty() {
                    "no reason given".to_owned()
                } else {
                    reasons.join("; ")
                };
                let message = format!(
                    "{action}: Cloudflare answered HTTP {}: {reasons}",
                    status.as_u16()
                );
                let error = match status.as_u16() {
                    408 | 429 | 500..=599 => Error::transient(message),
                    _ => Error::permanent(message),
                };
                Err(Failure {
                    error,
                    codes: errors.iter().map(|error| error.code).collect(),
                    status: Some(status),
                })
            }
        }
    }

    /// The identifier of the TXT record with this name and content.
    async fn find_record(&self, zone: &Zone, name: &str, content: &str) -> Result<String> {
        let action = format!("looking up the record {name}");
        let query = [
            ("type", "TXT".to_owned()),
            ("name", name.to_owned()),
            ("content", content.to_owned()),
        ];
        let envelope = self
            .call(
                &action,
                Method::GET,
                &format!("/zones/{}/dns_records", zone.id),
                &query,
                None,
            )
            .await
            .map_err(|failure| failure.error)?;
        envelope
            .result
            .as_array()
            .and_then(|records| records.first())
            .and_then(|record| record["id"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::permanent(format!(
                    "{action}: Cloudflare says the record exists but does not list it"
                ))
            })
    }
}

/// A TXT value as Cloudflare stores it: in double quotes.
fn quoted(value: &str) -> String {
    format!("\"{value}\"")
}

#[async_trait]
impl DnsProvider for CloudflareProvider {
    async fn zones(&self) -> Result<Vec<Zone>> {
        let mut zones = Vec::new();
        let mut page = 1;
        loop {
            let query = [
                ("page", page.to_string()),
                ("per_page", ZONES_PER_PAGE.to_string()),
            ];
            let envelope = self
                .call("listing the zones", Method::GET, "/zones", &query, None)
                .await
                .map_err(|failure| failure.error)?;
            for zone in envelope.result.as_array().into_iter().flatten() {
                if let (Some(name), Some(id)) = (zone["name"].as_str(), zone["id"].as_str()) {
                    zones.push(Zone::new(name, id));
                }
            }
            let total_pages = envelope.result_info.map_or(1, |info| info.total_pages);
            if page >= total_pages {
                return Ok(zones);
            }
            page += 1;
        }
    }

    async fn present(
        &self,
        zone: &Zone,
        name: &str,
        values: &[String],
        ttl: u32,
    ) -> Result<RecordHandle> {
        let mut handle = RecordHandle {
            zone: zone.clone(),
            name: name.to_owned(),
            values: Vec::new(),
            record_ids: Vec::new(),
        };
        let path = format!("/zones/{}/dns_records", zone.id);
        for value in values {
            let action = format!("creating the record {name}");
            let content = quoted(value);
            let body = json!({ "type": "TXT", "name": name, "content": content, "ttl": ttl });
            let created = self
                .call(&action, Method::POST, &path, &[], Some(body))
                .await;
            let id = match created {
                Ok(envelope) => envelope.result["id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        Error::permanent(format!(
                            "{action}: Cloudflare returned no record identifier"
                        ))
                    }),
                // The value is one only this run knows. A record with it can
                // only be the result of an earlier attempt of this call whose
                // answer was lost, so it is this run's own record.
                Err(failure) if failure.codes.contains(&IDENTICAL_RECORD_EXISTS) => {
                    self.find_record(zone, name, &content).await
                }
                Err(failure) => Err(failure.error),
            };
            match id {
                Ok(id) => {
                    handle.values.push(value.clone());
                    handle.record_ids.push(id);
                }
                Err(error) => {
                    // What was created before the failure is removed again, so
                    // that a failed call leaves nothing behind.
                    let _ = self.cleanup(handle).await;
                    return Err(error);
                }
            }
        }
        Ok(handle)
    }

    async fn cleanup(&self, handle: RecordHandle) -> Result<()> {
        let mut first_failure = None;
        for id in &handle.record_ids {
            let action = format!("deleting the record {}", handle.name);
            let path = format!("/zones/{}/dns_records/{id}", handle.zone.id);
            match self.call(&action, Method::DELETE, &path, &[], None).await {
                Ok(_) => {}
                // A record that is already gone is what cleanup wants.
                Err(failure) if failure.status == Some(StatusCode::NOT_FOUND) => {}
                Err(failure) => {
                    first_failure.get_or_insert(failure.error);
                }
            }
        }
        first_failure.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests;
