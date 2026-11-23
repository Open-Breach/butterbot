//! The ACME client against the ACME test server (Pebble). These tests are
//! ignored unless the test servers are installed; see `src/testing.rs`.

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::testing::{AcmeTestServer, PebbleOptions};

mod account {
    use super::*;

    // acme-issuance, "Account reuse", scenario "Existing account" (task 7.2: a
    // second start reuses the account).
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn account_is_reused_from_its_credentials() {
        let server = AcmeTestServer::start();
        let (http, url) = (server.pebble.http(), server.pebble.directory_url.as_str());

        let (registered, credentials) = AcmeClient::register("pebble", url, None, &http)
            .await
            .expect("registered");
        assert!(
            registered
                .account_id()
                .starts_with(&format!("https://{}/", server.pebble.address))
        );

        // A "second start": nothing is kept but the serialised credentials.
        let credentials = Secret::new(credentials.expose().to_owned());
        let second = AcmeClient::with_account("pebble", url, &credentials, &http)
            .await
            .expect("the account is loaded");
        let third = AcmeClient::with_account("pebble", url, &credentials, &http)
            .await
            .expect("the account is loaded");

        assert_eq!(
            second.account_id(),
            registered.account_id(),
            "the same account, not a new one"
        );
        assert_eq!(third.account_id(), registered.account_id());
        assert_eq!(second.server(), "pebble");
    }

    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn each_registration_is_a_new_account() {
        let server = AcmeTestServer::start();
        let (http, url) = (server.pebble.http(), server.pebble.directory_url.as_str());
        let (first, _) = AcmeClient::register("pebble", url, None, &http)
            .await
            .expect("registered");
        let (second, _) = AcmeClient::register("pebble", url, None, &http)
            .await
            .expect("registered");
        assert_ne!(first.account_id(), second.account_id());
    }

    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn serialised_credentials_are_json_that_names_the_server() {
        let server = AcmeTestServer::start();
        let (http, url) = (server.pebble.http(), server.pebble.directory_url.as_str());
        let (_, credentials) = AcmeClient::register("pebble", url, None, &http)
            .await
            .expect("registered");

        let value: serde_json::Value = serde_json::from_str(credentials.expose()).expect("JSON");
        assert_eq!(value["directory"], url);
        assert!(
            value["id"].is_string() && value["key_pkcs8"].is_string(),
            "{:?}",
            value
                .as_object()
                .map(|object| object.keys().collect::<Vec<_>>())
        );
        assert!(
            !format!("{credentials:?}").contains("key_pkcs8"),
            "the credentials are a secret"
        );
    }

    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn credentials_of_another_server_are_refused() {
        let server = AcmeTestServer::start();
        let other = AcmeTestServer::start();
        let http = server.pebble.http();
        let (_, credentials) = AcmeClient::register(
            "staging",
            &other.pebble.directory_url,
            None,
            &other.pebble.http(),
        )
        .await
        .expect("registered at the other server");

        let error = AcmeClient::with_account(
            "production",
            &server.pebble.directory_url,
            &credentials,
            &http,
        )
        .await
        .err()
        .expect("the credentials belong to the other server");

        assert_eq!(error.class(), "permanent");
        assert_eq!(
            error.to_string(),
            format!(
                "the account credentials in acme.production.access_key are not usable: they belong \
                 to the server {}, not to {}",
                other.pebble.directory_url, server.pebble.directory_url
            )
        );
    }

    #[tokio::test]
    async fn credentials_that_are_not_credentials_are_refused_without_being_shown() {
        let http = crate::http::client().expect("an HTTP client");
        for text in [
            "hunter2-is-not-json",
            r#"{"hunter2": true}"#,
            r#"["hunter2"]"#,
            "",
        ] {
            let error = AcmeClient::with_account(
                "letsencrypt",
                "https://acme.example.com/directory",
                &Secret::new(text),
                &http,
            )
            .await
            .err()
            .expect("unusable");
            assert_eq!(error.class(), "permanent", "{text}");
            let message = error.to_string();
            assert!(
                message.starts_with(
                    "the account credentials in acme.letsencrypt.access_key are not usable"
                ),
                "{message}"
            );
            assert!(!message.contains("hunter2"), "{message}");
        }
    }

