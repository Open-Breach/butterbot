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

mod issuance {
    use std::collections::BTreeMap;
    use std::net::UdpSocket;

    use time::macros::datetime;

    use super::*;
    use crate::config::KeyType;
    use crate::dns::DnsProvider;
    use crate::dns::challtestsrv::ChallTestSrvProvider;
    use crate::logging::{Capture, Level, LogFormat, StepLog};
    use crate::policy::{RenewalInfoSource, lookup_renewal_window};
    use crate::store::encode::{KeyAlgorithm, key_algorithm, key_belongs_to};
    use crate::store::{IssuedCert, StoredCert};
    use crate::testing::{ChallTestSrv, Pebble};

    /// Polling fast enough for tests.
    const FAST: Polling = Polling {
        interval: Duration::from_millis(100),
        timeout: Duration::from_secs(30),
    };

    struct Fixture {
        acme: AcmeTestServer,
        client: AcmeClient,
        dns: ChallTestSrvProvider,
    }

    impl Fixture {
        async fn new() -> Self {
            Self::with(&PebbleOptions::default()).await
        }

        async fn with(options: &PebbleOptions) -> Self {
            let acme = AcmeTestServer::start_with(options);
            let http = acme.pebble.http();
            let (client, _) =
                AcmeClient::register("pebble", &acme.pebble.directory_url, None, &http)
                    .await
                    .expect("registered");
            let dns = ChallTestSrvProvider::new(
                http,
                &acme.dns.management_url(),
                &["example.com", "example.org"],
            );
            Self {
                acme,
                client: client.with_polling(FAST),
                dns,
            }
        }

        fn request(names: &[&str]) -> OrderRequest {
            OrderRequest {
                names: names.iter().map(|name| name.to_string()).collect(),
                profile: None,
                replaces: None,
                key_type: KeyType::EcdsaP256,
            }
        }

        /// Publishes the challenges of an order as the run will: grouped by record.
        async fn publish(&self, challenges: &[Challenge]) {
            let records = crate::dns::group_by_record(
                challenges
                    .iter()
                    .map(|challenge| (challenge.dns_name.as_str(), challenge.value.as_str())),
            );
            let zones = self.dns.zones().await.expect("zones");
            for (record, values) in records {
                let zone = crate::dns::resolve_zone("test", &zones, &record).expect("a zone");
                self.dns
                    .present(zone, &record, &values, 60)
                    .await
                    .expect("published");
            }
        }

        /// Goes through a whole order.
        async fn issue(&self, request: &OrderRequest) -> Result<IssuedCert> {
            let mut order = self.client.order(request).await?;
            self.publish(order.challenges()).await;
            order.validate().await?;
            order.finalize().await
        }
    }

    fn names_of(certificate: &StoredCert) -> Vec<&str> {
        certificate.names.iter().map(String::as_str).collect()
    }

    fn log() -> StepLog {
        Capture::new()
            .logger(
                LogFormat::Json,
                Level::Info,
                datetime!(2026-11-23 02:00:00 UTC),
            )
            .step("test", "decide")
    }

    // acme-issuance, "Order for the configured names", scenario "Wildcard and apex";
    // "Issued certificate with chain", scenario "Successful issuance" (task 7.5).
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn wildcard_and_apex_are_issued_as_one_certificate_with_its_chain() {
        let fixture = Fixture::new().await;
        let request = Fixture::request(&["*.example.com", "example.com"]);

        let mut order = fixture.client.order(&request).await.expect("an order");

        let challenges = order.challenges().to_vec();
        assert_eq!(challenges.len(), 2, "one challenge per name");
        let by_name: BTreeMap<&str, &Challenge> = challenges
            .iter()
            .map(|challenge| (challenge.dns_name.as_str(), challenge))
            .collect();
        let (wildcard, apex) = (by_name["*.example.com"], by_name["example.com"]);
        assert_eq!(wildcard.record_name, "_acme-challenge.example.com");
        assert_eq!(
            apex.record_name, "_acme-challenge.example.com",
            "both share one record"
        );
        assert_ne!(wildcard.value, apex.value, "with one value each");

        fixture.publish(&challenges).await;
        order
            .validate()
            .await
            .expect("the server accepts the records");
        let issued = order.finalize().await.expect("the certificate is issued");

        assert_eq!(
            names_of(issued.leaf()),
            ["*.example.com", "example.com"],
            "exactly the two names"
        );
        assert!(
            !issued.chain().is_empty(),
            "the intermediate certificates come with it"
        );
        assert!(
            key_belongs_to(issued.leaf(), issued.key_pkcs8_der()),
            "with its private key"
        );
        let intermediate = StoredCert::from_der(&issued.chain()[0]).expect("a certificate");
        assert_ne!(intermediate.serial, issued.leaf().serial);
    }

