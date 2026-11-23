//! Task 4.2: what is written is read back by an independent parser, the
//! `openssl` command.

use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::TempDir;

use super::*;
use crate::config::KeyType;
use crate::store::testing::{NOT_AFTER, NOT_BEFORE, issue, issue_with};

/// Runs `openssl` with `arguments`, giving it `input`, and returns its output.
fn openssl(arguments: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("openssl")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("these tests need the `openssl` command");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input)
        .expect("openssl reads its input");
    let output = child.wait_with_output().expect("openssl finishes");
    assert!(
        output.status.success(),
        "openssl {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn openssl_text(arguments: &[&str], input: &[u8]) -> String {
    String::from_utf8(openssl(arguments, input)).expect("openssl prints text")
}

/// The DER content of every PEM block with `tag` in `text`.
fn pem_blocks(text: &str, tag: &str) -> Vec<Vec<u8>> {
    pem::parse_many(text)
        .expect("PEM")
        .into_iter()
        .filter(|block| block.tag() == tag)
        .map(pem::Pem::into_contents)
        .collect()
}

/// The public key of a certificate, as openssl prints it.
fn public_key_of_certificate(der: &[u8]) -> String {
    openssl_text(&["x509", "-inform", "DER", "-pubkey", "-noout"], der)
}

/// The public key of a private key given as PEM, as openssl prints it.
fn public_key_of_private_key(pem: &str) -> String {
    openssl_text(&["pkey", "-pubout"], pem.as_bytes())
}

fn key_types() -> [KeyType; 4] {
    [
        KeyType::Rsa2048,
        KeyType::Rsa3072,
        KeyType::EcdsaP256,
        KeyType::EcdsaP384,
    ]
}

fn issued(key_type: KeyType) -> IssuedCert {
    issue_with(
        &["*.example.com", "example.com"],
        key_type,
        NOT_BEFORE,
        NOT_AFTER,
    )
}

#[test]
fn pem_is_read_by_openssl_for_rsa_and_ecdsa_keys() {
    for key_type in key_types() {
        let certificate = issued(key_type);
        let fullchain = fullchain_pem(&certificate);
        let key = private_key_pem(&certificate);

        // The certificate file: the certificate first, then its chain.
        let first = openssl(&["x509", "-outform", "DER"], fullchain.as_bytes());
        assert_eq!(first, certificate.leaf().der, "{key_type}");
        let listed = openssl_text(
            &["crl2pkcs7", "-nocrl", "-certfile", "/dev/stdin"],
            fullchain.as_bytes(),
        );
        let printed = openssl_text(&["pkcs7", "-print_certs"], listed.as_bytes());
        let read = pem_blocks(&printed, "CERTIFICATE");
        assert_eq!(
            read.len(),
            2,
            "{key_type}: the certificate and one intermediate"
        );
        assert_eq!(read[0], certificate.leaf().der);
        assert_eq!(read[1], certificate.chain()[0]);

        // The key file: a key openssl accepts, and the key of the certificate.
        openssl(&["pkey", "-noout", "-check"], key.as_bytes());
        assert_eq!(
            public_key_of_private_key(&key),
            public_key_of_certificate(&certificate.leaf().der),
            "{key_type}: the key belongs to the certificate"
        );
        let expected_kind = if key_type.as_str().starts_with("rsa") {
            "RSA"
        } else {
            "EC"
        };
        let described = openssl_text(&["pkey", "-noout", "-text"], key.as_bytes());
        let bits = match key_type {
            KeyType::Rsa2048 => "2048 bit",
            KeyType::Rsa3072 => "3072 bit",
            KeyType::Rsa4096 => "4096 bit",
            KeyType::EcdsaP256 => "256 bit",
            KeyType::EcdsaP384 => "384 bit",
        };
        assert!(
            described.contains(bits),
            "{key_type}: {expected_kind} key with {bits}"
        );
    }
}