    // acme-issuance, "Any ACME server by configuration", scenario "Directory unreachable".
    #[tokio::test]
    async fn unreachable_directory_is_transient_and_names_the_server() {
        let address = crate::testing::free_address();
        let http = crate::http::client().expect("an HTTP client");
        let url = format!("http://{address}/directory");

        let error = AcmeClient::register("letsencrypt-staging", &url, None, &http)
            .await
            .err()
            .expect("nothing listens there");

        assert!(error.is_transient(), "{error:?}");
        assert!(
            error
                .to_string()
                .starts_with("ACME server letsencrypt-staging: registering an account: "),
            "{error}"
        );
    }

    // acme-issuance, "Certificate profile": the profiles the server offers are known.
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn profiles_the_server_offers_are_listed() {
        let server = AcmeTestServer::start();
        let (client, _) = AcmeClient::register(
            "pebble",
            &server.pebble.directory_url,
            None,
            &server.pebble.http(),
        )
        .await
        .expect("registered");
        assert_eq!(client.profiles(), ["default", "shortlived"]);
    }
}

mod external_account_binding {
    use super::*;

    const KID: &str = "kid-1";
    const KEY: &str = "zWNDZM6eQGHWpSRTPal5eIUYFTu7EajVIoguysqZ9wG44nMEtx3MUAsUDkMTQ12W";

    fn options() -> PebbleOptions {
        PebbleOptions {
            external_account: Some((KID.into(), KEY.into())),
            ..Default::default()
        }
    }

    // acme-issuance, "External account binding", scenario "Server that requires binding".
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn registration_carries_the_binding_when_it_is_configured() {
        let server = AcmeTestServer::start_with(&options());
        let (http, url) = (server.pebble.http(), server.pebble.directory_url.as_str());

        // The server requires the binding: without it, registration is refused.
        let error = AcmeClient::register("bound", url, None, &http)
            .await
            .err()
            .expect("refused");
        assert_eq!(error.class(), "permanent");
        assert!(
            error.to_string().contains("externalAccountRequired"),
            "{error}"
        );

        // With the configured key identifier and key, it is accepted, which it
        // can only be if the request carried a valid binding.
        let key = Secret::new(KEY);
        let (client, credentials) = AcmeClient::register("bound", url, Some((KID, &key)), &http)
            .await
            .expect("registered with the binding");
        let again = AcmeClient::with_account("bound", url, &credentials, &http)
            .await
            .expect("loaded");
        assert_eq!(again.account_id(), client.account_id());
    }

    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn binding_with_the_wrong_key_is_refused() {
        let server = AcmeTestServer::start_with(&options());
        let wrong = Secret::new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let error = AcmeClient::register(
            "bound",
            &server.pebble.directory_url,
            Some((KID, &wrong)),
            &server.pebble.http(),
        )
        .await
        .err()
        .expect("the binding does not verify");
        assert_eq!(error.class(), "permanent");
        assert!(!error.to_string().contains(wrong.expose()));
    }

    #[tokio::test]
    async fn key_that_is_not_base64url_is_refused_without_being_shown() {
        let http = crate::http::client().expect("an HTTP client");
        let key = Secret::new("not base64url!");
        let error = AcmeClient::register(
            "bound",
            "https://acme.example.com/directory",
            Some((KID, &key)),
            &http,
        )
        .await
        .err()
        .expect("unusable");
        assert_eq!(
            error.to_string(),
            "acme.bound.eab_key is not usable: it is not base64url text"
        );
    }

