//! The retry policy for transient errors. It lives here and nowhere else.
//!
//! An operation is attempted up to [`MAX_ATTEMPTS`] times in total. Only an
//! [`Error::Transient`](crate::Error::Transient) failure leads to another
//! attempt; every other class is returned at once. The pause before the next
//! attempt doubles each time, starting at [`FIRST_PAUSE`].

use std::future::Future;
use std::time::Duration;

use crate::Result;

/// Total number of attempts, the first one included.
pub const MAX_ATTEMPTS: u32 = 3;

/// Pause after the first failed attempt. Each further pause is twice the previous one.
pub const FIRST_PAUSE: Duration = Duration::from_secs(2);

/// Runs `op`, repeating it after a transient failure.
///
/// Returns the first success, the first non-transient failure, or the failure
/// of the last attempt.
pub async fn retry<T, F, Fut>(mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 1;
    let mut pause = FIRST_PAUSE;
    loop {
        match op().await {
            Err(error) if error.is_transient() && attempt < MAX_ATTEMPTS => {
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

    use tokio::time::Instant;

    use super::*;
    use crate::Error;

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
        assert_eq!(retry(|| script.call()).await, Ok("done"));
        assert_eq!(script.call_count(), 1);
    }

    // run-orchestration, "Transient errors are retried", scenario "Temporary network failure".
    #[tokio::test(start_paused = true)]
    async fn transient_failure_is_retried_and_the_second_attempt_succeeds() {
        let script = Script::new([Err(Error::transient("connection reset")), Ok("done")]);
        assert_eq!(retry(|| script.call()).await, Ok("done"));
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
        assert_eq!(
            retry(|| script.call()).await,
            Err(Error::transient("failure 3"))
        );
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
            assert_eq!(retry(|| script.call()).await, Err(error));
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
        assert_eq!(
            retry(|| script.call()).await,
            Err(Error::permanent("forbidden"))
        );
        assert_eq!(script.call_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn pauses_grow_between_attempts() {
        let script = Script::new([
            Err(Error::transient("failure 1")),
            Err(Error::transient("failure 2")),
            Err(Error::transient("failure 3")),
        ]);
        let _ = retry(|| script.call()).await;
        let pauses = script.pauses();
        assert_eq!(pauses, [FIRST_PAUSE, FIRST_PAUSE * 2]);
        assert!(pauses[1] > pauses[0]);
    }
}