    // acme-issuance, "Order for the configured names", scenario "Several unrelated names".
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn unrelated_names_are_issued_as_one_certificate() {
        let fixture = Fixture::new().await;
        let issued = fixture
            .issue(&Fixture::request(&["api.example.com", "www.example.org"]))
            .await
            .expect("issued");
        assert_eq!(
            names_of(issued.leaf()),
            ["api.example.com", "www.example.org"]
        );
    }

    // acme-issuance, "Key type", scenarios "Default key type" and "Override on the
    // certificate" (task 7.4: the requested key type and exactly the requested names).
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn issued_certificate_has_the_requested_key_type_and_names() {
        let fixture = Fixture::new().await;
        for (key_type, expected) in [
            (KeyType::Rsa2048, KeyAlgorithm::Rsa { bits: 2048 }),
            (KeyType::Rsa3072, KeyAlgorithm::Rsa { bits: 3072 }),
            (KeyType::Rsa4096, KeyAlgorithm::Rsa { bits: 4096 }),
            (KeyType::EcdsaP256, KeyAlgorithm::EcdsaP256),
            (KeyType::EcdsaP384, KeyAlgorithm::EcdsaP384),
        ] {
            let name = format!("{key_type}.example.com");
            let request = OrderRequest {
                key_type,
                ..Fixture::request(&[&name, "example.org"])
            };

            let issued = fixture.issue(&request).await.expect("issued");

            assert_eq!(key_algorithm(issued.leaf()), Ok(expected), "{key_type}");
            let expected_names: std::collections::BTreeSet<String> =
                [name.clone(), "example.org".to_owned()].into();
            assert_eq!(
                issued.leaf().names,
                expected_names,
                "{key_type}: exactly the names"
            );
            assert!(
                key_belongs_to(issued.leaf(), issued.key_pkcs8_der()),
                "{key_type}"
            );
        }
    }

    // acme-issuance, "Renewal names its predecessor", both scenarios, and "Fresh
    // private key per issuance", scenario "Renewal" (task 7.5).
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn renewal_names_the_stored_certificate_as_the_one_it_replaces() {
        let fixture = Fixture::new().await;
        let request = Fixture::request(&["example.com"]);

        // First issuance: there is no predecessor to name.
        let first = fixture
            .issue(&request)
            .await
            .expect("issued without a predecessor");

        // The server offers renewal information for the certificate it issued.
        let window = fixture
            .client
            .renewal_window(first.leaf())
            .await
            .expect("the server answers")
            .expect("the server offers renewal information");
        assert!(window.start < window.end);
        assert!(window.start > first.leaf().not_before && window.end < first.leaf().not_after);

        // The renewal names it.
        let renewal = OrderRequest {
            replaces: Some(first.leaf().clone()),
            ..request.clone()
        };
        let second = fixture
            .issue(&renewal)
            .await
            .expect("the renewal is issued");
        assert_ne!(second.leaf().serial, first.leaf().serial);
        assert_ne!(
            second.key_pkcs8_der(),
            first.key_pkcs8_der(),
            "a different private key"
        );

        // The server remembers that the first certificate has been replaced: an
        // order that names it again is refused. That shows the order above
        // named it.
        let error = fixture
            .client
            .order(&renewal)
            .await
            .err()
            .expect("already replaced");
        assert_eq!(error.class(), "permanent");
        assert!(error.to_string().contains("alreadyReplaced"), "{error}");
    }

    // renewal-policy, "Renewal information is best effort", scenario "Server does not
    // know the certificate".
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn renewal_information_for_a_certificate_of_another_server_is_no_information() {
        let fixture = Fixture::new().await;
        let other = Fixture::new().await;
        let foreign = other
            .issue(&Fixture::request(&["example.com"]))
            .await
            .expect("issued elsewhere");

        let error = fixture.client.renewal_window(foreign.leaf()).await;
        assert!(
            error.is_err(),
            "the server does not know the certificate: {error:?}"
        );

        let window = lookup_renewal_window(Some(&fixture.client), foreign.leaf(), &log()).await;
        assert_eq!(
            window, None,
            "the certificate is judged by the other rules and does not fail"
        );
    }

    // acme-issuance, "Certificate profile", scenario "Profile set".
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn configured_profile_is_requested() {
        let fixture = Fixture::new().await;
        let day = Duration::from_secs(86_400);
        let with_profile = |profile: &str, name: &str| OrderRequest {
            profile: Some(profile.to_owned()),
            ..Fixture::request(&[name])
        };

        // The test server's profiles differ in the lifetime they give.
        let issued = fixture
            .issue(&with_profile("shortlived", "example.com"))
            .await
            .expect("issued");
        assert!(
            issued.leaf().lifetime() < day * 7,
            "{:?}",
            issued.leaf().lifetime()
        );

        let issued = fixture
            .issue(&with_profile("default", "example.org"))
            .await
            .expect("issued");
        assert!(
            issued.leaf().lifetime() > day * 80,
            "{:?}",
            issued.leaf().lifetime()
        );
    }

