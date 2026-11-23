//! Certificate stores: reading the certificate a destination holds and writing
//! a newly issued one in the configured format.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;
use x509_parser::extensions::GeneralName;
use zeroize::Zeroizing;

pub use crate::config::Format;
use crate::logging::StepLog;
use crate::retry::retry;
use crate::{Error, Result};

pub mod directory;
pub mod encode;
pub mod key_vault;

#[cfg(test)]
pub(crate) mod testing;

/// A destination certificates are delivered to.
///
/// A store keeps one certificate per certificate name. The stores are the only
/// state Butterbot has: what `current` returns, for each store of a
/// certificate, is what decides whether the certificate is issued.
#[async_trait]
pub trait CertStore: Send + Sync {
    /// The certificate the store holds under `name` in `format`, or `None`
    /// when it holds none, which is not an error.
    ///
    /// An error means the store could not be read, or holds something that is
    /// not a readable certificate.
    async fn current(&self, name: &str, format: Format) -> Result<Option<StoredCert>>;

    /// Writes `certificate`, its chain and its private key under `name` in
    /// `format`, replacing what the store held.
    async fn put(&self, name: &str, certificate: &IssuedCert, format: Format) -> Result<()>;
}

/// What is known about a certificate a store holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCert {
    /// The DNS names the certificate covers, in lower case.
    pub names: BTreeSet<String>,
    pub not_before: OffsetDateTime,
    pub not_after: OffsetDateTime,
    /// The distinguished name of the issuer.
    pub issuer: String,
    /// The serial number, as lower-case hexadecimal digits.
    pub serial: String,
    /// The certificate itself.
    pub der: Vec<u8>,
}

impl StoredCert {
    /// Reads a DER-encoded certificate. The error says why it is unreadable.
    pub fn from_der(der: &[u8]) -> std::result::Result<Self, String> {
        let (rest, certificate) = x509_parser::parse_x509_certificate(der)
            .map_err(|error| format!("not a certificate: {error}"))?;
        if !rest.is_empty() {
            return Err("not a certificate: data follows the certificate".to_owned());
        }
        let mut names = BTreeSet::new();
        let alternative_names = certificate
            .subject_alternative_name()
            .map_err(|error| format!("unreadable subject alternative names: {error}"))?;
        if let Some(extension) = alternative_names {
            for name in &extension.value.general_names {
                if let GeneralName::DNSName(name) = name {
                    names.insert(name.to_ascii_lowercase());
                }
            }
        }
        let serial = certificate
            .raw_serial()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(Self {
            names,
            not_before: certificate.validity().not_before.to_datetime(),
            not_after: certificate.validity().not_after.to_datetime(),
            issuer: certificate.issuer().to_string(),
            serial,
            der: der.to_vec(),
        })
    }

    /// The whole validity period of the certificate.
    pub fn lifetime(&self) -> Duration {
        Duration::try_from(self.not_after - self.not_before).unwrap_or_default()
    }
}

/// A newly issued certificate: the certificate, the chain the CA supplied and
/// the private key. The three are handed to the stores as one unit.
///
/// The private key lives only in this value until a store has written it. It
/// is wiped from memory when the value is dropped and is never printed.
#[derive(Clone)]
pub struct IssuedCert {
    leaf: StoredCert,
    chain: Vec<Vec<u8>>,
    key: Zeroizing<Vec<u8>>,
}

impl IssuedCert {
    /// Puts an issued certificate together.
    ///
    /// `leaf` is the certificate and `chain` the intermediate certificates in
    /// order, all DER-encoded. `key` is the private key as PKCS#8 DER.
    pub fn new(
        leaf: &[u8],
        chain: Vec<Vec<u8>>,
        key: Vec<u8>,
    ) -> std::result::Result<Self, String> {
        let key = Zeroizing::new(key);
        for (index, der) in chain.iter().enumerate() {
            StoredCert::from_der(der)
                .map_err(|error| format!("chain certificate {}: {error}", index + 1))?;
        }
        Ok(Self {
            leaf: StoredCert::from_der(leaf)?,
            chain,
            key,
        })
    }

