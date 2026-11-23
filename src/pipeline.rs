//! Run orchestration: the per-certificate pipeline with its undo stack, and
//! the run loop around it.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

use crate::Result;
use crate::acme::Issuer;
use crate::clock::SharedClock;
use crate::config::Resolved;
use crate::dns::DnsProvider;
use crate::dns::propagation::{Confirmed, Published};
use crate::logging::{Logger, StepLog};
use crate::policy::RenewalInfoSource;
use crate::store::{CertStore, StoredCert};

mod certificate;
mod providers;
mod run;
mod undo;

pub use providers::ConfiguredProviders;
pub use run::{RunOutcome, RunReport, Selection, run};
pub use undo::UndoStack;

/// The steps a certificate goes through. The name of the step is in every
/// log record about the certificate and in its `failure` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Reading what each store holds.
    ReadStores,
    /// Checking that what the certificate needs is configured.
    Configuration,
    /// Deciding whether the certificate is due.
    Decide,
    /// Getting the account at the ACME server.
    Account,
    /// Placing the order.
    Order,
    /// Finding the zones of the challenge records and waiting for them to be free.
    Zones,
    /// Publishing the challenge records.
    Present,
    /// Checking that the records are served.
    Propagation,
    /// Waiting after the check.
    Settle,
    /// Asking the ACME server to validate, and waiting for its verdict.
    Validate,
    /// Having the certificate issued and downloading it.
    Finalize,
    /// Writing the certificate to its stores.
    Store,
    /// Removing the challenge records.
    Cleanup,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadStores => "read_stores",
            Self::Configuration => "configuration",
            Self::Decide => "decide",
            Self::Account => "account",
            Self::Order => "order",
            Self::Zones => "zones",
            Self::Present => "present",
            Self::Propagation => "propagation",
            Self::Settle => "settle",
            Self::Validate => "validate",
            Self::Finalize => "finalize",
            Self::Store => "store",
            Self::Cleanup => "cleanup",
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a run needs from the outside world, by instance name.
///
/// The program implements this with the configured providers. Tests
/// implement it with fakes.
#[async_trait]
pub trait Providers: Send + Sync {
    /// What keeps the entry from being issued and is known without asking
    /// anyone, such as a secret that no configuration layer supplies. The
    /// error names the missing configuration path.
    fn check(&self, entry: &Resolved<'_>) -> Result<()>;

    /// Whom to ask for renewal information about certificates of the ACME
    /// server `acme`. `None` when the server is not to be asked: renewal
    /// information is switched off, or there is no account yet and asking
    /// would mean registering one.
    async fn renewal_info(&self, acme: &str, log: &StepLog) -> Option<Arc<dyn RenewalInfoSource>>;

    /// The ACME server `acme` with its account.
    async fn issuer(&self, acme: &str, log: &StepLog) -> Result<Arc<dyn Issuer>>;

    /// The DNS provider instance `dns`.
    fn dns(&self, dns: &str) -> Result<Arc<dyn DnsProvider>>;

    /// The store instance `store`.
    fn store(&self, store: &str) -> Result<Arc<dyn CertStore>>;

    /// Waits until the published records are served, as the DNS provider
    /// instance `dns` is configured to check, for at most `timeout`.
    async fn confirm_propagation(
        &self,
        dns: &str,
        timeout: Duration,
        records: &[Published],
        log: &StepLog,
    ) -> Result<Confirmed>;
}

/// The request to stop a run. The first termination signal makes it; every
/// part of the run that waits for something can be woken by it.
#[derive(Debug, Clone, Default)]
pub struct Shutdown {
    token: CancellationToken,
}

impl Shutdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the run to stop.
    pub fn request(&self) {
        self.token.cancel();
    }

