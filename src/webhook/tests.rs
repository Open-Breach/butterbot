//! Tasks 3.1 to 3.5 of `add-webhook-notifier`: the webhook sink against a
//! local test server.
//!
//! No test leaves the machine, with one exception that is meant to fail: the
//! host of the `.invalid` domain, which no resolver knows.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};
use time::macros::datetime;
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::config::{Layer, MemoryVault, Secret, SecretVault, env_layer};
use crate::events::{EventKind, Failure, examples};
use crate::logging::{Capture, LogFormat};

/// Stands for the credential a webhook URL carries.
const SECRET_PATH: &str = "/hook/credential-in-the-path";
/// The value of `notify.hook.access_key` where a test supplies one.
const KEY: &str = "console-key-1";

/// The pause after the first failed attempt in these tests.
const SHORT_PAUSE: Duration = Duration::from_millis(10);

fn capture() -> (Capture, Logger) {
    let capture = Capture::new();
    let logger = capture.logger(
        LogFormat::Json,
        Level::Info,
        datetime!(2026-11-23 02:01:00 UTC),
    );
    (capture, logger)
}

/// The configuration of one TOML document and environment variables.
fn config_with_env(text: &str, variables: &[(&str, &str)]) -> Config {
    let file = Layer::from_toml("test.toml", text).expect("valid TOML");
    let variables: Vec<_> = variables
        .iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect();
    let environment = env_layer(variables).expect("usable variables");
    Config::from_layers([file, environment]).unwrap_or_else(|error| panic!("{error}"))
}

fn config(text: &str) -> Config {
    config_with_env(text, &[])
}

/// The settings of the webhook instance `name`.
fn webhook(config: &Config, name: &str) -> Webhook {
    let NotifierKind::Webhook(webhook) = &config.notify[name].kind;
    webhook.clone()
}

/// A configuration with the webhook instance `hook`, whose URL is `url` and
/// which has the given further settings.
fn hook_config(url: &str, settings: &str) -> Config {
    config(&format!(
        "[notify.hook]\ntype = \"webhook\"\nurl = \"{url}\"\n{settings}"
    ))
}

/// The sink of the instance `hook` with the given URL and further settings,
/// with short pauses between attempts.
fn sink_for(url: &str, settings: &str, logger: &Logger) -> WebhookSink {
    let config = hook_config(url, settings);
    WebhookSink::open("hook", &webhook(&config, "hook"), logger)
        .unwrap_or_else(|error| panic!("{error}"))
        .with_first_pause(SHORT_PAUSE)
}

/// A sink that delivers to the secret path of `server`.
fn sink(server: &MockServer, settings: &str) -> WebhookSink {
    let (_, logger) = capture();
    sink_for(&secret_url(server), settings, &logger)
}

fn secret_url(server: &MockServer) -> String {
    format!("{}{SECRET_PATH}", server.uri())
}

/// A server that answers every request with `status`.
async fn server_answering(status: u16) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(status))
        .mount(&server)
        .await;
    server
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("the server records its requests")
}

fn body_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap_or_else(|error| {
        panic!(
            "the body is not JSON ({error}): {}",
            String::from_utf8_lossy(&request.body)
        )
    })
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers
        .get(name)
        .map(|value| value.to_str().expect("a text header"))
}

/// A failure of `wildcard-example-com` at step `validate` whose reason has a
/// quotation mark, a backslash, a line break and non-ASCII text.
fn awkward_failure() -> Event {
    Event::Failure(Failure {
        timestamp: datetime!(2026-11-23 02:00:38 UTC),
        certificate: "wildcard-example-com".into(),
        step: "validate".into(),
        reason: AWKWARD_REASON.into(),
        stored_expiry: None,
    })
}

const AWKWARD_REASON: &str =
    "the server said \"no\" for C:\\zones\nZertifikat für *.example.com abgelehnt";

/// The records of a captured log that are not events.
fn records_about_delivery(capture: &Capture) -> Vec<Map<String, Value>> {
    capture
        .json()
        .into_iter()
        .filter(|record| !record.contains_key("event"))
        .collect()
}

/// Asserts that nothing of the URL and no key is in `text`.
fn assert_nothing_secret(text: &str, url: &str) {
    let authority = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .expect("the URL has a host");
    for secret in [url, authority, SECRET_PATH, "credential-in-the-path", KEY] {
        assert!(!text.contains(secret), "{secret:?} is in:\n{text}");
    }
}

// --- Task 3.1: the request ---------------------------------------------------------

// webhook-notifier, "Method and default body", scenario "Default request".
#[tokio::test]
async fn default_request_is_a_post_with_the_event_as_json() {
    let server = server_answering(200).await;
    let event = examples::renewed();

    sink(&server, "").deliver(&event).await.expect("delivered");

    let requests = requests(&server).await;
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), SECRET_PATH);
    assert_eq!(header(request, "content-type"), Some("application/json"));
    let body = body_json(request);
    assert_eq!(body, Value::Object(event.to_json()));
    assert_eq!(body["event"], "renewed");
    assert_eq!(
        String::from_utf8_lossy(&request.body),
        r#"{"event":"renewed","timestamp":"2026-11-23T02:00:35Z","certificate":"wildcard-example-com","names":["*.example.com","example.com"],"expiry":"2027-02-21T01:00:34Z","acme":"letsencrypt","stores":["vault","local"]}"#
    );
}

// webhook-notifier, "Method and default body", scenario "GET request".
#[tokio::test]
async fn get_request_has_no_body() {
    let server = server_answering(200).await;
    let sink = sink(&server, "method = \"GET\"\n");

    for event in [examples::renewed(), examples::failure()] {
        sink.deliver(&event).await.expect("delivered");
    }

    let requests = requests(&server).await;
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method.as_str(), "GET");
        assert!(request.body.is_empty(), "no body");
        assert_eq!(header(request, "content-type"), None);
    }
}

#[tokio::test]
async fn get_request_has_no_body_even_when_one_is_configured() {
    let server = server_answering(200).await;
    let sink = sink(&server, "method = \"GET\"\nbody = \"{{summary}}\"\n");

    sink.deliver(&examples::failure()).await.expect("delivered");

    let requests = requests(&server).await;
    assert!(requests[0].body.is_empty());
}