    /// Puts an issued certificate together from the PEM text an ACME server
    /// returns: the certificate first, then its chain.
    pub fn from_pem_chain(pem_chain: &str, key: Vec<u8>) -> std::result::Result<Self, String> {
        let key = Zeroizing::new(key);
        let blocks = pem::parse_many(pem_chain).map_err(|error| format!("not PEM: {error}"))?;
        let mut certificates = blocks
            .into_iter()
            .filter(|block| block.tag() == "CERTIFICATE")
            .map(pem::Pem::into_contents);
        let leaf = certificates
            .next()
            .ok_or("the PEM text contains no certificate")?;
        Self::new(&leaf, certificates.collect(), key.to_vec())
    }

    /// The certificate itself, with what is known about it.
    pub fn leaf(&self) -> &StoredCert {
        &self.leaf
    }

    /// The intermediate certificates, DER-encoded, in order.
    pub fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }

    /// The private key as PKCS#8 DER.
    pub fn key_pkcs8_der(&self) -> &[u8] {
        &self.key
    }
}

impl fmt::Debug for IssuedCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedCert")
            .field("names", &self.leaf.names)
            .field("not_after", &self.leaf.not_after)
            .field("serial", &self.leaf.serial)
            .field("chain", &self.chain.len())
            .field("key", &"[private key]")
            .finish()
    }
}

/// One store of a certificate: the instance name, the store and the format
/// the certificate is written in.
pub struct Destination<'a> {
    pub name: &'a str,
    pub store: &'a dyn CertStore,
    pub format: Format,
}

