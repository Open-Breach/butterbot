//! A DNS provider for tests: it publishes challenge records in the challenge
//! DNS server of the ACME test server (`pebble-challtestsrv`).
//!
//! This is not a provider for real use and is compiled only for tests and
//! with the `test-support` feature. The server has no zones, so the zones are
//! given to the provider. Its management interface can add a TXT value to a
//! name and clear a name, but not remove one value; cleanup therefore clears
//! the whole name.

use async_trait::async_trait;
use serde_json::json;

use super::{DnsProvider, RecordHandle, Zone};
use crate::{Error, Result};

/// A DNS provider that drives a `pebble-challtestsrv`.
pub struct ChallTestSrvProvider {
    http: reqwest::Client,
    management_url: String,
    zones: Vec<Zone>,
}

impl ChallTestSrvProvider {
    /// A provider for the server whose management interface is at
    /// `management_url`, pretending to host `zones`.
    pub fn new(http: reqwest::Client, management_url: &str, zones: &[&str]) -> Self {
        Self {
            http,
            management_url: management_url.trim_end_matches('/').to_owned(),
            zones: zones.iter().map(|zone| Zone::new(zone, zone)).collect(),
        }
    }

    async fn post(&self, action: &str, endpoint: &str, body: serde_json::Value) -> Result<()> {
        let response = self
            .http
            .post(format!("{}/{endpoint}", self.management_url))
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                Error::transient(format!(
                    "{action}: no answer from the challenge DNS server: {}",
                    error.without_url()
                ))
            })?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Error::permanent(format!(
                "{action}: the challenge DNS server answered HTTP {}",
                response.status().as_u16()
            )))
        }
    }
}

/// The server wants names with a trailing dot.
fn host(name: &str) -> String {
    format!("{}.", super::normalize(name))
}

#[async_trait]
impl DnsProvider for ChallTestSrvProvider {
    async fn zones(&self) -> Result<Vec<Zone>> {
        Ok(self.zones.clone())
    }

    async fn present(
        &self,
        zone: &Zone,
        name: &str,
        values: &[String],
        _ttl: u32,
    ) -> Result<RecordHandle> {
        for value in values {
            let action = format!("setting the record {name}");
            self.post(
                &action,
                "set-txt",
                json!({ "host": host(name), "value": value }),
            )
            .await?;
        }
        Ok(RecordHandle {
            zone: zone.clone(),
            name: name.to_owned(),
            values: values.to_vec(),
            record_ids: Vec::new(),
        })
    }

    async fn cleanup(&self, handle: RecordHandle) -> Result<()> {
        let action = format!("clearing the record {}", handle.name);
        self.post(&action, "clear-txt", json!({ "host": host(&handle.name) }))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use time::macros::datetime;

    use super::*;
    use crate::config::PropagationCheck;
    use crate::dns::propagation::{Published, Settings, wait_for_propagation};
    use crate::logging::{Capture, Level, LogFormat};
    use crate::testing::ChallTestSrv;

    const RECORD: &str = "_acme-challenge.example.com";

    /// Settings that ask the challenge DNS server directly, as a resolver.
    fn settings(server: &ChallTestSrv, timeout: Duration) -> Settings {
        Settings {
            mode: PropagationCheck::Resolver,
            resolvers: vec![server.dns],
            discovery: Vec::new(),
            interval: Duration::from_millis(50),
            timeout,
            nameserver_port: server.dns.port(),
            query_timeout: Duration::from_secs(2),
        }
    }

    // Task 6.5: a TXT value set through the provider is returned by a DNS query.
    #[tokio::test]
    #[ignore = "needs pebble-challtestsrv; see src/testing.rs"]
    async fn value_set_through_the_provider_is_returned_by_a_dns_query() {
        let server = ChallTestSrv::start();
        let http = crate::http::client().expect("an HTTP client");
        let provider = ChallTestSrvProvider::new(http, &server.management_url(), &["example.com"]);
        let log = Capture::new()
            .logger(
                LogFormat::Json,
                Level::Info,
                datetime!(2026-11-23 02:00:00 UTC),
            )
            .step("test", "propagation");

        let zones = provider.zones().await.expect("the zones it was given");
        let zone = crate::dns::resolve_zone("test", &zones, RECORD).expect("example.com");
        let values = vec!["for-the-wildcard".to_owned(), "for-the-apex".to_owned()];
        let published = [Published {
            name: RECORD.to_owned(),
            values: values.clone(),
        }];

        // Before publishing, the server does not return the values.
        wait_for_propagation(
            &settings(&server, Duration::from_millis(100)),
            &published,
            &log,
        )
        .await
        .expect_err("nothing is published yet");

        let handle = provider
            .present(zone, RECORD, &values, 60)
            .await
            .expect("published");
        wait_for_propagation(&settings(&server, Duration::from_secs(5)), &published, &log)
            .await
            .expect("both values are returned by a DNS query");

        provider.cleanup(handle).await.expect("cleared");
        wait_for_propagation(
            &settings(&server, Duration::from_millis(100)),
            &published,
            &log,
        )
        .await
        .expect_err("the values are no longer served");
    }

    #[test]
    fn name_is_sent_with_a_trailing_dot() {
        assert_eq!(
            host("_ACME-challenge.Example.com"),
            "_acme-challenge.example.com."
        );
        assert_eq!(
            host("_acme-challenge.example.com."),
            "_acme-challenge.example.com."
        );
    }
}
