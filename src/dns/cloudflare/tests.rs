//! Task 6.3: the Cloudflare provider against constructed HTTP exchanges.
//!
//! The answers are built from the Cloudflare API reference (operations "List
//! Zones", "Create DNS Record", "List DNS Records" and "Delete DNS Record",
//! and the documented error envelope). They are constructed, not recorded
//! from the real API. Identifiers are made up.

use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;

const TOKEN: &str = "cloudflare-test-token";
const ZONE_ID: &str = "023e105f4ecef8ad9ca31a8372d0c353";
const RECORD: &str = "_acme-challenge.example.com";

async fn provider() -> (MockServer, CloudflareProvider) {
    let server = MockServer::start().await;
    let http = crate::http::client().expect("an HTTP client");
    let provider = CloudflareProvider::with_api_url(http, Secret::new(TOKEN), &server.uri());
    (server, provider)
}

fn zone() -> Zone {
    Zone::new("example.com", ZONE_ID)
}

fn values(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn ok(result: Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(json!({ "success": true, "errors": [], "messages": [], "result": result }))
}

fn failed(status: u16, code: i64, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({
        "success": false,
        "errors": [{ "code": code, "message": message }],
        "messages": [],
        "result": null
    }))
}

/// A zone as "List Zones" returns it.
fn zone_object(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "status": "active",
        "paused": false,
        "type": "full",
        "development_mode": 0,
        "name_servers": ["bob.ns.cloudflare.com", "lola.ns.cloudflare.com"],
        "account": { "id": "01a7362d577a6c3019a474fd6f485823", "name": "Example Account Name" },
        "created_on": "2014-01-01T05:20:00.12345Z",
        "modified_on": "2014-01-01T05:20:00.12345Z",
        "activated_on": "2014-01-02T00:01:00.12345Z"
    })
}

/// A record as "Create DNS Record" returns it.
fn record_object(id: &str, content: &str) -> Value {
    json!({
        "id": id,
        "name": RECORD,
        "type": "TXT",
        "content": content,
        "proxiable": false,
        "proxied": false,
        "ttl": 60,
        "settings": {},
        "meta": {},
        "comment": null,
        "tags": [],
        "created_on": "2014-01-01T05:20:00.12345Z",
        "modified_on": "2014-01-01T05:20:00.12345Z"
    })
}

/// Makes the server create a record with identifier `id` for `value`.
async fn expect_create(server: &MockServer, value: &str, id: &str) {
    let content = format!("\"{value}\"");
    Mock::given(method("POST"))
        .and(path(format!("/zones/{ZONE_ID}/dns_records")))
        .and(body_partial_json(json!({ "content": content })))
        .respond_with(ok(record_object(id, &content)))
        .mount(server)
        .await;
}

async fn expect_delete(server: &MockServer, id: &str) {
    Mock::given(method("DELETE"))
        .and(path(format!("/zones/{ZONE_ID}/dns_records/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": { "id": id } })))
        .mount(server)
        .await;
}

/// The requests the server received, as "METHOD path".
async fn requests(server: &MockServer) -> Vec<String> {
    let received: Vec<Request> = server
        .received_requests()
        .await
        .expect("requests are recorded");
    received
        .iter()
        .map(|request| format!("{} {}", request.method, request.url.path()))
        .collect()
}

#[tokio::test]
async fn zones_are_listed_page_by_page_with_the_instance_token() {
    let (server, provider) = provider().await;
    for (page, zone) in [
        ("1", zone_object(ZONE_ID, "example.com")),
        (
            "2",
            zone_object("9a7806061c88ada191ed06f989cc3dac", "Example.ORG"),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(query_param("page", page))
            .and(query_param("per_page", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "errors": [],
                "messages": [],
                "result": [zone],
                "result_info": { "count": 1, "page": page.parse::<u32>().unwrap(), "per_page": 50, "total_count": 2, "total_pages": 2 }
            })))
            .mount(&server)
            .await;
    }

    let zones = provider.zones().await.expect("listed");

    assert_eq!(
        zones,
        [
            Zone {
                name: "example.com".into(),
                id: ZONE_ID.into()
            },
            Zone {
                name: "example.org".into(),
                id: "9a7806061c88ada191ed06f989cc3dac".into()
            },
        ]
    );
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 2);
    for request in &received {
        // dns-validation, "DNS provider types", scenario "Cloudflare instance".
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer cloudflare-test-token")
        );
    }
}

