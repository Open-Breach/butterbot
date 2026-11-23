//! Task 6.4: the Azure DNS provider against constructed HTTP exchanges.
//!
//! The answers are built from the Azure DNS REST API reference (operations
//! "Zones - List By Resource Group" and "Record Sets - Get", "Create Or
//! Update" and "Delete", API version 2018-05-01). They are constructed, not
//! recorded from a real subscription. The subscription identifier is zeros.

use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;

const SUBSCRIPTION: &str = "00000000-0000-0000-0000-000000000000";
const GROUP: &str = "dns-zones";
const RECORD: &str = "_acme-challenge.example.com";

async fn provider() -> (MockServer, AzureDnsProvider) {
    let server = MockServer::start().await;
    let provider = AzureDnsProvider::with(
        crate::http::client().expect("an HTTP client"),
        crate::azure::testing::credential(),
        &server.uri(),
        SUBSCRIPTION,
        GROUP,
    );
    (server, provider)
}

fn zone() -> Zone {
    Zone::new("example.com", "example.com")
}

fn values(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn zones_path() -> String {
    format!(
        "/subscriptions/{SUBSCRIPTION}/resourceGroups/{GROUP}/providers/Microsoft.Network/dnsZones"
    )
}

fn record_set_path(relative: &str) -> String {
    format!("{}/example.com/TXT/{relative}", zones_path())
}

/// A TXT record set as "Record Sets - Get" returns it.
fn record_set(etag: &str, ttl: u32, values: &[&str]) -> Value {
    let records: Vec<Value> = values
        .iter()
        .map(|value| json!({ "value": [value] }))
        .collect();
    json!({
        "id": format!("{}/example.com/TXT/_acme-challenge", zones_path()),
        "etag": etag,
        "name": "_acme-challenge",
        "type": "Microsoft.Network/dnsZones/TXT",
        "properties": {
            "TTL": ttl,
            "fqdn": "_acme-challenge.example.com.",
            "TXTRecords": records,
            "provisioningState": "Succeeded"
        }
    })
}

fn not_found() -> ResponseTemplate {
    ResponseTemplate::new(404).set_body_json(json!({
        "code": "NotFound",
        "message": "The resource record '_acme-challenge' does not exist in resource group 'dns-zones' of subscription '00000000-0000-0000-0000-000000000000'."
    }))
}

/// Mounts one answer to GET for the record set. Answers are used in the order
/// they are mounted, each once.
async fn on_get(server: &MockServer, answer: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(record_set_path("_acme-challenge")))
        .and(query_param("api-version", "2018-05-01"))
        .respond_with(answer)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

async fn on_put(server: &MockServer, answer: ResponseTemplate) {
    Mock::given(method("PUT"))
        .and(path(record_set_path("_acme-challenge")))
        .respond_with(answer)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

async fn on_delete(server: &MockServer, answer: ResponseTemplate) {
    Mock::given(method("DELETE"))
        .and(path(record_set_path("_acme-challenge")))
        .respond_with(answer)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

/// The requests that change something, as (method, headers of interest, body).
async fn writes(server: &MockServer) -> Vec<(String, Option<String>, Option<String>, Value)> {
    let received: Vec<Request> = server
        .received_requests()
        .await
        .expect("requests are recorded");
    received
        .iter()
        .filter(|request| request.method.as_str() != "GET")
        .map(|request| {
            let text = |name: &str| {
                request
                    .headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            };
            (
                request.method.to_string(),
                text("if-match"),
                text("if-none-match"),
                request.body_json().unwrap_or(Value::Null),
            )
        })
        .collect()
}

fn txt_values(body: &Value) -> Vec<String> {
    body["properties"]["TXTRecords"]
        .as_array()
        .unwrap()
        .iter()
        .map(entry_value)
        .collect()
}

// dns-validation, "DNS provider types", scenario "Azure DNS instance".
#[tokio::test]
async fn zones_of_the_configured_subscription_and_resource_group_are_listed() {
    let (server, provider) = provider().await;
    let zone_object = |name: &str| {
        json!({
            "id": format!("{}/{name}", zones_path()),
            "name": name,
            "type": "Microsoft.Network/dnsZones",
            "etag": "00000000-0000-0000-0000-000000000000",
            "location": "global",
            "tags": {},
            "properties": {
                "maxNumberOfRecordSets": 10000,
                "numberOfRecordSets": 4,
                "nameServers": ["ns1-01.example.net", "ns2-01.example.net"],
                "zoneType": "Public"
            }
        })
    };
    let next = format!(
        "{}{}?api-version=2018-05-01&$skipToken=page2",
        server.uri(),
        zones_path()
    );
    Mock::given(method("GET"))
        .and(path(zones_path()))
        .and(query_param("$skipToken", "page2"))
        .respond_with(ok(json!({ "value": [zone_object("dev.example.com")] })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(zones_path()))
        .and(query_param("api-version", "2018-05-01"))
        .respond_with(ok(
            json!({ "value": [zone_object("Example.com")], "nextLink": next }),
        ))
        .mount(&server)
        .await;

    let zones = provider.zones().await.expect("listed");

    assert_eq!(
        zones,
        [
            // The name is normalised for matching; the zone is addressed as Azure spells it.
            Zone {
                name: "example.com".into(),
                id: "Example.com".into()
            },
            Zone {
                name: "dev.example.com".into(),
                id: "dev.example.com".into()
            },
        ]
    );
    for request in server.received_requests().await.unwrap() {
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-token"),
            "every request carries a token of the Azure identity"
        );
    }
}

// dns-validation, "All values for a name are published together", scenario "Wildcard and apex".
#[tokio::test]
async fn present_creates_the_record_set_with_both_values_of_wildcard_and_apex() {
    let (server, provider) = provider().await;
    on_get(&server, not_found()).await;
    on_put(
        &server,
        ResponseTemplate::new(201).set_body_json(record_set(
            "etag-1",
            60,
            &["for-the-wildcard", "for-the-apex"],
        )),
    )
    .await;

    let handle = provider
        .present(
            &zone(),
            RECORD,
            &values(&["for-the-wildcard", "for-the-apex"]),
            60,
        )
        .await
        .expect("published");

    assert_eq!(
        handle,
        RecordHandle {
            zone: zone(),
            name: RECORD.into(),
            values: values(&["for-the-wildcard", "for-the-apex"]),
            record_ids: Vec::new(),
        }
    );
    let writes = writes(&server).await;
    assert_eq!(writes.len(), 1);
    let (method, if_match, if_none_match, body) = &writes[0];
    assert_eq!(method, "PUT");
    assert_eq!(
        (if_match.as_deref(), if_none_match.as_deref()),
        (None, Some("*")),
        "a new record set is created only if none has appeared in the meantime"
    );
    assert_eq!(
        body,
        &json!({
            "properties": {
                "TTL": 60,
                "TXTRecords": [{ "value": ["for-the-wildcard"] }, { "value": ["for-the-apex"] }]
            }
        }),
        "one record set that serves two TXT values"
    );
}

// dns-validation, "Only own values are removed", scenario "Foreign value at the same name",
// and "Leftover values do not block validation".
#[tokio::test]
async fn present_adds_to_an_existing_record_set_and_keeps_what_is_there() {
    let (server, provider) = provider().await;
    let existing = json!({
        "id": format!("{}/example.com/TXT/_acme-challenge", zones_path()),
        "etag": "etag-7",
        "name": "_acme-challenge",
        "type": "Microsoft.Network/dnsZones/TXT",
        "properties": {
            "metadata": { "owner": "another-tool" },
            "TTL": 3600,
            "fqdn": "_acme-challenge.example.com.",
            "TXTRecords": [
                { "value": ["left-by-a-killed-run"] },
                { "value": ["a-long-value-", "in-two-parts"] }
            ]
        }
    });
    on_get(&server, ok(existing)).await;
    on_put(&server, ok(record_set("etag-8", 60, &[]))).await;

    provider
        .present(&zone(), RECORD, &values(&["value-one"]), 60)
        .await
        .expect("published");

    let writes = writes(&server).await;
    let (_, if_match, if_none_match, body) = &writes[0];
    assert_eq!(
        (if_match.as_deref(), if_none_match.as_deref()),
        (Some("etag-7"), None),
        "the write is conditional on the version that was read"
    );
    assert_eq!(
        body,
        &json!({
            "properties": {
                "TTL": 60,
                "metadata": { "owner": "another-tool" },
                "TXTRecords": [
                    { "value": ["left-by-a-killed-run"] },
                    { "value": ["a-long-value-", "in-two-parts"] },
                    { "value": ["value-one"] }
                ]
            }
        }),
        "the values of others are written back as they were, with this run's value added"
    );
}

#[tokio::test]
async fn present_reads_again_when_the_record_set_changed_in_between() {
    let (server, provider) = provider().await;
    on_get(&server, ok(record_set("etag-1", 60, &["foreign-one"]))).await;
    on_put(
        &server,
        ResponseTemplate::new(412).set_body_json(json!({
            "code": "PreconditionFailed",
            "message": "The Record set _acme-challenge has been modified (etag mismatch)."
        })),
    )
    .await;
    on_get(
        &server,
        ok(record_set("etag-2", 60, &["foreign-one", "foreign-two"])),
    )
    .await;
    on_put(&server, ok(record_set("etag-3", 60, &[]))).await;

    provider
        .present(&zone(), RECORD, &values(&["value-one"]), 60)
        .await
        .expect("published");

    let writes = writes(&server).await;
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[1].1.as_deref(), Some("etag-2"));
    assert_eq!(
        txt_values(&writes[1].3),
        ["foreign-one", "foreign-two", "value-one"],
        "the value that someone else added in between is not lost"
    );
}

#[tokio::test]
async fn present_gives_up_when_the_record_set_keeps_changing() {
    let (server, provider) = provider().await;
    Mock::given(method("GET"))
        .respond_with(ok(record_set("etag-1", 60, &[])))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(
            ResponseTemplate::new(412)
                .set_body_json(json!({ "code": "PreconditionFailed", "message": "etag mismatch" })),
        )
        .mount(&server)
        .await;

    let error = provider
        .present(&zone(), RECORD, &values(&["value-one"]), 60)
        .await
        .expect_err("refused");

    assert!(error.is_transient(), "{error:?}");
    assert_eq!(writes(&server).await.len(), 4);
}

// dns-validation, "Only own values are removed", scenarios "Cleanup after success" and
// "Foreign value at the same name".
#[tokio::test]
async fn cleanup_takes_out_its_own_values_and_writes_back_the_rest() {
    let (server, provider) = provider().await;
    on_get(
        &server,
        ok(record_set(
            "etag-4",
            3600,
            &["foreign-one", "value-one", "value-two", "foreign-two"],
        )),
    )
    .await;
    on_put(
        &server,
        ok(record_set("etag-5", 3600, &["foreign-one", "foreign-two"])),
    )
    .await;
    let handle = RecordHandle {
        zone: zone(),
        name: RECORD.into(),
        values: values(&["value-one", "value-two"]),
        record_ids: Vec::new(),
    };

    provider.cleanup(handle).await.expect("removed");

    let writes = writes(&server).await;
    assert_eq!(writes.len(), 1);
    let (method, if_match, _, body) = &writes[0];
    assert_eq!(method, "PUT");
    assert_eq!(if_match.as_deref(), Some("etag-4"));
    assert_eq!(
        txt_values(body),
        ["foreign-one", "foreign-two"],
        "the foreign values are still there"
    );
    assert_eq!(
        body["properties"]["TTL"], 3600,
        "the time-to-live of a shared record set is kept"
    );
}

#[tokio::test]
async fn cleanup_deletes_a_record_set_that_it_emptied() {
    let (server, provider) = provider().await;
    on_get(
        &server,
        ok(record_set("etag-4", 60, &["value-one", "value-two"])),
    )
    .await;
    on_delete(&server, ResponseTemplate::new(200)).await;
    let handle = RecordHandle {
        zone: zone(),
        name: RECORD.into(),
        values: values(&["value-one", "value-two"]),
        record_ids: Vec::new(),
    };

    provider.cleanup(handle).await.expect("removed");

    let writes = writes(&server).await;
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, "DELETE");
    assert_eq!(
        writes[0].1.as_deref(),
        Some("etag-4"),
        "the record set is deleted only if nobody added a value in the meantime"
    );
}

#[tokio::test]
async fn cleanup_has_nothing_to_do_when_its_values_are_gone() {
    for answer in [not_found(), ok(record_set("etag-9", 60, &["foreign-one"]))] {
        let (server, provider) = provider().await;
        on_get(&server, answer).await;
        let handle = RecordHandle {
            zone: zone(),
            name: RECORD.into(),
            values: values(&["value-one"]),
            record_ids: Vec::new(),
        };
        provider.cleanup(handle).await.expect("nothing to remove");
        assert!(writes(&server).await.is_empty(), "nothing is written");
    }
}

#[tokio::test]
async fn cleanup_reads_again_when_the_record_set_changed_in_between() {
    let (server, provider) = provider().await;
    on_get(&server, ok(record_set("etag-1", 60, &["value-one"]))).await;
    on_delete(
        &server,
        ResponseTemplate::new(412)
            .set_body_json(json!({ "code": "PreconditionFailed", "message": "etag mismatch" })),
    )
    .await;
    on_get(
        &server,
        ok(record_set(
            "etag-2",
            60,
            &["value-one", "added-by-someone-else"],
        )),
    )
    .await;
    on_put(
        &server,
        ok(record_set("etag-3", 60, &["added-by-someone-else"])),
    )
    .await;
    let handle = RecordHandle {
        zone: zone(),
        name: RECORD.into(),
        values: values(&["value-one"]),
        record_ids: Vec::new(),
    };

    provider.cleanup(handle).await.expect("removed");

    let writes = writes(&server).await;
    assert_eq!(
        writes
            .iter()
            .map(|write| write.0.as_str())
            .collect::<Vec<_>>(),
        ["DELETE", "PUT"]
    );
    assert_eq!(txt_values(&writes[1].3), ["added-by-someone-else"]);
}

// run-orchestration, "Transient errors are retried", scenario "Permission denied".
#[tokio::test]
async fn refused_identity_is_permanent() {
    let (server, provider) = provider().await;
    Mock::given(method("GET"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {
                "code": "AuthorizationFailed",
                "message": "The client does not have authorization to perform action 'Microsoft.Network/dnsZones/TXT/read' over scope '/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/dns-zones'."
            }
        })))
        .mount(&server)
        .await;

    let error = provider
        .present(&zone(), RECORD, &values(&["value-one"]), 60)
        .await
        .expect_err("refused");

    assert_eq!(error.class(), "permanent");
    assert!(
        error.to_string().starts_with(
            "reading the record set _acme-challenge.example.com: Azure answered HTTP 403: \
             AuthorizationFailed: The client does not have authorization"
        ),
        "{error}"
    );
    assert!(!error.to_string().contains("test-token"));
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn throttling_and_server_failures_are_transient() {
    for status in [429, 500, 503] {
        let (server, provider) = provider().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_json(
                json!({ "error": { "code": "Unavailable", "message": "try later" } }),
            ))
            .mount(&server)
            .await;
        let error = provider.zones().await.expect_err("failed");
        assert!(error.is_transient(), "{status}: {error:?}");
    }
}

#[test]
fn record_name_is_given_relative_to_the_zone() {
    let zone = zone();
    assert_eq!(
        relative_name(&zone, "_acme-challenge.example.com"),
        "_acme-challenge"
    );
    assert_eq!(
        relative_name(&zone, "_acme-challenge.www.example.com"),
        "_acme-challenge.www"
    );
    assert_eq!(
        relative_name(&zone, "_ACME-Challenge.Example.com."),
        "_acme-challenge"
    );
    assert_eq!(relative_name(&zone, "example.com"), "@");
}

#[test]
fn value_in_several_parts_is_one_value() {
    assert_eq!(entry_value(&json!({ "value": ["abc", "def"] })), "abcdef");
    assert_eq!(entry_value(&json!({ "value": ["abc"] })), "abc");
    assert_eq!(entry_value(&json!({})), "");
}
