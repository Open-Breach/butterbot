//! Certificates for tests, issued by a throwaway certificate authority.

use rcgen::{
    BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, PKCS_ECDSA_P256_SHA256,
    PKCS_ECDSA_P384_SHA384, PKCS_RSA_SHA256, RsaKeySize,
};
use time::OffsetDateTime;
use time::macros::datetime;

use super::IssuedCert;
use crate::config::KeyType;

pub(crate) const NOT_BEFORE: OffsetDateTime = datetime!(2026-11-01 00:00:00 UTC);
pub(crate) const NOT_AFTER: OffsetDateTime = datetime!(2027-01-30 00:00:00 UTC);

/// Generates a private key of the given type.
pub(crate) fn key(key_type: KeyType) -> KeyPair {
    match key_type {
        KeyType::Rsa2048 => KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_2048),
        KeyType::Rsa3072 => KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_3072),
        KeyType::Rsa4096 => KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_4096),
        KeyType::EcdsaP256 => KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256),
        KeyType::EcdsaP384 => KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384),
    }
    .expect("a key can be generated")
}

/// Issues a certificate for `names` with a key of `key_type`, valid from
/// `not_before` to `not_after`, with a chain of one intermediate certificate.
pub(crate) fn issue_with(
    names: &[&str],
    key_type: KeyType,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
) -> IssuedCert {
    let authority_key = key(KeyType::EcdsaP256);
    let mut authority = CertificateParams::new(Vec::<String>::new()).expect("valid parameters");
    authority.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    authority
        .distinguished_name
        .push(DnType::CommonName, "Butterbot Test Intermediate");
    let authority_certificate = authority
        .self_signed(&authority_key)
        .expect("the authority can sign itself");
    let issuer = Issuer::from_params(&authority, &authority_key);

    let leaf_key = key(key_type);
    let names: Vec<String> = names.iter().map(|name| name.to_string()).collect();
    let mut leaf = CertificateParams::new(names).expect("valid names");
    leaf.not_before = not_before;
    leaf.not_after = not_after;
    let certificate = leaf
        .signed_by(&leaf_key, &issuer)
        .expect("the authority can sign");

    IssuedCert::new(
        certificate.der(),
        vec![authority_certificate.der().to_vec()],
        leaf_key.serialize_der(),
    )
    .expect("the issued certificate is readable")
}

/// Issues a certificate for `names` with an ECDSA P-256 key and the default
/// validity period.
pub(crate) fn issue(names: &[&str]) -> IssuedCert {
    issue_with(names, KeyType::EcdsaP256, NOT_BEFORE, NOT_AFTER)
}
