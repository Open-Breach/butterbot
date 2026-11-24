//! `butterbot notify test`: sends one sample event through a notifier, to
//! check its wiring without waiting for a real failure.
//!
//! The sample goes through the sink a run would build. It does not go through
//! the event bus, so no event that looks real is written to the log.

use super::{Cli, Exit, report_invalid, usage_error};
use crate::clock::{Clock, SystemClock};
use crate::config::NotifierKind;
use crate::events::{DELIVERY_TIMEOUT, Event, EventKind, EventSink};
use crate::webhook::{WebhookSink, warn_unencrypted};

pub(super) async fn test(cli: &Cli, name: &str, kind: EventKind) -> Exit {
    let logger = cli.diagnostics();
    let config = match cli.load(&logger).await {
        Ok(loaded) => loaded.config,
        Err(error) => {
            report_invalid(&logger, &error);
            return Exit::Invalid;
        }
    };
    let Some(instance) = config.notify.get(name) else {
        let known: Vec<&str> = config.notify.keys().map(String::as_str).collect();
        return usage_error(format!(
            "<NAME> names a notifier that is not configured: {name} ({})",
            if known.is_empty() {
                "no notifier is configured".to_owned()
            } else {
                format!("configured: {}", known.join(", "))
            }
        ));
    };
    let NotifierKind::Webhook(webhook) = &instance.kind;
    let sink = match WebhookSink::open(name, webhook, &logger) {
        Ok(sink) => sink,
        Err(error) => {
            logger.error(format!("invalid configuration: {error}"));
            return Exit::Invalid;
        }
    };
    if sink.is_unencrypted() {
        warn_unencrypted(&logger, name);
    }

    let sample = Event::sample(kind, SystemClock.now());
    // The same time limit as in a run.
    let delivery = tokio::time::timeout(DELIVERY_TIMEOUT, sink.deliver(&sample)).await;
    let (text, exit) = match delivery {
        Ok(Ok(())) => (delivered_text(name, kind), Exit::Success),
        Ok(Err(error)) => (
            not_delivered_text(name, kind, &error.to_string()),
            Exit::Failed,
        ),
        Err(_) => (
            not_delivered_text(
                name,
                kind,
                &format!("no answer after {}s", DELIVERY_TIMEOUT.as_secs()),
            ),
            Exit::Failed,
        ),
    };
    print!("{text}");
    if !instance.events.includes(kind) {
        println!(
            "Note: the notifier {name} does not select {kind} events. A run does not send it one."
        );
    }
    exit
}

/// What is printed when the sample was delivered.
pub(super) fn delivered_text(name: &str, kind: EventKind) -> String {
    format!("A {kind} test event was delivered to the notifier {name}.\n")
}

/// What is printed when the sample was not delivered. `reason` is the status
/// the endpoint answered with or the kind of error. It never has the URL.
pub(super) fn not_delivered_text(name: &str, kind: EventKind, reason: &str) -> String {
    format!("A {kind} test event was not delivered to the notifier {name}: {reason}.\n")
}
