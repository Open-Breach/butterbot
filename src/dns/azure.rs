//! The `azure-dns` DNS provider.
//!
//! Azure DNS keeps all TXT values of a name in one record set, which can only
//! be written as a whole. Publishing therefore reads the record set, adds its
//! values to what is there and writes the set back; cleanup reads it again,
//! takes out exactly the values it added and writes back what remains, or
//! deletes the record set when nothing remains. Values that others put there
//! are written back unchanged.
//!
//! Every write carries the version (ETag) of the record set that was read, so
//! a change made by someone else in between is not overwritten: the write is
//! refused and the read-modify-write is done again.
//!
//! There is no maintained management crate for Azure DNS, so the Resource
//! Manager REST API is called directly, with a token of the ambient Azure
//! identity.

use std::sync::Arc;

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::{DnsProvider, RecordHandle, Zone};
use crate::{Error, Result};

/// The address of Azure Resource Manager.
pub const MANAGEMENT_URL: &str = "https://management.azure.com";

/// The scope a token for Azure Resource Manager is asked for.
const MANAGEMENT_SCOPE: &str = "https://management.azure.com/.default";

/// The version of the DNS zones API.
const API_VERSION: &str = "2018-05-01";

/// How many times a read-modify-write is done when the record set keeps
/// changing in between.
const WRITE_ATTEMPTS: u32 = 4;

/// A DNS provider instance of type `azure-dns`.
pub struct AzureDnsProvider {
    http: reqwest::Client,
    credential: Arc<dyn TokenCredential>,
    management_url: String,
    subscription_id: String,
    resource_group: String,
}

/// An answer of Resource Manager that is not a success.
struct Refusal {
    status: StatusCode,
    error: Error,
}

/// A TXT record set as it was read.
struct RecordSet {
    etag: Option<String>,
    ttl: Option<u32>,
    /// The entries of `TXTRecords`, each as Resource Manager returned it.
    entries: Vec<Value>,
    metadata: Option<Value>,
}

impl AzureDnsProvider {
    /// A provider for the zones in `resource_group` of `subscription_id`,
    /// with the ambient Azure identity.
    pub fn connect(
        http: &reqwest::Client,
        subscription_id: &str,
        resource_group: &str,
    ) -> Result<Self> {
        let credential = crate::azure::credential(http)?;
        Ok(Self::with(
            http.clone(),
            credential,
            MANAGEMENT_URL,
            subscription_id,
            resource_group,
        ))
    }

    fn with(
        http: reqwest::Client,
        credential: Arc<dyn TokenCredential>,
        management_url: &str,
        subscription_id: &str,
        resource_group: &str,
    ) -> Self {
        Self {
            http,
            credential,
            management_url: management_url.trim_end_matches('/').to_owned(),
            subscription_id: subscription_id.to_owned(),
            resource_group: resource_group.to_owned(),
        }
    }

    fn zones_url(&self) -> String {
        format!(
            "{}/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Network/dnsZones?api-version={API_VERSION}",
            self.management_url, self.subscription_id, self.resource_group
        )
    }

    fn record_set_url(&self, zone: &Zone, name: &str) -> String {
        format!(
            "{}/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Network/dnsZones/{}/TXT/{}?api-version={API_VERSION}",
            self.management_url,
            self.subscription_id,
            self.resource_group,
            zone.id,
            relative_name(zone, name)
        )
    }

