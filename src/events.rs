//! Run events and their delivery to the log and to notifiers.
//!
//! The field names of an event are part of the program's interface: other
//! systems read them. They do not change between versions.

use std::fmt;
use std::str::FromStr;

use serde::Serialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::clock::{format_utc, serialize_optional_utc, serialize_utc};
use crate::logging::Level;

/// The kinds of event a run emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventKind {
    RunCompleted,
    Failure,
    Renewed,
    Expiring,
}

impl EventKind {
    pub const ALL: [Self; 4] = [
        Self::RunCompleted,
        Self::Failure,
        Self::Renewed,
        Self::Expiring,
    ];

    /// The name of the kind as it appears in the `event` field and in configuration.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunCompleted => "run_completed",
            Self::Failure => "failure",
            Self::Renewed => "renewed",
            Self::Expiring => "expiring",
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EventKind {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text).ok_or_else(|| {
            format!("unknown event {text:?} (known events: run_completed, failure, renewed, expiring)")
        })
    }
}

/// An event of a run.
///
/// Serialised, an event is a JSON object whose `event` member is the kind and
/// whose `timestamp` member is the time of the event in UTC.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    RunCompleted(RunCompleted),
    Failure(Failure),
    Renewed(Renewed),
    Expiring(Expiring),
}

/// A run reached its end. Emitted exactly once per finished run, also when
/// nothing was due and when certificates failed; never for a run that was
/// interrupted or that ended with a configuration error.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunCompleted {
    #[serde(serialize_with = "serialize_utc")]
    pub timestamp: OffsetDateTime,
    pub outcome: CompletedOutcome,
    /// How many certificates were checked.
    pub checked: u32,
    /// How many were issued and written to all their stores.
    pub renewed: u32,
    /// How many failed.
    pub failed: u32,
    /// How long the run took, in whole seconds.
    pub duration_seconds: u64,
    /// The configured certificate that expires soonest, or `null` when no
    /// store holds a certificate.
    pub soonest_certificate: Option<String>,
    /// When that certificate expires.
    #[serde(serialize_with = "serialize_optional_utc")]
    pub soonest_expiry: Option<OffsetDateTime>,
}

/// The outcome of a run that reached its end. An interrupted run has no
/// `run_completed` event, so there is no third value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletedOutcome {
    /// Every checked certificate was either not due or renewed.
    Success,
    /// At least one certificate failed.
    Failed,
}

/// A certificate failed in this run. Emitted once per failed certificate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Failure {
    #[serde(serialize_with = "serialize_utc")]
    pub timestamp: OffsetDateTime,
    pub certificate: String,
    /// The step that failed, for example `propagation` or `validate`.
    pub step: String,
    pub reason: String,
    /// When the certificate currently stored expires, or `null` when none is stored.
    #[serde(serialize_with = "serialize_optional_utc")]
    pub stored_expiry: Option<OffsetDateTime>,
}

/// A certificate was issued and written to all its stores.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Renewed {
    #[serde(serialize_with = "serialize_utc")]
    pub timestamp: OffsetDateTime,
    pub certificate: String,
    /// The DNS names the new certificate covers.
    pub names: Vec<String>,
    /// When the new certificate expires.
    #[serde(serialize_with = "serialize_utc")]
    pub expiry: OffsetDateTime,
    /// The ACME server instance that issued it.
    pub acme: String,
    /// The store instances it was written to.
    pub stores: Vec<String>,
}

