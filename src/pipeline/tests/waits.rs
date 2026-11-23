//! Tasks 8.3 and 8.4: rate limits, time limits, and stopping a run.

use super::*;

const RECORD: &str = "_acme-challenge.web.example.com";

fn rate_limited(wait: Option<Duration>) -> Error {
    Error::rate_limited(
        "ACME server le: placing the order: rateLimited: too many certificates",
        wait,
    )
}

// --- Task 8.3: rate limits ---------------------------------------------------------------

// run-orchestration, "Rate limits are obeyed", scenario "Short wait".
#[tokio::test(start_paused = true)]
async fn rate_limit_with_a_short_wait_is_waited_out_and_the_request_repeated() {
    let harness = Harness::new(&[entry("web")]);
    harness
        .issuer()
        .order_failures
        .lock()
        .unwrap()
        .push_back(rate_limited(Some(Duration::from_secs(30))));
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    assert!(is_renewed(&report, "web"));
    assert_eq!(harness.issuer().orders(), 2, "the request is repeated once");
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(30),
        "after the time the server stated"
    );
    assert_eq!(
        harness.records("web", "warn"),
        [(
            "order".to_owned(),
            "the ACME server limits the rate; waiting as it asks, then trying once more".to_owned()
        )]
    );
}

// run-orchestration, "Rate limits are obeyed", scenario "Long wait".
#[tokio::test(start_paused = true)]
async fn rate_limit_with_a_long_wait_fails_the_certificate_and_says_when_the_limit_ends() {
    let harness = Harness::new(&[entry("web")]);
    harness
        .issuer()
        .order_failures
        .lock()
        .unwrap()
        .push_back(rate_limited(Some(Duration::from_secs(3 * 3600))));
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    let (step, reason) = failure_of(&report, "web");
    assert_eq!(step, Step::Order);
    assert_eq!(
        reason,
        "ACME server le: placing the order: rateLimited: too many certificates (the limit ends \
         at 2026-11-23T05:00:00Z)",
        "the log records when the limit ends"
    );
    assert_eq!(
        harness.issuer().orders(),
        1,
        "it is not attempted again in this run"
    );
    assert_eq!(started.elapsed(), Duration::ZERO, "nothing is waited for");
}

#[tokio::test(start_paused = true)]
async fn wait_of_exactly_retry_wait_max_is_still_waited_out() {
    let harness = Harness::new(&[entry("web")]);
    harness
        .issuer()
        .order_failures
        .lock()
        .unwrap()
        .push_back(rate_limited(Some(Duration::from_secs(120))));
    let report = harness.run().await;
    assert!(
        is_renewed(&report, "web"),
        "run.retry_wait_max is 2 minutes by default"
    );
}

#[tokio::test(start_paused = true)]
async fn second_rate_limit_fails_the_certificate() {
    let harness = Harness::new(&[entry("web")]);
    {
        let mut failures = harness.issuer().order_failures.lock().unwrap();
        failures.push_back(rate_limited(Some(Duration::from_secs(30))));
        failures.push_back(rate_limited(Some(Duration::from_secs(30))));
    }
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    assert_eq!(failure_of(&report, "web").0, Step::Order);
    assert_eq!(
        harness.issuer().orders(),
        2,
        "one wait, one repetition, then no further attempt"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(30));
}

#[tokio::test(start_paused = true)]
async fn rate_limit_without_a_stated_wait_fails_the_certificate() {
    let harness = Harness::new(&[entry("web")]);
    harness
        .issuer()
        .order_failures
        .lock()
        .unwrap()
        .push_back(rate_limited(None));
    let report = harness.run().await;
    assert_eq!(failure_of(&report, "web").0, Step::Order);
    assert_eq!(harness.issuer().orders(), 1);
}

// run-orchestration, "Transient errors are retried", scenario "Temporary network failure".
#[tokio::test(start_paused = true)]
async fn transient_failure_of_a_request_is_retried() {
    let harness = Harness::new(&[entry("web")]);
    harness
        .issuer()
        .order_failures
        .lock()
        .unwrap()
        .push_back(Error::transient("connection reset"));

    let report = harness.run().await;

    assert!(
        is_renewed(&report, "web"),
        "the certificate continues normally"
    );
    assert_eq!(harness.issuer().orders(), 2);
}