    #[test]
    fn key_is_accepted_with_and_without_padding() {
        assert_eq!(decode_base64url("aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(decode_base64url("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(decode_base64url(" aGVsbG8\n"), Some(b"hello".to_vec()));
        assert_eq!(
            decode_base64url("a+b/"),
            None,
            "base64url, not standard base64"
        );
    }
}

mod errors {
    use super::*;

    /// A server that speaks just enough ACME to refuse a registration with
    /// `refusal`. Constructed from RFC 8555; not a recording.
    async fn refusing_server(refusal: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        let uri = server.uri();
        Mock::given(method("GET"))
            .and(path("/directory"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "newNonce": format!("{uri}/new-nonce"),
                "newAccount": format!("{uri}/new-account"),
                "newOrder": format!("{uri}/new-order"),
                "revokeCert": format!("{uri}/revoke-cert"),
                "keyChange": format!("{uri}/key-change")
            })))
            .mount(&server)
            .await;
        Mock::given(path("/new-nonce"))
            .respond_with(
                ResponseTemplate::new(200).insert_header("Replay-Nonce", "oFvnlFP1wIhRlYS2jTaXbA"),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/new-account"))
            .respond_with(refusal.insert_header("Replay-Nonce", "D8s4D2mLs8Vn-goWuPQeKA"))
            .mount(&server)
            .await;
        server
    }

    fn problem(status: u16, kind: &str, detail: &str) -> ResponseTemplate {
        ResponseTemplate::new(status)
            .insert_header("Content-Type", "application/problem+json")
            .set_body_json(json!({
                "type": format!("urn:ietf:params:acme:error:{kind}"),
                "detail": detail,
                "status": status
            }))
    }

    async fn register_at(server: &MockServer) -> Error {
        let http = crate::http::client().expect("an HTTP client");
        AcmeClient::register(
            "letsencrypt",
            &format!("{}/directory", server.uri()),
            None,
            &http,
        )
        .await
        .err()
        .expect("the registration is refused")
    }

    // run-orchestration, "Rate limits are obeyed": the wait the server states is known.
    #[tokio::test]
    async fn rate_limit_carries_the_wait_the_server_states() {
        let server = refusing_server(
            problem(
                429,
                "rateLimited",
                "too many new registrations from this IP address",
            )
            .insert_header("Retry-After", "30"),
        )
        .await;

        let error = register_at(&server).await;

        assert_eq!(
            error,
            Error::rate_limited(
                "ACME server letsencrypt: registering an account: rateLimited: too many new \
                 registrations from this IP address",
                Some(Duration::from_secs(30))
            )
        );
    }

    #[tokio::test]
    async fn rate_limit_without_a_stated_wait_has_none() {
        let server = refusing_server(problem(429, "rateLimited", "slow down")).await;
        let error = register_at(&server).await;
        assert!(
            matches!(
                error,
                Error::RateLimited {
                    retry_after: None,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn server_failure_is_transient_and_a_refusal_is_permanent() {
        let server = refusing_server(problem(500, "serverInternal", "something broke")).await;
        assert!(register_at(&server).await.is_transient());

        let server =
            refusing_server(problem(403, "unauthorized", "account creation is disabled")).await;
        let error = register_at(&server).await;
        assert_eq!(error.class(), "permanent");
        assert_eq!(
            error.to_string(),
            "ACME server letsencrypt: registering an account: unauthorized: account creation is \
             disabled"
        );
    }

    #[test]
    fn retry_after_is_read_as_seconds_or_as_a_date() {
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after(" 120 "), Some(Duration::from_secs(120)));
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(Duration::ZERO),
            "a date in the past means no wait"
        );
        let in_an_hour = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(3600));
        let wait = parse_retry_after(&in_an_hour).expect("a date");
        assert!(
            wait > Duration::from_secs(3590) && wait <= Duration::from_secs(3600),
            "{wait:?}"
        );
        assert_eq!(parse_retry_after("soon"), None);
    }

    #[test]
    fn problem_is_described_with_its_kind_detail_and_subproblems() {
        let problem: instant_acme::Problem = serde_json::from_value(json!({
            "type": "urn:ietf:params:acme:error:rejectedIdentifier",
            "detail": "some of the names were rejected",
            "status": 400,
            "subproblems": [
                {
                    "type": "urn:ietf:params:acme:error:rejectedIdentifier",
                    "detail": "the name is on the block list",
                    "identifier": { "type": "dns", "value": "blocked.example.com" }
                }
            ]
        }))
        .expect("a problem document");
        assert_eq!(
            describe_problem(&problem),
            "rejectedIdentifier: some of the names were rejected; blocked.example.com: the name \
             is on the block list"
        );
    }
}