/// A certificate was due, was not renewed in this run, and its stored
/// certificate has less than half of its renewal period left or has expired.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Expiring {
    #[serde(serialize_with = "serialize_utc")]
    pub timestamp: OffsetDateTime,
    pub certificate: String,
    /// When the stored certificate expires or expired.
    #[serde(serialize_with = "serialize_utc")]
    pub expiry: OffsetDateTime,
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Self::RunCompleted(_) => EventKind::RunCompleted,
            Self::Failure(_) => EventKind::Failure,
            Self::Renewed(_) => EventKind::Renewed,
            Self::Expiring(_) => EventKind::Expiring,
        }
    }

    pub fn timestamp(&self) -> OffsetDateTime {
        match self {
            Self::RunCompleted(event) => event.timestamp,
            Self::Failure(event) => event.timestamp,
            Self::Renewed(event) => event.timestamp,
            Self::Expiring(event) => event.timestamp,
        }
    }

    /// The event as a JSON object with all its fields.
    pub fn to_json(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(object)) => object,
            // An event is a struct of strings, numbers and lists of strings,
            // which always serialises to an object.
            _ => Map::new(),
        }
    }

    /// The severity with which the event is written to the log.
    pub fn level(&self) -> Level {
        match self {
            Self::RunCompleted(_) | Self::Renewed(_) => Level::Info,
            Self::Expiring(_) => Level::Warn,
            Self::Failure(_) => Level::Error,
        }
    }

    /// The event in one sentence, for the message of its log record.
    pub fn summary(&self) -> String {
        match self {
            Self::RunCompleted(event) => format!(
                "run completed: {} checked, {} renewed, {} failed",
                event.checked, event.renewed, event.failed
            ),
            Self::Failure(event) => {
                format!(
                    "certificate {} failed at step {}",
                    event.certificate, event.step
                )
            }
            Self::Renewed(event) => format!(
                "certificate {} renewed, expires {}",
                event.certificate,
                format_utc(event.expiry)
            ),
            Self::Expiring(event) => format!(
                "certificate {} was not renewed and expires {}",
                event.certificate,
                format_utc(event.expiry)
            ),
        }
    }
}

/// One event of each kind, as used in the tests and in `docs/events.md`.
#[cfg(test)]
pub(crate) mod examples {
    use time::macros::datetime;

    use super::*;

    pub(crate) fn run_completed() -> Event {
        Event::RunCompleted(RunCompleted {
            timestamp: datetime!(2026-11-23 02:00:41 UTC),
            outcome: CompletedOutcome::Failed,
            checked: 15,
            renewed: 2,
            failed: 1,
            duration_seconds: 41,
            soonest_certificate: Some("api-example-org".into()),
            soonest_expiry: Some(datetime!(2026-12-05 08:15:00 UTC)),
        })
    }

    pub(crate) fn run_completed_nothing_due() -> Event {
        Event::RunCompleted(RunCompleted {
            timestamp: datetime!(2026-11-23 02:00:03 UTC),
            outcome: CompletedOutcome::Success,
            checked: 0,
            renewed: 0,
            failed: 0,
            duration_seconds: 3,
            soonest_certificate: None,
            soonest_expiry: None,
        })
    }

    pub(crate) fn failure() -> Event {
        Event::Failure(Failure {
            timestamp: datetime!(2026-11-23 02:00:38 UTC),
            certificate: "api-example-org".into(),
            step: "validate".into(),
            reason: "the server rejected the challenge for api.example.org: incorrect TXT record"
                .into(),
            stored_expiry: Some(datetime!(2026-12-05 08:15:00 UTC)),
        })
    }

    pub(crate) fn failure_nothing_stored() -> Event {
        Event::Failure(Failure {
            timestamp: datetime!(2026-11-23 02:00:38 UTC),
            certificate: "new-example-net".into(),
            step: "propagation".into(),
            reason: "the challenge records were not served after 5m".into(),
            stored_expiry: None,
        })
    }

    pub(crate) fn renewed() -> Event {
        Event::Renewed(Renewed {
            timestamp: datetime!(2026-11-23 02:00:35 UTC),
            certificate: "wildcard-example-com".into(),
            names: vec!["*.example.com".into(), "example.com".into()],
            expiry: datetime!(2027-02-21 01:00:34 UTC),
            acme: "letsencrypt".into(),
            stores: vec!["vault".into(), "local".into()],
        })
    }

