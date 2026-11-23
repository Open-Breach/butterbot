//! Renewal policy: deciding whether a certificate has to be issued in this run.
//!
//! The decision is made from what the stores hold, the configuration, the
//! clock and, when available, the ACME server's renewal information. Nothing
//! else goes in: there is no record of earlier runs, and which ACME server an
//! entry names plays no part.
//!
//! [`evaluate`] is a pure function, so every rule is tested with fixed times.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::clock::format_utc;
use crate::config::format_duration;
use crate::logging::StepLog;
use crate::store::StoredCert;

/// The window in which an ACME server suggests replacing a certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenewalWindow {
    pub start: OffsetDateTime,
    pub end: OffsetDateTime,
}

/// What one store of a certificate holds.
#[derive(Debug, Clone, Copy)]
pub struct StoreState<'a> {
    /// The store instance name.
    pub store: &'a str,
    /// The certificate the store holds, or `None` when it holds none.
    pub current: Option<&'a StoredCert>,
    /// The server's renewal window for that certificate, when the server was
    /// asked and answered.
    pub renewal_window: Option<RenewalWindow>,
}

/// Everything the decision for one certificate entry is made from.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    /// The DNS names configured for the entry.
    pub names: &'a [String],
    /// The resolved `renew_before` of the entry.
    pub renew_before: Duration,
    /// `run.min_reissue`.
    pub min_reissue: Duration,
    /// Whether the entry was selected with `--only` and `--force`.
    pub forced: bool,
    /// One state per store of the entry, in the order of the entry.
    pub stores: &'a [StoreState<'a>],
    pub now: OffsetDateTime,
}

impl<'a> Input<'a> {
    /// The input for a certificate entry with its resolved settings.
    pub fn for_entry(
        entry: &'a crate::config::Resolved<'a>,
        min_reissue: Duration,
        forced: bool,
        stores: &'a [StoreState<'a>],
        now: OffsetDateTime,
    ) -> Self {
        Self {
            names: &entry.certificate.names,
            renew_before: entry.renew_before,
            min_reissue,
            forced,
            stores,
            now,
        }
    }
}

/// Why a certificate is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// Selected with `--only` and `--force`.
    Forced,
    /// A store holds no certificate under the name.
    Missing { store: String },
    /// The names of the stored certificate differ from the configured names.
    NamesChanged { store: String },
    /// The stored certificate has expired.
    Expired {
        store: String,
        expiry: OffsetDateTime,
    },
    /// The stored certificate is within the renewal period.
    RenewalPeriod {
        store: String,
        expiry: OffsetDateTime,
        renewal_period: Duration,
    },
    /// The window the ACME server suggests for replacing the stored
    /// certificate has started.
    RenewalInfo {
        store: String,
        window_start: OffsetDateTime,
    },
}

impl Reason {
    /// A short, stable name of the reason, for structured output.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Forced => "forced",
            Self::Missing { .. } => "missing",
            Self::NamesChanged { .. } => "names_changed",
            Self::Expired { .. } => "expired",
            Self::RenewalPeriod { .. } => "renewal_period",
            Self::RenewalInfo { .. } => "renewal_info",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forced => write!(f, "issuance was forced"),
            Self::Missing { store } => write!(f, "store {store} holds no certificate"),
            Self::NamesChanged { store } => {
                write!(
                    f,
                    "the certificate in store {store} does not have the configured names"
                )
            }
            Self::Expired { store, expiry } => {
                write!(
                    f,
                    "the certificate in store {store} expired on {}",
                    format_utc(*expiry)
                )
            }
            Self::RenewalPeriod {
                store,
                expiry,
                renewal_period,
            } => write!(
                f,
                "the certificate in store {store} expires on {}, within the renewal period of {}",
                format_utc(*expiry),
                format_days(*renewal_period)
            ),
            Self::RenewalInfo {
                store,
                window_start,
            } => write!(
                f,
                "the ACME server asks for the certificate in store {store} to be replaced \
                 from {} on",
                format_utc(*window_start)
            ),
        }
    }
}

/// The decision for one certificate entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The certificate has to be issued in this run.
    Due { reasons: Vec<Reason> },
    /// Nothing has to be done. `due_at` is when the certificate becomes due,
    /// as far as that can be known now.
    NotDue { due_at: OffsetDateTime },
    /// The certificate is due, but a store holds a certificate that was
    /// issued so recently that issuing again is held back until `until`.
    /// The certificate counts as failed in this run.
    HeldBack {
        reasons: Vec<Reason>,
        fresh_store: String,
        until: OffsetDateTime,
    },
}

impl Decision {
    pub fn is_due(&self) -> bool {
        matches!(self, Self::Due { .. })
    }
}

/// How serious a [`Note`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// Something about the configuration that the decision brought to light and
/// that is to be logged in every run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub severity: Severity,
    pub message: String,
}

/// The decision together with what led to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub decision: Decision,
    /// For each store, in the order given, the renewal period that applies to
    /// the certificate it holds; `None` for a store that holds none.
    pub renewal_periods: Vec<Option<Duration>>,
    pub notes: Vec<Note>,
}

/// The renewal period that applies to a certificate with the given lifetime.
///
/// A `renew_before` at or above the lifetime would make the certificate due
/// on every run, so half the lifetime is used in its place. That is an error
/// in the configuration, but one that must not be able to let a certificate
/// expire. A `renew_before` above half the lifetime is applied as configured
/// and is worth a warning.
pub fn renewal_period(renew_before: Duration, lifetime: Duration) -> (Duration, Option<Severity>) {
    if renew_before >= lifetime {
        (lifetime / 2, Some(Severity::Error))
    } else if renew_before > lifetime / 2 {
        (renew_before, Some(Severity::Warning))
    } else {
        (renew_before, None)
    }
}