    // acme-issuance, "Certificate profile", scenario "Profile not offered" (task 7.6).
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn profile_the_server_does_not_offer_fails_and_names_the_offered_ones() {
        let fixture = Fixture::new().await;
        let request = OrderRequest {
            profile: Some("tlsserver".into()),
            ..Fixture::request(&["example.com"])
        };

        let error = fixture
            .client
            .order(&request)
            .await
            .err()
            .expect("not offered");

        assert_eq!(error.class(), "permanent");
        assert_eq!(
            error.to_string(),
            "the ACME server pebble does not offer the profile tlsserver (it offers: default, \
             shortlived)"
        );
    }

    // acme-issuance, "Validation is attempted once per run", scenario "Validation fails"
    // (task 7.6).
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn rejected_validation_fails_with_the_reason_of_the_server_for_each_name() {
        let fixture = Fixture::new().await;
        let mut order = fixture
            .client
            .order(&Fixture::request(&["www.example.com", "api.example.org"]))
            .await
            .expect("an order");

        // One name gets a wrong value, the other gets none.
        let zones = fixture.dns.zones().await.expect("zones");
        let zone =
            crate::dns::resolve_zone("test", &zones, "_acme-challenge.www.example.com").unwrap();
        fixture
            .dns
            .present(
                zone,
                "_acme-challenge.www.example.com",
                &["not-the-value".to_owned()],
                60,
            )
            .await
            .expect("published");

        let error = order
            .validate()
            .await
            .expect_err("the server rejects the challenges");

        assert_eq!(error.class(), "validation_failed", "{error:?}");
        let message = error.to_string();
        assert!(
            message.starts_with("the ACME server pebble rejected the validation: "),
            "{message}"
        );
        // Pebble stops at the first name it cannot validate, so at least one of
        // the two names is listed with what the server said about it.
        let names_given = ["www.example.com: ", "api.example.org: "]
            .iter()
            .filter(|name| message.contains(**name))
            .count();
        assert!(names_given >= 1, "{message}");
        assert!(
            message.contains("unauthorized") || message.contains("dns"),
            "{message}"
        );
    }