    pub fn is_requested(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Completes when the run has been asked to stop.
    pub async fn requested(&self) {
        self.token.cancelled().await;
    }
}

/// Reacts to termination signals. The first one asks the run to stop: no
/// further certificate is started, and certificates in progress stop at their
/// next step or wait and undo what they did. A second one calls `exit`, which
/// ends the program at once.
///
/// `signals` yields an item for every signal received.
pub async fn watch_signals<S>(mut signals: S, shutdown: Shutdown, log: Logger, exit: impl FnOnce())
where
    S: futures_util::Stream<Item = ()> + Unpin,
{
    use futures_util::StreamExt;

    if signals.next().await.is_none() {
        return;
    }
    log.warn("termination signal received: stopping the run");
    shutdown.request();
    if signals.next().await.is_some() {
        log.error("second termination signal received: exiting at once");
        exit();
    }
}

/// The interrupt and termination signals of the process, as a stream.
pub fn termination_signals() -> std::io::Result<impl futures_util::Stream<Item = ()> + Unpin> {
    use tokio::signal::unix::{SignalKind, signal};

    let interrupt = signal(SignalKind::interrupt())?;
    let terminate = signal(SignalKind::terminate())?;
    Ok(Box::pin(futures_util::stream::unfold(
        (interrupt, terminate),
        |(mut interrupt, mut terminate)| async move {
            let received = tokio::select! {
                received = interrupt.recv() => received,
                received = terminate.recv() => received,
            };
            received.map(|()| ((), (interrupt, terminate)))
        },
    )))
}

/// Time limits of the steps that wait for something outside the program and
/// have no limit of their own.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// For one call to a store, a DNS provider or an ACME server.
    pub call: Duration,
    /// For the ACME server's verdict, and again for the issued certificate.
    pub verdict: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            call: Duration::from_secs(60),
            verdict: Duration::from_secs(180),
        }
    }
}

/// A zone of a DNS provider instance: the instance name and the zone name.
type ZoneKey = (String, String);

/// One lock per DNS zone of a DNS provider instance. A certificate holds the
/// locks of its zones from before it publishes its challenge records until it
/// has removed them, so that two certificates never write to the same zone at
/// the same time.
#[derive(Default)]
pub struct ZoneLocks {
    locks: Mutex<HashMap<ZoneKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl ZoneLocks {
    /// Waits for the lock of `zone` of the DNS provider instance `dns`.
    pub async fn lock(&self, dns: &str, zone: &str) -> OwnedMutexGuard<()> {
        let lock = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry((dns.to_owned(), zone.to_owned()))
            .or_default()
            .clone();
        lock.lock_owned().await
    }
}

/// What every certificate of a run shares.
pub struct RunContext {
    pub providers: Arc<dyn Providers>,
    pub clock: SharedClock,
    pub logger: Logger,
    pub shutdown: Shutdown,
    pub limits: Limits,
    /// `run.min_reissue`.
    pub min_reissue: Duration,
    /// `run.retry_wait_max`.
    pub retry_wait_max: Duration,
    pub zone_locks: ZoneLocks,
}

/// How a certificate ended in a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateOutcome {
    /// Nothing had to be done. `due_at` is when it becomes due.
    NotDue { due_at: OffsetDateTime },
    /// A certificate was issued and written to all its stores.
    Renewed { issued: StoredCert },
    /// The certificate failed at `step`.
    Failed { step: Step, reason: String },
    /// The run was stopped while the certificate was in progress, at `step`.
    Interrupted { step: Step },
}

/// A certificate entry after a run went through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateReport {
    pub name: String,
    pub outcome: CertificateOutcome,
    /// What each store held before the run, in the order of the entry. Empty
    /// when the stores could not all be read.
    pub stored: Vec<Option<StoredCert>>,
    /// Whether the certificate was due.
    pub was_due: bool,
    /// The renewal period that applies to the stored certificate that expires
    /// soonest.
    pub renewal_period: Option<Duration>,
}

impl CertificateReport {
    /// When the stored certificate that expires soonest expires, as it was
    /// before the run.
    pub fn stored_expiry(&self) -> Option<OffsetDateTime> {
        self.stored
            .iter()
            .flatten()
            .map(|certificate| certificate.not_after)
            .min()
    }

    /// When the certificate expires after the run: the new certificate's
    /// expiry when it was renewed, the stored one's otherwise.
    pub fn expiry(&self) -> Option<OffsetDateTime> {
        match &self.outcome {
            CertificateOutcome::Renewed { issued } => Some(issued.not_after),
            _ => self.stored_expiry(),
        }
    }
}

#[cfg(test)]
mod tests;