// run-orchestration, "Transient errors are retried", scenario "Permission denied".
#[tokio::test(start_paused = true)]
async fn permission_error_fails_at_once_without_a_retry() {
    let harness = Harness::new(&[entry("web")]);
    *harness.dns().present.lock().unwrap() = Behaviour::Fail(Error::permanent(
        "the credential is not allowed to change the zone",
    ));
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    assert_eq!(
        failure_of(&report, "web"),
        (
            Step::Present,
            "the credential is not allowed to change the zone".to_owned()
        )
    );
    assert_eq!(harness.journal.count("present"), 1, "no retry");
    assert_eq!(started.elapsed(), Duration::ZERO);
}

// --- Task 8.3: every wait has a timeout --------------------------------------------------------

// run-orchestration, "Every wait has a timeout", scenario "Service hangs".
#[tokio::test(start_paused = true)]
async fn store_that_does_not_answer_fails_the_certificate_after_the_time_limit() {
    let harness = Harness::new(&[entry("web")]);
    *harness.store("one").read.lock().unwrap() = Behaviour::Hang;
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    let (step, reason) = failure_of(&report, "web");
    assert_eq!(step, Step::ReadStores);
    assert_eq!(reason, "no answer within 1m");
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(60),
        "it fails instead of blocking the run"
    );
}

#[tokio::test(start_paused = true)]
async fn dns_service_that_does_not_answer_fails_the_certificate_after_the_time_limit() {
    let harness = Harness::new(&[entry("web")]);
    *harness.dns().present.lock().unwrap() = Behaviour::Hang;
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    assert_eq!(
        failure_of(&report, "web"),
        (Step::Present, "no answer within 1m".to_owned())
    );
    assert_eq!(started.elapsed(), Duration::from_secs(60));
}

// acme-issuance, "Bounded wait for the server's verdict", scenario "Verdict never arrives":
// the challenge records are removed.
#[tokio::test(start_paused = true)]
async fn verdict_that_never_arrives_fails_the_certificate_and_removes_the_records() {
    let harness = Harness::new(&[entry("web")]);
    *harness.issuer().validate.lock().unwrap() = Behaviour::Hang;
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    let (step, reason) = failure_of(&report, "web");
    assert_eq!(step, Step::Validate);
    assert_eq!(reason, "no answer within 3m");
    assert_eq!(started.elapsed(), Duration::from_secs(180));
    assert_eq!(
        harness.journal.count(&format!("cleanup {RECORD}")),
        1,
        "its records are removed"
    );
}

#[tokio::test(start_paused = true)]
async fn undo_action_that_does_not_answer_is_given_up_after_the_time_limit() {
    let harness = Harness::new(&[entry("web")]);
    *harness.dns().cleanup.lock().unwrap() = Behaviour::Hang;
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    assert!(
        is_renewed(&report, "web"),
        "the certificate itself is stored"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(60));
    assert_eq!(
        harness.records("web", "error"),
        [(
            "cleanup".to_owned(),
            format!("failed: removing the challenge record {RECORD}")
        )]
    );
}

#[tokio::test(start_paused = true)]
async fn hanging_certificate_does_not_keep_the_others_from_finishing() {
    let harness = Harness::new(&[
        entry_with("hangs", "le-no-ari", "dns", &["one"], ""),
        entry_in_zone("fine", "dns", "z1.example.net"),
    ]);
    *harness.fakes.issuers["le-no-ari"].validate.lock().unwrap() = Behaviour::Hang;

    let report = harness.run().await;

    assert_eq!(failure_of(&report, "hangs").0, Step::Validate);
    assert!(is_renewed(&report, "fine"));
}

// --- Task 8.4: stopping a run -----------------------------------------------------------------------