// webhook-notifier, "Custom body", scenario "Custom JSON body".
#[tokio::test]
async fn custom_json_body_has_its_placeholders_replaced() {
    let server = server_answering(200).await;
    let sink = sink(&server, "body = '{\"message\": \"{{summary}}\"}'\n");

    sink.deliver(&awkward_failure()).await.expect("delivered");

    let requests = requests(&server).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        header(&requests[0], "content-type"),
        Some("application/json")
    );
    assert_eq!(
        body_json(&requests[0]),
        json!({ "message": "certificate wildcard-example-com failed at step validate" })
    );
}

// webhook-notifier, "Custom body": the content type given by `content_type`.
#[tokio::test]
async fn custom_body_is_sent_with_its_content_type() {
    let server = server_answering(200).await;
    let sink = sink(
        &server,
        "body = \"{{event}}: {{summary}}\"\ncontent_type = \"text/plain; charset=utf-8\"\n",
    );

    sink.deliver(&awkward_failure()).await.expect("delivered");

    let requests = requests(&server).await;
    assert_eq!(
        header(&requests[0], "content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        String::from_utf8_lossy(&requests[0].body),
        "failure: certificate wildcard-example-com failed at step validate"
    );
}

// webhook-notifier, "Placeholder values are escaped for their position",
// scenario "Reason with quotation marks".
#[tokio::test]
async fn reason_with_quotation_marks_and_a_line_break_leaves_the_body_valid_json() {
    let server = server_answering(200).await;
    let sink = sink(&server, "body = '{\"text\": \"{{details}}\"}'\n");

    sink.deliver(&awkward_failure()).await.expect("delivered");

    let body = body_json(&requests(&server).await[0]);
    let text = body["text"].as_str().expect("the text");
    assert!(text.contains(AWKWARD_REASON), "{text}");
    assert!(text.contains('"') && text.contains('\n'), "{text}");
    assert_eq!(body.as_object().expect("an object").len(), 1);
}

// webhook-notifier, "Teams format", scenario "Failure posted to Teams".
#[tokio::test]
async fn teams_format_sends_a_message_with_one_adaptive_card() {
    let server = server_answering(202).await;
    let sink = sink(&server, "format = \"teams\"\n");
    let event = examples::failure();

    sink.deliver(&event).await.expect("delivered");

    let requests = requests(&server).await;
    assert_eq!(requests[0].method.as_str(), "POST");
    assert_eq!(
        header(&requests[0], "content-type"),
        Some("application/json")
    );
    let body = body_json(&requests[0]);
    assert_eq!(body, teams::message(&event));
    assert_eq!(body["type"], "message");
    let attachments = body["attachments"].as_array().expect("attachments");
    assert_eq!(attachments.len(), 1);
    assert_eq!(
        attachments[0]["contentType"],
        "application/vnd.microsoft.card.adaptive"
    );
    let card = &attachments[0]["content"];
    assert_eq!(card["type"], "AdaptiveCard");
    assert_eq!(
        card["body"][0]["text"],
        "certificate api-example-org failed at step validate"
    );
    let facts = card["body"][1]["facts"].as_array().expect("facts");
    for (name, value) in [
        ("certificate", "api-example-org"),
        ("step", "validate"),
        (
            "reason",
            "the server rejected the challenge for api.example.org: incorrect TXT record",
        ),
    ] {
        assert!(
            facts.contains(&json!({ "title": name, "value": value })),
            "{name} in {facts:?}"
        );
    }
}

// webhook-notifier, "Headers and access key", scenario "Key in a header".
#[tokio::test]
async fn headers_are_sent_with_every_request_and_may_carry_the_key() {
    let server = server_answering(200).await;
    let config = config_with_env(
        &format!(
            "[notify.console]\ntype = \"webhook\"\nurl = \"{}\"\n\n[notify.console.headers]\n\
             X-API-KEY = \"{{{{access_key}}}}\"\nX-Source = \"butterbot {{{{event}}}}\"\n",
            secret_url(&server)
        ),
        &[("BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY", KEY)],
    );
    let (_, logger) = capture();
    let sink = WebhookSink::open("console", &webhook(&config, "console"), &logger).expect("usable");

    for event in [examples::failure(), examples::run_completed()] {
        sink.deliver(&event).await.expect("delivered");
    }

    let requests = requests(&server).await;
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(header(request, "X-API-KEY"), Some(KEY));
    }
    assert_eq!(header(&requests[0], "X-Source"), Some("butterbot failure"));
    assert_eq!(
        header(&requests[1], "X-Source"),
        Some("butterbot run_completed")
    );
}

#[tokio::test]
async fn key_may_be_part_of_the_url_and_of_the_body() {
    let server = server_answering(200).await;
    let config = config_with_env(
        &format!(
            "[notify.console]\ntype = \"webhook\"\nurl = \"{}?key={{{{access_key}}}}\"\nbody = \
             '{{\"key\": \"{{{{access_key}}}}\"}}'\n",
            secret_url(&server)
        ),
        &[("BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY", "a key/with&signs")],
    );
    let (_, logger) = capture();
    let sink = WebhookSink::open("console", &webhook(&config, "console"), &logger).expect("usable");

    sink.deliver(&examples::failure()).await.expect("delivered");

    let requests = requests(&server).await;
    assert_eq!(requests[0].url.query(), Some("key=a%20key%2Fwith%26signs"));
    assert_eq!(
        body_json(&requests[0]),
        json!({ "key": "a key/with&signs" })
    );
}

// webhook-notifier, "Placeholders", scenarios "Exit code in the URL" and
// "Successful run"; "Missing and unknown placeholders", scenario "No value for
// this event".
#[tokio::test]
async fn exit_code_in_the_url_is_that_of_the_run() {
    let server = server_answering(200).await;
    let (_, logger) = capture();
    let sink = sink_for(
        &format!("{}/ping/{{{{exit_code}}}}", server.uri()),
        "method = \"GET\"\n",
        &logger,
    );

    for event in [
        // A run with one failed certificate, a run without one, and an event
        // for which the placeholder has no value.
        examples::run_completed(),
        examples::run_completed_nothing_due(),
        examples::renewed(),
    ] {
        sink.deliver(&event).await.expect("delivered");
    }

    let paths: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert_eq!(paths, ["/ping/1", "/ping/0", "/ping/"]);
}

// --- The sink on the event bus ------------------------------------------------------

/// A bus with a sink for every webhook instance of `config`, with short
/// pauses, as `register` builds it.
fn bus_with(config: &Config, logger: &Logger) -> EventBus {
    let mut bus = EventBus::new(logger.clone());
    for (name, instance) in &config.notify {
        let NotifierKind::Webhook(webhook) = &instance.kind;
        let sink = WebhookSink::open(name, webhook, logger)
            .unwrap_or_else(|error| panic!("{error}"))
            .with_first_pause(SHORT_PAUSE);
        bus.register(name, instance.events.clone(), Arc::new(sink));
    }
    bus
}