    pub(crate) fn expiring() -> Event {
        Event::Expiring(Expiring {
            timestamp: datetime!(2026-11-23 02:00:38 UTC),
            certificate: "api-example-org".into(),
            expiry: datetime!(2026-12-05 08:15:00 UTC),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(event: &Event) -> String {
        serde_json::to_string(event).expect("an event serialises")
    }

    // Snapshots. A change to any of these strings changes the interface that
    // other systems read; `docs/events.md` shows the same events.

    // run-events, "Run completed on every finished run", scenario "Run with failures".
    #[test]
    fn snapshot_run_completed() {
        assert_eq!(
            json(&examples::run_completed()),
            r#"{"event":"run_completed","timestamp":"2026-11-23T02:00:41Z","outcome":"failed","checked":15,"renewed":2,"failed":1,"duration_seconds":41,"soonest_certificate":"api-example-org","soonest_expiry":"2026-12-05T08:15:00Z"}"#
        );
    }

    // run-events, "Run completed on every finished run", scenario "Nothing was due".
    #[test]
    fn snapshot_run_completed_when_nothing_is_stored() {
        assert_eq!(
            json(&examples::run_completed_nothing_due()),
            r#"{"event":"run_completed","timestamp":"2026-11-23T02:00:03Z","outcome":"success","checked":0,"renewed":0,"failed":0,"duration_seconds":3,"soonest_certificate":null,"soonest_expiry":null}"#
        );
    }

    // run-events, "Failure event", scenario "Validation failure".
    #[test]
    fn snapshot_failure() {
        assert_eq!(
            json(&examples::failure()),
            r#"{"event":"failure","timestamp":"2026-11-23T02:00:38Z","certificate":"api-example-org","step":"validate","reason":"the server rejected the challenge for api.example.org: incorrect TXT record","stored_expiry":"2026-12-05T08:15:00Z"}"#
        );
    }

    #[test]
    fn snapshot_failure_when_nothing_is_stored() {
        assert_eq!(
            json(&examples::failure_nothing_stored()),
            r#"{"event":"failure","timestamp":"2026-11-23T02:00:38Z","certificate":"new-example-net","step":"propagation","reason":"the challenge records were not served after 5m","stored_expiry":null}"#
        );
    }

    // run-events, "Renewed event", scenario "Successful renewal".
    #[test]
    fn snapshot_renewed() {
        assert_eq!(
            json(&examples::renewed()),
            r#"{"event":"renewed","timestamp":"2026-11-23T02:00:35Z","certificate":"wildcard-example-com","names":["*.example.com","example.com"],"expiry":"2027-02-21T01:00:34Z","acme":"letsencrypt","stores":["vault","local"]}"#
        );
    }

    #[test]
    fn snapshot_expiring() {
        assert_eq!(
            json(&examples::expiring()),
            r#"{"event":"expiring","timestamp":"2026-11-23T02:00:38Z","certificate":"api-example-org","expiry":"2026-12-05T08:15:00Z"}"#
        );
    }

    fn one_of_each() -> [Event; 4] {
        [
            examples::run_completed(),
            examples::failure(),
            examples::renewed(),
            examples::expiring(),
        ]
    }

    // run-events, "Event model", scenario "Event shape".
    #[test]
    fn every_event_has_its_kind_and_a_utc_timestamp() {
        for (event, kind) in one_of_each().iter().zip(EventKind::ALL) {
            assert_eq!(event.kind(), kind);
            let object = event.to_json();
            assert_eq!(object["event"], kind.as_str());
            let timestamp = object["timestamp"].as_str().expect("the timestamp is text");
            assert_eq!(timestamp, format_utc(event.timestamp()));
            assert!(
                timestamp.ends_with('Z') && timestamp.len() == 20,
                "{timestamp}"
            );
            let mut keys = object.keys();
            assert_eq!(keys.next().map(String::as_str), Some("event"));
            assert_eq!(keys.next().map(String::as_str), Some("timestamp"));
        }
    }

    #[test]
    fn kind_names_are_stable() {
        let names: Vec<_> = EventKind::ALL.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(names, ["run_completed", "failure", "renewed", "expiring"]);
        for kind in EventKind::ALL {
            assert_eq!(kind.as_str().parse::<EventKind>(), Ok(kind));
        }
        assert!("renewal".parse::<EventKind>().is_err());
    }

    #[test]
    fn failures_are_errors_and_expiring_is_a_warning() {
        let levels: Vec<_> = one_of_each().iter().map(Event::level).collect();
        assert_eq!(
            levels,
            [Level::Info, Level::Error, Level::Info, Level::Warn]
        );
    }
}