    // acme-issuance, "Bounded wait for the server's verdict", scenario "Verdict never arrives".
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn verdict_that_does_not_arrive_in_time_is_a_timeout() {
        // The server validates against a DNS server that never answers, so the
        // order stays pending for longer than the client waits.
        // A port that is free for UDP can be taken for TCP; try until both are had.
        let (_silent_udp, _silent_tcp, address) = (0..50)
            .find_map(|_| {
                let udp = UdpSocket::bind("127.0.0.1:0").ok()?;
                let address = udp.local_addr().ok()?;
                let tcp = std::net::TcpListener::bind(address).ok()?;
                Some((udp, tcp, address))
            })
            .expect("a port that is free for UDP and TCP");
        let pebble = Pebble::start(address, &PebbleOptions::default());
        let (client, _) =
            AcmeClient::register("pebble", &pebble.directory_url, None, &pebble.http())
                .await
                .expect("registered");
        let client = client.with_polling(Polling {
            interval: Duration::from_millis(100),
            timeout: Duration::from_millis(400),
        });
        let mut order = client
            .order(&Fixture::request(&["example.com"]))
            .await
            .expect("an order");
        let started = std::time::Instant::now();

        let error = order.validate().await.expect_err("no verdict in time");

        assert_eq!(error.class(), "timeout", "{error:?}");
        assert_eq!(
            error.to_string(),
            "the ACME server pebble gave no verdict on the validation within 0s"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn name_that_is_already_proven_needs_no_challenge() {
        // The server reuses a valid authorization for every order.
        let fixture = Fixture::with(&PebbleOptions {
            authorization_reuse: 100,
            ..Default::default()
        })
        .await;
        let request = Fixture::request(&["example.com"]);
        fixture
            .issue(&request)
            .await
            .expect("issued; the name is now proven");
        fixture
            .acme
            .dns_cleared_for("_acme-challenge.example.com", &fixture.dns)
            .await;

        let mut order = fixture.client.order(&request).await.expect("an order");
        assert!(order.challenges().is_empty(), "nothing has to be published");
        order.validate().await.expect("nothing has to be validated");
        let issued = order.finalize().await.expect("issued");
        assert_eq!(names_of(issued.leaf()), ["example.com"]);
    }

    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn silent_dns_server_of_the_harness_is_not_the_challenge_server() {
        // A check of the harness itself: the two servers of a fixture belong together.
        let fixture = Fixture::new().await;
        let other: ChallTestSrv = ChallTestSrv::start();
        assert_ne!(fixture.acme.dns.dns, other.dns);
    }

    #[test]
    fn results_are_polled_every_5_seconds_for_at_most_2_minutes() {
        assert_eq!(
            Polling::default(),
            Polling {
                interval: Duration::from_secs(5),
                timeout: Duration::from_secs(120)
            }
        );
    }
}

/// What exactly is sent in a new order, read from the request a mock server
/// received. The mock speaks just enough ACME for an account to exist; it is
/// constructed from RFC 8555 and the ACME profiles draft, not a recording.
mod order_request {
    use base64::prelude::BASE64_URL_SAFE_NO_PAD;

    use super::*;
    use crate::config::KeyType;

    async fn server_that_records_orders() -> MockServer {
        let server = MockServer::start().await;
        let uri = server.uri();
        Mock::given(method("GET"))
            .and(path("/directory"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "newNonce": format!("{uri}/new-nonce"),
                "newAccount": format!("{uri}/new-account"),
                "newOrder": format!("{uri}/new-order"),
                "revokeCert": format!("{uri}/revoke-cert"),
                "keyChange": format!("{uri}/key-change"),
                "meta": {
                    "profiles": {
                        "tlsserver": "certificates for TLS servers",
                        "shortlived": "certificates that are valid for six days"
                    }
                }
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
            .respond_with(
                ResponseTemplate::new(201)
                    .insert_header("Replay-Nonce", "D8s4D2mLs8Vn-goWuPQeKA")
                    .insert_header("Location", format!("{uri}/account/1").as_str())
                    .set_body_json(
                        json!({ "status": "valid", "orders": format!("{uri}/account/1/orders") }),
                    ),
            )
            .mount(&server)
            .await;
        // The order itself is refused; only what was asked for matters here.
        Mock::given(method("POST"))
            .and(path("/new-order"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("Replay-Nonce", "taa2RvJ8Qc3Z1p8Vn6vQ9w")
                    .insert_header("Content-Type", "application/problem+json")
                    .set_body_json(json!({
                        "type": "urn:ietf:params:acme:error:unauthorized",
                        "detail": "this server only records orders",
                        "status": 403
                    })),
            )
            .mount(&server)
            .await;
        server
    }

    /// Places an order at the mock and returns the payload of the request.
    async fn payload_of_order(request: OrderRequest) -> serde_json::Value {
        let server = server_that_records_orders().await;
        let http = crate::http::client().expect("an HTTP client");
        let (client, _) = AcmeClient::register(
            "letsencrypt",
            &format!("{}/directory", server.uri()),
            None,
            &http,
        )
        .await
        .expect("registered at the mock");

        let error = client
            .order(&request)
            .await
            .err()
            .expect("the mock refuses every order");
        assert!(
            error
                .to_string()
                .contains("this server only records orders"),
            "{error}"
        );

        let requests = server
            .received_requests()
            .await
            .expect("requests are recorded");
        let order = requests
            .iter()
            .find(|request| request.url.path() == "/new-order")
            .expect("an order was placed");
        let signed: serde_json::Value = order.body_json().expect("a signed request");
        let payload = BASE64_URL_SAFE_NO_PAD
            .decode(signed["payload"].as_str().expect("a payload"))
            .expect("base64url");
        serde_json::from_slice(&payload).expect("the payload is JSON")
    }

    fn request(profile: Option<&str>) -> OrderRequest {
        OrderRequest {
            names: vec!["*.Example.com".to_owned(), "example.com".to_owned()],
            profile: profile.map(str::to_owned),
            replaces: None,
            key_type: KeyType::Rsa2048,
        }
    }

    // acme-issuance, "Certificate profile": without a profile none is requested;
    // "Renewal names its predecessor", scenario "First issuance".
    #[tokio::test]
    async fn order_asks_for_the_names_and_for_nothing_else() {
        let payload = payload_of_order(request(None)).await;
        assert_eq!(
            payload,
            json!({
                "identifiers": [
                    { "type": "dns", "value": "*.example.com" },
                    { "type": "dns", "value": "example.com" }
                ]
            }),
            "exactly the configured names, no profile and no predecessor"
        );
    }

    // acme-issuance, "Certificate profile", scenario "Profile set".
    #[tokio::test]
    async fn order_requests_the_profile_that_is_set() {
        let payload = payload_of_order(request(Some("tlsserver"))).await;
        assert_eq!(payload["profile"], "tlsserver");
        assert!(payload.get("replaces").is_none());
    }
}