// run-orchestration, "Graceful stop", scenario "Signal during validation".
#[tokio::test(start_paused = true)]
async fn signal_during_the_propagation_wait_removes_the_records_and_interrupts_the_run() {
    let providers = PROVIDERS.replace("parallel = 4", "parallel = 1");
    let harness =
        Harness::with_providers(&providers, &[entry("web"), entry("next"), entry("last")]);
    let gate = Gate::default();
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Wait(gate.clone());

    let (report, ()) = tokio::join!(harness.run(), async {
        gate.reached().await;
        harness.context.shutdown.request();
    });

    assert_eq!(
        outcome_of(&report, "web"),
        &CertificateOutcome::Interrupted {
            step: Step::Propagation
        }
    );
    assert_eq!(
        harness.journal.count(&format!("cleanup {RECORD}")),
        1,
        "its records are removed"
    );
    assert_eq!(harness.journal.count("validate"), 0);
    assert_eq!(report.outcome, RunOutcome::Interrupted);
    assert_eq!(
        report.certificates.len(),
        1,
        "no further certificate is started"
    );
    assert_eq!(
        harness.journal.only(&["read one next", "read one last"]),
        Vec::<String>::new()
    );
    // run-events, "No completion event for an unfinished run", scenario "Interrupted run".
    assert!(!harness.event_kinds().contains(&EventKind::RunCompleted));
}

// run-orchestration, "Graceful stop", scenario "Signal after issuance".
#[tokio::test(start_paused = true)]
async fn signal_after_issuance_still_writes_the_certificate_to_its_stores() {
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one", "two"], "")]);
    let gate = Gate::default();
    // The first store's write is under way when the signal arrives.
    *harness.store("one").write.lock().unwrap() = Behaviour::Wait(gate.clone());

    let (report, ()) = tokio::join!(harness.run(), async {
        gate.reached().await;
        harness.context.shutdown.request();
        gate.open();
    });

    assert!(
        is_renewed(&report, "web"),
        "{:?}",
        outcome_of(&report, "web")
    );
    for store in ["one", "two"] {
        assert!(
            harness.store(store).inner.held("web").is_some(),
            "store {store} holds it"
        );
    }
    assert_eq!(harness.journal.count(&format!("cleanup {RECORD}")), 1);
    assert_eq!(
        report.outcome,
        RunOutcome::Interrupted,
        "the run was stopped by a signal all the same"
    );
    assert_eq!(
        harness.event_kinds(),
        [EventKind::Renewed],
        "renewed, and no completion event"
    );
}

#[tokio::test(start_paused = true)]
async fn signal_while_the_certificate_is_being_issued_lets_it_finish() {
    let harness = Harness::new(&[entry("web")]);
    let gate = Gate::default();
    *harness.issuer().finalize.lock().unwrap() = Behaviour::Wait(gate.clone());

    let (report, ()) = tokio::join!(harness.run(), async {
        gate.reached().await;
        harness.context.shutdown.request();
        gate.open();
    });

    assert!(
        is_renewed(&report, "web"),
        "a certificate the server is issuing is not given up"
    );
    assert!(harness.store("one").inner.held("web").is_some());
}

#[tokio::test(start_paused = true)]
async fn signal_while_waiting_for_the_verdict_interrupts_and_removes_the_records() {
    let harness = Harness::new(&[entry("web")]);
    let gate = Gate::default();
    *harness.issuer().validate.lock().unwrap() = Behaviour::Wait(gate.clone());

    let (report, ()) = tokio::join!(harness.run(), async {
        gate.reached().await;
        harness.context.shutdown.request();
    });

    assert_eq!(
        outcome_of(&report, "web"),
        &CertificateOutcome::Interrupted {
            step: Step::Validate
        }
    );
    assert_eq!(harness.journal.count("finalize"), 0);
    assert_eq!(harness.journal.count(&format!("cleanup {RECORD}")), 1);
}

#[tokio::test(start_paused = true)]
async fn signal_during_the_settle_wait_interrupts() {
    let harness = Harness::new(&[entry_with("web", "le", "slow-dns", &["one"], "")]);

    let (report, ()) = tokio::join!(harness.run(), async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        harness.context.shutdown.request();
    });

    assert_eq!(
        outcome_of(&report, "web"),
        &CertificateOutcome::Interrupted { step: Step::Settle }
    );
    assert_eq!(harness.journal.count("cleanup"), 1);
}

