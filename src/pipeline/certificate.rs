//! The pipeline of one certificate.
//!
//! A certificate goes through a fixed sequence of steps. A step that changes
//! something outside the program registers how to undo it. When the
//! certificate is done, whether it succeeded, failed or was interrupted, the
//! registered actions run in reverse order.
//!
//! Until the certificate has been issued, a request to stop the run ends the
//! pipeline at the next step or wait. From the moment the ACME server is asked
//! to issue, the pipeline is not interrupted any more: the certificate is
//! downloaded and written to its stores, and the challenge records are
//! removed.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::OwnedMutexGuard;

use super::{CertificateOutcome, CertificateReport, RunContext, Step, UndoStack};
use crate::acme::{Issuer, OrderRequest, PendingOrder};
use crate::clock::format_utc;
use crate::config::{Resolved, format_duration};
use crate::dns::propagation::Published;
use crate::dns::{DnsProvider, group_by_record, resolve_zone};
use crate::logging::{Level, StepLog};
use crate::policy::{
    Decision, Evaluation, Input, RenewalWindow, Severity, StoreState, evaluate,
    lookup_renewal_window,
};
use crate::retry::retry;
use crate::store::{Destination, IssuedCert, StoredCert, write_to_stores};
use crate::{Error, Result};

/// Why the pipeline of a certificate ended before the certificate was stored.
enum Stop {
    Failed { step: Step, error: Error },
    Interrupted { step: Step },
}

/// How the pipeline of a certificate ended when it was not stopped.
enum Done {
    NotDue { due_at: time::OffsetDateTime },
    Renewed(Box<IssuedCert>),
}

/// What the pipeline of a certificate has gathered and done so far.
#[derive(Default)]
struct State {
    stored: Vec<Option<StoredCert>>,
    stores_read: bool,
    was_due: bool,
    renewal_period: Option<Duration>,
    undo: UndoStack,
    /// Held from before the challenge records are published until they have
    /// been removed.
    zone_locks: Vec<OwnedMutexGuard<()>>,
}

/// Takes one certificate entry through the pipeline.
pub(super) async fn process(
    context: &RunContext,
    entry: &Resolved<'_>,
    forced: bool,
) -> CertificateReport {
    let name = entry.certificate.name.as_str();
    let mut state = State::default();
    let result = steps(context, entry, forced, &mut state).await;

    // Undo what was done, last first: remove the challenge records. This
    // runs on every way out and is not interrupted.
    let cleanup = context.logger.step(name, Step::Cleanup.as_str());
    state.undo.unwind(&cleanup).await;
    drop(std::mem::take(&mut state.zone_locks));

    let outcome = match result {
        Ok(Done::NotDue { due_at }) => CertificateOutcome::NotDue { due_at },
        Ok(Done::Renewed(issued)) => CertificateOutcome::Renewed {
            issued: issued.leaf().clone(),
        },
        Err(Stop::Failed { step, error }) => {
            let reason = describe(&error, context);
            context
                .logger
                .step(name, step.as_str())
                .record(Level::Error, "the certificate failed")
                .field("class", error.class())
                .reason(&reason)
                .write();
            CertificateOutcome::Failed { step, reason }
        }
        Err(Stop::Interrupted { step }) => {
            context
                .logger
                .step(name, step.as_str())
                .warn("the run was stopped; the certificate was not completed");
            CertificateOutcome::Interrupted { step }
        }
    };
    CertificateReport {
        name: name.to_owned(),
        outcome,
        stored: if state.stores_read {
            state.stored
        } else {
            Vec::new()
        },
        was_due: state.was_due,
        renewal_period: state.renewal_period,
    }
}

/// The reason of a failure, with the time a rate limit ends when the server
/// stated it.
fn describe(error: &Error, context: &RunContext) -> String {
    match error {
        Error::RateLimited {
            message,
            retry_after: Some(wait),
        } => {
            let ends = context.clock.now() + *wait;
            format!("{message} (the limit ends at {})", format_utc(ends))
        }
        other => other.to_string(),
    }
}

/// Runs `work` as part of `step`: with a time limit when one is given, and
/// ended early when the run is asked to stop.
async fn guarded<T>(
    context: &RunContext,
    step: Step,
    limit: Option<Duration>,
    work: impl Future<Output = Result<T>>,
) -> std::result::Result<T, Stop> {
    tokio::select! {
        biased;
        () = context.shutdown.requested() => Err(Stop::Interrupted { step }),
        result = limited(limit, work) => result.map_err(|error| Stop::Failed { step, error }),
    }
}