async fn emit_one_of_each(bus: &EventBus) {
    for event in [
        examples::renewed(),
        examples::failure(),
        examples::expiring(),
        examples::run_completed(),
    ] {
        bus.emit(event).await;
    }
}

// webhook-notifier, "Webhook notifier type", scenario "One notifier".
#[tokio::test]
async fn notifier_that_selects_failure_gets_one_request_for_a_failure() {
    let server = server_answering(200).await;
    let config = hook_config(&secret_url(&server), "events = [\"failure\"]\n");
    let (_, logger) = capture();

    emit_one_of_each(&bus_with(&config, &logger)).await;

    let requests = requests(&server).await;
    assert_eq!(requests.len(), 1, "one request for the one failure");
    assert_eq!(
        body_json(&requests[0]),
        Value::Object(examples::failure().to_json())
    );
}

// webhook-notifier, "Webhook notifier type", scenario "Two notifiers".
#[tokio::test]
async fn two_notifiers_that_select_the_completion_each_get_one_request() {
    let first = server_answering(200).await;
    let second = server_answering(200).await;
    let config = config(&format!(
        "[notify.first]\ntype = \"webhook\"\nurl = \"{}/first\"\nevents = [\"run_completed\"]\n\n\
         [notify.second]\ntype = \"webhook\"\nurl = \"{}/second\"\nevents = \
         [\"run_completed\", \"expiring\"]\n",
        first.uri(),
        second.uri()
    ));
    let (_, logger) = capture();

    emit_one_of_each(&bus_with(&config, &logger)).await;

    let to_first = requests(&first).await;
    assert_eq!(to_first.len(), 1);
    assert_eq!(to_first[0].url.path(), "/first");
    assert_eq!(body_json(&to_first[0])["event"], "run_completed");
    let completions_to_second: Vec<_> = requests(&second)
        .await
        .into_iter()
        .filter(|request| body_json(request)["event"] == "run_completed")
        .collect();
    assert_eq!(completions_to_second.len(), 1);
    assert_eq!(completions_to_second[0].url.path(), "/second");
}

// webhook-notifier, "Completion is delivered last": events reach the endpoint
// in the order they are emitted, and a run emits its completion last. The run
// itself is tested in `tests/cli.rs`.
#[tokio::test]
async fn events_reach_the_endpoint_in_the_order_they_are_emitted() {
    let server = server_answering(200).await;
    let config = hook_config(&secret_url(&server), "");
    let (_, logger) = capture();

    emit_one_of_each(&bus_with(&config, &logger)).await;

    let kinds: Vec<Value> = requests(&server)
        .await
        .iter()
        .map(|request| body_json(request)["event"].clone())
        .collect();
    assert_eq!(kinds, ["renewed", "failure", "expiring", "run_completed"]);
}

// --- Task 3.2: retries ---------------------------------------------------------------

/// A server that answers the first request with `first` and every later one
/// with `then`.
async fn server_answering_first(first: u16, then: u16) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(first))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(then))
        .mount(&server)
        .await;
    server
}

// webhook-notifier, "Success and retries", scenario "Endpoint fails once".
#[tokio::test]
async fn endpoint_that_fails_once_gets_a_second_request_and_nothing_is_logged_as_an_error() {
    let server = server_answering_first(503, 200).await;
    let config = hook_config(&secret_url(&server), "");
    let (capture, logger) = capture();

    bus_with(&config, &logger).emit(examples::renewed()).await;

    assert_eq!(requests(&server).await.len(), 2);
    let records = capture.json();
    assert!(
        records.iter().all(|record| record["level"] != "error"),
        "{}",
        capture.text()
    );
    let about_delivery = records_about_delivery(&capture);
    assert_eq!(about_delivery.len(), 1, "one warning for the retry");
    assert_eq!(about_delivery[0]["level"], "warn");
    assert_eq!(
        about_delivery[0]["message"],
        "delivering the renewed event to the notifier hook failed, trying again in 0s"
    );
    assert_eq!(
        about_delivery[0]["reason"],
        "the endpoint answered with status 503"
    );
    assert_nothing_secret(&capture.text(), &secret_url(&server));
}

#[tokio::test]
async fn status_429_is_tried_again() {
    let server = server_answering_first(429, 204).await;

    sink(&server, "")
        .deliver(&examples::failure())
        .await
        .expect("delivered by the second attempt");

    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn endpoint_that_always_fails_is_given_up_after_three_attempts() {
    let server = server_answering(500).await;

    let error = sink(&server, "")
        .deliver(&examples::failure())
        .await
        .expect_err("not delivered");

    assert_eq!(requests(&server).await.len(), 3);
    assert_eq!(error.to_string(), "the endpoint answered with status 500");
    assert_eq!(crate::retry::MAX_ATTEMPTS, 3);
}

// webhook-notifier, "Success and retries", scenario "Request rejected".
#[tokio::test]
async fn rejected_request_fails_after_one_attempt_and_is_logged() {
    let server = server_answering(400).await;
    let config = hook_config(&secret_url(&server), "");
    let (capture, logger) = capture();

    bus_with(&config, &logger).emit(examples::renewed()).await;

    assert_eq!(requests(&server).await.len(), 1, "no second attempt");
    let about_delivery = records_about_delivery(&capture);
    assert_eq!(about_delivery.len(), 1);
    assert_eq!(about_delivery[0]["level"], "error");
    assert_eq!(about_delivery[0]["notifier"], "hook");
    assert_eq!(about_delivery[0]["undelivered_event"], "renewed");
    assert_eq!(
        about_delivery[0]["reason"],
        "the endpoint answered with status 400"
    );
}

#[tokio::test]
async fn every_status_outside_2xx_5xx_and_429_fails_at_once() {
    for status in [301, 304, 401, 403, 404, 410, 418] {
        let server = server_answering(status).await;
        let error = sink(&server, "")
            .deliver(&examples::failure())
            .await
            .expect_err("not delivered");
        assert_eq!(requests(&server).await.len(), 1, "status {status}");
        assert!(
            error
                .to_string()
                .starts_with(&format!("the endpoint answered with status {status}")),
            "{error}"
        );
        assert!(!error.is_transient());
    }
    for status in [200, 201, 202, 204] {
        let server = server_answering(status).await;
        sink(&server, "")
            .deliver(&examples::failure())
            .await
            .unwrap_or_else(|error| panic!("status {status}: {error}"));
    }
}

// webhook-notifier, "Redirects are not followed", scenario "Endpoint redirects".
#[tokio::test]
async fn redirect_is_not_followed_and_fails_the_delivery() {
    let elsewhere = server_answering(200).await;
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/moved", elsewhere.uri()).as_str()),
        )
        .mount(&server)
        .await;
    let (_, logger) = capture();
    let sink = sink_for(
        &secret_url(&server),
        &format!(
            "access_key = \"{KEY}\"\n\n[notify.hook.headers]\nX-API-KEY = \"{{{{access_key}}}}\"\n"
        ),
        &logger,
    );

    let error = sink
        .deliver(&examples::failure())
        .await
        .expect_err("not delivered");

    assert_eq!(
        error.to_string(),
        "the endpoint answered with status 302; redirects are not followed"
    );
    assert_eq!(requests(&server).await.len(), 1, "no second attempt");
    assert!(
        requests(&elsewhere).await.is_empty(),
        "nothing was sent to the location"
    );
}

