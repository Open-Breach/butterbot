//! The HTTP client every provider shares.
//!
//! TLS is rustls with the bundled web PKI roots, so that the program verifies
//! the services it calls without a system certificate store. Every request has
//! a time limit.

use std::sync::Arc;
use std::time::Duration;

use crate::{Error, Result};

/// Time limit for establishing a connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Time limit for one request, from sending it to the end of the response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Builds the shared HTTP client.
pub fn client() -> Result<reqwest::Client> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| Error::permanent(format!("TLS cannot be set up: {error}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .user_agent(concat!("butterbot/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| Error::permanent(format!("the HTTP client cannot be set up: {error}")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn client_can_be_built() {
        super::client().expect("the bundled roots and the TLS backend are usable");
    }
}
