//! Task 8.2: the run loop: selection, isolation of failures, bounded
//! parallelism and the lock per zone.

use super::*;

// run-orchestration, "Failures are isolated per certificate", scenario "One certificate fails".
#[tokio::test(start_paused = true)]
async fn one_failing_certificate_does_not_stop_the_others() {
    let harness = Harness::new(&[
        entry_in_zone("first", "dns", "z1.example.net"),
        // The second certificate is on a server that rejects its validation.
        entry_with("second", "le-no-ari", "dns", &["one"], ""),
        entry_in_zone("third", "dns", "z3.example.net"),
    ]);
    *harness.fakes.issuers["le-no-ari"].validate.lock().unwrap() =
        Behaviour::Fail(Error::validation_failed("incorrect TXT record"));

    let report = harness.run().await;

    assert!(is_renewed(&report, "first") && is_renewed(&report, "third"));
    assert_eq!(failure_of(&report, "second").0, Step::Validate);
    assert_eq!(
        (report.evaluated(), report.renewed(), report.failed()),
        (3, 2, 1)
    );
    assert_eq!(report.outcome, RunOutcome::Failed);
    for name in ["first", "third"] {
        assert!(
            harness.store("one").inner.held(name).is_some(),
            "{name} is stored"
        );
    }
    assert!(harness.store("one").inner.held("second").is_none());
}

// run-orchestration, "Run once and exit", scenario "Selection".
#[tokio::test(start_paused = true)]
async fn only_the_selected_certificate_is_evaluated() {
    let harness = Harness::new(&[entry("a"), entry("b"), entry("c")]);

    let selection = Selection {
        only: vec!["b".to_owned()],
        force: false,
    };
    let report = harness.run_selected(&selection).await;

    assert_eq!(report.certificates.len(), 1);
    assert!(is_renewed(&report, "b"));
    assert_eq!(
        harness.journal.only(&["read"]),
        ["read one b"],
        "the others are not even read"
    );
    assert_eq!(harness.issuer().orders(), 1);
}

#[tokio::test(start_paused = true)]
async fn several_certificates_can_be_selected() {
    let harness = Harness::new(&[entry("a"), entry("b"), entry("c")]);
    let selection = Selection {
        only: vec!["c".to_owned(), "a".to_owned()],
        force: false,
    };
    let report = harness.run_selected(&selection).await;
    let names: Vec<_> = report
        .certificates
        .iter()
        .map(|report| report.name.as_str())
        .collect();
    assert_eq!(names, ["a", "c"], "in the order of the configuration");
}

// run-orchestration, "Run once and exit", scenario "Plain run".
#[tokio::test(start_paused = true)]
async fn without_a_selection_every_certificate_is_evaluated_in_configuration_order() {
    let harness = Harness::new(&[entry("b"), entry("a"), entry("c")]);
    harness.holds("one", "a", 60).await;

    let report = harness.run().await;

    let names: Vec<_> = report
        .certificates
        .iter()
        .map(|report| report.name.as_str())
        .collect();
    assert_eq!(names, ["b", "a", "c"]);
    assert_eq!(
        (report.evaluated(), report.renewed(), report.failed()),
        (3, 2, 0)
    );
}

// run-orchestration, "Bounded parallelism", scenario "Different zones".
#[tokio::test(start_paused = true)]
async fn certificates_in_different_zones_are_processed_at_the_same_time() {
    let entries: Vec<String> = (1..=4)
        .map(|number| {
            entry_in_zone(
                &format!("c{number}"),
                "dns",
                &format!("z{number}.example.net"),
            )
        })
        .collect();
    let harness = Harness::new(&entries);
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Take(Duration::from_secs(10));
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    assert_eq!(report.renewed(), 4);
    assert_eq!(
        harness.fakes.in_propagation.lock().unwrap().1,
        4,
        "all four at the same time"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(10),
        "not one after the other"
    );
}

#[tokio::test(start_paused = true)]
async fn no_more_than_run_parallel_certificates_are_processed_at_the_same_time() {
    let entries: Vec<String> = (1..=6)
        .map(|number| {
            entry_in_zone(
                &format!("c{number}"),
                "dns",
                &format!("z{number}.example.net"),
            )
        })
        .collect();
    let harness = Harness::new(&entries);
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Take(Duration::from_secs(10));
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    assert_eq!(report.renewed(), 6);
    assert_eq!(
        harness.fakes.in_propagation.lock().unwrap().1,
        4,
        "run.parallel is 4"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(20),
        "four, then the remaining two"
    );
}