// dns-validation, "All values for a name are published together", scenario "Wildcard and apex".
#[tokio::test]
async fn present_creates_one_txt_record_per_value() {
    let (server, provider) = provider().await;
    expect_create(&server, "value-for-the-wildcard", "record-1").await;
    expect_create(&server, "value-for-the-apex", "record-2").await;

    let handle = provider
        .present(
            &zone(),
            RECORD,
            &values(&["value-for-the-wildcard", "value-for-the-apex"]),
            60,
        )
        .await
        .expect("published");

    assert_eq!(
        handle,
        RecordHandle {
            zone: zone(),
            name: RECORD.into(),
            values: values(&["value-for-the-wildcard", "value-for-the-apex"]),
            record_ids: values(&["record-1", "record-2"]),
        }
    );
    let received = server.received_requests().await.unwrap();
    let bodies: Vec<Value> = received
        .iter()
        .map(|request| request.body_json().unwrap())
        .collect();
    assert_eq!(
        bodies,
        [
            json!({ "type": "TXT", "name": RECORD, "content": "\"value-for-the-wildcard\"", "ttl": 60 }),
            json!({ "type": "TXT", "name": RECORD, "content": "\"value-for-the-apex\"", "ttl": 60 }),
        ]
    );
}

#[tokio::test]
async fn present_passes_the_configured_ttl() {
    let (server, provider) = provider().await;
    expect_create(&server, "value-one", "record-1").await;
    provider
        .present(&zone(), RECORD, &values(&["value-one"]), 120)
        .await
        .expect("published");
    let body: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(body["ttl"], 120);
}

// dns-validation, "Only own values are removed", scenarios "Cleanup after success" and
// "Foreign value at the same name".
#[tokio::test]
async fn cleanup_deletes_exactly_the_records_it_created() {
    let (server, provider) = provider().await;
    // The zone also holds a record at the same name that another tool created,
    // with the identifier "foreign-record". Nothing below may touch it: no
    // mock exists for it, so a request for it would fail the cleanup.
    expect_create(&server, "value-one", "record-1").await;
    expect_create(&server, "value-two", "record-2").await;
    expect_delete(&server, "record-1").await;
    expect_delete(&server, "record-2").await;

    let handle = provider
        .present(&zone(), RECORD, &values(&["value-one", "value-two"]), 60)
        .await
        .expect("published");
    provider.cleanup(handle).await.expect("removed");

    assert_eq!(
        requests(&server).await,
        [
            format!("POST /zones/{ZONE_ID}/dns_records"),
            format!("POST /zones/{ZONE_ID}/dns_records"),
            format!("DELETE /zones/{ZONE_ID}/dns_records/record-1"),
            format!("DELETE /zones/{ZONE_ID}/dns_records/record-2"),
        ],
        "the records at the name are not listed, and only the two created records are deleted"
    );
}

