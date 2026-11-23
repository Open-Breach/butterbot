//! The run: every selected certificate goes through its pipeline, a bounded
//! number at a time, and the results become the outcome of the run.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use time::OffsetDateTime;

use super::certificate::process;
use super::{CertificateOutcome, CertificateReport, RunContext};
use crate::clock::format_utc;
use crate::config::Config;
use crate::events::{CompletedOutcome, Event, EventBus, Expiring, Failure, Renewed, RunCompleted};
use crate::logging::Level;
use crate::policy::is_expiring;

/// Which certificates a run goes through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// The names given with `--only`. Empty means every configured certificate.
    pub only: Vec<String>,
    /// Whether the selected certificates are treated as due (`--force`).
    pub force: bool,
}

/// How a run ended. The outcome determines the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Every evaluated certificate was either not due or renewed.
    Success,
    /// At least one certificate failed.
    Failed,
    /// The run was stopped by a signal.
    Interrupted,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

/// What a run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub outcome: RunOutcome,
    /// The certificates that were evaluated, in the order of the
    /// configuration. A certificate that was not started because the run was
    /// stopped is not in the list.
    pub certificates: Vec<CertificateReport>,
    pub duration: Duration,
}

impl RunReport {
    pub fn evaluated(&self) -> usize {
        self.certificates.len()
    }

    pub fn renewed(&self) -> usize {
        self.count(|outcome| matches!(outcome, CertificateOutcome::Renewed { .. }))
    }

    pub fn failed(&self) -> usize {
        self.count(|outcome| matches!(outcome, CertificateOutcome::Failed { .. }))
    }

    fn count(&self, matches: impl Fn(&CertificateOutcome) -> bool) -> usize {
        self.certificates
            .iter()
            .filter(|report| matches(&report.outcome))
            .count()
    }

    /// The evaluated certificate that expires soonest, with its expiry.
    pub fn soonest(&self) -> Option<(&str, OffsetDateTime)> {
        self.certificates
            .iter()
            .filter_map(|report| Some((report.name.as_str(), report.expiry()?)))
            .min_by_key(|(_, expiry)| *expiry)
    }
}

/// Performs one run.
///
/// `selection.only` has been checked against the configuration by the caller:
/// a name that is not configured is ignored here.
pub async fn run(
    context: &Arc<RunContext>,
    config: &Config,
    selection: &Selection,
    events: &EventBus,
) -> RunReport {
    let started = context.clock.now();
    let selected: Vec<_> = config
        .certificates
        .iter()
        .filter(|certificate| {
            selection.only.is_empty() || selection.only.contains(&certificate.name)
        })
        .collect();
    context
        .logger
        .record(Level::Info, "run started")
        .field("certificates", selected.len())
        .field("parallel", config.run.parallel)
        .write();

    // Up to `run.parallel` certificates are in their pipeline at a time. Once
    // the run is asked to stop, no further certificate is started.
    let mut finished: Vec<(usize, CertificateReport)> =
        futures_util::stream::iter(selected.iter().enumerate())
            .map(|(index, certificate)| async move {
                if context.shutdown.is_requested() {
                    return None;
                }
                let entry = config.resolve(certificate);
                let report = process(context, &entry, selection.force).await;
                emit_for_certificate(context, events, &entry, &report).await;
                Some((index, report))
            })
            .buffer_unordered(config.run.parallel.max(1))
            .filter_map(|finished| async move { finished })
            .collect()
            .await;
    finished.sort_by_key(|(index, _)| *index);
    let certificates: Vec<CertificateReport> =
        finished.into_iter().map(|(_, report)| report).collect();

    let interrupted = context.shutdown.is_requested();
    let any_failed = certificates
        .iter()
        .any(|report| matches!(report.outcome, CertificateOutcome::Failed { .. }));
    let outcome = if interrupted {
        RunOutcome::Interrupted
    } else if any_failed {
        RunOutcome::Failed
    } else {
        RunOutcome::Success
    };
    let finished_at = context.clock.now();
    let duration = Duration::try_from(finished_at - started).unwrap_or_default();
    let report = RunReport {
        outcome,
        certificates,
        duration,
    };

    let soonest = report.soonest();
    context
        .logger
        .record(
            if interrupted {
                Level::Warn
            } else {
                Level::Info
            },
            format!(
                "run {}: {} evaluated, {} renewed, {} failed",
                if interrupted {
                    "interrupted"
                } else {
                    "finished"
                },
                report.evaluated(),
                report.renewed(),
                report.failed()
            ),
        )
        .field("outcome", outcome.as_str())
        .field("evaluated", report.evaluated())
        .field("renewed", report.renewed())
        .field("failed", report.failed())
        .field(
            "soonest_certificate",
            soonest.map(|(name, _)| name.to_owned()),
        )
        .field(
            "soonest_expiry",
            soonest.map(|(_, expiry)| format_utc(expiry)),
        )
        .write();

    // The completion event says that a run reached its end. An interrupted
    // run did not, so it has none.
    if !interrupted {
        events
            .emit(Event::RunCompleted(RunCompleted {
                timestamp: finished_at,
                outcome: if any_failed {
                    CompletedOutcome::Failed
                } else {
                    CompletedOutcome::Success
                },
                checked: count(report.evaluated()),
                renewed: count(report.renewed()),
                failed: count(report.failed()),
                duration_seconds: duration.as_secs(),
                soonest_certificate: soonest.map(|(name, _)| name.to_owned()),
                soonest_expiry: soonest.map(|(_, expiry)| expiry),
            }))
            .await;
    }
    report
}

fn count(number: usize) -> u32 {
    u32::try_from(number).unwrap_or(u32::MAX)
}

/// Emits the events of one certificate when it is done: `renewed`, or
/// `failure` and, when its stored certificate is close to expiry, `expiring`.
async fn emit_for_certificate(
    context: &RunContext,
    events: &EventBus,
    entry: &crate::config::Resolved<'_>,
    report: &CertificateReport,
) {
    let now = context.clock.now();
    match &report.outcome {
        CertificateOutcome::Renewed { issued } => {
            events
                .emit(Event::Renewed(Renewed {
                    timestamp: now,
                    certificate: report.name.clone(),
                    names: issued.names.iter().cloned().collect(),
                    expiry: issued.not_after,
                    acme: entry.certificate.acme.clone(),
                    stores: entry.certificate.stores.clone(),
                }))
                .await;
        }
        CertificateOutcome::Failed { step, reason } => {
            events
                .emit(Event::Failure(Failure {
                    timestamp: now,
                    certificate: report.name.clone(),
                    step: step.as_str().to_owned(),
                    reason: reason.clone(),
                    stored_expiry: report.stored_expiry(),
                }))
                .await;
            // A certificate that was due and was not renewed, and whose
            // stored certificate has less than half its renewal period left.
            if let (true, Some(expiry), Some(period)) = (
                report.was_due,
                report.stored_expiry(),
                report.renewal_period,
            ) && is_expiring(expiry, period, now)
            {
                events
                    .emit(Event::Expiring(Expiring {
                        timestamp: now,
                        certificate: report.name.clone(),
                        expiry,
                    }))
                    .await;
            }
        }
        CertificateOutcome::NotDue { .. } | CertificateOutcome::Interrupted { .. } => {}
    }
}