/// The address of a port on which nothing listens.
fn closed_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("its address")
}

#[tokio::test]
async fn network_error_is_tried_again() {
    let url = format!("http://{}{SECRET_PATH}", closed_port());
    let config = hook_config(&url, "");
    let (capture, logger) = capture();

    bus_with(&config, &logger).emit(examples::renewed()).await;

    let about_delivery = records_about_delivery(&capture);
    assert_eq!(about_delivery.len(), 3, "{}", capture.text());
    assert_eq!(about_delivery[0]["level"], "warn");
    assert_eq!(about_delivery[0]["attempt"], 1);
    assert_eq!(about_delivery[1]["level"], "warn");
    assert_eq!(about_delivery[1]["attempt"], 2);
    assert_eq!(about_delivery[2]["level"], "error");
    for record in &about_delivery {
        assert_eq!(record["reason"], "the connection failed");
    }
    assert_nothing_secret(&capture.text(), &url);
}

// webhook-notifier, "Success and retries": each attempt is limited by `timeout`.
#[tokio::test]
async fn attempt_is_limited_by_the_timeout_of_the_instance() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;
    let config = hook_config(&secret_url(&server), "timeout = \"1s\"\n");
    let mut settings = webhook(&config, "hook");
    assert_eq!(settings.timeout, Duration::from_secs(1));
    // Shorter than the configuration allows, to keep the test quick.
    settings.timeout = Duration::from_millis(100);
    let (capture, logger) = capture();
    let sink = WebhookSink::open("hook", &settings, &logger)
        .expect("usable")
        .with_first_pause(SHORT_PAUSE);

    let started = std::time::Instant::now();
    let error = sink
        .deliver(&examples::failure())
        .await
        .expect_err("no answer in time");

    assert!(error.is_transient(), "a timeout is tried again");
    assert_eq!(error.to_string(), "the endpoint did not answer within 0s");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "three short attempts"
    );
    assert_eq!(capture.json().len(), 2, "two retries were logged");
    assert_nothing_secret(&capture.text(), &secret_url(&server));
}

#[test]
fn default_timeout_is_ten_seconds() {
    let config = hook_config("https://hooks.example.com/a", "");
    assert_eq!(webhook(&config, "hook").timeout, Duration::from_secs(10));
}

// --- Task 3.3: nothing secret in the log ---------------------------------------------

/// Emits a failure to the instance `hook` of `config` through a bus and
/// returns what was logged about the delivery.
async fn log_of_a_failed_delivery(config: &Config) -> (Capture, Vec<Map<String, Value>>) {
    let (capture, logger) = capture();
    bus_with(config, &logger).emit(examples::failure()).await;
    let about_delivery = records_about_delivery(&capture);
    (capture, about_delivery)
}

const WITH_KEY: &str = "access_key = \"console-key-1\"\n\n[notify.hook.headers]\nX-API-KEY = \
                        \"{{access_key}}\"\n";

// webhook-notifier, "Nothing secret in the output", scenario "Failed delivery".
#[tokio::test]
async fn failed_delivery_is_logged_with_the_notifier_the_kind_and_the_status() {
    let server = server_answering(404).await;
    let url = secret_url(&server);
    let config = hook_config(&url, WITH_KEY);

    let (capture, records) = log_of_a_failed_delivery(&config).await;

    assert_eq!(records.len(), 1, "{}", capture.text());
    assert_eq!(records[0]["level"], "error");
    assert_eq!(
        records[0]["message"],
        "an event could not be delivered to a notifier"
    );
    assert_eq!(records[0]["notifier"], "hook");
    assert_eq!(records[0]["undelivered_event"], "failure");
    assert_eq!(
        records[0]["reason"],
        "the endpoint answered with status 404"
    );
    assert_nothing_secret(&capture.text(), &url);
    assert_eq!(
        header(&requests(&server).await[0], "X-API-KEY"),
        Some(KEY),
        "the key was sent"
    );
}

// webhook-notifier, "Nothing secret in the output", scenario "Connection error".
#[tokio::test]
async fn host_that_cannot_be_resolved_is_logged_as_a_failed_connection() {
    let url = format!("https://butterbot-webhook-test.invalid{SECRET_PATH}");
    let config = hook_config(&url, WITH_KEY);

    let (capture, records) = log_of_a_failed_delivery(&config).await;

    let last = records.last().expect("the failure is logged");
    assert_eq!(last["level"], "error");
    assert_eq!(last["notifier"], "hook");
    assert_eq!(last["undelivered_event"], "failure");
    assert_eq!(last["reason"], "the connection failed");
    assert_nothing_secret(&capture.text(), &url);
    assert!(
        !capture.text().contains("butterbot-webhook-test"),
        "{}",
        capture.text()
    );
}

#[tokio::test]
async fn no_failure_path_logs_the_url_or_the_key() {
    // A server error, with its retries.
    let server = server_answering(500).await;
    let url = secret_url(&server);
    let (capture, records) = log_of_a_failed_delivery(&hook_config(&url, WITH_KEY)).await;
    assert_eq!(records.len(), 3, "two retries and the failure");
    assert_nothing_secret(&capture.text(), &url);

    // A redirect.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(SECRET_PATH))
        .respond_with(
            ResponseTemplate::new(307).insert_header("Location", "https://elsewhere.example.com/"),
        )
        .mount(&server)
        .await;
    let url = secret_url(&server);
    let (capture, records) = log_of_a_failed_delivery(&hook_config(&url, WITH_KEY)).await;
    assert_eq!(
        records[0]["reason"],
        "the endpoint answered with status 307; redirects are not followed"
    );
    assert_nothing_secret(&capture.text(), &url);

    // A connection that is refused.
    let url = format!("http://{}{SECRET_PATH}", closed_port());
    let (capture, records) = log_of_a_failed_delivery(&hook_config(&url, WITH_KEY)).await;
    assert_eq!(records.len(), 3);
    assert_nothing_secret(&capture.text(), &url);

    // A URL that is not one.
    let url = format!("https://hooks example com{SECRET_PATH}");
    let (capture, records) = log_of_a_failed_delivery(&hook_config(&url, WITH_KEY)).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["reason"], "notify.hook.url is not a valid URL");
    assert_nothing_secret(&capture.text(), &url);
    assert!(!capture.text().contains("hooks example"));
}

