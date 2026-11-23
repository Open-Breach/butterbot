//! Run events and their delivery to the log and to notifiers.
//!
//! The field names of an event are part of the program's interface: other
//! systems read them. They do not change between versions.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::clock::{format_utc, serialize_optional_utc, serialize_utc};
use crate::logging::{Level, Logger};
use crate::{Error, Result};

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

/// Something events are delivered to.
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn deliver(&self, event: &Event) -> Result<()>;
}

/// The sink that is always present: it writes every event to the log, as a
/// record that contains all the event's fields.
///
/// The record is written whatever the log level is. A monitor that waits for
/// `run_completed` must not miss it because the level was raised.
#[derive(Debug, Clone)]
pub struct LogSink {
    logger: Logger,
}

impl LogSink {
    pub fn new(logger: Logger) -> Self {
        Self { logger }
    }

    fn write(&self, event: &Event) {
        let mut fields = event.to_json();
        // The record's own timestamp is the event's.
        fields.shift_remove("timestamp");
        self.logger
            .record(event.level(), event.summary())
            .at(format_utc(event.timestamp()))
            .fields(fields)
            .always()
            .write();
    }
}

#[async_trait]
impl EventSink for LogSink {
    async fn deliver(&self, event: &Event) -> Result<()> {
        self.write(event);
        Ok(())
    }
}

/// The event kinds a notifier receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Every kind.
    All,
    /// Only the listed kinds.
    Only(BTreeSet<EventKind>),
}

impl Selection {
    pub fn only(kinds: impl IntoIterator<Item = EventKind>) -> Self {
        Self::Only(kinds.into_iter().collect())
    }

    pub fn includes(&self, kind: EventKind) -> bool {
        match self {
            Self::All => true,
            Self::Only(kinds) => kinds.contains(&kind),
        }
    }
}

/// How long the delivery of one event to one notifier may take.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);

struct Notifier {
    name: String,
    selection: Selection,
    sink: Arc<dyn EventSink>,
}

/// Delivers events: always to the log, and to every registered notifier that
/// selected the event's kind.
///
/// This version has no notifier type, so in the program the registry stays
/// empty. It is the extension point for a notification channel: a channel is
/// added by registering a sink, without changing how events are produced.
pub struct EventBus {
    logger: Logger,
    log: LogSink,
    notifiers: Vec<Notifier>,
}

impl EventBus {
    /// A bus that delivers to the log only.
    pub fn new(logger: Logger) -> Self {
        Self {
            log: LogSink::new(logger.clone()),
            logger,
            notifiers: Vec::new(),
        }
    }

    /// Adds the notifier instance `name` (`notify.<name>`).
    pub fn register(&mut self, name: &str, selection: Selection, sink: Arc<dyn EventSink>) {
        self.notifiers.push(Notifier {
            name: name.to_owned(),
            selection,
            sink,
        });
    }