/// Runs `work` with a time limit when one is given. It is not ended when the
/// run is asked to stop.
pub(super) async fn limited<T>(
    limit: Option<Duration>,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    match limit {
        None => work.await,
        Some(limit) => match tokio::time::timeout(limit, work).await {
            Ok(result) => result,
            Err(_) => Err(Error::timeout(format!(
                "no answer within {}",
                format_duration(limit)
            ))),
        },
    }
}

/// What the decision for a certificate entry is, and what it was made from.
pub(super) struct Assessment {
    /// For each store, in the order of the entry, the renewal window the ACME
    /// server gave for the certificate it holds.
    pub windows: Vec<Option<RenewalWindow>>,
    pub evaluation: Evaluation,
}

/// Decides whether the entry is due, from what its stores hold. The ACME
/// server is asked for renewal information when that applies; nothing is
/// changed anywhere.
pub(super) async fn assess(
    context: &RunContext,
    entry: &Resolved<'_>,
    forced: bool,
    stored: &[Option<StoredCert>],
    log: &StepLog,
) -> Assessment {
    let holds_any = stored.iter().any(Option::is_some);
    let source = if entry.acme.ari && holds_any {
        context
            .providers
            .renewal_info(&entry.certificate.acme, log)
            .await
    } else {
        None
    };
    let mut windows: Vec<Option<RenewalWindow>> = Vec::with_capacity(stored.len());
    for current in stored {
        windows.push(match current {
            Some(certificate) => lookup_renewal_window(source.as_deref(), certificate, log).await,
            None => None,
        });
    }
    let states: Vec<StoreState<'_>> = entry
        .stores
        .iter()
        .zip(stored)
        .zip(&windows)
        .map(|((store, current), window)| StoreState {
            store: store.name,
            current: current.as_ref(),
            renewal_window: *window,
        })
        .collect();
    let evaluation = evaluate(&Input::for_entry(
        entry,
        context.min_reissue,
        forced,
        &states,
        context.clock.now(),
    ));
    Assessment {
        windows,
        evaluation,
    }
}

