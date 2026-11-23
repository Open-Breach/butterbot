//! Task 8.5: the outcome of a run, its summary, and the events it emits.

use super::*;
use crate::events::CompletedOutcome;

/// Three certificates: `a` and `c` are issued, `b` fails its validation.
fn two_renewed_one_failed() -> Harness {
    let harness = Harness::new(&[
        entry_in_zone("a", "dns", "z1.example.net"),
        entry_with("b", "le-no-ari", "dns", &["one", "two"], ""),
        entry_in_zone("c", "dns", "z2.example.net"),
    ]);
    *harness.fakes.issuers["le-no-ari"].validate.lock().unwrap() = Behaviour::Fail(
        Error::validation_failed(
            "the ACME server le-no-ari rejected the validation: b.example.com: incorrect TXT record",
        ),
    );
    harness
}

fn completed(harness: &Harness) -> crate::events::RunCompleted {
    let completed: Vec<_> = harness
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::RunCompleted(completed) => Some(completed),
            _ => None,
        })
        .collect();
    assert_eq!(completed.len(), 1, "exactly one run_completed event");
    completed.into_iter().next().unwrap()
}

// run-orchestration, "Run outcome", scenario "Mixed results"; run-events, "Run completed
// on every finished run", scenario "Run with failures".
#[tokio::test(start_paused = true)]
async fn run_with_a_failure_has_the_outcome_failed_and_still_completes() {
    let harness = two_renewed_one_failed();

    let report = harness.run().await;

    assert_eq!(report.outcome, RunOutcome::Failed);
    let completed = completed(&harness);
    assert_eq!(completed.outcome, CompletedOutcome::Failed);
    assert_eq!(
        (completed.checked, completed.renewed, completed.failed),
        (3, 2, 1)
    );
    assert_eq!(completed.timestamp, NOW);
    assert_eq!(
        harness.event_kinds().last(),
        Some(&EventKind::RunCompleted),
        "the completion event is the last event of the run"
    );
}

// run-orchestration, "Run outcome", scenario "All not due"; run-events, "Run completed on
// every finished run", scenario "Nothing was due".
#[tokio::test(start_paused = true)]
async fn run_in_which_nothing_is_due_succeeds_and_completes() {
    let harness = Harness::new(&[entry("a"), entry("b")]);
    harness.holds("one", "a", 60).await;
    let soonest = harness.holds("one", "b", 45).await;

    let report = harness.run().await;

    assert_eq!(report.outcome, RunOutcome::Success);
    assert_eq!(
        harness.event_kinds(),
        [EventKind::RunCompleted],
        "no other event"
    );
    let completed = completed(&harness);
    assert_eq!(completed.outcome, CompletedOutcome::Success);
    assert_eq!(
        (completed.checked, completed.renewed, completed.failed),
        (2, 0, 0)
    );
    assert_eq!(completed.soonest_certificate.as_deref(), Some("b"));
    assert_eq!(completed.soonest_expiry, Some(soonest.leaf().not_after));
}

#[tokio::test(start_paused = true)]
async fn run_in_which_everything_due_is_renewed_succeeds() {
    let harness = Harness::new(&[entry_in_zone("a", "dns", "z1.example.net"), entry("b")]);
    harness.holds("one", "b", 60).await;
    let report = harness.run().await;
    assert_eq!(
        report.outcome,
        RunOutcome::Success,
        "not due or renewed, for every certificate"
    );
    assert_eq!(
        (report.evaluated(), report.renewed(), report.failed()),
        (2, 1, 0)
    );
}

#[tokio::test(start_paused = true)]
async fn run_without_certificates_completes_with_nothing_to_report() {
    let harness = Harness::new(&[]);
    let report = harness.run().await;
    assert_eq!(report.outcome, RunOutcome::Success);
    let completed = completed(&harness);
    assert_eq!(
        (completed.checked, completed.renewed, completed.failed),
        (0, 0, 0)
    );
    assert_eq!(
        (completed.soonest_certificate, completed.soonest_expiry),
        (None, None)
    );
}

