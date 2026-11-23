//! The `azure-key-vault` certificate store.
//!
//! A certificate is held as a Key Vault certificate under the certificate's
//! name. It is written by importing the certificate, its chain and its key in
//! one piece, with a policy that makes the key exportable and that records
//! the stored format as the content type. A consumer that reads the secret of
//! the same name receives the certificate, the chain and the key in that
//! format.

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use azure_core::error::ErrorKind;
use azure_core::http::{ClientOptions, StatusCode};
use azure_security_keyvault_certificates::models::{
    CertificatePolicy, CurveName, ImportCertificateParameters, KeyProperties, KeyType,
    SecretProperties,
};
use azure_security_keyvault_certificates::{CertificateClient, CertificateClientOptions};

use super::encode::{self, KeyAlgorithm};
use super::{CertStore, Format, IssuedCert, StoredCert};
use crate::{Error, Result};

/// The content type Key Vault uses for a PKCS#12 bundle.
pub const PKCS12_CONTENT_TYPE: &str = "application/x-pkcs12";

/// The content type Key Vault uses for PEM text.
pub const PEM_CONTENT_TYPE: &str = "application/x-pem-file";

/// The error code with which Key Vault says that it holds no certificate
/// under a name.
const NOT_FOUND_CODE: &str = "CertificateNotFound";

/// A certificate store that is an Azure Key Vault.
pub struct KeyVaultStore {
    url: String,
    client: CertificateClient,
}

impl KeyVaultStore {
    /// Opens the vault at `url` with the ambient Azure identity. Nothing is
    /// sent until the first operation.
    pub fn connect(url: &str, http: &reqwest::Client) -> Result<Self> {
        Self::with(
            url,
            crate::azure::credential(http)?,
            crate::azure::client_options(http),
        )
    }

    fn with(
        url: &str,
        credential: std::sync::Arc<dyn TokenCredential>,
        client_options: ClientOptions,
    ) -> Result<Self> {
        let options = CertificateClientOptions {
            client_options,
            ..Default::default()
        };
        let client = CertificateClient::new(url, credential, Some(options)).map_err(|error| {
            Error::permanent(format!("{url} is not usable as a Key Vault URL: {error}"))
        })?;
        Ok(Self {
            url: url.to_owned(),
            client,
        })
    }

    fn classify(&self, action: &str, error: &azure_core::Error) -> Error {
        crate::azure::classify(&format!("{action} in Key Vault {}", self.url), error)
    }
}

#[async_trait]
impl CertStore for KeyVaultStore {
    /// Reads the certificate from its public part. The format plays no role:
    /// the vault holds one certificate under a name, whatever its format.
    async fn current(&self, name: &str, _format: Format) -> Result<Option<StoredCert>> {
        let failed = |error: azure_core::Error| {
            self.classify(&format!("reading the certificate {name}"), &error)
        };
        let response = match self.client.get_certificate(name, None).await {
            Ok(response) => response,
            Err(error) if is_not_found(&error) => return Ok(None),
            Err(error) => return Err(failed(error)),
        };
        let certificate = response.into_model().map_err(failed)?;
        let unreadable = |problem: String| {
            Error::permanent(format!(
                "the certificate {name} in Key Vault {} is unreadable: {problem}",
                self.url
            ))
        };
        let der = certificate
            .cer
            .ok_or_else(|| unreadable("it has no content".to_owned()))?;
        StoredCert::from_der(&der).map(Some).map_err(unreadable)
    }

    async fn put(&self, name: &str, certificate: &IssuedCert, format: Format) -> Result<()> {
        let failed = |error: azure_core::Error| {
            self.classify(&format!("importing the certificate {name}"), &error)
        };
        let (value, content_type) = match format {
            Format::Pfx => {
                let bundle = encode::pkcs12(certificate, name).map_err(Error::permanent)?;
                (
                    azure_core::base64::encode(bundle.as_slice()),
                    PKCS12_CONTENT_TYPE,
                )
            }
            Format::Pem => (
                encode::bundle_pem(certificate).to_string(),
                PEM_CONTENT_TYPE,
            ),
        };
        let parameters = ImportCertificateParameters {
            base64_encoded_certificate: Some(value),
            // No password: the bundle has an empty one, and PEM has none.
            password: None,
            certificate_policy: Some(CertificatePolicy {
                key_properties: Some(key_properties(certificate)?),
                secret_properties: Some(SecretProperties {
                    content_type: Some(content_type.to_owned()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let content = parameters.try_into().map_err(failed)?;
        self.client
            .import_certificate(name, content, None)
            .await
            .map_err(failed)?;
        Ok(())
    }
}

/// The key part of the import policy: the kind of key the certificate has,
/// exportable, and not to be reused.
fn key_properties(certificate: &IssuedCert) -> Result<KeyProperties> {
    let algorithm = encode::key_algorithm(certificate.leaf()).map_err(|problem| {
        Error::permanent(format!(
            "the certificate cannot be imported: it has {problem}"
        ))
    })?;
    let (key_type, key_size, curve) = match algorithm {
        KeyAlgorithm::Rsa { bits } => (KeyType::Rsa, Some(bits as i32), None),
        KeyAlgorithm::EcdsaP256 => (KeyType::Ec, None, Some(CurveName::P256)),
        KeyAlgorithm::EcdsaP384 => (KeyType::Ec, None, Some(CurveName::P384)),
    };
    Ok(KeyProperties {
        exportable: Some(true),
        key_type: Some(key_type),
        key_size,
        curve,
        reuse_key: Some(false),
    })
}

fn is_not_found(error: &azure_core::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::HttpResponse { status: StatusCode::NotFound, error_code: Some(code), .. }
            if code == NOT_FOUND_CODE
    )
}

#[cfg(test)]
mod tests;
