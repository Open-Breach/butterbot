//! Task 4.4: the Key Vault store against constructed HTTP exchanges.
//!
//! The responses are built from the Key Vault REST API reference (operations
//! "Get Certificate" and "Import Certificate", and the authentication
//! challenge). They are constructed, not recorded from a real vault. The
//! certificates in them are issued by the tests' throwaway authority.

use serde_json::{Value, json};

use super::*;
use crate::azure::testing::{Exchange, MockHttp};
use crate::config::KeyType as ConfiguredKeyType;
use crate::store::testing::{NOT_AFTER, NOT_BEFORE, issue, issue_with};

const URL: &str = "https://butterbot-certs.vault.example.com";
const NAME: &str = "wildcard-example-com";

fn store(http: &MockHttp) -> KeyVaultStore {
    KeyVaultStore::with(
        URL,
        crate::azure::testing::credential(),
        http.client_options(),
    )
    .expect("the URL is usable")
}

/// The body Key Vault returns for a certificate: its identifiers, the public
/// part in `cer`, its attributes and its policy.
fn certificate_bundle(cer: &[u8], content_type: &str) -> String {
    json!({
        "id": format!("{URL}/certificates/{NAME}/f60f2a4f8ae442cfb41ca2090bd4b769"),
        "kid": format!("{URL}/keys/{NAME}/f60f2a4f8ae442cfb41ca2090bd4b769"),
        "sid": format!("{URL}/secrets/{NAME}/f60f2a4f8ae442cfb41ca2090bd4b769"),
        "x5t": "fLi3U52HunIVNXubkEnf8tP6Wbo",
        "cer": azure_core::base64::encode(cer),
        "attributes": {
            "enabled": true,
            "nbf": 1793491200,
            "exp": 1801267200,
            "created": 1793498400,
            "updated": 1793498400,
            "recoveryLevel": "Recoverable+Purgeable"
        },
        "policy": {
            "id": format!("{URL}/certificates/{NAME}/policy"),
            "key_props": { "exportable": true, "kty": "RSA", "key_size": 2048, "reuse_key": false },
            "secret_props": { "contentType": content_type },
            "x509_props": {
                "subject": "CN=example.com",
                "sans": { "dns_names": ["*.example.com", "example.com"] },
                "ekus": ["1.3.6.1.5.5.7.3.1", "1.3.6.1.5.5.7.3.2"],
                "key_usage": ["digitalSignature", "keyEncipherment"],
                "validity_months": 3,
                "basic_constraints": { "ca": false }
            },
            "lifetime_actions": [
                { "trigger": { "lifetime_percentage": 80 }, "action": { "action_type": "EmailContacts" } }
            ],
            "issuer": { "name": "Unknown" },
            "attributes": { "enabled": true, "created": 1793498400, "updated": 1793498400 }
        }
    })
    .to_string()
}

fn error_body(code: &str, message: &str) -> String {
    json!({ "error": { "code": code, "message": message } }).to_string()
}

// certificate-stores, "Reading the current certificate", scenario "Certificate present",
// and "Adoption of existing certificates", scenario "Certificate from the previous tool".
#[tokio::test]
async fn present_certificate_is_read_from_its_public_part() {
    let held = issue(&["*.example.com", "example.com"]);
    let http = MockHttp::key_vault([Exchange::json(
        200,
        &certificate_bundle(&held.leaf().der, PKCS12_CONTENT_TYPE),
    )]);

    let current = store(&http)
        .current(NAME, Format::Pfx)
        .await
        .expect("readable");

    let current = current.expect("the vault holds the certificate");
    assert_eq!(&current, held.leaf());
    assert_eq!(
        current.names.iter().collect::<Vec<_>>(),
        ["*.example.com", "example.com"]
    );
    assert_eq!(
        (current.not_before, current.not_after),
        (NOT_BEFORE, NOT_AFTER)
    );
    let requests = http.authorized_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, format!("/certificates/{NAME}/"));
}

// certificate-stores, "Reading the current certificate", scenario "Nothing stored".
#[tokio::test]
async fn absent_certificate_is_no_certificate_and_not_an_error() {
    let http = MockHttp::key_vault([Exchange::json(
        404,
        &error_body(
            "CertificateNotFound",
            "A certificate with (name/id) wildcard-example-com was not found in this key vault. \
             If you recently deleted this certificate you may be able to recover it using the \
             correct recovery command.",
        ),
    )]);

    assert_eq!(store(&http).current(NAME, Format::Pfx).await, Ok(None));
}

