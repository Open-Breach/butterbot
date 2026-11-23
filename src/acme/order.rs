//! One order: from asking for a certificate to holding it.
//!
//! The steps of an order are separate calls, because other work happens in
//! between: the challenge records are published and checked after
//! [`Issuer::order`], and removed after [`PendingOrder::validate`]. The traits
//! are what the run needs from an ACME server, so that a run can be tested
//! without one.

use std::time::Duration;

use async_trait::async_trait;
use instant_acme::{
    AuthorizationStatus, CertificateIdentifier, ChallengeType, Identifier, NewOrder, Order,
    OrderStatus,
};
use rustls::pki_types::CertificateDer;
use tokio::time::Instant;

use super::{AcmeClient, describe_problem, keys};
use crate::config::KeyType;
use crate::dns::challenge_record_name;
use crate::policy::{RenewalInfoSource, RenewalWindow};
use crate::store::{IssuedCert, StoredCert};
use crate::{Error, Result};

/// How long and how often the server is asked for the result of a validation
/// or an issuance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Polling {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for Polling {
    /// Every 5 seconds, for at most 2 minutes.
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(120),
        }
    }
}

/// What is asked of an ACME server for one certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRequest {
    /// The DNS names the certificate is to cover, wildcards included.
    pub names: Vec<String>,
    /// The profile to request, or `None` to request no profile.
    pub profile: Option<String>,
    /// The stored certificate the new one replaces, when the server is known
    /// to have issued it.
    pub replaces: Option<StoredCert>,
    pub key_type: KeyType,
}

/// A DNS challenge of an order: the value that has to be served as TXT at the
/// record, to prove control of the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// The DNS name of the certificate the challenge is for.
    pub dns_name: String,
    /// The record the value is published at: `_acme-challenge.<base name>`.
    pub record_name: String,
    /// The TXT value.
    pub value: String,
}

/// An ACME server with an account, as a run uses it.
#[async_trait]
pub trait Issuer: RenewalInfoSource {
    /// Places an order and returns it with the DNS challenges to fulfil.
    async fn order(&self, request: &OrderRequest) -> Result<Box<dyn PendingOrder>>;
}

/// An order that has been placed.
#[async_trait]
pub trait PendingOrder: Send {
    /// The DNS challenges to publish before [`PendingOrder::validate`]. Empty
    /// when the server needs no proof for this order.
    fn challenges(&self) -> &[Challenge];

    /// Asks the server to validate the challenges and waits for its verdict.
    /// Called once: a rejected challenge is a [`Error::ValidationFailed`] and
    /// ends the order.
    async fn validate(&mut self) -> Result<()>;

    /// Has the certificate issued for a new private key and downloads it with
    /// its chain.
    async fn finalize(&mut self) -> Result<IssuedCert>;
}

impl AcmeClient {
    /// The identifier under which the server knows a certificate.
    fn certificate_identifier(certificate: &StoredCert) -> Result<CertificateIdentifier<'static>> {
        let der = CertificateDer::from(certificate.der.as_slice());
        CertificateIdentifier::try_from(&der)
            .map(CertificateIdentifier::into_owned)
            .map_err(|problem| {
                Error::permanent(format!(
                    "the stored certificate cannot be identified: {problem}"
                ))
            })
    }
}

#[async_trait]
impl RenewalInfoSource for AcmeClient {
    async fn renewal_window(&self, certificate: &StoredCert) -> Result<Option<RenewalWindow>> {
        let identifier = Self::certificate_identifier(certificate)?;
        match self.account.renewal_info(&identifier).await {
            Ok((info, _)) => Ok(Some(RenewalWindow {
                start: info.suggested_window.start,
                end: info.suggested_window.end,
            })),
            // The server has no renewal information to offer.
            Err(instant_acme::Error::Unsupported(_)) => Ok(None),
            Err(error) => Err(self.classify("asking for renewal information", &error)),
        }
    }
}

#[async_trait]
impl Issuer for AcmeClient {
    async fn order(&self, request: &OrderRequest) -> Result<Box<dyn PendingOrder>> {
        if let Some(profile) = &request.profile {
            let offered = self.profiles();
            if !offered.contains(profile) {
                let offered = if offered.is_empty() {
                    "none".to_owned()
                } else {
                    offered.join(", ")
                };
                return Err(Error::permanent(format!(
                    "the ACME server {} does not offer the profile {profile} (it offers: {offered})",
                    self.server
                )));
            }
        }
        let identifiers: Vec<Identifier> = request
            .names
            .iter()
            .map(|name| Identifier::Dns(name.to_ascii_lowercase()))
            .collect();
        let mut new_order = NewOrder::new(&identifiers);
        if let Some(profile) = &request.profile {
            new_order = new_order.profile(profile);
        }
        if let Some(predecessor) = &request.replaces {
            new_order = new_order.replaces(Self::certificate_identifier(predecessor)?);
        }
        let mut order = self
            .account
            .new_order(&new_order)
            .await
            .map_err(|error| self.classify("placing the order", &error))?;

        // Collect the DNS challenge of every name that still has to be proven.
        let mut challenges = Vec::new();
        let mut authorizations = order.authorizations();
        while let Some(authorization) = authorizations.next().await {
            let mut authorization =
                authorization.map_err(|error| self.classify("reading an authorization", &error))?;
            let identifier = authorization.identifier();
            let base = match identifier.identifier {
                Identifier::Dns(name) => name.clone(),
                other => {
                    return Err(Error::permanent(format!(
                        "the ACME server {} asks to prove {other:?}, which is not a DNS name",
                        self.server
                    )));
                }
            };
            let dns_name = if identifier.wildcard {
                format!("*.{base}")
            } else {
                base.clone()
            };
            match authorization.status {
                AuthorizationStatus::Pending => {}
                // Proven earlier and still valid: nothing to publish.
                AuthorizationStatus::Valid => continue,
                other => {
                    return Err(Error::permanent(format!(
                        "the ACME server {} has an authorization for {dns_name} in the state \
                         {other:?}, which cannot be used",
                        self.server
                    )));
                }
            }
            let challenge = authorization
                .challenge(ChallengeType::Dns01)
                .ok_or_else(|| {
                    Error::permanent(format!(
                        "the ACME server {} offers no DNS challenge for {dns_name}",
                        self.server
                    ))
                })?;
            challenges.push(Challenge {
                record_name: challenge_record_name(&base),
                value: challenge.key_authorization().dns_value(),
                dns_name,
            });
        }
        Ok(Box::new(AcmeOrder {
            server: self.server.clone(),
            retry_after: self.retry_after.clone(),
            polling: self.polling,
            order,
            challenges,
            names: request.names.clone(),
            key_type: request.key_type,
        }))
    }
}

