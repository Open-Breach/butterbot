//! The undo stack of a certificate.
//!
//! A step that changes something outside the program registers how to undo
//! it. When the certificate is done, whether it succeeded, failed or was
//! interrupted, the registered actions run in the reverse of the order in
//! which they were registered: later steps depend on earlier ones, so they
//! are undone first.

use std::future::Future;
use std::pin::Pin;

use crate::Result;
use crate::logging::{Level, StepLog};

type Action = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send>;

/// The undo actions registered so far, last one first when run.
#[derive(Default)]
pub struct UndoStack {
    actions: Vec<(String, Action)>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an undo action. `description` says what it does, for the log.
    pub fn push<F, Fut>(&mut self, description: impl Into<String>, action: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.actions
            .push((description.into(), Box::new(move || Box::pin(action()))));
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Runs the registered actions, last registered first, and empties the
    /// stack. An action that fails is logged; the remaining actions still
    /// run, and the failure is not passed on: it must not replace the reason
    /// the certificate failed for.
    pub async fn unwind(&mut self, log: &StepLog) {
        while let Some((description, action)) = self.actions.pop() {
            match action().await {
                Ok(()) => log
                    .record(Level::Debug, format!("done: {description}"))
                    .write(),
                Err(error) => log
                    .record(Level::Error, format!("failed: {description}"))
                    .reason(error)
                    .write(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use time::macros::datetime;

    use super::*;
    use crate::Error;
    use crate::logging::{Capture, LogFormat};

    fn log() -> (Capture, StepLog) {
        let capture = Capture::new();
        let logger = capture.logger(
            LogFormat::Json,
            Level::Info,
            datetime!(2026-11-23 02:00:00 UTC),
        );
        (capture, logger.step("wildcard-example-com", "cleanup"))
    }

    // run-orchestration, "Rollback in reverse order".
    #[tokio::test]
    async fn actions_run_last_in_first_out() {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let mut stack = UndoStack::new();
        for name in ["first", "second", "third"] {
            let ran = ran.clone();
            stack.push(name, move || async move {
                ran.lock().unwrap().push(name);
                Ok(())
            });
        }
        let (_, log) = log();

        stack.unwind(&log).await;

        assert_eq!(*ran.lock().unwrap(), ["third", "second", "first"]);
        assert!(stack.is_empty(), "an action runs once");
        stack.unwind(&log).await;
        assert_eq!(ran.lock().unwrap().len(), 3);
    }

    // run-orchestration, "Rollback in reverse order", scenario "Undo action fails".
    #[tokio::test]
    async fn failing_action_is_logged_and_the_others_still_run() {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let mut stack = UndoStack::new();
        let first = ran.clone();
        stack.push("removing the first record", move || async move {
            first.lock().unwrap().push("first");
            Ok(())
        });
        stack.push("removing the second record", || async {
            Err(Error::permanent("the DNS provider refused"))
        });
        let third = ran.clone();
        stack.push("removing the third record", move || async move {
            third.lock().unwrap().push("third");
            Ok(())
        });
        let (capture, log) = log();

        // `unwind` returns nothing: a failing undo action cannot replace the
        // failure the certificate is reported with.
        stack.unwind(&log).await;

        assert_eq!(
            *ran.lock().unwrap(),
            ["third", "first"],
            "the remaining actions still run"
        );
        let records = capture.json();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["level"], "error");
        assert_eq!(records[0]["message"], "failed: removing the second record");
        assert_eq!(records[0]["reason"], "the DNS provider refused");
        assert_eq!(records[0]["certificate"], "wildcard-example-com");
        assert_eq!(records[0]["step"], "cleanup");
    }
}
