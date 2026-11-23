//! The log: one record per line on standard output, as JSON or as text.
//!
//! A [`Logger`] is handed to whatever needs to log. A record about a
//! certificate is written through a [`StepLog`], which adds the certificate's
//! name and the step being performed to every record, so that neither can be
//! forgotten.

use std::fmt;
use std::io::Write;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use crate::clock::{SharedClock, SystemClock, format_utc_millis};

/// How records are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LogFormat {
    /// One JSON object per line.
    Json,
    /// Human-readable text.
    Text,
}

impl LogFormat {
    /// The format to use: the one asked for, or else text on a terminal and
    /// JSON everywhere else.
    pub fn choose(asked_for: Option<Self>, output_is_terminal: bool) -> Self {
        match asked_for {
            Some(format) => format,
            None if output_is_terminal => Self::Text,
            None => Self::Json,
        }
    }
}

/// The severity of a record, most severe first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Something records can be written to: the [`Logger`] itself, or the
/// [`StepLog`] of a certificate.
pub trait Log: Send + Sync {
    /// Starts a record. Nothing is written until [`Record::write`] is called.
    fn record(&self, level: Level, message: String) -> Record<'_>;
}

impl Log for Logger {
    fn record(&self, level: Level, message: String) -> Record<'_> {
        Logger::record(self, level, message)
    }
}

impl Log for StepLog {
    fn record(&self, level: Level, message: String) -> Record<'_> {
        StepLog::record(self, level, message)
    }
}

/// The keys every record has. A field cannot use them.
const RESERVED: [&str; 3] = ["timestamp", "level", "message"];

/// Writes log records. Cheap to clone; all clones write to the same output.
#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}

struct Inner {
    format: LogFormat,
    level: Level,
    clock: SharedClock,
    output: Mutex<Box<dyn Write + Send>>,
}

impl Logger {
    /// A logger that writes to standard output.
    pub fn stdout(format: LogFormat, level: Level) -> Self {
        Self::new(
            format,
            level,
            Arc::new(SystemClock),
            Box::new(std::io::stdout()),
        )
    }

    /// A logger that writes to standard error, for the commands whose
    /// standard output is their result.
    pub fn stderr(format: LogFormat, level: Level) -> Self {
        Self::new(
            format,
            level,
            Arc::new(SystemClock),
            Box::new(std::io::stderr()),
        )
    }

    pub fn new(
        format: LogFormat,
        level: Level,
        clock: SharedClock,
        output: Box<dyn Write + Send>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                format,
                level,
                clock,
                output: Mutex::new(output),
            }),
        }
    }

    /// Starts a record. Nothing is written until [`Record::write`] is called.
    pub fn record(&self, level: Level, message: impl Into<String>) -> Record<'_> {
        Record {
            logger: self,
            level,
            message: message.into(),
            timestamp: None,
            always: false,
            fields: Map::new(),
        }
    }

    pub fn error(&self, message: impl Into<String>) {
        self.record(Level::Error, message).write();
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.record(Level::Warn, message).write();
    }

    pub fn info(&self, message: impl Into<String>) {
        self.record(Level::Info, message).write();
    }

    pub fn debug(&self, message: impl Into<String>) {
        self.record(Level::Debug, message).write();
    }

    /// A log for one step of one certificate.
    pub fn step(&self, certificate: &str, step: &str) -> StepLog {
        StepLog {
            logger: self.clone(),
            certificate: certificate.to_owned(),
            step: step.to_owned(),
        }
    }

    fn emit(&self, record: Record<'_>) {
        if !record.always && record.level > self.inner.level {
            return;
        }
        let timestamp = record
            .timestamp
            .unwrap_or_else(|| format_utc_millis(self.inner.clock.now()));
        let line = match self.inner.format {
            LogFormat::Json => json_line(&timestamp, record.level, &record.message, record.fields),
            LogFormat::Text => text_line(&timestamp, record.level, &record.message, &record.fields),
        };
        // A log line that cannot be written must not stop the run.
        if let Ok(mut output) = self.inner.output.lock() {
            let _ = output.write_all(line.as_bytes());
            let _ = output.flush();
        }
    }
}

impl fmt::Debug for Logger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Logger")
            .field("format", &self.inner.format)
            .field("level", &self.inner.level)
            .finish_non_exhaustive()
    }
}

/// A record that is being put together.
#[must_use = "a record is not written until `write` is called"]
pub struct Record<'a> {
    logger: &'a Logger,
    level: Level,
    message: String,
    timestamp: Option<String>,
    always: bool,
    fields: Map<String, Value>,
}

