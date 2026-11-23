//! The private key of a certificate and the request that asks for it.
//!
//! Every issuance gets a new key, generated in memory with the crypto backend
//! the TLS stack already uses. The certificate signing request carries the
//! DNS names and nothing else: no subject, no further extensions.

use rcgen::{
    CertificateParams, DistinguishedName, KeyPair, PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384,
    PKCS_RSA_SHA256, RsaKeySize,
};

use crate::config::KeyType;
use crate::{Error, Result};

/// Generates a new private key of the given type.
pub fn generate(key_type: KeyType) -> Result<KeyPair> {
    match key_type {
        KeyType::Rsa2048 => KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_2048),
        KeyType::Rsa3072 => KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_3072),
        KeyType::Rsa4096 => KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_4096),
        KeyType::EcdsaP256 => KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256),
        KeyType::EcdsaP384 => KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384),
    }
    .map_err(|error| Error::permanent(format!("a {key_type} key could not be generated: {error}")))
}

/// Builds the certificate signing request for `names`, signed with `key`, as
/// DER.
pub fn signing_request(names: &[String], key: &KeyPair) -> Result<Vec<u8>> {
    let unusable = |error: rcgen::Error| {
        Error::permanent(format!(
            "the certificate request could not be built: {error}"
        ))
    };
    let mut parameters = CertificateParams::new(names.to_vec()).map_err(unusable)?;
    // Without this the request would carry a placeholder subject.
    parameters.distinguished_name = DistinguishedName::new();
    let request = parameters.serialize_request(key).map_err(unusable)?;
    Ok(request.der().to_vec())
}

#[cfg(test)]
mod tests {
    use x509_parser::certification_request::X509CertificationRequest;
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    use x509_parser::prelude::FromDer;
    use x509_parser::public_key::PublicKey;

    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    // acme-issuance, "Key type": each of the five types gives a key of that type.
    #[test]
    fn key_of_each_type_is_generated() {
        for (key_type, expected_bits) in [
            (KeyType::Rsa2048, 2048),
            (KeyType::Rsa3072, 3072),
            (KeyType::Rsa4096, 4096),
            (KeyType::EcdsaP256, 256),
            (KeyType::EcdsaP384, 384),
        ] {
            let key = generate(key_type).expect("a key");
            let der = signing_request(&names(&["example.com"]), &key).expect("a request");
            let (_, request) =
                X509CertificationRequest::from_der(&der).expect("a readable request");
            let public_key = request
                .certification_request_info
                .subject_pki
                .parsed()
                .expect("a key");
            let is_rsa = matches!(public_key, PublicKey::RSA(_));
            assert_eq!(is_rsa, key_type.as_str().starts_with("rsa"), "{key_type}");
            assert_eq!(public_key.key_size(), expected_bits, "{key_type}");
        }
    }

    // acme-issuance, "Fresh private key per issuance".
    #[test]
    fn every_generated_key_is_a_new_one() {
        let first = generate(KeyType::EcdsaP256).expect("a key");
        let second = generate(KeyType::EcdsaP256).expect("a key");
        assert_ne!(first.serialize_der(), second.serialize_der());
    }

    // acme-issuance, "Order for the configured names": the request names exactly those.
    #[test]
    fn request_carries_the_dns_names_and_nothing_else() {
        let key = generate(KeyType::EcdsaP256).expect("a key");
        let der = signing_request(
            &names(&["*.example.com", "example.com", "api.example.org"]),
            &key,
        )
        .expect("a request");

        let (rest, request) = X509CertificationRequest::from_der(&der).expect("a readable request");
        assert!(rest.is_empty());
        let info = &request.certification_request_info;
        assert_eq!(info.subject.to_string(), "", "no subject");

        let extensions: Vec<_> = request
            .requested_extensions()
            .expect("extensions")
            .collect();
        assert_eq!(extensions.len(), 1, "one extension: the alternative names");
        let ParsedExtension::SubjectAlternativeName(alternative_names) = &extensions[0] else {
            panic!(
                "the extension is not the alternative names: {:?}",
                extensions[0]
            );
        };
        let listed: Vec<String> = alternative_names
            .general_names
            .iter()
            .map(|name| match name {
                GeneralName::DNSName(name) => name.to_string(),
                other => panic!("not a DNS name: {other:?}"),
            })
            .collect();
        assert_eq!(listed, ["*.example.com", "example.com", "api.example.org"]);
    }
}