/// Decides whether the certificate has to be issued.
pub fn evaluate(input: &Input<'_>) -> Evaluation {
    let configured: BTreeSet<String> = input
        .names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let now = input.now;

    let mut reasons = Vec::new();
    let mut renewal_periods = Vec::with_capacity(input.stores.len());
    let mut notes = Vec::new();
    // When each stored certificate becomes due, for the stores that are not due.
    let mut due_at: Option<OffsetDateTime> = None;
    // When issuing becomes possible again, for each store whose certificate
    // is fresh and not itself due.
    let mut fresh: Option<(&str, OffsetDateTime)> = None;

    for state in input.stores {
        let store = state.store.to_owned();
        let Some(current) = state.current else {
            reasons.push(Reason::Missing { store });
            renewal_periods.push(None);
            continue;
        };
        let lifetime = current.lifetime();
        let (period, severity) = renewal_period(input.renew_before, lifetime);
        renewal_periods.push(Some(period));
        if let Some(severity) = severity {
            let message = match severity {
                Severity::Error => format!(
                    "renew_before is {}, which is not shorter than the lifetime of the stored \
                     certificate ({}); half the lifetime, {}, is used as the renewal period",
                    format_duration(input.renew_before),
                    format_days(lifetime),
                    format_days(period)
                ),
                Severity::Warning => format!(
                    "renew_before is {}, which is more than half the lifetime of the stored \
                     certificate ({})",
                    format_duration(input.renew_before),
                    format_days(lifetime)
                ),
            };
            let note = Note { severity, message };
            if !notes.contains(&note) {
                notes.push(note);
            }
        }

        let expiry = current.not_after;
        let period_start = expiry - period;
        let window_start = state.renewal_window.map(|window| window.start);
        let reason = if current.names != configured {
            Some(Reason::NamesChanged { store })
        } else if expiry <= now {
            Some(Reason::Expired { store, expiry })
        } else if period_start <= now {
            Some(Reason::RenewalPeriod {
                store,
                expiry,
                renewal_period: period,
            })
        } else {
            window_start
                .filter(|start| *start <= now)
                .map(|window_start| Reason::RenewalInfo {
                    store,
                    window_start,
                })
        };
        match reason {
            Some(reason) => reasons.push(reason),
            None => {
                // Renewal information can only bring the time forward.
                let becomes_due =
                    window_start.map_or(period_start, |start| start.min(period_start));
                due_at = Some(due_at.map_or(becomes_due, |earliest| earliest.min(becomes_due)));
                let reissue_from = current.not_before + input.min_reissue;
                if reissue_from > now && fresh.is_none_or(|(_, until)| reissue_from > until) {
                    fresh = Some((state.store, reissue_from));
                }
            }
        }
    }

    let decision = if input.forced {
        reasons.insert(0, Reason::Forced);
        Decision::Due { reasons }
    } else if reasons.is_empty() {
        // Without stores there is nothing that could become due; validation
        // requires at least one store, so `due_at` is always set here.
        Decision::NotDue {
            due_at: due_at.unwrap_or(now),
        }
    } else if let Some((fresh_store, until)) = fresh {
        Decision::HeldBack {
            reasons,
            fresh_store: fresh_store.to_owned(),
            until,
        }
    } else {
        Decision::Due { reasons }
    };
    Evaluation {
        decision,
        renewal_periods,
        notes,
    }
}

/// Writes a duration as days with one decimal place, for example `22.5 days`.
pub fn format_days(duration: Duration) -> String {
    let days = duration.as_secs_f64() / 86_400.0;
    if (days - 1.0).abs() < 0.05 {
        "1.0 day".to_owned()
    } else {
        format!("{days:.1} days")
    }
}

/// Whether the `expiring` event applies to a certificate that was due and was
/// not renewed: its stored certificate has less than half of its renewal
/// period left, or has expired.
pub fn is_expiring(expiry: OffsetDateTime, renewal_period: Duration, now: OffsetDateTime) -> bool {
    expiry - now < renewal_period / 2
}

/// Something that can be asked for an ACME server's renewal information.
#[async_trait]
pub trait RenewalInfoSource: Send + Sync {
    /// The window in which the server suggests replacing `certificate`, or
    /// `None` when the server offers no renewal information.
    async fn renewal_window(
        &self,
        certificate: &StoredCert,
    ) -> crate::Result<Option<RenewalWindow>>;
}

/// How long a renewal-information lookup may take.
pub const RENEWAL_INFO_TIMEOUT: Duration = Duration::from_secs(30);

/// Looks up the renewal window of a stored certificate, best effort.
///
/// `source` is `None` when renewal information is switched off for the ACME
/// server; the server is then not asked. Any failure, and a server that does
/// not answer in time, gives `None`: the certificate is then judged by the
/// other rules, and it does not fail.
pub async fn lookup_renewal_window(
    source: Option<&dyn RenewalInfoSource>,
    certificate: &StoredCert,
    log: &StepLog,
) -> Option<RenewalWindow> {
    let source = source?;
    let lookup = tokio::time::timeout(RENEWAL_INFO_TIMEOUT, source.renewal_window(certificate));
    let reason = match lookup.await {
        Ok(Ok(window)) => return window,
        Ok(Err(error)) => error.to_string(),
        Err(_) => format!("no answer after {}s", RENEWAL_INFO_TIMEOUT.as_secs()),
    };
    log.record(
        crate::logging::Level::Info,
        "renewal information is not available; the renewal period alone decides",
    )
    .field("serial", certificate.serial.as_str())
    .reason(reason)
    .write();
    None
}

#[cfg(test)]
mod tests;