impl Record<'_> {
    /// Adds a field. A field named like one of the keys every record has is
    /// written with a leading underscore.
    pub fn field(mut self, name: &str, value: impl Into<Value>) -> Self {
        let name = if RESERVED.contains(&name) {
            format!("_{name}")
        } else {
            name.to_owned()
        };
        self.fields.insert(name, value.into());
        self
    }

    /// Adds every member of a JSON object as a field.
    pub fn fields(mut self, fields: Map<String, Value>) -> Self {
        for (name, value) in fields {
            self = self.field(&name, value);
        }
        self
    }

    /// Adds the reason of a failure.
    pub fn reason(self, reason: impl fmt::Display) -> Self {
        self.field("reason", reason.to_string())
    }

    /// Uses `timestamp` in place of the time of writing.
    pub fn at(mut self, timestamp: String) -> Self {
        self.timestamp = Some(timestamp);
        self
    }

    /// Writes the record whatever the log level is.
    pub fn always(mut self) -> Self {
        self.always = true;
        self
    }

    pub fn write(self) {
        self.logger.emit(self);
    }
}

/// The log of one step of one certificate. Every record carries the
/// certificate's name and the step.
#[derive(Debug, Clone)]
pub struct StepLog {
    logger: Logger,
    certificate: String,
    step: String,
}

impl StepLog {
    pub fn certificate(&self) -> &str {
        &self.certificate
    }

    pub fn step(&self) -> &str {
        &self.step
    }

    /// The log of another step of the same certificate.
    pub fn for_step(&self, step: &str) -> Self {
        Self {
            logger: self.logger.clone(),
            certificate: self.certificate.clone(),
            step: step.to_owned(),
        }
    }

    /// Starts a record that carries the certificate and the step.
    pub fn record(&self, level: Level, message: impl Into<String>) -> Record<'_> {
        self.logger
            .record(level, message)
            .field("certificate", self.certificate.as_str())
            .field("step", self.step.as_str())
    }

    pub fn error(&self, message: impl Into<String>) {
        self.record(Level::Error, message).write();
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.record(Level::Warn, message).write();
    }

    pub fn info(&self, message: impl Into<String>) {
        self.record(Level::Info, message).write();
    }

    pub fn debug(&self, message: impl Into<String>) {
        self.record(Level::Debug, message).write();
    }

    /// Writes the failure of the step, with its reason.
    pub fn failure(&self, message: impl Into<String>, reason: impl fmt::Display) {
        self.record(Level::Error, message).reason(reason).write();
    }
}

fn json_line(timestamp: &str, level: Level, message: &str, fields: Map<String, Value>) -> String {
    let mut object = Map::with_capacity(fields.len() + RESERVED.len());
    object.insert("timestamp".into(), timestamp.into());
    object.insert("level".into(), level.as_str().into());
    object.insert("message".into(), message.into());
    object.extend(fields);
    // Serialising a JSON value to a string escapes every line break, so the
    // record is one line whatever its content.
    let mut line = Value::Object(object).to_string();
    line.push('\n');
    line
}

fn text_line(timestamp: &str, level: Level, message: &str, fields: &Map<String, Value>) -> String {
    let mut line = format!("{timestamp} {:5} {message}", level.as_str().to_uppercase());
    for (name, value) in fields {
        match value {
            Value::String(text) if is_plain(text) => line.push_str(&format!(" {name}={text}")),
            other => line.push_str(&format!(" {name}={other}")),
        }
    }
    line.push('\n');
    line
}

/// Whether a text value can be shown without quotes.
fn is_plain(text: &str) -> bool {
    !text.is_empty()
        && !text
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '=')
}

/// A log output that keeps what is written, for tests.
#[derive(Clone, Default)]
pub struct Capture {
    written: Arc<Mutex<Vec<u8>>>,
}

impl Capture {
    pub fn new() -> Self {
        Self::default()
    }

    /// A logger that writes into this capture, with a clock fixed at `now`.
    pub fn logger(&self, format: LogFormat, level: Level, now: time::OffsetDateTime) -> Logger {
        Logger::new(
            format,
            level,
            Arc::new(crate::clock::FixedClock(now)),
            Box::new(self.clone()),
        )
    }

    /// Everything written so far.
    pub fn text(&self) -> String {
        let written = self
            .written
            .lock()
            .map(|bytes| bytes.clone())
            .unwrap_or_default();
        String::from_utf8_lossy(&written).into_owned()
    }

    /// The lines written so far.
    pub fn lines(&self) -> Vec<String> {
        self.text().lines().map(str::to_owned).collect()
    }