    /// Delivers `event`. A notifier that fails or does not answer in time is
    /// logged and otherwise ignored: delivery never changes the run.
    pub async fn emit(&self, event: Event) {
        self.log.write(&event);
        let kind = event.kind();
        for notifier in self
            .notifiers
            .iter()
            .filter(|notifier| notifier.selection.includes(kind))
        {
            let delivery = tokio::time::timeout(DELIVERY_TIMEOUT, notifier.sink.deliver(&event));
            let failure = match delivery.await {
                Ok(Ok(())) => continue,
                Ok(Err(error)) => error,
                Err(_) => {
                    Error::timeout(format!("no answer after {}s", DELIVERY_TIMEOUT.as_secs()))
                }
            };
            // The field is not called `event`, so that this record is never
            // taken for the event itself.
            self.logger
                .record(
                    Level::Error,
                    "an event could not be delivered to a notifier",
                )
                .field("notifier", notifier.name.as_str())
                .field("undelivered_event", kind.as_str())
                .reason(failure)
                .write();
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

    mod bus {
        use std::sync::Mutex;

        use time::macros::datetime;

        use super::*;
        use crate::logging::{Capture, LogFormat};

        /// A sink that keeps what it receives and can be made to fail or hang.
        #[derive(Default)]
        struct TestSink {
            received: Mutex<Vec<EventKind>>,
            behaviour: Behaviour,
        }

        #[derive(Default)]
        enum Behaviour {
            #[default]
            Accept,
            Fail,
            Hang,
        }

        impl TestSink {
            fn new(behaviour: Behaviour) -> Arc<Self> {
                Arc::new(Self {
                    received: Mutex::default(),
                    behaviour,
                })
            }

            fn received(&self) -> Vec<EventKind> {
                self.received.lock().unwrap().clone()
            }
        }

        #[async_trait]
        impl EventSink for TestSink {
            async fn deliver(&self, event: &Event) -> Result<()> {
                self.received.lock().unwrap().push(event.kind());
                match self.behaviour {
                    Behaviour::Accept => Ok(()),
                    Behaviour::Fail => Err(Error::transient("connection refused")),
                    Behaviour::Hang => std::future::pending().await,
                }
            }
        }

        fn bus(level: Level) -> (Capture, EventBus) {
            let capture = Capture::new();
            let logger = capture.logger(LogFormat::Json, level, datetime!(2026-11-23 02:01:00 UTC));
            (capture, EventBus::new(logger))
        }

        async fn emit_one_of_each(bus: &EventBus) {
            for event in one_of_each() {
                bus.emit(event).await;
            }
        }

        // run-events, "Events are written to the log", scenario "JSON log", and
        // "Notifier extension point", scenario "No notifier configured".
        #[tokio::test]
        async fn without_a_notifier_events_are_written_to_the_log_with_all_their_fields() {
            let (capture, bus) = bus(Level::Info);

            emit_one_of_each(&bus).await;

            let records = capture.json();
            assert_eq!(records.len(), 4);
            for (record, event) in records.iter().zip(one_of_each()) {
                for (name, value) in event.to_json() {
                    assert_eq!(record[&name], value, "{name} of {}", event.kind());
                }
                assert_eq!(record["level"], event.level().as_str());
                assert_eq!(record["message"], event.summary());
            }
            assert_eq!(
                capture.lines()[0],
                r#"{"timestamp":"2026-11-23T02:00:41Z","level":"info","message":"run completed: 15 checked, 2 renewed, 1 failed","event":"run_completed","outcome":"failed","checked":15,"renewed":2,"failed":1,"duration_seconds":41,"soonest_certificate":"api-example-org","soonest_expiry":"2026-12-05T08:15:00Z"}"#
            );
        }

        #[tokio::test]
        async fn events_are_written_to_the_log_at_any_log_level() {
            let (capture, bus) = bus(Level::Error);
            emit_one_of_each(&bus).await;
            let kinds: Vec<_> = capture
                .json()
                .iter()
                .map(|record| record["event"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(kinds, ["run_completed", "failure", "renewed", "expiring"]);
        }

        // run-events, "Notifier extension point", scenario "Notifier with a filter".
        #[tokio::test]
        async fn notifier_receives_only_the_kinds_it_selected() {
            let (_, mut bus) = bus(Level::Info);
            let selective = TestSink::new(Behaviour::Accept);
            let everything = TestSink::new(Behaviour::Accept);
            bus.register(
                "alerts",
                Selection::only([EventKind::Failure, EventKind::Expiring]),
                selective.clone(),
            );
            bus.register("archive", Selection::All, everything.clone());

            emit_one_of_each(&bus).await;

            assert_eq!(
                selective.received(),
                [EventKind::Failure, EventKind::Expiring]
            );
            assert_eq!(everything.received(), EventKind::ALL);
        }

        // run-events, "Delivery failures never change the run", scenario "Notifier unreachable".
        #[tokio::test]
        async fn failing_notifier_is_logged_and_does_not_stop_delivery() {
            let (capture, mut bus) = bus(Level::Info);
            let failing = TestSink::new(Behaviour::Fail);
            let healthy = TestSink::new(Behaviour::Accept);
            bus.register("pager", Selection::All, failing.clone());
            bus.register("archive", Selection::All, healthy.clone());

            // `emit` returns nothing, so a delivery failure has no way to reach the run.
            bus.emit(examples::run_completed_nothing_due()).await;

            assert_eq!(failing.received(), [EventKind::RunCompleted]);
            assert_eq!(
                healthy.received(),
                [EventKind::RunCompleted],
                "the next notifier still gets it"
            );
            let records = capture.json();
            assert_eq!(records.len(), 2);
            assert_eq!(records[0]["event"], "run_completed");
            assert_eq!(
                records[0]["outcome"], "success",
                "the event itself is unchanged"
            );
            assert_eq!(records[1]["level"], "error");
            assert_eq!(records[1]["notifier"], "pager");
            assert_eq!(records[1]["undelivered_event"], "run_completed");
            assert_eq!(records[1]["reason"], "connection refused");
            assert!(
                !records[1].contains_key("event"),
                "the failure record is not an event"
            );
        }

        #[tokio::test(start_paused = true)]
        async fn notifier_that_does_not_answer_is_given_up_after_the_time_limit() {
            let (capture, mut bus) = bus(Level::Info);
            let hanging = TestSink::new(Behaviour::Hang);
            let healthy = TestSink::new(Behaviour::Accept);
            bus.register("pager", Selection::All, hanging);
            bus.register("archive", Selection::All, healthy.clone());

            let started = tokio::time::Instant::now();
            bus.emit(examples::failure()).await;

            assert_eq!(started.elapsed(), DELIVERY_TIMEOUT);
            assert_eq!(healthy.received(), [EventKind::Failure]);
            let records = capture.json();
            assert_eq!(records[1]["notifier"], "pager");
            assert_eq!(records[1]["reason"], "no answer after 30s");
        }

        #[test]
        fn selection_includes_what_it_lists() {
            let selection = Selection::only([EventKind::Failure]);
            assert!(selection.includes(EventKind::Failure));
            assert!(!selection.includes(EventKind::Renewed));
            assert!(
                EventKind::ALL
                    .into_iter()
                    .all(|kind| Selection::All.includes(kind))
            );
        }
    }
}
