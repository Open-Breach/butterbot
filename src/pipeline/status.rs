//! The state of every configured certificate, as `butterbot status` shows it.
//!
//! The stores are read and the decision is made exactly as a run makes it,
//! and that is all: no account is registered, no order is placed, and nothing
//! is written.

use futures_util::StreamExt;
use time::OffsetDateTime;

use super::certificate::{assess, limited};
use super::{RunContext, Step};
use crate::config::{Config, Resolved};
use crate::policy::{Decision, Note, Reason};
use crate::retry::retry;
use crate::store::StoredCert;

/// What one store holds under the name of a certificate entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holding {
    /// No certificate.
    Nothing,
    Certificate(StoredCert),
    /// The store could not be read. The text says why.
    Unreadable(String),
}

/// One store of a certificate entry, and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStatus {
    /// The store instance name.
    pub store: String,
    pub holding: Holding,
}

/// Whether a certificate entry is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// A run would issue the certificate.
    Due { reasons: Vec<Reason> },
    /// A run would do nothing. `due_at` is when the certificate becomes due.
    NotDue { due_at: OffsetDateTime },
    /// The certificate is due, but a run would not issue it before `until`,
    /// because `fresh_store` holds a certificate that was issued only just.
    HeldBack {
        reasons: Vec<Reason>,
        fresh_store: String,
        until: OffsetDateTime,
    },
    /// Not known, because a store could not be read. A run would fail the
    /// certificate for the same reason.
    Unknown,
}

/// The state of one certificate entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateStatus {
    /// The name of the entry.
    pub name: String,
    /// The DNS names configured for it.
    pub names: Vec<String>,
    /// Its stores, in the order of the entry.
    pub stores: Vec<StoreStatus>,
    pub standing: Standing,
    /// What the decision brought to light about the configuration.
    pub notes: Vec<Note>,
}

impl CertificateStatus {
    /// Whether every store of the entry could be read.
    pub fn is_known(&self) -> bool {
        !matches!(self.standing, Standing::Unknown)
    }
}

/// Works out the state of every configured certificate, in the order of the
/// configuration.
pub async fn status(context: &RunContext, config: &Config) -> Vec<CertificateStatus> {
    futures_util::stream::iter(config.certificates.iter())
        .map(|certificate| async move { status_of(context, &config.resolve(certificate)).await })
        .buffered(config.run.parallel.max(1))
        .collect()
        .await
}

async fn status_of(context: &RunContext, entry: &Resolved<'_>) -> CertificateStatus {
    let name = entry.certificate.name.as_str();
    let log = context.logger.step(name, Step::ReadStores.as_str());
    let call = Some(context.limits.call);

    let mut stores = Vec::with_capacity(entry.stores.len());
    for store in &entry.stores {
        let read = async {
            let handle = context.providers.store(store.name)?;
            let action = format!("reading the store {}", store.name);
            retry(&log, &action, || {
                limited(call, handle.current(name, store.format))
            })
            .await
        };
        stores.push(StoreStatus {
            store: store.name.to_owned(),
            holding: match read.await {
                Ok(None) => Holding::Nothing,
                Ok(Some(certificate)) => Holding::Certificate(certificate),
                Err(error) => Holding::Unreadable(error.to_string()),
            },
        });
    }

    let stored: Option<Vec<Option<StoredCert>>> = stores
        .iter()
        .map(|status| match &status.holding {
            Holding::Nothing => Some(None),
            Holding::Certificate(certificate) => Some(Some(certificate.clone())),
            Holding::Unreadable(_) => None,
        })
        .collect();
    let (standing, notes) = match stored {
        None => (Standing::Unknown, Vec::new()),
        Some(stored) => {
            let log = log.for_step(Step::Decide.as_str());
            let evaluation = assess(context, entry, false, &stored, &log)
                .await
                .evaluation;
            let standing = match evaluation.decision {
                Decision::Due { reasons } => Standing::Due { reasons },
                Decision::NotDue { due_at } => Standing::NotDue { due_at },
                Decision::HeldBack {
                    reasons,
                    fresh_store,
                    until,
                } => Standing::HeldBack {
                    reasons,
                    fresh_store,
                    until,
                },
            };
            (standing, evaluation.notes)
        }
    };
    CertificateStatus {
        name: name.to_owned(),
        names: entry.certificate.names.clone(),
        stores,
        standing,
        notes,
    }
}