#[test]
fn pem_text_uses_unix_line_endings_and_standard_labels() {
    let certificate = issue(&["example.com"]);
    let fullchain = fullchain_pem(&certificate);
    let key = private_key_pem(&certificate);
    assert!(fullchain.starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(fullchain.ends_with("-----END CERTIFICATE-----\n"));
    assert!(key.starts_with("-----BEGIN PRIVATE KEY-----\n"));
    assert!(key.ends_with("-----END PRIVATE KEY-----\n"));
    assert!(!fullchain.contains('\r') && !key.contains('\r'));
    assert_eq!(*bundle_pem(&certificate), format!("{}{fullchain}", *key));
}

#[test]
fn pkcs12_with_empty_password_is_read_by_openssl_for_rsa_and_ecdsa_keys() {
    for key_type in key_types() {
        let certificate = issued(key_type);
        let bundle = pkcs12(&certificate, "wildcard-example-com").expect("a bundle");

        // `-passin pass:` is the empty password. openssl checks the MAC with it
        // and fails if the bundle was written with another password.
        let certificates = openssl_text(&["pkcs12", "-passin", "pass:", "-nokeys"], &bundle);
        let read = pem_blocks(&certificates, "CERTIFICATE");
        assert_eq!(
            read.len(),
            2,
            "{key_type}: the certificate and one intermediate"
        );
        assert_eq!(read[0], certificate.leaf().der);
        assert_eq!(read[1], certificate.chain()[0]);

        let key = openssl_text(
            &["pkcs12", "-passin", "pass:", "-nocerts", "-nodes"],
            &bundle,
        );
        assert_eq!(
            public_key_of_private_key(&key),
            public_key_of_certificate(&certificate.leaf().der),
            "{key_type}: the key belongs to the certificate"
        );
    }
}

#[test]
fn pkcs12_is_not_readable_with_another_password() {
    let bundle = pkcs12(&issue(&["example.com"]), "example-com").expect("a bundle");
    let output = Command::new("openssl")
        .args([
            "pkcs12",
            "-passin",
            "pass:not-empty",
            "-nokeys",
            "-in",
            "/dev/stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .expect("stdin is piped")
                .write_all(&bundle)?;
            child.wait()
        })
        .expect("these tests need the `openssl` command");
    assert!(
        !output.success(),
        "the password of the bundle is the empty one, and only that"
    );
}

#[test]
fn pkcs12_uses_current_algorithms() {
    let bundle = pkcs12(&issue(&["example.com"]), "example-com").expect("a bundle");
    let output = Command::new("openssl")
        .args([
            "pkcs12",
            "-passin",
            "pass:",
            "-info",
            "-noout",
            "-in",
            "/dev/stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .expect("stdin is piped")
                .write_all(&bundle)?;
            child.wait_with_output()
        })
        .expect("these tests need the `openssl` command");
    let info = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(info.contains("MAC: sha256"), "{info}");
    assert!(
        info.contains("PBES2") && info.contains("AES-256-CBC"),
        "{info}"
    );
}

#[test]
fn certificate_is_read_back_from_what_was_written() {
    for key_type in [KeyType::Rsa2048, KeyType::EcdsaP256] {
        let certificate = issued(key_type);
        assert_eq!(
            certificate_from_pem(&fullchain_pem(&certificate)).as_ref(),
            Ok(certificate.leaf())
        );
        assert_eq!(
            certificate_from_pem(&bundle_pem(&certificate)).as_ref(),
            Ok(certificate.leaf()),
            "a key in front of the certificate is skipped"
        );
        let bundle = pkcs12(&certificate, "wildcard-example-com").expect("a bundle");
        assert_eq!(
            certificate_from_pkcs12(&bundle).as_ref(),
            Ok(certificate.leaf())
        );
    }
}

/// A bundle that another tool placed in a store must be readable too. These
/// are made by openssl: once with its defaults, once with the older
/// algorithms that tools such as the previous job produce.
#[test]
fn certificate_is_read_from_a_pkcs12_bundle_written_by_openssl() {
    let certificate = issued(KeyType::Rsa2048);
    let directory = TempDir::new().unwrap();
    let pem_path = directory.path().join("bundle.pem");
    std::fs::write(&pem_path, bundle_pem(&certificate).as_bytes()).unwrap();
    let pem_path = pem_path.to_str().unwrap();

    let modern = openssl(
        &[
            "pkcs12", "-export", "-passout", "pass:", "-in", pem_path, "-name", "x",
        ],
        b"",
    );
    assert_eq!(
        certificate_from_pkcs12(&modern).as_ref(),
        Ok(certificate.leaf())
    );

    let older = openssl(
        &[
            "pkcs12",
            "-export",
            "-passout",
            "pass:",
            "-in",
            pem_path,
            "-name",
            "x",
            "-certpbe",
            "PBE-SHA1-3DES",
            "-keypbe",
            "PBE-SHA1-3DES",
            "-macalg",
            "sha1",
        ],
        b"",
    );
    assert_eq!(
        certificate_from_pkcs12(&older).as_ref(),
        Ok(certificate.leaf())
    );
}

#[test]
fn content_that_is_not_in_the_format_is_unreadable() {
    assert!(certificate_from_pem("").is_err());
    assert!(certificate_from_pem("not PEM").is_err());
    let key_only = private_key_pem(&issue(&["example.com"]));
    assert_eq!(
        certificate_from_pem(&key_only),
        Err("the PEM text contains no certificate".to_owned())
    );
    assert!(certificate_from_pkcs12(b"").is_err());
    assert!(certificate_from_pkcs12(b"not a bundle").is_err());

    // A bundle with a password is not one Butterbot can read.
    let directory = TempDir::new().unwrap();
    let pem_path = directory.path().join("bundle.pem");
    std::fs::write(&pem_path, bundle_pem(&issue(&["example.com"])).as_bytes()).unwrap();
    let protected = openssl(
        &[
            "pkcs12",
            "-export",
            "-passout",
            "pass:secret",
            "-in",
            pem_path.to_str().unwrap(),
        ],
        b"",
    );
    let error = certificate_from_pkcs12(&protected).expect_err("a password is set");
    assert!(
        error.starts_with("not a PKCS#12 bundle with an empty password"),
        "{error}"
    );
}

#[test]
fn key_algorithm_is_read_from_the_certificate() {
    for (key_type, expected) in [
        (KeyType::Rsa2048, KeyAlgorithm::Rsa { bits: 2048 }),
        (KeyType::Rsa3072, KeyAlgorithm::Rsa { bits: 3072 }),
        (KeyType::Rsa4096, KeyAlgorithm::Rsa { bits: 4096 }),
        (KeyType::EcdsaP256, KeyAlgorithm::EcdsaP256),
        (KeyType::EcdsaP384, KeyAlgorithm::EcdsaP384),
    ] {
        assert_eq!(
            key_algorithm(issued(key_type).leaf()),
            Ok(expected),
            "{key_type}"
        );
    }
}

#[test]
fn key_is_recognised_as_belonging_to_its_certificate_only() {
    for key_type in [KeyType::Rsa2048, KeyType::EcdsaP256, KeyType::EcdsaP384] {
        let certificate = issued(key_type);
        let other = issued(key_type);
        assert!(
            key_belongs_to(certificate.leaf(), certificate.key_pkcs8_der()),
            "{key_type}"
        );
        assert!(
            !key_belongs_to(certificate.leaf(), other.key_pkcs8_der()),
            "{key_type}"
        );
        assert!(
            !key_belongs_to(certificate.leaf(), b"not a key"),
            "{key_type}"
        );
    }
}