    /// The lines written so far, each read as a JSON object.
    ///
    /// # Panics
    ///
    /// If a line is not a JSON object.
    pub fn json(&self) -> Vec<Map<String, Value>> {
        self.lines()
            .iter()
            .map(|line| match serde_json::from_str(line) {
                Ok(Value::Object(object)) => object,
                other => panic!("not a JSON object: {line:?} ({other:?})"),
            })
            .collect()
    }
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut written) = self.written.lock() {
            written.extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    const NOW: time::OffsetDateTime = datetime!(2026-11-23 02:00:00.25 UTC);

    fn capture(format: LogFormat, level: Level) -> (Capture, Logger) {
        let capture = Capture::new();
        let logger = capture.logger(format, level, NOW);
        (capture, logger)
    }

    // command-line, "Log format", scenarios "In a container" and "In a terminal".
    #[test]
    fn format_is_json_unless_the_output_is_a_terminal() {
        assert_eq!(LogFormat::choose(None, false), LogFormat::Json);
        assert_eq!(LogFormat::choose(None, true), LogFormat::Text);
    }

    #[test]
    fn format_that_is_asked_for_overrides_the_choice() {
        assert_eq!(
            LogFormat::choose(Some(LogFormat::Text), false),
            LogFormat::Text
        );
        assert_eq!(
            LogFormat::choose(Some(LogFormat::Json), true),
            LogFormat::Json
        );
    }

    // command-line, "Log format", scenario "In a container".
    #[test]
    fn json_output_is_one_object_per_line() {
        let (capture, logger) = capture(LogFormat::Json, Level::Debug);

        logger.info("run started");
        logger.warn("a message\nover two lines");
        logger
            .record(Level::Error, "with fields")
            .field("text", "line one\nline two\r\nline three")
            .field("number", 3)
            .field("flag", true)
            .field("list", vec!["a", "b"])
            .write();
        logger
            .step("wildcard-example-com", "propagation")
            .debug("waiting");

        let text = capture.text();
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), 4, "four records, four lines");
        for line in text.lines() {
            let value: Value = serde_json::from_str(line).expect("every line is JSON");
            assert!(value.is_object(), "{line}");
        }
    }

    #[test]
    fn json_record_has_timestamp_level_message_and_fields() {
        let (capture, logger) = capture(LogFormat::Json, Level::Info);

        logger
            .record(Level::Warn, "store is slow")
            .field("store", "vault")
            .field("seconds", 12)
            .write();

        assert_eq!(
            capture.lines(),
            [
                r#"{"timestamp":"2026-11-23T02:00:00.250Z","level":"warn","message":"store is slow","store":"vault","seconds":12}"#
            ]
        );
    }

    // command-line, "Log content", scenario "Failed step".
    #[test]
    fn record_about_a_certificate_carries_its_name_the_step_and_the_reason() {
        let (capture, logger) = capture(LogFormat::Json, Level::Info);
        let log = logger.step("wildcard-example-com", "propagation");

        log.info("checking the nameservers");
        log.failure("the challenge records are not served", "timed out after 5m");

        let records = capture.json();
        assert_eq!(records.len(), 2);
        for record in &records {
            assert_eq!(record["certificate"], "wildcard-example-com");
            assert_eq!(record["step"], "propagation");
        }
        assert_eq!(records[1]["level"], "error");
        assert_eq!(records[1]["reason"], "timed out after 5m");
    }

    #[test]
    fn log_of_another_step_keeps_the_certificate() {
        let (capture, logger) = capture(LogFormat::Json, Level::Info);

        logger
            .step("wildcard-example-com", "propagation")
            .for_step("validate")
            .info("asking the server");

        let records = capture.json();
        assert_eq!(records[0]["certificate"], "wildcard-example-com");
        assert_eq!(records[0]["step"], "validate");
    }

    // command-line, "Log content": the level is selectable, default info.
    #[test]
    fn records_below_the_level_are_not_written() {
        for (level, expected) in [
            (Level::Error, vec!["error"]),
            (Level::Warn, vec!["error", "warn"]),
            (Level::Info, vec!["error", "warn", "info"]),
            (Level::Debug, vec!["error", "warn", "info", "debug"]),
        ] {
            let (capture, logger) = capture(LogFormat::Json, level);
            logger.error("e");
            logger.warn("w");
            logger.info("i");
            logger.debug("d");
            let written: Vec<_> = capture
                .json()
                .iter()
                .map(|record| record["level"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(written, expected, "{level}");
        }
    }

    #[test]
    fn record_marked_always_is_written_at_any_level() {
        let (capture, logger) = capture(LogFormat::Json, Level::Error);
        logger.record(Level::Info, "run completed").always().write();
        assert_eq!(capture.lines().len(), 1);
    }

    #[test]
    fn field_cannot_replace_a_key_every_record_has() {
        let (capture, logger) = capture(LogFormat::Json, Level::Info);
        logger
            .record(Level::Info, "the message")
            .field("message", "another")
            .field("level", "x")
            .write();
        let records = capture.json();
        assert_eq!(records[0]["message"], "the message");
        assert_eq!(records[0]["level"], "info");
        assert_eq!(records[0]["_message"], "another");
    }

    // command-line, "Log format", scenario "In a terminal".
    #[test]
    fn text_output_is_readable() {
        let (capture, logger) = capture(LogFormat::Text, Level::Info);

        logger.info("run started");
        logger
            .step("wildcard-example-com", "propagation")
            .failure("the challenge records are not served", "timed out after 5m");

        assert_eq!(
            capture.lines(),
            [
                "2026-11-23T02:00:00.250Z INFO  run started",
                "2026-11-23T02:00:00.250Z ERROR the challenge records are not served \
                 certificate=wildcard-example-com step=propagation reason=\"timed out after 5m\"",
            ]
        );
    }

    #[test]
    fn timestamp_can_be_given() {
        let (capture, logger) = capture(LogFormat::Json, Level::Info);
        logger
            .record(Level::Info, "event")
            .at("2026-01-01T00:00:00Z".into())
            .write();
        assert_eq!(capture.json()[0]["timestamp"], "2026-01-01T00:00:00Z");
    }
}
