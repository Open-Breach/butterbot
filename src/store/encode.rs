//! The two stored formats: PEM text and a PKCS#12 bundle.
//!
//! Both hold the same content: the certificate, its chain and the private
//! key. A PKCS#12 bundle is written with an empty password, which is what the
//! consumers of the certificates read.

use p12_keystore::{
    Certificate, KeyStore, KeyStoreEntry, Pkcs12Archive, PrivateKey, PrivateKeyChain,
};
use pem::{EncodeConfig, LineEnding, Pem};
use zeroize::Zeroizing;

use super::{IssuedCert, StoredCert};

/// The password of every PKCS#12 bundle Butterbot writes.
pub const PKCS12_PASSWORD: &str = "";

const CERTIFICATE: &str = "CERTIFICATE";
const PRIVATE_KEY: &str = "PRIVATE KEY";

fn encode_pem(tag: &str, der: &[u8]) -> String {
    pem::encode_config(
        &Pem::new(tag, der.to_vec()),
        EncodeConfig::new().set_line_ending(LineEnding::LF),
    )
}

/// The certificate followed by its chain, as PEM text.
pub fn fullchain_pem(certificate: &IssuedCert) -> String {
    let mut text = encode_pem(CERTIFICATE, &certificate.leaf().der);
    for intermediate in certificate.chain() {
        text.push_str(&encode_pem(CERTIFICATE, intermediate));
    }
    text
}

/// The private key as PEM text (PKCS#8, not encrypted).
pub fn private_key_pem(certificate: &IssuedCert) -> Zeroizing<String> {
    Zeroizing::new(encode_pem(PRIVATE_KEY, certificate.key_pkcs8_der()))
}

/// The private key, the certificate and its chain in one PEM text.
pub fn bundle_pem(certificate: &IssuedCert) -> Zeroizing<String> {
    let mut text = private_key_pem(certificate);
    text.push_str(&fullchain_pem(certificate));
    text
}

/// The certificate, its chain and the private key as a PKCS#12 bundle with an
/// empty password. `friendly_name` labels the entry in the bundle.
pub fn pkcs12(certificate: &IssuedCert, friendly_name: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let unusable =
        |error: p12_keystore::error::Error| format!("the PKCS#12 bundle cannot be built: {error}");
    let key = PrivateKey::from_der(certificate.key_pkcs8_der()).map_err(unusable)?;
    let mut certificates = vec![Certificate::from_der(&certificate.leaf().der).map_err(unusable)?];
    for intermediate in certificate.chain() {
        certificates.push(Certificate::from_der(intermediate).map_err(unusable)?);
    }
    // The key and its certificate are linked by an identifier that only has
    // to be the same in both places. The serial number serves.
    let key_id = certificate.leaf().serial.as_bytes();
    let mut store = KeyStore::new();
    store.add_entry(
        friendly_name,
        KeyStoreEntry::PrivateKeyChain(PrivateKeyChain::new(key_id, key, certificates)),
    );
    store
        .writer(PKCS12_PASSWORD)
        .write()
        .map(Zeroizing::new)
        .map_err(unusable)
}

/// Reads the certificate out of PEM text: the first certificate in it.
pub fn certificate_from_pem(text: &str) -> Result<StoredCert, String> {
    let blocks = pem::parse_many(text).map_err(|error| format!("not PEM: {error}"))?;
    let first = blocks
        .into_iter()
        .find(|block| block.tag() == CERTIFICATE)
        .ok_or("the PEM text contains no certificate")?;
    StoredCert::from_der(first.contents())
}

/// Reads the certificate out of a PKCS#12 bundle with an empty password: the
/// certificate that belongs to the private key in the bundle.
pub fn certificate_from_pkcs12(bundle: &[u8]) -> Result<StoredCert, String> {
    let archive = Pkcs12Archive::from_pkcs12(bundle, PKCS12_PASSWORD)
        .map_err(|error| format!("not a PKCS#12 bundle with an empty password: {error}"))?;
    let key_ids: Vec<&[u8]> = archive
        .keys
        .iter()
        .filter_map(|key| key.local_key_id.as_ref().map(AsRef::as_ref))
        .collect();
    let of_the_key = archive.certs.iter().find(|bag| {
        bag.local_key_id
            .as_deref()
            .is_some_and(|id| key_ids.contains(&id))
    });
    // A bundle without key identifiers lists the certificate first.
    let bag = of_the_key
        .or(archive.certs.first())
        .ok_or("the PKCS#12 bundle contains no certificate")?;
    StoredCert::from_der(bag.cert.as_der())
}

/// Whether the private key (PKCS#8 DER) is the key of the certificate.
pub fn key_belongs_to(certificate: &StoredCert, key_pkcs8_der: &[u8]) -> bool {
    use rcgen::PublicKeyData;

    let Ok(key) = rcgen::KeyPair::try_from(key_pkcs8_der) else {
        return false;
    };
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(&certificate.der) else {
        return false;
    };
    parsed.public_key().subject_public_key.data.as_ref() == key.der_bytes()
}

/// The kind of key a certificate has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAlgorithm {
    /// An RSA key with a modulus of this many bits.
    Rsa {
        bits: u32,
    },
    EcdsaP256,
    EcdsaP384,
}

/// The kind of key of a certificate, or an error for a kind Butterbot does
/// not issue.
pub fn key_algorithm(certificate: &StoredCert) -> Result<KeyAlgorithm, String> {
    use x509_parser::oid_registry::{OID_EC_P256, OID_NIST_EC_P384};
    use x509_parser::public_key::PublicKey;

    let (_, parsed) = x509_parser::parse_x509_certificate(&certificate.der)
        .map_err(|error| format!("not a certificate: {error}"))?;
    let key = parsed.public_key();
    match key
        .parsed()
        .map_err(|error| format!("unreadable public key: {error}"))?
    {
        PublicKey::RSA(rsa) => Ok(KeyAlgorithm::Rsa {
            bits: rsa.key_size() as u32,
        }),
        PublicKey::EC(_) => {
            let curve = key
                .algorithm
                .parameters
                .as_ref()
                .and_then(|curve| curve.as_oid().ok());
            match curve {
                Some(curve) if curve == OID_EC_P256 => Ok(KeyAlgorithm::EcdsaP256),
                Some(curve) if curve == OID_NIST_EC_P384 => Ok(KeyAlgorithm::EcdsaP384),
                _ => Err("an elliptic-curve key on a curve other than P-256 and P-384".to_owned()),
            }
        }
        _ => Err("a key that is neither RSA nor ECDSA".to_owned()),
    }
}

/// Reads the private key out of PEM text, as PKCS#8 DER.
pub fn private_key_from_pem(text: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let blocks = pem::parse_many(text).map_err(|error| format!("not PEM: {error}"))?;
    blocks
        .into_iter()
        .find(|block| block.tag() == PRIVATE_KEY)
        .map(|block| Zeroizing::new(block.into_contents()))
        .ok_or_else(|| "the PEM text contains no private key".to_owned())
}

#[cfg(test)]
mod tests;