// Design D2: a header value with a control character makes the delivery fail.
#[tokio::test]
async fn header_value_with_a_line_break_fails_the_delivery_without_a_request() {
    let server = server_answering(200).await;
    let url = secret_url(&server);
    let config = hook_config(
        &url,
        &format!(
            "access_key = \"{KEY}\"\n\n[notify.hook.headers]\nX-Details = \"{{{{access_key}}}} \
             {{{{details}}}}\"\n"
        ),
    );

    let (capture, records) = log_of_a_failed_delivery(&config).await;

    assert!(requests(&server).await.is_empty(), "nothing was sent");
    assert_eq!(records.len(), 1);
    assert!(
        records[0]["reason"]
            .as_str()
            .expect("a reason")
            .starts_with("the value of the header x-details cannot be sent"),
        "{:?}",
        records[0]["reason"]
    );
    assert_nothing_secret(&capture.text(), &url);
}

#[test]
fn error_of_a_sink_that_cannot_be_opened_shows_no_value() {
    let (_, logger) = capture();
    let url = format!("ftp://files.example.com{SECRET_PATH}");
    let config = hook_config(&url, WITH_KEY);

    let error = WebhookSink::open("hook", &webhook(&config, "hook"), &logger)
        .err()
        .expect("not usable");

    assert_nothing_secret(&error.to_string(), &url);
}

// --- Task 3.4: additional trusted certificates ----------------------------------------

/// An HTTPS endpoint whose certificate is issued by an authority made for the
/// test. It answers every request with status 200.
struct PrivateEndpoint {
    address: SocketAddr,
    /// The certificate of the authority, as PEM.
    authority: String,
    requests: Arc<AtomicUsize>,
}

impl PrivateEndpoint {
    fn start() -> Self {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
        use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

        let authority_key = KeyPair::generate().expect("a key");
        let mut authority = CertificateParams::new(Vec::<String>::new()).expect("parameters");
        authority.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        authority
            .distinguished_name
            .push(DnType::CommonName, "Butterbot Webhook Test Authority");
        let authority_certificate = authority
            .self_signed(&authority_key)
            .expect("the certificate of the authority");
        let key = KeyPair::generate().expect("a key");
        let certificate =
            CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
                .expect("parameters")
                .signed_by(&key, &Issuer::from_params(&authority, &authority_key))
                .expect("the certificate of the endpoint");

        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .expect("a usable certificate");
        let tls = Arc::new(tls);

        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = listener.local_addr().expect("its address");
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        // The thread ends with the test process.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let (Ok(stream), Ok(connection)) =
                    (stream, rustls::ServerConnection::new(tls.clone()))
                else {
                    continue;
                };
                let mut stream = rustls::StreamOwned::new(connection, stream);
                // A client that does not trust the certificate ends the
                // handshake, and reading fails.
                if read_request(&mut stream).is_some() {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    );
                    stream.conn.send_close_notify();
                    let _ = stream.flush();
                }
            }
        });
        Self {
            address,
            authority: authority_certificate.pem(),
            requests,
        }
    }

    fn url(&self) -> String {
        format!("https://{}{SECRET_PATH}", self.address)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// Reads one HTTP request with its body. `None` when the connection ends
/// before the request is complete.
fn read_request(stream: &mut impl Read) -> Option<Vec<u8>> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let end_of_head = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4);
        if let Some(end_of_head) = end_of_head {
            let head = String::from_utf8_lossy(&request[..end_of_head]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            if request.len() >= end_of_head + length {
                return Some(request);
            }
        }
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return None,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
    }
}

// webhook-notifier, "Additional trusted certificates", scenario "Endpoint with
// a private certificate authority".
#[tokio::test]
async fn endpoint_with_a_private_authority_is_trusted_with_its_ca_file() {
    let endpoint = PrivateEndpoint::start();
    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let ca_file = directory.path().join("authority.pem");
    std::fs::write(&ca_file, &endpoint.authority).expect("the file of the authority");
    let (_, logger) = capture();
    let sink = sink_for(
        &endpoint.url(),
        &format!("ca_file = \"{}\"\n", ca_file.display()),
        &logger,
    );

    sink.deliver(&examples::failure()).await.expect("delivered");

    assert_eq!(endpoint.requests(), 1);
}

// webhook-notifier, "Additional trusted certificates", scenario "Untrusted endpoint".
#[tokio::test]
async fn endpoint_with_a_private_authority_is_not_trusted_without_its_ca_file() {
    let endpoint = PrivateEndpoint::start();
    let url = endpoint.url();
    let config = hook_config(&url, WITH_KEY);

    let (capture, records) = log_of_a_failed_delivery(&config).await;

    assert_eq!(endpoint.requests(), 0, "no request reached the endpoint");
    assert_eq!(records.len(), 1, "{}", capture.text());
    assert_eq!(records[0]["level"], "error");
    assert_eq!(records[0]["notifier"], "hook");
    assert_eq!(records[0]["undelivered_event"], "failure");
    assert_eq!(
        records[0]["reason"],
        "the connection failed: the certificate of the endpoint was not accepted"
    );
    assert_nothing_secret(&capture.text(), &url);
}

// "The system SHALL NOT offer a way to switch certificate verification off."
#[test]
fn there_is_no_setting_that_switches_verification_off() {
    for setting in [
        "insecure = true",
        "verify = false",
        "tls_verify = false",
        "skip_verify = true",
        "danger_accept_invalid_certs = true",
    ] {
        let file = Layer::from_toml(
            "test.toml",
            &format!("[notify.hook]\ntype = \"webhook\"\n{setting}\n"),
        )
        .expect("valid TOML");
        let error = Config::from_layers([file]).expect_err("an unknown key");
        assert!(error.problems[0].contains("unknown key"), "{error}");
    }
}