// certificate-stores, "Reading the current certificate", scenario "Store cannot be read".
#[tokio::test]
async fn content_that_is_not_a_certificate_is_an_error() {
    let http = MockHttp::key_vault([Exchange::json(
        200,
        &certificate_bundle(b"not a certificate", PKCS12_CONTENT_TYPE),
    )]);

    let error = store(&http)
        .current(NAME, Format::Pfx)
        .await
        .expect_err("unreadable");

    assert_eq!(error.class(), "permanent");
    assert!(
        error.to_string().starts_with(&format!(
            "the certificate {NAME} in Key Vault {URL} is unreadable: not a certificate"
        )),
        "{error}"
    );
}

#[tokio::test]
async fn certificate_without_content_is_an_error() {
    let mut bundle: Value =
        serde_json::from_str(&certificate_bundle(b"", PKCS12_CONTENT_TYPE)).unwrap();
    bundle.as_object_mut().unwrap().remove("cer");
    let http = MockHttp::key_vault([Exchange::json(200, &bundle.to_string())]);

    let error = store(&http)
        .current(NAME, Format::Pfx)
        .await
        .expect_err("unreadable");
    assert!(
        error
            .to_string()
            .ends_with("is unreadable: it has no content"),
        "{error}"
    );
}

// certificate-stores, "Reading the current certificate", scenario "Store cannot be read".
#[tokio::test]
async fn vault_that_refuses_or_fails_is_an_error_of_the_right_class() {
    let refused = MockHttp::key_vault([Exchange::json(
        403,
        &error_body(
            "Forbidden",
            "Caller is not authorized to perform action on resource.",
        ),
    )]);
    let error = store(&refused)
        .current(NAME, Format::Pfx)
        .await
        .expect_err("refused");
    assert_eq!(error.class(), "permanent");
    assert_eq!(
        error.to_string(),
        format!(
            "reading the certificate {NAME} in Key Vault {URL}: HTTP 403: Caller is not \
             authorized to perform action on resource."
        )
    );

    let unavailable = MockHttp::key_vault([Exchange::json(
        503,
        &error_body("ServiceUnavailable", "The service is unavailable."),
    )]);
    let error = store(&unavailable)
        .current(NAME, Format::Pfx)
        .await
        .expect_err("unavailable");
    assert!(error.is_transient(), "{error:?}");

    // A 404 that is not "no such certificate" is not taken for an empty store.
    let other = MockHttp::key_vault([Exchange::json(
        404,
        &error_body("VaultNotFound", "The vault was not found."),
    )]);
    let error = store(&other)
        .current(NAME, Format::Pfx)
        .await
        .expect_err("not found");
    assert_eq!(error.class(), "permanent");
}

/// The body of the one import request that was sent.
fn import_request(http: &MockHttp) -> Value {
    let requests = http.requests();
    assert_eq!(requests.len(), 2, "the challenge, then the import");
    assert!(
        !requests[0].authorized && requests[0].body.is_empty(),
        "nothing of the certificate is sent before the vault has authenticated the caller"
    );
    assert!(requests[1].authorized);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, format!("/certificates/{NAME}/import"));
    serde_json::from_str(&requests[1].body).expect("the import request is JSON")
}

// certificate-stores, "Store types", scenario "Key Vault store"; "Stored format",
// scenario "Key Vault default"; "Private key is retrievable from Key Vault", scenario
// "Consumer reads the certificate": the import asks for an exportable key and the
// bundle's content type, which is what makes the vault hand out the bundle as the
// secret. Reading it back from a real vault is part of the end-to-end check.
#[tokio::test]
async fn import_as_pkcs12_sends_bundle_content_type_and_exportable_key() {
    let certificate = issue_with(
        &["*.example.com", "example.com"],
        ConfiguredKeyType::Rsa2048,
        NOT_BEFORE,
        NOT_AFTER,
    );
    let http = MockHttp::key_vault([Exchange::json(
        200,
        &certificate_bundle(&certificate.leaf().der, PKCS12_CONTENT_TYPE),
    )]);

    store(&http)
        .put(NAME, &certificate, Format::Pfx)
        .await
        .expect("imported");

    let request = import_request(&http);
    assert_eq!(
        request["policy"],
        json!({
            "key_props": { "exportable": true, "kty": "RSA", "key_size": 2048, "reuse_key": false },
            "secret_props": { "contentType": "application/x-pkcs12" }
        })
    );
    assert!(
        request.get("pwd").is_none(),
        "the bundle has an empty password, so none is sent"
    );
    let bundle = azure_core::base64::decode(request["value"].as_str().unwrap()).expect("base64");
    assert_eq!(
        encode::certificate_from_pkcs12(&bundle).as_ref(),
        Ok(certificate.leaf()),
        "the value is the PKCS#12 bundle with the certificate"
    );
}