/// Writes an issued certificate to every one of its stores.
///
/// A write that fails with a transient error is repeated, up to three attempts
/// in total. Only the write is repeated: this function has the issued
/// certificate and nothing else, so no failure here can lead to a new order.
///
/// Every store is attempted, also after another one has failed, so that as
/// many stores as possible hold the new certificate. The certificate counts
/// as delivered only when the result is `Ok`. The error names each store that
/// failed.
pub async fn write_to_stores(
    log: &StepLog,
    certificate_name: &str,
    certificate: &IssuedCert,
    destinations: &[Destination<'_>],
) -> Result<()> {
    let mut failures = Vec::new();
    for destination in destinations {
        let action = format!("writing to the store {}", destination.name);
        let written = retry(log, &action, || {
            destination
                .store
                .put(certificate_name, certificate, destination.format)
        })
        .await;
        match written {
            Ok(()) => log
                .record(crate::logging::Level::Info, "certificate written to store")
                .field("store", destination.name)
                .field("format", destination.format.as_str())
                .write(),
            Err(error) => {
                log.record(
                    crate::logging::Level::Error,
                    "certificate could not be written to store",
                )
                .field("store", destination.name)
                .reason(&error)
                .write();
                failures.push((destination.name, error));
            }
        }
    }
    match failures.as_slice() {
        [] => Ok(()),
        [(_, first), ..] => {
            let reasons: Vec<String> = failures
                .iter()
                .map(|(store, error)| format!("store {store}: {error}"))
                .collect();
            let message = format!(
                "the certificate was issued but not written to {} of its {} stores ({})",
                failures.len(),
                destinations.len(),
                reasons.join("; ")
            );
            Err(match first {
                Error::Transient(_) => Error::transient(message),
                Error::Timeout(_) => Error::timeout(message),
                _ => Error::permanent(message),
            })
        }
    }
}

/// A store that keeps certificates in memory. It exists for tests: it behaves
/// like a store, records what was written, and can be made to fail.
#[derive(Default)]
pub struct MemoryStore {
    state: Mutex<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    held: BTreeMap<String, (IssuedCert, Format)>,
    puts: Vec<String>,
    put_failures: VecDeque<Error>,
    read_failure: Option<Error>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the next calls of `put` fail with `failures`, in order.
    pub fn fail_next_puts(&self, failures: impl IntoIterator<Item = Error>) {
        self.lock().put_failures.extend(failures);
    }

    /// Makes every call of `current` fail with `failure`.
    pub fn fail_reads(&self, failure: Error) {
        self.lock().read_failure = Some(failure);
    }

    /// What the store holds under `name`, with its chain, key and format.
    pub fn held(&self, name: &str) -> Option<(IssuedCert, Format)> {
        self.lock().held.get(name).cloned()
    }

    /// The certificate names `put` was called with, failed calls included.
    pub fn puts(&self) -> Vec<String> {
        self.lock().puts.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl CertStore for MemoryStore {
    async fn current(&self, name: &str, format: Format) -> Result<Option<StoredCert>> {
        let state = self.lock();
        if let Some(failure) = &state.read_failure {
            return Err(failure.clone());
        }
        Ok(state
            .held
            .get(name)
            .filter(|(_, held_format)| *held_format == format)
            .map(|(certificate, _)| certificate.leaf().clone()))
    }

    async fn put(&self, name: &str, certificate: &IssuedCert, format: Format) -> Result<()> {
        let mut state = self.lock();
        state.puts.push(name.to_owned());
        if let Some(failure) = state.put_failures.pop_front() {
            return Err(failure);
        }
        state
            .held
            .insert(name.to_owned(), (certificate.clone(), format));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{NOT_AFTER, NOT_BEFORE, issue};
    use super::*;

    #[test]
    fn stored_certificate_gives_names_validity_issuer_and_serial() {
        let issued = issue(&["*.example.com", "Example.COM"]);
        let stored = StoredCert::from_der(&issued.leaf().der).expect("a certificate");

        assert_eq!(
            stored.names.iter().collect::<Vec<_>>(),
            ["*.example.com", "example.com"]
        );
        assert_eq!(stored.not_before, NOT_BEFORE);
        assert_eq!(stored.not_after, NOT_AFTER);
        assert_eq!(stored.lifetime(), Duration::from_secs(90 * 86_400));
        assert_eq!(stored.issuer, "CN=Butterbot Test Intermediate");
        assert!(!stored.serial.is_empty());
        assert!(
            stored.serial.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{}",
            stored.serial
        );
        assert_eq!(&stored, issued.leaf());
    }

    #[test]
    fn content_that_is_not_a_certificate_is_unreadable() {
        for content in [
            &b""[..],
            b"not a certificate",
            &[0x30, 0x03, 0x02, 0x01, 0x01],
        ] {
            let error = StoredCert::from_der(content).expect_err("unreadable");
            assert!(error.starts_with("not a certificate"), "{error}");
        }
        let mut trailing = issue(&["example.com"]).leaf().der.clone();
        trailing.push(0);
        assert!(StoredCert::from_der(&trailing).is_err());
    }

    #[test]
    fn issued_certificate_is_read_from_the_pem_chain_of_an_acme_server() {
        let issued = issue(&["example.com"]);
        let mut text = pem::encode(&pem::Pem::new("CERTIFICATE", issued.leaf().der.clone()));
        for certificate in issued.chain() {
            text.push_str(&pem::encode(&pem::Pem::new(
                "CERTIFICATE",
                certificate.clone(),
            )));
        }

        let read = IssuedCert::from_pem_chain(&text, issued.key_pkcs8_der().to_vec()).expect("PEM");

        assert_eq!(read.leaf(), issued.leaf());
        assert_eq!(read.chain(), issued.chain());
        assert_eq!(read.key_pkcs8_der(), issued.key_pkcs8_der());
        assert!(IssuedCert::from_pem_chain("", Vec::new()).is_err());
        assert!(
            IssuedCert::from_pem_chain("-----BEGIN CERTIFICATE-----\nAAAA\n", Vec::new()).is_err()
        );
    }

    // acme-issuance, "Fresh private key per issuance": the key does not appear in logs.
    #[test]
    fn printing_an_issued_certificate_does_not_show_the_key() {
        let issued = issue(&["example.com"]);
        let printed = format!("{issued:?} {issued:#?}");
        assert!(printed.contains("[private key]"));
        assert!(printed.contains("example.com"));
        let key_bytes = format!("{:?}", issued.key_pkcs8_der());
        assert!(!printed.contains(&key_bytes[1..key_bytes.len() - 1]));
    }

    // certificate-stores, "Reading the current certificate", scenario "Nothing stored".
    #[tokio::test]
    async fn memory_store_holds_nothing_at_first() {
        let store = MemoryStore::new();
        assert_eq!(
            store.current("wildcard-example-com", Format::Pem).await,
            Ok(None)
        );
    }

    // certificate-stores, "Reading the current certificate", scenario "Certificate present".
    #[tokio::test]
    async fn memory_store_round_trips_a_certificate() {
        let store = MemoryStore::new();
        let issued = issue(&["*.example.com", "example.com"]);

        store
            .put("wildcard-example-com", &issued, Format::Pfx)
            .await
            .expect("stored");

        let current = store
            .current("wildcard-example-com", Format::Pfx)
            .await
            .expect("readable");
        assert_eq!(current.as_ref(), Some(issued.leaf()));
        let (held, format) = store.held("wildcard-example-com").expect("held");
        assert_eq!(format, Format::Pfx);
        assert_eq!(held.chain(), issued.chain());
        assert_eq!(held.key_pkcs8_der(), issued.key_pkcs8_der());
        assert_eq!(
            store.current("other", Format::Pfx).await,
            Ok(None),
            "names are separate"
        );
        assert_eq!(
            store.current("wildcard-example-com", Format::Pem).await,
            Ok(None),
            "held in another format is not held in this one"
        );
    }

    #[tokio::test]
    async fn memory_store_replaces_what_it_held() {
        let store = MemoryStore::new();
        let first = issue(&["example.com"]);
        let second = issue(&["example.com"]);
        assert_ne!(first.leaf().serial, second.leaf().serial);

        store
            .put("example-com", &first, Format::Pem)
            .await
            .expect("stored");
        store
            .put("example-com", &second, Format::Pem)
            .await
            .expect("stored");

        let current = store
            .current("example-com", Format::Pem)
            .await
            .expect("readable");
        assert_eq!(current.as_ref(), Some(second.leaf()));
    }

    #[tokio::test]
    async fn memory_store_can_be_made_to_fail() {
        let store = MemoryStore::new();
        let issued = issue(&["example.com"]);

        store.fail_next_puts([Error::transient("busy")]);
        assert_eq!(
            store.put("example-com", &issued, Format::Pem).await,
            Err(Error::transient("busy"))
        );
        assert_eq!(store.current("example-com", Format::Pem).await, Ok(None));
        store
            .put("example-com", &issued, Format::Pem)
            .await
            .expect("the second call succeeds");
        assert_eq!(store.puts(), ["example-com", "example-com"]);

        store.fail_reads(Error::permanent("forbidden"));
        assert_eq!(
            store.current("example-com", Format::Pem).await,
            Err(Error::permanent("forbidden"))
        );
    }

    mod write_to_stores {
        use time::macros::datetime;

        use super::*;
        use crate::logging::{Capture, Level, LogFormat};

        const NAME: &str = "wildcard-example-com";

        fn log() -> (Capture, StepLog) {
            let capture = Capture::new();
            let logger = capture.logger(
                LogFormat::Json,
                Level::Info,
                datetime!(2026-11-23 02:00:00 UTC),
            );
            (capture, logger.step(NAME, "store"))
        }

        fn destinations<'a>(stores: &'a [(&'a str, &'a MemoryStore)]) -> Vec<Destination<'a>> {
            stores
                .iter()
                .map(|(name, store)| Destination {
                    name,
                    store: *store,
                    format: Format::Pem,
                })
                .collect()
        }

        // certificate-stores, "Several stores per certificate", scenario "Two stores".
        #[tokio::test]
        async fn every_store_receives_the_same_certificate_chain_and_key() {
            let (first, second) = (MemoryStore::new(), MemoryStore::new());
            let certificate = issue(&["*.example.com", "example.com"]);
            let (_, log) = log();

            write_to_stores(
                &log,
                NAME,
                &certificate,
                &destinations(&[("a", &first), ("b", &second)]),
            )
            .await
            .expect("written to both");

            for store in [&first, &second] {
                let (held, _) = store.held(NAME).expect("held");
                assert_eq!(held.leaf(), certificate.leaf());
                assert_eq!(held.chain(), certificate.chain());
                assert_eq!(held.key_pkcs8_der(), certificate.key_pkcs8_der());
            }
        }

        // certificate-stores, "Store writes are retried", scenario "Transient failure".
        #[tokio::test(start_paused = true)]
        async fn transient_failure_repeats_the_write_of_the_same_certificate() {
            let store = MemoryStore::new();
            store.fail_next_puts([Error::transient("the vault is busy")]);
            let certificate = issue(&["example.com"]);
            let (capture, log) = log();

            write_to_stores(
                &log,
                NAME,
                &certificate,
                &destinations(&[("vault", &store)]),
            )
            .await
            .expect("the second write succeeds");

            assert_eq!(store.puts(), [NAME, NAME], "the write was made twice");
            let (held, _) = store.held(NAME).expect("held");
            assert_eq!(
                held.leaf().serial,
                certificate.leaf().serial,
                "what is stored is the certificate that was issued once, not a newly ordered one"
            );
            let records = capture.json();
            assert_eq!(records[0]["level"], "warn");
            assert_eq!(records[0]["reason"], "the vault is busy");
            assert_eq!(records[0]["certificate"], NAME);
            assert_eq!(records[1]["message"], "certificate written to store");
        }

        #[tokio::test(start_paused = true)]
        async fn write_is_given_up_after_three_attempts() {
            let store = MemoryStore::new();
            store.fail_next_puts([
                Error::transient("busy 1"),
                Error::transient("busy 2"),
                Error::transient("busy 3"),
            ]);
            let (_, log) = log();

            let error = write_to_stores(
                &log,
                NAME,
                &issue(&["example.com"]),
                &destinations(&[("vault", &store)]),
            )
            .await
            .expect_err("three failures");

            assert_eq!(store.puts().len(), 3, "three attempts in total");
            assert_eq!(
                error,
                Error::transient(
                    "the certificate was issued but not written to 1 of its 1 stores \
                     (store vault: busy 3)"
                )
            );
        }

        #[tokio::test]
        async fn permanent_failure_is_not_repeated() {
            let store = MemoryStore::new();
            store.fail_next_puts([Error::permanent("forbidden")]);
            let (_, log) = log();

            let error = write_to_stores(
                &log,
                NAME,
                &issue(&["example.com"]),
                &destinations(&[("vault", &store)]),
            )
            .await
            .expect_err("refused");

            assert_eq!(store.puts().len(), 1);
            assert_eq!(error.class(), "permanent");
        }

        // run-orchestration, "A certificate is complete only when every store has it",
        // scenario "Second store fails".
        #[tokio::test]
        async fn failing_store_fails_the_delivery_and_the_others_keep_the_certificate() {
            let (first, second, third) =
                (MemoryStore::new(), MemoryStore::new(), MemoryStore::new());
            second.fail_next_puts([Error::permanent("forbidden")]);
            let certificate = issue(&["example.com"]);
            let (capture, log) = log();

            let error = write_to_stores(
                &log,
                NAME,
                &certificate,
                &destinations(&[("first", &first), ("second", &second), ("third", &third)]),
            )
            .await
            .expect_err("the second store failed");

            assert_eq!(
                error.to_string(),
                "the certificate was issued but not written to 1 of its 3 stores \
                 (store second: forbidden)"
            );
            assert!(
                first.held(NAME).is_some(),
                "the store written before keeps the certificate"
            );
            assert!(second.held(NAME).is_none());
            assert!(
                third.held(NAME).is_some(),
                "the store after the failing one is still written"
            );
            let failed: Vec<_> = capture
                .json()
                .into_iter()
                .filter(|record| record["level"] == "error")
                .collect();
            assert_eq!(failed.len(), 1);
            assert_eq!(failed[0]["store"], "second");
            assert_eq!(failed[0]["step"], "store");
        }
    }
}