#[test]
fn ca_file_that_is_not_usable_makes_the_instance_unusable() {
    let (_, logger) = capture();
    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let not_pem = directory.path().join("not-pem.txt");
    std::fs::write(&not_pem, "not a certificate").expect("the file");
    let missing = directory.path().join("missing.pem");

    for (file, expected) in [
        (&missing, "cannot be read"),
        (&not_pem, "notify.hook.ca_file: a root certificate"),
    ] {
        let config = hook_config(
            "https://hooks.example.com/a",
            &format!("ca_file = \"{}\"\n", file.display()),
        );
        let error = WebhookSink::open("hook", &webhook(&config, "hook"), &logger)
            .err()
            .expect("not usable");
        assert!(error.to_string().contains(expected), "{error}");
        assert!(error.to_string().contains("notify.hook.ca_file"), "{error}");
    }
}

// --- Task 3.5: a sink per configured instance -----------------------------------------

/// What `register` logs for `config`, and the bus it filled.
fn registered(config: &Config) -> (Capture, EventBus) {
    let (capture, logger) = capture();
    let mut bus = EventBus::new(logger.clone());
    register(config, &mut bus, &logger);
    (capture, bus)
}

// webhook-notifier, "Target URL is a secret", scenario "URL from the vault".
#[tokio::test]
async fn url_from_the_vault_is_where_the_notifier_delivers() {
    let server = server_answering(200).await;
    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let file = directory.path().join("butterbot.toml");
    std::fs::write(
        &file,
        "[secrets]\nkey_vault_url = \"https://butterbot-config.vault.example.com/\"\n\n\
         [notify.teams]\ntype = \"webhook\"\nformat = \"teams\"\n",
    )
    .expect("the configuration file");
    let vault = Arc::new(MemoryVault::new());
    vault
        .set("notify--teams--url", &Secret::new(secret_url(&server)))
        .await
        .expect("the secret is stored");
    let (_, logger) = capture();

    let connected: Arc<dyn SecretVault> = vault.clone();
    let loaded = crate::config::load(&logger, &[file], Vec::new(), |_| Ok(connected))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let (capture, bus) = registered(&loaded.config);
    bus.emit(examples::failure()).await;

    let requests = requests(&server).await;
    assert_eq!(requests.len(), 1, "{}", capture.text());
    assert_eq!(requests[0].url.path(), SECRET_PATH);
    assert_eq!(
        body_json(&requests[0]),
        teams::message(&examples::failure())
    );
}

// webhook-notifier, "Target URL is a secret", scenario "Unsupported scheme".
#[tokio::test]
async fn url_with_an_unsupported_scheme_is_reported_and_the_notifier_gets_no_events() {
    let url = format!("ftp://files.example.com{SECRET_PATH}");
    let config = hook_config(&url, "");

    let (capture, bus) = registered(&config);
    emit_one_of_each(&bus).await;

    let records = records_about_delivery(&capture);
    assert_eq!(records.len(), 1, "{}", capture.text());
    assert_eq!(records[0]["level"], "error");
    assert_eq!(
        records[0]["message"],
        "a notifier is misconfigured and receives no events"
    );
    assert_eq!(records[0]["notifier"], "hook");
    assert_eq!(
        records[0]["reason"],
        "notify.hook.url has an unsupported scheme: the URL of a webhook starts with https:// \
         or http://"
    );
    assert_nothing_secret(&capture.text(), &url);
    assert!(!capture.text().contains("ftp"), "{}", capture.text());
}

#[test]
fn scheme_is_judged_without_regard_to_case_and_has_to_be_written_out() {
    let (_, logger) = capture();
    let usable = |url: &str| {
        let config = hook_config(url, "");
        WebhookSink::open("hook", &webhook(&config, "hook"), &logger).is_ok()
    };
    for url in [
        "https://hooks.example.com/a",
        "HTTPS://hooks.example.com/a",
        "http://hooks.example.com/a",
        "https://hooks.example.com/{{event}}",
    ] {
        assert!(usable(url), "{url}");
    }
    for url in [
        "ftp://hooks.example.com/a",
        "file:///etc/passwd",
        "hooks.example.com/a",
        "httpx://hooks.example.com/a",
        "{{event}}://hooks.example.com/a",
        "https:/hooks.example.com",
    ] {
        assert!(!usable(url), "{url}");
    }
}

// webhook-notifier, "Target URL is a secret": an `http` URL is accepted, with a
// warning that the request is not encrypted.
#[tokio::test]
async fn http_url_is_accepted_with_a_warning() {
    let server = server_answering(200).await;
    let url = secret_url(&server);
    assert!(url.starts_with("http://"));
    let config = hook_config(&url, "");

    let (capture, bus) = registered(&config);
    bus.emit(examples::failure()).await;

    assert_eq!(requests(&server).await.len(), 1, "the notifier delivers");
    let records = records_about_delivery(&capture);
    assert_eq!(records.len(), 1, "{}", capture.text());
    assert_eq!(records[0]["level"], "warn");
    assert_eq!(
        records[0]["message"],
        "the URL of a notifier uses http: its requests are not encrypted"
    );
    assert_eq!(records[0]["notifier"], "hook");
    assert_nothing_secret(&capture.text(), &url);
}

#[test]
fn https_url_is_accepted_without_a_warning() {
    let config = hook_config("https://hooks.example.com/a", "");
    let (capture, _) = registered(&config);
    assert!(capture.lines().is_empty(), "{}", capture.text());
}

// webhook-notifier, "Notifier without a URL is skipped", scenario "Run with a
// missing URL": the log names the path, and nothing else changes. That the
// certificates are processed and the exit code is theirs is tested in
// `tests/cli.rs`.
#[tokio::test]
async fn notifier_without_a_url_is_reported_with_its_path_and_skipped() {
    let server = server_answering(200).await;
    let config = config(&format!(
        "[notify.teams]\ntype = \"webhook\"\nformat = \"teams\"\n\n[notify.other]\ntype = \
         \"webhook\"\nurl = \"{}\"\n",
        secret_url(&server)
    ));

    let (capture, bus) = registered(&config);
    emit_one_of_each(&bus).await;

    let errors: Vec<_> = records_about_delivery(&capture)
        .into_iter()
        .filter(|record| record["level"] == "error")
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "one error, at the start: {}",
        capture.text()
    );
    assert_eq!(errors[0]["notifier"], "teams");
    assert_eq!(
        errors[0]["reason"],
        "notify.teams.url is not set; supply it as the environment variable \
         BUTTERBOT__NOTIFY__TEAMS__URL or as the Key Vault secret notify--teams--url"
    );
    let all = capture.json();
    let error_at = all
        .iter()
        .position(|record| record["level"] == "error" && record["notifier"] == "teams")
        .expect("the error is logged");
    let first_event_at = all
        .iter()
        .position(|record| record.contains_key("event"))
        .expect("the events are logged");
    assert!(
        error_at < first_event_at,
        "the error is logged at the start, before any event"
    );
    assert_eq!(
        requests(&server).await.len(),
        4,
        "the other notifier gets every event"
    );
}