// run-orchestration, "Run summary", scenario "Summary after a run".
#[tokio::test(start_paused = true)]
async fn summary_states_the_counts_and_the_certificate_that_expires_soonest() {
    let harness = Harness::new(&[
        entry_in_zone("new-one", "dns", "z1.example.net"),
        entry_in_zone("new-two", "dns", "z2.example.net"),
        entry("kept"),
        entry("soon"),
    ]);
    harness.holds("one", "kept", 70).await;
    let soon = harness.holds("one", "soon", 40).await;

    let report = harness.run().await;

    assert_eq!(
        (report.evaluated(), report.renewed(), report.failed()),
        (4, 2, 0)
    );
    assert_eq!(report.soonest(), Some(("soon", soon.leaf().not_after)));
    let records = harness.log.json();
    let summary = records
        .iter()
        .find(|record| record["message"] == "run finished: 4 evaluated, 2 renewed, 0 failed")
        .expect("the summary is in the log");
    assert_eq!(summary["outcome"], "success");
    assert_eq!(summary["evaluated"], 4);
    assert_eq!(summary["renewed"], 2);
    assert_eq!(summary["failed"], 0);
    assert_eq!(summary["soonest_certificate"], "soon");
    assert_eq!(
        summary["soonest_expiry"],
        crate::clock::format_utc(soon.leaf().not_after)
    );
}

#[tokio::test(start_paused = true)]
async fn soonest_expiry_counts_a_renewed_certificate_with_its_new_expiry() {
    let harness = Harness::new(&[entry("renewed"), entry("kept")]);
    harness.holds("one", "renewed", 5).await;
    let kept = harness.holds("one", "kept", 50).await;

    let report = harness.run().await;

    assert!(is_renewed(&report, "renewed"));
    assert_eq!(
        report.soonest(),
        Some(("kept", kept.leaf().not_after)),
        "the renewed certificate now expires later than the one that was kept"
    );
}

// run-events, "Failure event", scenario "Validation failure".
#[tokio::test(start_paused = true)]
async fn failed_certificate_emits_a_failure_event_with_step_reason_and_stored_expiry() {
    let harness = two_renewed_one_failed();
    let stored = harness.holds("one", "b", 25).await;

    harness.run().await;

    let failures: Vec<_> = harness
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Failure(failure) => Some(failure),
            _ => None,
        })
        .collect();
    assert_eq!(
        failures.len(),
        1,
        "one failure event for the one failed certificate"
    );
    let failure = &failures[0];
    assert_eq!(failure.certificate, "b");
    assert_eq!(failure.step, "validate");
    assert_eq!(
        failure.reason,
        "the ACME server le-no-ari rejected the validation: b.example.com: incorrect TXT record",
        "the server's reason"
    );
    assert_eq!(failure.stored_expiry, Some(stored.leaf().not_after));
    assert_eq!(failure.timestamp, NOW);
}

#[tokio::test(start_paused = true)]
async fn failure_event_of_a_certificate_that_was_never_issued_has_no_stored_expiry() {
    let harness = two_renewed_one_failed();
    harness.run().await;
    let failure = harness
        .events()
        .into_iter()
        .find_map(|event| match event {
            Event::Failure(failure) => Some(failure),
            _ => None,
        })
        .expect("a failure event");
    assert_eq!(failure.stored_expiry, None);
}

// run-events, "Renewed event", scenario "Successful renewal".
#[tokio::test(start_paused = true)]
async fn renewed_certificate_emits_a_renewed_event_with_names_stores_and_expiry() {
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one", "two"], "")]);

    harness.run().await;

    let issued = harness.issuer().issued.lock().unwrap()[0].clone();
    assert_eq!(
        harness.events()[0],
        Event::Renewed(crate::events::Renewed {
            timestamp: NOW,
            certificate: "web".into(),
            names: vec!["web.example.com".into()],
            expiry: issued.leaf().not_after,
            acme: "le".into(),
            stores: vec!["one".into(), "two".into()],
        })
    );
    assert_eq!(
        harness.event_kinds(),
        [EventKind::Renewed, EventKind::RunCompleted]
    );
}

#[tokio::test(start_paused = true)]
async fn certificate_that_is_not_written_to_every_store_emits_no_renewed_event() {
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one", "two"], "")]);
    harness
        .store("two")
        .inner
        .fail_next_puts([Error::permanent("forbidden")]);

    harness.run().await;

    assert_eq!(
        harness.event_kinds(),
        [EventKind::Failure, EventKind::RunCompleted]
    );
}

