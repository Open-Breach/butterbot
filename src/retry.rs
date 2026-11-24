//! The retry policy for transient errors. It lives here and nowhere else.
//!
//! An operation is attempted up to [`MAX_ATTEMPTS`] times in total. Only an
//! [`Error::Transient`](crate::Error::Transient) failure leads to another
//! attempt; every other class is returned at once. The pause before the next
//! attempt doubles each time, starting at [`FIRST_PAUSE`].

use std::future::Future;
use std::time::Duration;

use crate::Result;
use crate::logging::{Level, Log};

/// Total number of attempts, the first one included.
pub const MAX_ATTEMPTS: u32 = 3;

/// Pause after the first failed attempt. Each further pause is twice the previous one.
pub const FIRST_PAUSE: Duration = Duration::from_secs(2);

/// Runs `op`, repeating it after a transient failure.
///
/// Returns the first success, the first non-transient failure, or the failure
/// of the last attempt. Every failure that leads to another attempt is written
/// to `log` as a warning; `action` says what was being done.
pub async fn retry<T, F, Fut>(log: &dyn Log, action: &str, op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    retry_with_first_pause(log, action, FIRST_PAUSE, op).await
}

/// [`retry`] with another pause after the first failed attempt. The number of
/// attempts and the doubling of the pause are the same.
pub async fn retry_with_first_pause<T, F, Fut>(
    log: &dyn Log,
    action: &str,
    first_pause: Duration,
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 1;
    let mut pause = first_pause;
    loop {
        match op().await {
            Err(error) if error.is_transient() && attempt < MAX_ATTEMPTS => {
                log.record(
                    Level::Warn,
                    format!("{action} failed, trying again in {}s", pause.as_secs()),
                )
                .reason(&error)
                .field("attempt", attempt)
                .field("attempts", MAX_ATTEMPTS)
                .write();
                tokio::time::sleep(pause).await;
                attempt += 1;
                pause *= 2;
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use time::macros::datetime;
    use tokio::time::Instant;

    use super::*;
    use crate::Error;
    use crate::logging::{Capture, LogFormat, Logger};

    /// Runs `retry` on a script with a log nobody reads.
    async fn run(script: &Script) -> Result<&'static str> {
        let (_, logger) = captured();
        retry(&logger, "the operation", || script.call()).await
    }

    fn captured() -> (Capture, Logger) {
        let capture = Capture::new();
        let logger = capture.logger(
            LogFormat::Json,
            Level::Info,
            datetime!(2026-11-23 02:00:00 UTC),
        );
        (capture, logger)
    }

    /// An operation that returns the scripted results in order and records
    /// when it was called.
    struct Script {
        results: RefCell<VecDeque<Result<&'static str>>>,
        calls: RefCell<Vec<Instant>>,
    }

    impl Script {
        fn new(results: impl IntoIterator<Item = Result<&'static str>>) -> Self {
            Self {
                results: RefCell::new(results.into_iter().collect()),
                calls: RefCell::new(Vec::new()),
            }
        }

        async fn call(&self) -> Result<&'static str> {
            self.calls.borrow_mut().push(Instant::now());
            self.results
                .borrow_mut()
                .pop_front()
                .expect("called more often than scripted")
        }

        fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }

        fn pauses(&self) -> Vec<Duration> {
            self.calls
                .borrow()
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .collect()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn success_is_returned_without_a_second_attempt() {
        let script = Script::new([Ok("done")]);
        assert_eq!(run(&script).await, Ok("done"));
        assert_eq!(script.call_count(), 1);
    }

    // run-orchestration, "Transient errors are retried", scenario "Temporary network failure".
    #[tokio::test(start_paused = true)]
    async fn transient_failure_is_retried_and_the_second_attempt_succeeds() {
        let script = Script::new([Err(Error::transient("connection reset")), Ok("done")]);
        assert_eq!(run(&script).await, Ok("done"));
        assert_eq!(script.call_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn third_transient_failure_is_returned() {
        let script = Script::new([
            Err(Error::transient("failure 1")),
            Err(Error::transient("failure 2")),
            Err(Error::transient("failure 3")),
            Ok("never reached"),
        ]);
        assert_eq!(run(&script).await, Err(Error::transient("failure 3")));
        assert_eq!(script.call_count(), 3);
    }

    // run-orchestration, "Transient errors are retried", scenario "Permission denied".
    #[tokio::test(start_paused = true)]
    async fn other_classes_are_returned_at_once() {
        let not_retried = [
            Error::permanent("the credential may not change the zone"),
            Error::rate_limited("too many requests", Some(Duration::from_secs(30))),
            Error::validation_failed("incorrect TXT record"),
            Error::timeout("no answer after 2m"),
        ];
        for error in not_retried {
            let script = Script::new([Err(error.clone()), Ok("never reached")]);
            let started = Instant::now();
            assert_eq!(run(&script).await, Err(error));
            assert_eq!(script.call_count(), 1);
            assert_eq!(
                started.elapsed(),
                Duration::ZERO,
                "failed at once, without a pause"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn non_transient_failure_after_a_transient_one_ends_the_retries() {
        let script = Script::new([
            Err(Error::transient("connection reset")),
            Err(Error::permanent("forbidden")),
            Ok("never reached"),
        ]);
        assert_eq!(run(&script).await, Err(Error::permanent("forbidden")));
        assert_eq!(script.call_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn pauses_grow_between_attempts() {
        let script = Script::new([
            Err(Error::transient("failure 1")),
            Err(Error::transient("failure 2")),
            Err(Error::transient("failure 3")),
        ]);
        let _ = run(&script).await;
        let pauses = script.pauses();
        assert_eq!(pauses, [FIRST_PAUSE, FIRST_PAUSE * 2]);
        assert!(pauses[1] > pauses[0]);
    }

    #[tokio::test(start_paused = true)]
    async fn each_retry_is_logged_with_its_reason() {
        let (capture, logger) = captured();
        let script = Script::new([
            Err(Error::transient("connection reset")),
            Err(Error::transient("connection refused")),
            Ok("done"),
        ]);

        let log = logger.step("wildcard-example-com", "present");
        assert_eq!(
            retry(&log, "publishing the records", || script.call()).await,
            Ok("done")
        );

        let records = capture.json();
        assert_eq!(records.len(), 2, "one record per failure that is retried");
        assert_eq!(records[0]["level"], "warn");
        assert_eq!(
            records[0]["message"],
            "publishing the records failed, trying again in 2s"
        );
        assert_eq!(records[0]["reason"], "connection reset");
        assert_eq!(records[0]["attempt"], 1);
        assert_eq!(records[0]["certificate"], "wildcard-example-com");
        assert_eq!(records[0]["step"], "present");
        assert_eq!(
            records[1]["message"],
            "publishing the records failed, trying again in 4s"
        );
        assert_eq!(records[1]["reason"], "connection refused");
    }

    #[tokio::test(start_paused = true)]
    async fn failure_that_is_not_retried_is_left_to_the_caller_to_log() {
        let (capture, logger) = captured();
        let script = Script::new([Err(Error::permanent("forbidden"))]);
        assert!(
            retry(&logger, "the operation", || script.call())
                .await
                .is_err()
        );
        assert!(capture.lines().is_empty());
    }
}
