//! `butterbot status`: what each store holds and whether each certificate is
//! due. Nothing is changed.

use std::fmt::Write as _;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::{Cli, Exit, report_invalid};
use crate::clock::{SystemClock, format_utc};
use crate::pipeline::{
    CertificateStatus, ConfiguredProviders, Holding, Limits, RunContext, Shutdown, Standing,
    ZoneLocks,
};
use crate::policy::{Reason, Severity};

pub(super) async fn status(cli: &Cli, json: bool) -> Exit {
    let logger = cli.diagnostics();
    let loaded = match cli.load(&logger).await {
        Ok(loaded) => loaded,
        Err(error) => {
            report_invalid(&logger, &error);
            return Exit::Invalid;
        }
    };
    let config = Arc::new(loaded.config);
    // Without the vault: nothing this command does may save anything.
    let providers = match ConfiguredProviders::new(config.clone(), None) {
        Ok(providers) => providers,
        Err(error) => {
            logger.error(format!("invalid configuration: {error}"));
            return Exit::Invalid;
        }
    };
    let context = RunContext {
        providers: Arc::new(providers),
        clock: Arc::new(SystemClock),
        logger,
        shutdown: Shutdown::new(),
        limits: Limits::default(),
        min_reissue: config.run.min_reissue,
        retry_wait_max: config.run.retry_wait_max,
        zone_locks: ZoneLocks::default(),
    };
    let statuses = crate::pipeline::status(&context, &config).await;

    if json {
        println!("{:#}", to_json(&statuses));
    } else {
        print!("{}", to_text(&statuses));
    }
    if statuses.iter().all(CertificateStatus::is_known) {
        Exit::Success
    } else {
        Exit::Failed
    }
}

/// The status as one JSON document, with one entry per certificate.
pub(super) fn to_json(statuses: &[CertificateStatus]) -> Value {
    let certificates: Vec<Value> = statuses.iter().map(certificate_json).collect();
    json!({ "certificates": certificates })
}

fn certificate_json(status: &CertificateStatus) -> Value {
    let stores: Vec<Value> = status
        .stores
        .iter()
        .map(|store| match &store.holding {
            Holding::Nothing => json!({ "store": store.store, "state": "empty" }),
            Holding::Certificate(certificate) => json!({
                "store": store.store,
                "state": "present",
                "names": certificate.names,
                "not_before": format_utc(certificate.not_before),
                "expiry": format_utc(certificate.not_after),
                "issuer": certificate.issuer,
                "serial": certificate.serial,
            }),
            Holding::Unreadable(error) => json!({
                "store": store.store,
                "state": "unreadable",
                "error": error,
            }),
        })
        .collect();

    let mut entry = Map::new();
    entry.insert("name".to_owned(), json!(status.name));
    entry.insert("names".to_owned(), json!(status.names));
    entry.insert("stores".to_owned(), json!(stores));
    match &status.standing {
        Standing::Due { reasons } => {
            entry.insert("state".to_owned(), json!("due"));
            entry.insert("reasons".to_owned(), reasons_json(reasons));
        }
        Standing::NotDue { due_at } => {
            entry.insert("state".to_owned(), json!("not_due"));
            entry.insert("due_at".to_owned(), json!(format_utc(*due_at)));
        }
        Standing::HeldBack {
            reasons,
            fresh_store,
            until,
        } => {
            entry.insert("state".to_owned(), json!("held_back"));
            entry.insert("reasons".to_owned(), reasons_json(reasons));
            entry.insert("held_back_by".to_owned(), json!(fresh_store));
            entry.insert("held_back_until".to_owned(), json!(format_utc(*until)));
        }
        Standing::Unknown => {
            entry.insert("state".to_owned(), json!("unknown"));
        }
    }
    if !status.notes.is_empty() {
        let notes: Vec<Value> = status
            .notes
            .iter()
            .map(|note| {
                json!({
                    "severity": severity(note.severity),
                    "message": note.message,
                })
            })
            .collect();
        entry.insert("notes".to_owned(), json!(notes));
    }
    Value::Object(entry)
}

fn reasons_json(reasons: &[Reason]) -> Value {
    reasons
        .iter()
        .map(|reason| json!({ "code": reason.code(), "text": reason.to_string() }))
        .collect()
}

fn severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

/// The status as text, for a person.
pub(super) fn to_text(statuses: &[CertificateStatus]) -> String {
    let mut text = String::new();
    if statuses.is_empty() {
        text.push_str("No certificate is configured.\n");
    }
    for (index, status) in statuses.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        let _ = writeln!(text, "{}: {}", status.name, headline(&status.standing));
        let _ = writeln!(text, "  names: {}", status.names.join(", "));
        for store in &status.stores {
            let holds = match &store.holding {
                Holding::Nothing => "nothing".to_owned(),
                Holding::Certificate(certificate) => format!(
                    "a certificate for {}, expires {}",
                    certificate
                        .names
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                    format_utc(certificate.not_after)
                ),
                Holding::Unreadable(error) => format!("cannot be read: {error}"),
            };
            let _ = writeln!(text, "  store {}: {holds}", store.store);
        }
        if let Standing::Due { reasons } | Standing::HeldBack { reasons, .. } = &status.standing {
            for reason in reasons {
                let _ = writeln!(text, "  due because {reason}");
            }
        }
        if let Standing::HeldBack { fresh_store, .. } = &status.standing {
            let _ = writeln!(
                text,
                "  held back because store {fresh_store} holds a certificate that was issued \
                 only just"
            );
        }
        for note in &status.notes {
            let _ = writeln!(text, "  {}: {}", severity(note.severity), note.message);
        }
    }
    text
}

fn headline(standing: &Standing) -> String {
    match standing {
        Standing::Due { .. } => "due".to_owned(),
        Standing::NotDue { due_at } => format!("not due, becomes due {}", format_utc(*due_at)),
        Standing::HeldBack { until, .. } => {
            format!("due, but not issued before {}", format_utc(*until))
        }
        Standing::Unknown => "unknown, because a store cannot be read".to_owned(),
    }
}