async fn steps(
    context: &RunContext,
    entry: &Resolved<'_>,
    forced: bool,
    state: &mut State,
) -> std::result::Result<Done, Stop> {
    let name = entry.certificate.name.as_str();
    let providers = &context.providers;
    let call = Some(context.limits.call);
    let failed = |step: Step| move |error: Error| Stop::Failed { step, error };

    // --- Read what each store holds ------------------------------------------
    let log = context.logger.step(name, Step::ReadStores.as_str());
    let mut stores = Vec::with_capacity(entry.stores.len());
    for store in &entry.stores {
        stores.push(
            providers
                .store(store.name)
                .map_err(failed(Step::ReadStores))?,
        );
    }
    for (store, handle) in entry.stores.iter().zip(&stores) {
        let action = format!("reading the store {}", store.name);
        let current = guarded(
            context,
            Step::ReadStores,
            None,
            retry(&log, &action, || {
                limited(call, handle.current(name, store.format))
            }),
        )
        .await?;
        state.stored.push(current);
    }
    state.stores_read = true;

    // --- Check that what the certificate needs is configured -------------------
    providers
        .check(entry)
        .map_err(failed(Step::Configuration))?;

    // --- Decide ---------------------------------------------------------------------
    let log = context.logger.step(name, Step::Decide.as_str());
    let assessment = async { Ok(assess(context, entry, forced, &state.stored, &log).await) };
    let Assessment {
        windows,
        evaluation,
    } = guarded(context, Step::Decide, None, assessment).await?;
    report_notes(&evaluation, &log);
    state.renewal_period = soonest_renewal_period(&state.stored, &evaluation);

    let reasons = match evaluation.decision {
        Decision::NotDue { due_at } => {
            log.record(Level::Info, "the certificate is not due")
                .field("due_at", format_utc(due_at))
                .write();
            return Ok(Done::NotDue { due_at });
        }
        Decision::HeldBack {
            reasons,
            fresh_store,
            until,
        } => {
            state.was_due = true;
            let reasons: Vec<String> = reasons.iter().map(ToString::to_string).collect();
            return Err(Stop::Failed {
                step: Step::Decide,
                error: Error::permanent(format!(
                    "the certificate is due ({}), but store {fresh_store} holds a certificate \
                     that was issued less than {} ago; issuing becomes possible at {}",
                    reasons.join("; "),
                    format_duration(context.min_reissue),
                    format_utc(until)
                )),
            });
        }
        Decision::Due { reasons } => reasons,
    };
    state.was_due = true;
    log.record(Level::Info, "the certificate is due")
        .field(
            "reasons",
            reasons.iter().map(ToString::to_string).collect::<Vec<_>>(),
        )
        .write();

    // --- Account -----------------------------------------------------------------------
    let log = context.logger.step(name, Step::Account.as_str());
    let issuer = guarded(
        context,
        Step::Account,
        None,
        providers.issuer(&entry.certificate.acme, &log),
    )
    .await?;

    // --- Order ---------------------------------------------------------------------------
    let log = context.logger.step(name, Step::Order.as_str());
    let request = OrderRequest {
        names: entry.certificate.names.clone(),
        profile: entry.profile.map(str::to_owned),
        replaces: predecessor(&state.stored, &windows),
        key_type: entry.key_type,
    };
    let mut order = guarded(
        context,
        Step::Order,
        None,
        place_order(context, &*issuer, &request, &log),
    )
    .await?;
    log.record(Level::Info, "the order is placed")
        .field("names", request.names.clone())
        .field("challenges", order.challenges().len())
        .field(
            "replaces",
            request
                .replaces
                .as_ref()
                .map(|predecessor| predecessor.serial.clone()),
        )
        .write();

    // --- Publish the challenge records, and wait until they are served -------------------
    let records = group_by_record(
        order
            .challenges()
            .iter()
            .map(|challenge| (challenge.dns_name.as_str(), challenge.value.as_str())),
    );
    if !records.is_empty() {
        let dns_name = entry.certificate.dns.as_str();
        let log = context.logger.step(name, Step::Zones.as_str());
        let dns = providers.dns(dns_name).map_err(failed(Step::Zones))?;
        let zones = guarded(
            context,
            Step::Zones,
            None,
            retry(&log, "listing the zones", || limited(call, dns.zones())),
        )
        .await?;
        let mut placed = Vec::with_capacity(records.len());
        for (record, values) in &records {
            let zone = resolve_zone(dns_name, &zones, record).map_err(failed(Step::Zones))?;
            placed.push((zone.clone(), record.clone(), values.clone()));
        }
        // Locks are taken in a fixed order, so that two certificates that
        // span the same zones cannot wait for each other.
        let zone_names: BTreeSet<&str> = placed
            .iter()
            .map(|(zone, _, _)| zone.name.as_str())
            .collect();
        for zone in zone_names {
            let lock = async { Ok(context.zone_locks.lock(dns_name, zone).await) };
            state
                .zone_locks
                .push(guarded(context, Step::Zones, None, lock).await?);
        }

        let log = context.logger.step(name, Step::Present.as_str());
        let mut published = Vec::with_capacity(placed.len());
        for (zone, record, values) in &placed {
            let action = format!("publishing the challenge record {record}");
            let handle = guarded(
                context,
                Step::Present,
                None,
                retry(&log, &action, || {
                    limited(call, dns.present(zone, record, values, entry.dns.ttl))
                }),
            )
            .await?;
            log.record(Level::Info, "the challenge record is published")
                .field("record", record.as_str())
                .field("zone", zone.name.as_str())
                .field("values", values.len())
                .write();
            register_cleanup(&mut state.undo, dns.clone(), handle, context);
            published.push(Published {
                name: record.clone(),
                values: values.clone(),
            });
        }

        let log = context.logger.step(name, Step::Propagation.as_str());
        guarded(
            context,
            Step::Propagation,
            // The check has its own time limit. This one is a safety net.
            Some(entry.propagation_timeout + context.limits.call),
            providers.confirm_propagation(dns_name, entry.propagation_timeout, &published, &log),
        )
        .await?;

        let log = context.logger.step(name, Step::Settle.as_str());
        if !entry.settle.is_zero() {
            log.record(Level::Debug, "waiting before validation is requested")
                .field("settle_seconds", entry.settle.as_secs())
                .write();
        }
        let settle = async {
            tokio::time::sleep(entry.settle).await;
            Ok(())
        };
        guarded(context, Step::Settle, None, settle).await?;
    }

    // --- Validate: once per run -------------------------------------------------------------
    let log = context.logger.step(name, Step::Validate.as_str());
    guarded(
        context,
        Step::Validate,
        Some(context.limits.verdict),
        order.validate(),
    )
    .await?;
    log.info("the ACME server accepted the validation");

    // --- Finalise and download: from here on the certificate is not interrupted ----------------
    let log = context.logger.step(name, Step::Finalize.as_str());
    let issued = limited(Some(context.limits.verdict), order.finalize())
        .await
        .map_err(failed(Step::Finalize))?;
    log.record(Level::Info, "the certificate is issued")
        .field("serial", issued.leaf().serial.as_str())
        .field("expiry", format_utc(issued.leaf().not_after))
        .write();

    // --- Write to every store -------------------------------------------------------------------
    let log = context.logger.step(name, Step::Store.as_str());
    let destinations: Vec<Destination<'_>> = entry
        .stores
        .iter()
        .zip(&stores)
        .map(|(store, handle)| Destination {
            name: store.name,
            store: handle.as_ref(),
            format: store.format,
        })
        .collect();
    write_bounded(context, &log, name, &issued, &destinations)
        .await
        .map_err(failed(Step::Store))?;

    Ok(Done::Renewed(Box::new(issued)))
}