#[tokio::test(start_paused = true)]
async fn certificate_waiting_for_a_zone_is_interrupted_too() {
    let harness = Harness::new(&[entry("a"), entry("b")]);
    let gate = Gate::default();
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Wait(gate.clone());

    let (report, ()) = tokio::join!(harness.run(), async {
        gate.reached().await;
        harness.context.shutdown.request();
    });

    assert_eq!(
        outcome_of(&report, "a"),
        &CertificateOutcome::Interrupted {
            step: Step::Propagation
        }
    );
    assert_eq!(
        outcome_of(&report, "b"),
        &CertificateOutcome::Interrupted { step: Step::Zones }
    );
    assert_eq!(
        harness.journal.count("present"),
        1,
        "the second never published anything"
    );
}

#[tokio::test(start_paused = true)]
async fn run_that_is_stopped_before_it_starts_evaluates_nothing() {
    let harness = Harness::new(&[entry("a"), entry("b")]);
    harness.context.shutdown.request();

    let report = harness.run().await;

    assert_eq!(report.outcome, RunOutcome::Interrupted);
    assert!(report.certificates.is_empty());
    assert!(harness.journal.all().is_empty());
    assert!(harness.events().is_empty());
}

// run-orchestration, "Runs are repeatable and restartable", scenario "Killed run".
#[tokio::test(start_paused = true)]
async fn run_after_a_stopped_run_processes_the_certificate_from_the_beginning() {
    let stopped = Harness::new(&[entry("web")]);
    let gate = Gate::default();
    *stopped.issuer().validate.lock().unwrap() = Behaviour::Wait(gate.clone());
    let (first, ()) = tokio::join!(stopped.run(), async {
        gate.reached().await;
        stopped.context.shutdown.request();
    });
    assert!(matches!(
        outcome_of(&first, "web"),
        CertificateOutcome::Interrupted { .. }
    ));
    assert!(stopped.store("one").inner.held("web").is_none());

    // A new run has nothing but the configuration and the stores.
    let next = Harness::new(&[entry("web")]);
    let second = next.run().await;

    assert!(
        is_renewed(&second, "web"),
        "it starts from the beginning and completes"
    );
    assert_eq!(next.journal.count("order"), 1);
}

// run-orchestration, "Graceful stop", scenario "Second signal".
#[tokio::test]
async fn first_signal_asks_the_run_to_stop_and_the_second_exits_at_once() {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<()>();
    let signals = Box::pin(futures_util::stream::unfold(
        receiver,
        |mut receiver| async move { receiver.recv().await.map(|()| ((), receiver)) },
    ));
    let shutdown = Shutdown::new();
    let capture = Capture::new();
    let logger = capture.logger(LogFormat::Json, Level::Info, NOW);
    let exited = Arc::new(Mutex::new(false));
    let watcher = tokio::spawn({
        let (shutdown, exited) = (shutdown.clone(), exited.clone());
        async move {
            watch_signals(signals, shutdown, logger, move || {
                *exited.lock().unwrap() = true
            })
            .await;
        }
    });

    assert!(!shutdown.is_requested());
    sender.send(()).unwrap();
    shutdown.requested().await;
    assert!(
        !*exited.lock().unwrap(),
        "the first signal does not end the program"
    );

    sender.send(()).unwrap();
    watcher.await.unwrap();
    assert!(*exited.lock().unwrap(), "the second signal ends it at once");
    let messages: Vec<_> = capture
        .json()
        .iter()
        .map(|record| record["message"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        messages,
        [
            "termination signal received: stopping the run",
            "second termination signal received: exiting at once"
        ]
    );
}

#[tokio::test]
async fn signal_watcher_ends_quietly_when_no_signal_comes() {
    let shutdown = Shutdown::new();
    let logger = Capture::new().logger(LogFormat::Json, Level::Info, NOW);
    let none = Box::pin(futures_util::stream::empty::<()>());
    watch_signals(none, shutdown.clone(), logger, || {
        panic!("no signal, no exit")
    })
    .await;
    assert!(!shutdown.is_requested());
}