// webhook-notifier, "Headers and access key", scenario "Key referenced but
// missing": the instance is treated like a notifier without a URL.
#[tokio::test]
async fn notifier_whose_referenced_key_is_missing_is_reported_and_skipped() {
    let server = server_answering(200).await;
    let config = config(&format!(
        "[notify.console]\ntype = \"webhook\"\nurl = \"{}\"\n\n[notify.console.headers]\n\
         X-API-KEY = \"{{{{access_key}}}}\"\n",
        secret_url(&server)
    ));

    let (capture, bus) = registered(&config);
    emit_one_of_each(&bus).await;

    assert!(requests(&server).await.is_empty(), "no event is delivered");
    let records = records_about_delivery(&capture);
    assert_eq!(records.len(), 1, "{}", capture.text());
    assert_eq!(records[0]["level"], "error");
    assert_eq!(records[0]["notifier"], "console");
    assert_eq!(
        records[0]["reason"],
        "notify.console.access_key is not set, and a template of the notifier uses \
         {{access_key}}; supply it as the environment variable \
         BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY or as the Key Vault secret \
         notify--console--access-key"
    );
    assert_nothing_secret(&capture.text(), &secret_url(&server));
}

#[tokio::test]
async fn key_that_no_template_uses_is_not_needed_and_not_sent() {
    let server = server_answering(200).await;
    let (_, logger) = capture();
    let sink = sink_for(&secret_url(&server), "", &logger);

    sink.deliver(&examples::failure()).await.expect("delivered");

    let requests = requests(&server).await;
    assert!(
        !String::from_utf8_lossy(&requests[0].body).contains(KEY)
            && header(&requests[0], "X-API-KEY").is_none()
    );
}

// run-events, "Notifier extension point", scenario "Notifier with a filter",
// with the notifier type of the program.
#[tokio::test]
async fn registered_notifier_receives_only_the_kinds_it_selected() {
    let server = server_answering(200).await;
    let config = hook_config(
        &secret_url(&server),
        "events = [\"failure\", \"expiring\"]\n",
    );

    let (_, bus) = registered(&config);
    emit_one_of_each(&bus).await;

    let kinds: Vec<Value> = requests(&server)
        .await
        .iter()
        .map(|request| body_json(request)["event"].clone())
        .collect();
    assert_eq!(kinds, ["failure", "expiring"]);
    assert!(config.notify["hook"].events.includes(EventKind::Failure));
}

// run-events, "Notifier extension point", scenario "No notifier configured".
#[tokio::test]
async fn configuration_without_a_notifier_registers_nothing() {
    let config = config("");

    let (capture, bus) = registered(&config);
    bus.emit(examples::run_completed()).await;

    let records = capture.json();
    assert_eq!(records.len(), 1, "the event in the log and nothing else");
    assert_eq!(records[0]["event"], "run_completed");
}

/// Task 6.2 of `add-webhook-notifier`: `docs/notifications.md` agrees with the
/// program.
mod docs {
    use super::*;
    use crate::config::WebhookBody;
    use crate::events::Selection;

    const DOCUMENT: &str = include_str!("../../docs/notifications.md");