#[tokio::test(start_paused = true)]
async fn parallel_of_one_processes_one_certificate_after_the_other() {
    let providers = PROVIDERS.replace("parallel = 4", "parallel = 1");
    let harness = Harness::with_providers(
        &providers,
        &[
            entry_in_zone("a", "dns", "z1.example.net"),
            entry_in_zone("b", "dns", "z2.example.net"),
        ],
    );
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Take(Duration::from_secs(10));

    harness.run().await;

    assert_eq!(harness.fakes.in_propagation.lock().unwrap().1, 1);
    assert!(
        harness
            .journal
            .position("cleanup _acme-challenge.a.z1.example.net")
            < harness.journal.position("read one b"),
        "the second is not started before the first is done"
    );
}

// run-orchestration, "Bounded parallelism", scenario "Same zone".
#[tokio::test(start_paused = true)]
async fn certificates_in_the_same_zone_do_not_overlap_in_their_dns_steps() {
    let harness = Harness::new(&[entry("a"), entry("b"), entry("c")]);
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Take(Duration::from_secs(10));

    let report = harness.run().await;

    assert_eq!(report.renewed(), 3);
    assert_eq!(
        *harness.dns().most_in_one_zone.lock().unwrap(),
        1,
        "one certificate in the zone at a time"
    );
    // Each certificate publishes only after the one before has removed its records.
    let dns_steps = harness.journal.only(&["present", "cleanup"]);
    assert_eq!(dns_steps.len(), 6);
    for pair in dns_steps.chunks(2) {
        let record = pair[0]
            .strip_prefix("present ")
            .expect("a certificate publishes first");
        assert_eq!(
            pair[1],
            format!("cleanup {record}"),
            "and removes before the next publishes"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn same_zone_name_at_another_dns_provider_is_another_zone() {
    let harness = Harness::new(&[
        entry_with("a", "le", "dns", &["one"], ""),
        entry_with("b", "le", "other-dns", &["one"], ""),
    ]);
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Take(Duration::from_secs(10));
    let started = tokio::time::Instant::now();

    harness.run().await;

    assert_eq!(
        harness.fakes.in_propagation.lock().unwrap().1,
        2,
        "they do not wait for each other"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(10));
}

#[tokio::test(start_paused = true)]
async fn certificates_that_span_the_same_zones_do_not_block_each_other_for_ever() {
    let spanning = |name: &str, first: &str, second: &str| {
        format!(
            "\n[[certificate]]\nname = \"{name}\"\nnames = [\"{name}.{first}\", \"{name}.{second}\"]\n\
             acme = \"le\"\ndns = \"dns\"\nstores = [\"one\"]\n"
        )
    };
    let harness = Harness::new(&[
        spanning("a", "z1.example.net", "z2.example.net"),
        spanning("b", "z2.example.net", "z1.example.net"),
    ]);
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Take(Duration::from_secs(10));

    let report = tokio::time::timeout(Duration::from_secs(3600), harness.run())
        .await
        .expect("the run ends: the locks are taken in one order");

    assert_eq!(report.renewed(), 2);
    assert_eq!(*harness.dns().most_in_one_zone.lock().unwrap(), 1);
}

// run-orchestration, "Runs are repeatable and restartable", scenario "Run twice in a row".
#[tokio::test(start_paused = true)]
async fn second_run_right_after_a_successful_one_issues_nothing() {
    let harness = Harness::new(&[entry("a"), entry("b")]);

    let first = harness.run().await;
    let second = harness.run().await;

    assert_eq!((first.renewed(), second.renewed()), (2, 0));
    assert_eq!(second.outcome, RunOutcome::Success);
    assert_eq!(
        harness.issuer().orders(),
        2,
        "the second run orders nothing"
    );
}

// certificate-stores, "No destructive actions", scenario "Entry removed from configuration".
#[tokio::test(start_paused = true)]
async fn certificate_of_a_removed_entry_is_left_untouched() {
    let before = Harness::new(&[entry("kept"), entry("removed")]);
    before.run().await;
    let removed = before.store("one").inner.held("removed").expect("issued").0;

    // The same stores, with one entry less in the configuration.
    let after = Harness::new(&[entry("kept")]);
    let (kept, _) = before.store("one").inner.held("kept").unwrap();
    after
        .store("one")
        .inner
        .put("kept", &kept, Format::Pem)
        .await
        .unwrap();
    after
        .store("one")
        .inner
        .put("removed", &removed, Format::Pem)
        .await
        .unwrap();
    let report = after.run().await;

    assert_eq!(
        report.certificates.len(),
        1,
        "the removed entry is not evaluated any more"
    );
    let (still_there, _) = after
        .store("one")
        .inner
        .held("removed")
        .expect("left in the store");
    assert_eq!(still_there.leaf(), removed.leaf());
    assert_eq!(
        after.journal.only(&["put", "read one removed"]),
        Vec::<String>::new()
    );
}