/// Writes the certificate to its stores, each call with the time limit.
async fn write_bounded(
    context: &RunContext,
    log: &StepLog,
    name: &str,
    issued: &IssuedCert,
    destinations: &[Destination<'_>],
) -> Result<()> {
    let bounded: Vec<Bounded<'_>> = destinations
        .iter()
        .map(|destination| Bounded {
            inner: destination.store,
            limit: context.limits.call,
        })
        .collect();
    let destinations: Vec<Destination<'_>> = destinations
        .iter()
        .zip(&bounded)
        .map(|(destination, bounded)| Destination {
            name: destination.name,
            store: bounded,
            format: destination.format,
        })
        .collect();
    write_to_stores(log, name, issued, &destinations).await
}

/// A store whose calls have a time limit.
struct Bounded<'a> {
    inner: &'a dyn crate::store::CertStore,
    limit: Duration,
}

#[async_trait::async_trait]
impl crate::store::CertStore for Bounded<'_> {
    async fn current(
        &self,
        name: &str,
        format: crate::config::Format,
    ) -> Result<Option<StoredCert>> {
        limited(Some(self.limit), self.inner.current(name, format)).await
    }

    async fn put(
        &self,
        name: &str,
        certificate: &IssuedCert,
        format: crate::config::Format,
    ) -> Result<()> {
        limited(Some(self.limit), self.inner.put(name, certificate, format)).await
    }
}

/// Places the order. A transient failure is retried. A rate limit is waited
/// out once, when the server states a wait that is within `run.retry_wait_max`.
async fn place_order(
    context: &RunContext,
    issuer: &dyn Issuer,
    request: &OrderRequest,
    log: &StepLog,
) -> Result<Box<dyn PendingOrder>> {
    let call = Some(context.limits.call);
    let attempt = || {
        retry(log, "placing the order", || {
            limited(call, issuer.order(request))
        })
    };
    match attempt().await {
        Err(Error::RateLimited {
            message,
            retry_after: Some(wait),
        }) if wait <= context.retry_wait_max => {
            log.record(
                Level::Warn,
                "the ACME server limits the rate; waiting as it asks, then trying once more",
            )
            .field("wait_seconds", wait.as_secs())
            .reason(message)
            .write();
            tokio::time::sleep(wait).await;
            attempt().await
        }
        other => other,
    }
}

/// The stored certificate the new one replaces: the most recent one the
/// server gave renewal information for, which shows that the server issued it.
fn predecessor(
    stored: &[Option<StoredCert>],
    windows: &[Option<RenewalWindow>],
) -> Option<StoredCert> {
    stored
        .iter()
        .zip(windows)
        .filter_map(|(certificate, window)| window.and(certificate.as_ref()))
        .max_by_key(|certificate| certificate.not_before)
        .cloned()
}

/// Registers the removal of a published challenge record.
fn register_cleanup(
    undo: &mut UndoStack,
    dns: Arc<dyn DnsProvider>,
    handle: crate::dns::RecordHandle,
    context: &RunContext,
) {
    let description = format!("removing the challenge record {}", handle.name);
    let limit = context.limits.call;
    let log = context.logger.clone();
    let record = handle.name.clone();
    undo.push(description, move || async move {
        let action = format!("removing the challenge record {record}");
        retry(&log, &action, || {
            limited(Some(limit), dns.cleanup(handle.clone()))
        })
        .await
    });
}

/// Writes the notes of the decision, such as a `renew_before` that does not
/// fit the certificate's lifetime, to the log. This happens in every run.
fn report_notes(evaluation: &Evaluation, log: &StepLog) {
    for note in &evaluation.notes {
        let level = match note.severity {
            Severity::Error => Level::Error,
            Severity::Warning => Level::Warn,
        };
        log.record(level, note.message.clone()).write();
    }
}

/// The renewal period of the stored certificate that expires soonest.
fn soonest_renewal_period(
    stored: &[Option<StoredCert>],
    evaluation: &Evaluation,
) -> Option<Duration> {
    stored
        .iter()
        .zip(&evaluation.renewal_periods)
        .filter_map(|(certificate, period)| Some((certificate.as_ref()?.not_after, (*period)?)))
        .min_by_key(|(expiry, _)| *expiry)
        .map(|(_, period)| period)
}