    /// Sends one request to Resource Manager. `action` says what is being
    /// done, for the error message.
    async fn call(
        &self,
        action: &str,
        method: Method,
        url: &str,
        condition: Option<(&'static str, &str)>,
        body: Option<&Value>,
    ) -> std::result::Result<Value, Refusal> {
        let no_answer = |error: Error| Refusal {
            status: StatusCode::SERVICE_UNAVAILABLE,
            error,
        };
        let token = self
            .credential
            .get_token(&[MANAGEMENT_SCOPE], None)
            .await
            .map_err(|error| no_answer(crate::azure::classify("signing in to Azure", &error)))?;
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(token.token.secret());
        if let Some((header, value)) = condition {
            request = request.header(header, value);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|error| {
            no_answer(Error::transient(format!(
                "{action}: no answer from Azure: {}",
                error.without_url()
            )))
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            no_answer(Error::transient(format!(
                "{action}: the answer from Azure could not be read: {}",
                error.without_url()
            )))
        })?;
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        // Resource Manager wraps the error in `error`; the DNS service does not.
        let detail = if body["error"].is_object() {
            &body["error"]
        } else {
            &body
        };
        let reason = match (detail["code"].as_str(), detail["message"].as_str()) {
            (Some(code), Some(message)) => format!("{code}: {message}"),
            (Some(text), None) | (None, Some(text)) => text.to_owned(),
            (None, None) => "no reason given".to_owned(),
        };
        let message = format!(
            "{action}: Azure answered HTTP {}: {reason}",
            status.as_u16()
        );
        let error = match status.as_u16() {
            408 | 409 | 412 | 429 | 500..=599 => Error::transient(message),
            _ => Error::permanent(message),
        };
        Err(Refusal { status, error })
    }

    /// Reads the TXT record set at `name`, or `None` when there is none.
    async fn read(&self, zone: &Zone, name: &str) -> Result<Option<RecordSet>> {
        let action = format!("reading the record set {name}");
        let url = self.record_set_url(zone, name);
        match self.call(&action, Method::GET, &url, None, None).await {
            Ok(body) => Ok(Some(RecordSet {
                etag: body["etag"].as_str().map(str::to_owned),
                ttl: body["properties"]["TTL"]
                    .as_u64()
                    .and_then(|ttl| u32::try_from(ttl).ok()),
                entries: body["properties"]["TXTRecords"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
                metadata: body["properties"]
                    .get("metadata")
                    .filter(|value| !value.is_null())
                    .cloned(),
            })),
            Err(refusal) if refusal.status == StatusCode::NOT_FOUND => Ok(None),
            Err(refusal) => Err(refusal.error),
        }
    }

    /// Writes the TXT record set at `name`, on the condition that it is still
    /// as it was read. `Ok(false)` means that it has changed since.
    async fn write(
        &self,
        zone: &Zone,
        name: &str,
        was: Option<&RecordSet>,
        entries: &[Value],
        ttl: u32,
    ) -> Result<bool> {
        let action = format!("writing the record set {name}");
        let mut properties = json!({ "TTL": ttl, "TXTRecords": entries });
        if let Some(metadata) = was.and_then(|was| was.metadata.clone()) {
            properties["metadata"] = metadata;
        }
        let body = json!({ "properties": properties });
        let condition = match was.and_then(|was| was.etag.as_deref()) {
            Some(etag) => ("If-Match", etag),
            // No record set was there: create one, but do not replace one
            // that appeared in the meantime.
            None => ("If-None-Match", "*"),
        };
        let url = self.record_set_url(zone, name);
        match self
            .call(&action, Method::PUT, &url, Some(condition), Some(&body))
            .await
        {
            Ok(_) => Ok(true),
            Err(refusal) if refusal.status == StatusCode::PRECONDITION_FAILED => Ok(false),
            Err(refusal) => Err(refusal.error),
        }
    }

    /// Deletes the TXT record set at `name`, on the condition that it is
    /// still as it was read. `Ok(false)` means that it has changed since.
    async fn delete(&self, zone: &Zone, name: &str, was: &RecordSet) -> Result<bool> {
        let action = format!("deleting the record set {name}");
        let url = self.record_set_url(zone, name);
        let condition = was.etag.as_deref().map(|etag| ("If-Match", etag));
        match self
            .call(&action, Method::DELETE, &url, condition, None)
            .await
        {
            Ok(_) => Ok(true),
            Err(refusal) if refusal.status == StatusCode::NOT_FOUND => Ok(true),
            Err(refusal) if refusal.status == StatusCode::PRECONDITION_FAILED => Ok(false),
            Err(refusal) => Err(refusal.error),
        }
    }
}

/// The name of a record relative to its zone, as Azure DNS addresses it:
/// `_acme-challenge.www` for `_acme-challenge.www.example.com` in
/// `example.com`, and `@` for the zone's own name.
fn relative_name(zone: &Zone, name: &str) -> String {
    let name = super::normalize(name);
    match name.strip_suffix(zone.name.as_str()) {
        Some("") => "@".to_owned(),
        Some(prefix) => prefix.trim_end_matches('.').to_owned(),
        None => name,
    }
}

/// The value of one entry of `TXTRecords`. Azure holds a value as a list of
/// strings of up to 255 characters, which together are the value.
fn entry_value(entry: &Value) -> String {
    entry["value"]
        .as_array()
        .map(|parts| parts.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn changed_too_often(action: &str, name: &str) -> Error {
    Error::transient(format!(
        "{action} the record set {name}: it was changed by someone else {WRITE_ATTEMPTS} times in a row"
    ))
}

#[async_trait]
impl DnsProvider for AzureDnsProvider {
    async fn zones(&self) -> Result<Vec<Zone>> {
        let mut zones = Vec::new();
        let mut url = self.zones_url();
        loop {
            let body = self
                .call("listing the zones", Method::GET, &url, None, None)
                .await
                .map_err(|refusal| refusal.error)?;
            for zone in body["value"].as_array().into_iter().flatten() {
                if let Some(name) = zone["name"].as_str() {
                    zones.push(Zone::new(name, name));
                }
            }
            match body["nextLink"].as_str() {
                Some(next) if !next.is_empty() => url = next.to_owned(),
                _ => return Ok(zones),
            }
        }
    }

    async fn present(
        &self,
        zone: &Zone,
        name: &str,
        values: &[String],
        ttl: u32,
    ) -> Result<RecordHandle> {
        for _ in 0..WRITE_ATTEMPTS {
            let was = self.read(zone, name).await?;
            let mut entries = was
                .as_ref()
                .map(|was| was.entries.clone())
                .unwrap_or_default();
            for value in values {
                if !entries.iter().any(|entry| entry_value(entry) == *value) {
                    entries.push(json!({ "value": [value] }));
                }
            }
            if self.write(zone, name, was.as_ref(), &entries, ttl).await? {
                return Ok(RecordHandle {
                    zone: zone.clone(),
                    name: name.to_owned(),
                    values: values.to_vec(),
                    record_ids: Vec::new(),
                });
            }
        }
        Err(changed_too_often("writing", name))
    }

    async fn cleanup(&self, handle: RecordHandle) -> Result<()> {
        let (zone, name) = (&handle.zone, handle.name.as_str());
        for _ in 0..WRITE_ATTEMPTS {
            let Some(was) = self.read(zone, name).await? else {
                return Ok(());
            };
            let remaining: Vec<Value> = was
                .entries
                .iter()
                .filter(|entry| !handle.values.contains(&entry_value(entry)))
                .cloned()
                .collect();
            if remaining.len() == was.entries.len() {
                // None of this run's values is there any more.
                return Ok(());
            }
            let done = if remaining.is_empty() {
                self.delete(zone, name, &was).await?
            } else {
                // The time-to-live of a record set that others also use is
                // left as it is.
                let ttl = was.ttl.unwrap_or(crate::config::defaults::TTL_SECONDS);
                self.write(zone, name, Some(&was), &remaining, ttl).await?
            };
            if done {
                return Ok(());
            }
        }
        Err(changed_too_often("cleaning up", name))
    }
}

#[cfg(test)]
mod tests;