#[tokio::test]
async fn cleanup_of_a_record_that_is_already_gone_succeeds() {
    let (server, provider) = provider().await;
    Mock::given(method("DELETE"))
        .and(path(format!("/zones/{ZONE_ID}/dns_records/record-1")))
        .respond_with(failed(404, 81044, "Record does not exist."))
        .mount(&server)
        .await;
    expect_delete(&server, "record-2").await;
    let handle = RecordHandle {
        zone: zone(),
        name: RECORD.into(),
        values: values(&["value-one", "value-two"]),
        record_ids: values(&["record-1", "record-2"]),
    };

    provider
        .cleanup(handle)
        .await
        .expect("nothing is left to remove");
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn cleanup_goes_on_after_a_failure_and_reports_it() {
    let (server, provider) = provider().await;
    Mock::given(method("DELETE"))
        .and(path(format!("/zones/{ZONE_ID}/dns_records/record-1")))
        .respond_with(failed(500, 1000, "Internal server error"))
        .mount(&server)
        .await;
    expect_delete(&server, "record-2").await;
    let handle = RecordHandle {
        zone: zone(),
        name: RECORD.into(),
        values: values(&["value-one", "value-two"]),
        record_ids: values(&["record-1", "record-2"]),
    };

    let error = provider
        .cleanup(handle)
        .await
        .expect_err("one record could not be deleted");

    assert!(error.is_transient(), "{error:?}");
    assert_eq!(
        requests(&server).await.len(),
        2,
        "the second record is still deleted"
    );
}

// run-orchestration, "Transient errors are retried", scenario "Permission denied".
#[tokio::test]
async fn refused_credential_is_permanent_and_the_message_has_no_token() {
    let (server, provider) = provider().await;
    Mock::given(method("POST"))
        .respond_with(failed(403, 10000, "Authentication error"))
        .mount(&server)
        .await;

    let error = provider
        .present(&zone(), RECORD, &values(&["value-one"]), 60)
        .await
        .expect_err("refused");

    assert_eq!(error.class(), "permanent");
    assert_eq!(
        error.to_string(),
        "creating the record _acme-challenge.example.com: Cloudflare answered HTTP 403: \
         Authentication error (code 10000)"
    );
    assert!(!format!("{error} {error:?}").contains(TOKEN));
    assert_eq!(
        requests(&server).await.len(),
        1,
        "the provider itself does not try again"
    );
}

#[tokio::test]
async fn throttling_and_server_failures_are_transient() {
    for (status, code, message) in [
        (
            429,
            971,
            "Please wait and consider throttling your request speed",
        ),
        (500, 1000, "Internal server error"),
        (503, 1001, "Service unavailable"),
    ] {
        let (server, provider) = provider().await;
        Mock::given(method("GET"))
            .respond_with(failed(status, code, message))
            .mount(&server)
            .await;
        let error = provider.zones().await.expect_err("failed");
        assert!(error.is_transient(), "{status}: {error:?}");
        assert!(
            error
                .to_string()
                .contains(&format!("HTTP {status}: {message}")),
            "{error}"
        );
    }
}

#[tokio::test]
async fn answer_that_is_not_json_is_classified_by_its_status() {
    let (server, provider) = provider().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>Bad Gateway</html>"))
        .mount(&server)
        .await;
    let error = provider.zones().await.expect_err("failed");
    assert!(error.is_transient());
    assert_eq!(
        error.to_string(),
        "listing the zones: Cloudflare answered HTTP 502: no reason given"
    );
}

#[tokio::test]
async fn unreachable_api_is_transient() {
    // An address at which nothing listens: a port that was just released.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    let address = listener.local_addr().expect("an address");
    drop(listener);
    let http = crate::http::client().expect("an HTTP client");
    let provider =
        CloudflareProvider::with_api_url(http, Secret::new(TOKEN), &format!("http://{address}"));

    let error = provider.zones().await.expect_err("nothing listens there");

    assert!(error.is_transient(), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with("listing the zones: no answer from Cloudflare"),
        "{error}"
    );
    assert!(!error.to_string().contains(TOKEN));
}

#[tokio::test]
async fn record_left_by_a_lost_answer_of_the_same_call_is_taken_over() {
    let (server, provider) = provider().await;
    Mock::given(method("POST"))
        .and(path(format!("/zones/{ZONE_ID}/dns_records")))
        .respond_with(failed(400, 81058, "An identical record already exists."))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/zones/{ZONE_ID}/dns_records")))
        .and(query_param("type", "TXT"))
        .and(query_param("name", RECORD))
        .and(query_param("content", "\"value-one\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "errors": [],
            "messages": [],
            "result": [record_object("record-1", "\"value-one\"")],
            "result_info": { "count": 1, "page": 1, "per_page": 100, "total_count": 1, "total_pages": 1 }
        })))
        .mount(&server)
        .await;

    let handle = provider
        .present(&zone(), RECORD, &values(&["value-one"]), 60)
        .await
        .expect("published");

    assert_eq!(
        handle.record_ids,
        ["record-1"],
        "the existing record with this run's value"
    );
}

#[tokio::test]
async fn failed_present_removes_what_it_had_already_created() {
    let (server, provider) = provider().await;
    expect_create(&server, "value-one", "record-1").await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "content": "\"value-two\"" })))
        .respond_with(failed(403, 10000, "Authentication error"))
        .mount(&server)
        .await;
    expect_delete(&server, "record-1").await;

    let error = provider
        .present(&zone(), RECORD, &values(&["value-one", "value-two"]), 60)
        .await
        .expect_err("the second value is refused");

    assert_eq!(error.class(), "permanent");
    assert_eq!(
        requests(&server).await.last().map(String::as_str),
        Some(format!("DELETE /zones/{ZONE_ID}/dns_records/record-1").as_str()),
        "nothing is left behind"
    );
}