// certificate-stores, "Stored format", scenario "Override on the certificate".
#[tokio::test]
async fn import_as_pem_sends_pem_text_and_pem_content_type() {
    let certificate = issue_with(
        &["example.com"],
        ConfiguredKeyType::EcdsaP256,
        NOT_BEFORE,
        NOT_AFTER,
    );
    let http = MockHttp::key_vault([Exchange::json(
        200,
        &certificate_bundle(&certificate.leaf().der, PEM_CONTENT_TYPE),
    )]);

    store(&http)
        .put(NAME, &certificate, Format::Pem)
        .await
        .expect("imported");

    let request = import_request(&http);
    assert_eq!(
        request["policy"],
        json!({
            "key_props": { "exportable": true, "kty": "EC", "crv": "P-256", "reuse_key": false },
            "secret_props": { "contentType": "application/x-pem-file" }
        })
    );
    let text = request["value"].as_str().unwrap();
    assert_eq!(
        text,
        encode::bundle_pem(&certificate).as_str(),
        "key, certificate and chain as PEM"
    );
    assert!(text.starts_with("-----BEGIN PRIVATE KEY-----\n"));
    assert_eq!(text.matches("-----BEGIN CERTIFICATE-----").count(), 2);
}

#[tokio::test]
async fn import_policy_names_the_kind_of_key_the_certificate_has() {
    for (key_type, expected) in [
        (
            ConfiguredKeyType::Rsa3072,
            json!({ "kty": "RSA", "key_size": 3072 }),
        ),
        (
            ConfiguredKeyType::Rsa4096,
            json!({ "kty": "RSA", "key_size": 4096 }),
        ),
        (
            ConfiguredKeyType::EcdsaP384,
            json!({ "kty": "EC", "crv": "P-384" }),
        ),
    ] {
        let certificate = issue_with(&["example.com"], key_type, NOT_BEFORE, NOT_AFTER);
        let http = MockHttp::key_vault([Exchange::json(
            200,
            &certificate_bundle(&certificate.leaf().der, PEM_CONTENT_TYPE),
        )]);

        store(&http)
            .put(NAME, &certificate, Format::Pem)
            .await
            .expect("imported");

        let key_props = import_request(&http)["policy"]["key_props"].clone();
        for (name, value) in expected.as_object().unwrap() {
            assert_eq!(&key_props[name], value, "{key_type}: {name}");
        }
        assert_eq!(key_props["exportable"], true, "{key_type}");
    }
}

#[tokio::test]
async fn failed_import_is_an_error_of_the_right_class() {
    let certificate = issue(&["example.com"]);

    let conflict = MockHttp::key_vault([Exchange::json(
        409,
        &error_body(
            "Conflict",
            "Certificate wildcard-example-com is currently in a deleted but recoverable state, \
             and its name cannot be reused; in this state, the certificate can only be recovered \
             or purged.",
        ),
    )]);
    let error = store(&conflict)
        .put(NAME, &certificate, Format::Pem)
        .await
        .expect_err("conflict");
    assert_eq!(error.class(), "permanent");
    assert!(
        error.to_string().starts_with(&format!(
            "importing the certificate {NAME} in Key Vault {URL}: HTTP 409: Certificate"
        )),
        "{error}"
    );

    let throttled = MockHttp::key_vault([Exchange::json(
        429,
        &error_body(
            "Throttled",
            "Request was not processed because too many requests were received.",
        ),
    )]);
    let error = store(&throttled)
        .put(NAME, &certificate, Format::Pem)
        .await
        .expect_err("throttled");
    assert!(error.is_transient(), "{error:?}");
    assert_eq!(
        throttled.authorized_requests().len(),
        1,
        "the SDK does not retry by itself"
    );
}

#[tokio::test]
async fn error_message_never_contains_the_private_key() {
    let certificate = issue(&["example.com"]);
    let http = MockHttp::key_vault([Exchange::json(
        400,
        &error_body(
            "BadParameter",
            "The specified PEM X.509 certificate content is in an unexpected format.",
        ),
    )]);

    let error = store(&http)
        .put(NAME, &certificate, Format::Pem)
        .await
        .expect_err("rejected");

    let key = encode::private_key_pem(&certificate);
    let key_line = key.lines().nth(1).expect("the key has content");
    assert!(!format!("{error} {error:?}").contains(key_line));
}