    /// The content of the fenced blocks with the given language, in order.
    fn blocks(language: &str) -> Vec<&'static str> {
        let opening = format!("```{language}\n");
        DOCUMENT
            .split(opening.as_str())
            .skip(1)
            .map(|rest| rest.split("\n```").next().expect("the block is closed"))
            .collect()
    }

    /// The configuration of each example, with the name of its instance.
    fn examples() -> Vec<(String, Config)> {
        blocks("toml")
            .into_iter()
            .map(|text| {
                let file = Layer::from_toml("notifications.md", text)
                    .unwrap_or_else(|error| panic!("{error}:\n{text}"));
                let config =
                    Config::from_layers([file]).unwrap_or_else(|error| panic!("{error}:\n{text}"));
                assert_eq!(config.notify.len(), 1, "one instance per example:\n{text}");
                let name = config.notify.keys().next().expect("an instance").clone();
                (name, config)
            })
            .collect()
    }

    fn example(name: &str) -> Config {
        examples()
            .into_iter()
            .find(|(instance, _)| instance == name)
            .unwrap_or_else(|| panic!("the document has an example called {name}"))
            .1
    }

    #[test]
    fn every_configuration_example_loads_without_errors() {
        let names: Vec<String> = examples().into_iter().map(|(name, _)| name).collect();
        assert_eq!(names, ["teams", "monitor", "console", "archive"]);
    }

    #[test]
    fn every_example_keeps_its_secrets_out_of_the_file_and_names_them() {
        for (name, config) in examples() {
            let secrets = config.expected_secrets();
            assert!(!secrets.is_empty(), "{name} has a URL");
            for secret in secrets {
                assert!(!secret.found, "{} is not in the example", secret.path);
                if name != "archive" {
                    assert!(DOCUMENT.contains(&secret.path), "{}", secret.path);
                }
                if name == "teams" || name == "console" {
                    assert!(DOCUMENT.contains(&secret.env_name), "{}", secret.env_name);
                    assert!(
                        DOCUMENT.contains(&secret.vault_name),
                        "{}",
                        secret.vault_name
                    );
                }
            }
        }
    }

    #[test]
    fn teams_example_uses_the_teams_format_and_selects_failures() {
        let config = example("teams");
        assert_eq!(webhook(&config, "teams").body, WebhookBody::Teams);
        assert_eq!(
            config.notify["teams"].events,
            Selection::only([EventKind::Failure, EventKind::Expiring])
        );
        // The heading the walk-through says the test card has.
        let sample = Event::sample(EventKind::Failure, datetime!(2026-11-24 10:00:00 UTC));
        assert_eq!(
            teams::message(&sample)["attachments"][0]["content"]["body"][0]["text"],
            "certificate butterbot-test failed at step validate"
        );
        assert!(DOCUMENT.contains("\"certificate butterbot-test failed at step validate\""));
    }

    #[tokio::test]
    async fn monitor_example_calls_the_url_that_ends_in_the_exit_code() {
        let config = example("monitor");
        assert_eq!(webhook(&config, "monitor").method, WebhookMethod::Get);
        assert_eq!(
            config.notify["monitor"].events,
            Selection::only([EventKind::RunCompleted])
        );

        // The URL of the document, pointed at a local server.
        let documented = blocks("text")
            .into_iter()
            .find(|block| block.contains("{{exit_code}}"))
            .expect("the document shows the URL");
        assert_eq!(
            documented,
            "https://monitor.example.com/ping/YOUR-CHECK-ID/{{exit_code}}"
        );
        let server = server_answering(200).await;
        let url = documented.replace("https://monitor.example.com", &server.uri());
        let mut settings = webhook(&config, "monitor");
        settings.url = Some(Template::parse(&url).expect("a template"));
        let (_, logger) = capture();
        let sink = WebhookSink::open("monitor", &settings, &logger).expect("usable");

        for event in [
            examples::run_completed(),
            Event::sample(EventKind::RunCompleted, datetime!(2026-11-24 10:00:00 UTC)),
        ] {
            sink.deliver(&event).await.expect("delivered");
        }

        let requests = requests(&server).await;
        assert_eq!(requests[0].method.as_str(), "GET");
        assert_eq!(requests[0].url.path(), "/ping/YOUR-CHECK-ID/1");
        assert_eq!(
            requests[1].url.path(),
            "/ping/YOUR-CHECK-ID/0",
            "the sample of `notify test` is a successful run"
        );
    }

    #[tokio::test]
    async fn console_example_sends_its_body_and_the_key_in_the_header() {
        let server = server_answering(200).await;
        let mut settings = webhook(&example("console"), "console");
        assert!(settings.uses_access_key());
        assert!(settings.ca_file.is_some());
        settings.url = Some(Template::parse(&secret_url(&server)).expect("a template"));
        settings.access_key = Some(Secret::new(KEY));
        // The file of the example does not exist here, and the local server
        // needs none.
        settings.ca_file = None;
        let (_, logger) = capture();
        let sink = WebhookSink::open("console", &settings, &logger).expect("usable");

        sink.deliver(&awkward_failure()).await.expect("delivered");

        let requests = requests(&server).await;
        assert_eq!(header(&requests[0], "X-API-KEY"), Some(KEY));
        let body = body_json(&requests[0]);
        assert_eq!(
            body["title"],
            "certificate wildcard-example-com failed at step validate"
        );
        assert!(
            body["text"]
                .as_str()
                .is_some_and(|text| text.contains(AWKWARD_REASON))
        );
    }

    #[test]
    fn body_with_the_whole_event_is_a_valid_template() {
        let documented = r#"'{"source": "butterbot", "event": {{event_json}}}'"#;
        assert!(DOCUMENT.contains(documented));
        let config = hook_config(
            "https://hooks.example.com/a",
            &format!("body = {documented}\n"),
        );
        let WebhookBody::Custom { template, .. } = webhook(&config, "hook").body else {
            panic!("a custom body");
        };
        let rendered = template.render(Position::JsonBody, &Values::of(&examples::failure(), ""));
        let parsed: Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(
            parsed["event"],
            Value::Object(examples::failure().to_json())
        );
    }

    #[test]
    fn json_example_is_the_default_body_of_a_failure() {
        let documented = blocks("json");
        assert_eq!(documented.len(), 1);
        let shown: Map<String, Value> = serde_json::from_str(documented[0]).expect("valid JSON");
        let actual = examples::failure().to_json();
        assert_eq!(shown, actual);
        assert_eq!(
            shown.keys().collect::<Vec<_>>(),
            actual.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            webhook(&example("archive"), "archive").body,
            WebhookBody::Json
        );
    }

    /// The lines of the `text` blocks that are log records.
    fn documented_records() -> Vec<&'static str> {
        blocks("text")
            .into_iter()
            .flat_map(str::lines)
            .filter(|line| line.starts_with('{'))
            .collect()
    }

    #[tokio::test]
    async fn log_record_of_a_failed_delivery_is_what_the_program_writes() {
        let server = server_answering(404).await;
        let config = config(&format!(
            "[notify.teams]\ntype = \"webhook\"\nurl = \"{}\"\n",
            secret_url(&server)
        ));
        let (capture, logger) = capture();

        bus_with(&config, &logger).emit(examples::failure()).await;

        let written = capture.lines();
        assert_eq!(written.len(), 2, "the event and the failed delivery");
        assert_eq!(documented_records()[0], written[1]);
    }

    #[test]
    fn log_record_of_a_misconfigured_notifier_is_what_the_program_writes() {
        let config = config("[notify.teams]\ntype = \"webhook\"\n");
        let capture = Capture::new();
        let logger = capture.logger(
            LogFormat::Json,
            Level::Info,
            datetime!(2026-11-23 02:00:00 UTC),
        );
        let mut bus = EventBus::new(logger.clone());

        register(&config, &mut bus, &logger);

        assert_eq!(capture.lines(), [documented_records()[1]]);
    }

    #[test]
    fn documented_reasons_are_the_ones_the_sink_gives() {
        for reason in [
            "the endpoint answered with status 4xx",
            "the endpoint answered with status 3xx; redirects are not followed",
            "the endpoint answered with status 5xx",
            "the connection failed",
            "the connection failed: the certificate of the endpoint was not accepted",
            "the endpoint did not answer within 10s",
        ] {
            assert!(DOCUMENT.contains(&format!("| `{reason}`")), "{reason}");
        }
        assert_eq!(
            answered(reqwest::StatusCode::NOT_FOUND),
            "the endpoint answered with status 404"
        );
        assert_eq!(
            crate::config::format_duration(defaults::WEBHOOK_TIMEOUT),
            "10s"
        );
    }

    #[test]
    fn documented_secret_names_follow_the_rules() {
        for (path, environment, vault) in [
            (
                "notify.teams.url",
                "BUTTERBOT__NOTIFY__TEAMS__URL",
                "notify--teams--url",
            ),
            (
                "notify.console.url",
                "BUTTERBOT__NOTIFY__CONSOLE__URL",
                "notify--console--url",
            ),
            (
                "notify.console.access_key",
                "BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY",
                "notify--console--access-key",
            ),
        ] {
            assert_eq!(env_name(path), environment);
            assert_eq!(vault_name(path), vault);
            assert!(DOCUMENT.contains(environment) && DOCUMENT.contains(vault));
        }
    }
}