// run-events, "Expiring event", scenario "Renewal keeps failing".
#[tokio::test(start_paused = true)]
async fn failing_certificate_with_less_than_half_its_renewal_period_left_also_emits_expiring() {
    let harness = Harness::new(&[entry("web")]);
    let stored = harness.holds("one", "web", 12).await;
    *harness.issuer().validate.lock().unwrap() =
        Behaviour::Fail(Error::validation_failed("incorrect TXT record"));

    harness.run().await;

    assert_eq!(
        harness.event_kinds(),
        [
            EventKind::Failure,
            EventKind::Expiring,
            EventKind::RunCompleted
        ]
    );
    assert_eq!(
        harness.events()[1],
        Event::Expiring(crate::events::Expiring {
            timestamp: NOW,
            certificate: "web".into(),
            expiry: stored.leaf().not_after,
        })
    );
}

// run-events, "Expiring event", scenario "Early in the renewal period".
#[tokio::test(start_paused = true)]
async fn failing_certificate_early_in_its_renewal_period_emits_only_failure() {
    let harness = Harness::new(&[entry("web")]);
    harness.holds("one", "web", 25).await;
    *harness.issuer().validate.lock().unwrap() =
        Behaviour::Fail(Error::validation_failed("incorrect TXT record"));

    harness.run().await;

    assert_eq!(
        harness.event_kinds(),
        [EventKind::Failure, EventKind::RunCompleted]
    );
}

#[tokio::test(start_paused = true)]
async fn expired_certificate_that_fails_emits_expiring() {
    let harness = Harness::new(&[entry("web")]);
    let expired = issue_with(
        &["web.example.com"],
        crate::config::KeyType::EcdsaP256,
        NOW - DAY * 95,
        NOW - DAY * 5,
    );
    harness
        .store("one")
        .inner
        .put("web", &expired, Format::Pem)
        .await
        .unwrap();
    *harness.issuer().validate.lock().unwrap() =
        Behaviour::Fail(Error::validation_failed("incorrect TXT record"));

    harness.run().await;

    assert_eq!(
        harness.event_kinds(),
        [
            EventKind::Failure,
            EventKind::Expiring,
            EventKind::RunCompleted
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn failure_of_a_certificate_that_was_not_due_emits_no_expiring() {
    // The stores cannot be read, so nothing is known about being due.
    let harness = Harness::new(&[entry("web")]);
    harness.holds("one", "web", 3).await;
    *harness.store("one").read.lock().unwrap() = Behaviour::Fail(Error::permanent("forbidden"));

    harness.run().await;

    assert_eq!(
        harness.event_kinds(),
        [EventKind::Failure, EventKind::RunCompleted]
    );
}

// run-events, "Events are written to the log", scenario "JSON log".
#[tokio::test(start_paused = true)]
async fn events_of_a_run_are_in_the_log_with_all_their_fields() {
    let harness = two_renewed_one_failed();

    harness.run().await;

    let records = harness.log.json();
    let logged: Vec<&str> = records
        .iter()
        .filter_map(|record| record.get("event")?.as_str())
        .collect();
    assert_eq!(logged.iter().filter(|kind| **kind == "renewed").count(), 2);
    assert_eq!(logged.iter().filter(|kind| **kind == "failure").count(), 1);
    assert_eq!(logged.last(), Some(&"run_completed"));
    let completed = records
        .iter()
        .find(|record| record.get("event") == Some(&"run_completed".into()))
        .expect("the completion event is a log record");
    assert_eq!(completed["outcome"], "failed");
    assert_eq!(completed["checked"], 3);
    assert_eq!(completed["renewed"], 2);
    assert_eq!(completed["failed"], 1);
    assert_eq!(completed["duration_seconds"], 0);
    assert!(
        completed.contains_key("soonest_certificate") && completed.contains_key("soonest_expiry")
    );
}

// run-events, "Delivery failures never change the run", scenario "Notifier unreachable".
#[tokio::test(start_paused = true)]
async fn notifier_that_fails_does_not_change_the_outcome_of_the_run() {
    struct Unreachable;

    #[async_trait]
    impl EventSink for Unreachable {
        async fn deliver(&self, _event: &Event) -> Result<()> {
            Err(Error::transient("connection refused"))
        }
    }

    let mut harness = Harness::new(&[entry("web")]);
    harness
        .events
        .register("pager", EventSelection::All, Arc::new(Unreachable));

    let report = harness.run().await;

    assert_eq!(report.outcome, RunOutcome::Success);
    assert!(is_renewed(&report, "web"));
    let undelivered: Vec<_> = harness
        .log
        .json()
        .into_iter()
        .filter_map(|record| record.get("undelivered_event")?.as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        undelivered,
        ["renewed", "run_completed"],
        "each delivery failure is logged"
    );
}
