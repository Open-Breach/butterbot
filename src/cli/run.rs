//! `butterbot run`: one run over the configured certificates.

use std::sync::Arc;

use super::{Cli, Exit, report_invalid, usage_error};
use crate::clock::SystemClock;
use crate::events::EventBus;
use crate::pipeline::{
    ConfiguredProviders, Limits, RunContext, RunOutcome, Selection, Shutdown, ZoneLocks,
    termination_signals, watch_signals,
};

pub(super) async fn run(cli: &Cli, only: &[String], force: bool) -> Exit {
    let logger = cli.logger();

    // The first termination signal asks the run to stop, the second ends the
    // program at once.
    let shutdown = Shutdown::new();
    match termination_signals() {
        Ok(signals) => {
            tokio::spawn(watch_signals(
                signals,
                shutdown.clone(),
                logger.clone(),
                || std::process::exit(Exit::Interrupted.code().into()),
            ));
        }
        Err(error) => {
            logger.error(format!(
                "the program cannot wait for termination signals: {error}"
            ));
            return Exit::Failed;
        }
    }

    let loaded = tokio::select! {
        biased;
        () = shutdown.requested() => return Exit::Interrupted,
        loaded = cli.load(&logger) => loaded,
    };
    let loaded = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            report_invalid(&logger, &error);
            return Exit::Invalid;
        }
    };
    let config = Arc::new(loaded.config);

    let unknown: Vec<&str> = only
        .iter()
        .filter(|name| {
            !config
                .certificates
                .iter()
                .any(|certificate| &certificate.name == *name)
        })
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return usage_error(format!(
            "--only names a certificate that is not configured: {}",
            unknown.join(", ")
        ));
    }

    let providers = match ConfiguredProviders::new(config.clone(), loaded.vault) {
        Ok(providers) => providers,
        Err(error) => {
            logger.error(format!("invalid configuration: {error}"));
            return Exit::Invalid;
        }
    };
    let context = Arc::new(RunContext {
        providers: Arc::new(providers),
        clock: Arc::new(SystemClock),
        logger: logger.clone(),
        shutdown,
        limits: Limits::default(),
        min_reissue: config.run.min_reissue,
        retry_wait_max: config.run.retry_wait_max,
        zone_locks: ZoneLocks::default(),
    });
    let selection = Selection {
        only: only.to_vec(),
        force,
    };
    let mut events = EventBus::new(logger.clone());
    crate::webhook::register(&config, &mut events, &logger);
    let report = crate::pipeline::run(&context, &config, &selection, &events).await;
    match report.outcome {
        RunOutcome::Success => Exit::Success,
        RunOutcome::Failed => Exit::Failed,
        RunOutcome::Interrupted => Exit::Interrupted,
    }
}