/// An order at a real ACME server.
struct AcmeOrder {
    server: String,
    retry_after: std::sync::Arc<std::sync::Mutex<Option<Duration>>>,
    polling: Polling,
    order: Order,
    challenges: Vec<Challenge>,
    names: Vec<String>,
    key_type: KeyType,
}

impl AcmeOrder {
    fn classify(&self, action: &str, error: &instant_acme::Error) -> Error {
        super::classify(&self.server, action, error, &self.retry_after)
    }

    /// The reason the server gave for each name it did not accept.
    async fn rejections(&mut self) -> String {
        let mut reasons = Vec::new();
        let mut authorizations = self.order.authorizations();
        while let Some(Ok(authorization)) = authorizations.next().await {
            if authorization.status != AuthorizationStatus::Invalid {
                continue;
            }
            let identifier = authorization.identifier();
            let name = match identifier.identifier {
                Identifier::Dns(name) if identifier.wildcard => format!("*.{name}"),
                Identifier::Dns(name) => name.clone(),
                other => format!("{other:?}"),
            };
            let reason = authorization
                .challenges
                .iter()
                .find_map(|challenge| challenge.error.as_ref())
                .map(describe_problem)
                .unwrap_or_else(|| "no reason given".to_owned());
            reasons.push(format!("{name}: {reason}"));
        }
        if reasons.is_empty() {
            "no reason given".to_owned()
        } else {
            reasons.join("; ")
        }
    }
}

#[async_trait]
impl PendingOrder for AcmeOrder {
    fn challenges(&self) -> &[Challenge] {
        &self.challenges
    }

    async fn validate(&mut self) -> Result<()> {
        // Tell the server that the records are in place.
        let mut authorizations = self.order.authorizations();
        let mut failure = None;
        while let Some(authorization) = authorizations.next().await {
            let asked = async {
                let mut authorization = authorization?;
                if authorization.status == AuthorizationStatus::Pending
                    && let Some(mut challenge) = authorization.challenge(ChallengeType::Dns01)
                {
                    challenge.set_ready().await?;
                }
                Ok::<(), instant_acme::Error>(())
            };
            if let Err(error) = asked.await {
                failure = Some(error);
                break;
            }
        }
        if let Some(error) = failure {
            return Err(self.classify("asking for validation", &error));
        }

        // Wait for the verdict.
        let started = Instant::now();
        loop {
            let status = match self.order.refresh().await {
                Ok(state) => state.status,
                Err(error) => return Err(self.classify("asking for the verdict", &error)),
            };
            match status {
                OrderStatus::Ready | OrderStatus::Valid => return Ok(()),
                OrderStatus::Invalid => {
                    let reasons = self.rejections().await;
                    return Err(Error::validation_failed(format!(
                        "the ACME server {} rejected the validation: {reasons}",
                        self.server
                    )));
                }
                OrderStatus::Pending | OrderStatus::Processing => {}
            }
            if started.elapsed() + self.polling.interval > self.polling.timeout {
                return Err(Error::timeout(format!(
                    "the ACME server {} gave no verdict on the validation within {}",
                    self.server,
                    crate::config::format_duration(self.polling.timeout)
                )));
            }
            tokio::time::sleep(self.polling.interval).await;
        }
    }

    async fn finalize(&mut self) -> Result<IssuedCert> {
        let key = keys::generate(self.key_type)?;
        let request = keys::signing_request(&self.names, &key)?;
        if let Err(error) = self.order.finalize_csr(&request).await {
            return Err(self.classify("asking for the certificate", &error));
        }

        let started = Instant::now();
        let chain = loop {
            match self.order.certificate().await {
                Ok(Some(chain)) => break chain,
                Ok(None) => {}
                Err(error) => return Err(self.classify("downloading the certificate", &error)),
            }
            if started.elapsed() + self.polling.interval > self.polling.timeout {
                return Err(Error::timeout(format!(
                    "the ACME server {} did not issue the certificate within {}",
                    self.server,
                    crate::config::format_duration(self.polling.timeout)
                )));
            }
            tokio::time::sleep(self.polling.interval).await;
        };
        IssuedCert::from_pem_chain(&chain, key.serialize_der()).map_err(|problem| {
            Error::permanent(format!(
                "the ACME server {} returned a certificate that cannot be read: {problem}",
                self.server
            ))
        })
    }
}
