//! Resolution of settings that exist on a provider and on a certificate.
//!
//! The order is always: the certificate entry, then the provider instance,
//! then the built-in default.

use std::time::Duration;

use super::model::*;

/// A certificate entry with the settings that apply to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved<'a> {
    pub certificate: &'a Certificate,
    /// The ACME server instance the entry names.
    pub acme: &'a AcmeServer,
    /// The DNS provider instance the entry names.
    pub dns: &'a DnsInstance,
    /// The stores the entry names, in the order written, each with its format.
    pub stores: Vec<ResolvedStore<'a>>,
    pub renew_before: Duration,
    pub key_type: KeyType,
    /// The profile to request, or `None` to request no profile.
    pub profile: Option<&'a str>,
    pub propagation_timeout: Duration,
    pub settle: Duration,
}

/// One store of a certificate with the format the certificate is written in.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedStore<'a> {
    pub name: &'a str,
    pub store: &'a StoreInstance,
    pub format: Format,
}

impl Config {
    /// Every certificate entry with the settings that apply to it.
    pub fn resolve_all(&self) -> Vec<Resolved<'_>> {
        self.certificates
            .iter()
            .map(|certificate| self.resolve(certificate))
            .collect()
    }

    /// Works out the settings that apply to `certificate`.
    ///
    /// # Panics
    ///
    /// If `certificate` refers to an instance this configuration does not
    /// have. Validation rules that out for the entries of a loaded
    /// configuration.
    pub fn resolve<'a>(&'a self, certificate: &'a Certificate) -> Resolved<'a> {
        let acme = self
            .acme
            .get(&certificate.acme)
            .expect("validated: the ACME server exists");
        let dns = self
            .dns
            .get(&certificate.dns)
            .expect("validated: the DNS provider exists");
        let stores = certificate
            .stores
            .iter()
            .map(|name| {
                let store = self.stores.get(name).expect("validated: the store exists");
                let format = certificate
                    .format
                    .or(store.format)
                    .unwrap_or(store.kind.default_format());
                ResolvedStore {
                    name,
                    store,
                    format,
                }
            })
            .collect();
        Resolved {
            certificate,
            acme,
            dns,
            stores,
            renew_before: certificate
                .renew_before
                .or(acme.renew_before)
                .unwrap_or(defaults::RENEW_BEFORE),
            key_type: certificate
                .key_type
                .or(acme.key_type)
                .unwrap_or(defaults::KEY_TYPE),
            profile: certificate.profile.as_deref().or(acme.profile.as_deref()),
            propagation_timeout: certificate
                .propagation_timeout
                .or(dns.propagation_timeout)
                .unwrap_or(defaults::PROPAGATION_TIMEOUT),
            settle: certificate
                .settle
                .or(dns.settle)
                .unwrap_or(defaults::SETTLE),
        }
    }
}
