//! Task 8.1: the pipeline of one certificate, its steps and its undo stack.

use super::*;

const RECORD: &str = "_acme-challenge.web.example.com";

// renewal-policy, "Missing certificate is due", scenario "New certificate entry";
// acme-issuance, "Issued certificate with chain", scenario "Successful issuance".
#[tokio::test(start_paused = true)]
async fn due_certificate_goes_through_every_step_in_order() {
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one", "two"], "")]);

    let report = harness.run().await;

    assert!(
        is_renewed(&report, "web"),
        "{:?}",
        outcome_of(&report, "web")
    );
    assert_eq!(
        harness.journal.all(),
        [
            "read one web",
            "read two web",
            "account le",
            "order web.example.com",
            &format!("present {RECORD}"),
            &format!("propagation {RECORD}"),
            "validate web.example.com",
            "finalize web.example.com",
            "put one web",
            "put two web",
            &format!("cleanup {RECORD}"),
        ],
        "the stores are written before the challenge records are removed"
    );
    let issued = harness.issuer().issued.lock().unwrap()[0].clone();
    for store in ["one", "two"] {
        let (held, _) = harness
            .store(store)
            .inner
            .held("web")
            .expect("the store holds it");
        assert_eq!(
            held.leaf(),
            issued.leaf(),
            "every store received the same certificate"
        );
        assert_eq!(held.chain(), issued.chain());
        assert_eq!(held.key_pkcs8_der(), issued.key_pkcs8_der());
    }
}

// renewal-policy, "Decision from the stores", scenario "Nothing due";
// run-orchestration, "Run once and exit", scenario "Nothing to do".
#[tokio::test(start_paused = true)]
async fn certificate_that_is_not_due_causes_no_order_and_no_change() {
    let harness = Harness::new(&[entry("web")]);
    let stored = harness.holds("one", "web", 60).await;

    let report = harness.run().await;

    assert_eq!(
        outcome_of(&report, "web"),
        &CertificateOutcome::NotDue {
            due_at: stored.leaf().not_after - DAY * 30
        }
    );
    assert_eq!(
        harness
            .journal
            .only(&["account", "order", "present", "put", "cleanup"]),
        Vec::<String>::new(),
        "no order at the ACME server, no change to DNS or to any store"
    );
    assert_eq!(report.outcome, RunOutcome::Success);
}

// run-orchestration, "Rollback in reverse order", scenario "Failure after publishing
// challenge records"; dns-validation, "Only own values are removed", scenario "Cleanup
// after failure".
#[tokio::test(start_paused = true)]
async fn failure_after_the_records_are_published_removes_them() {
    let harness = Harness::new(&[entry("web")]);
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Fail(Error::timeout(
        "the challenge records were not served within 5m",
    ));

    let report = harness.run().await;

    assert_eq!(
        failure_of(&report, "web"),
        (
            Step::Propagation,
            "the challenge records were not served within 5m".to_owned()
        )
    );
    let journal = harness.journal.all();
    assert!(
        harness.journal.position(&format!("cleanup {RECORD}"))
            > harness.journal.position(&format!("present {RECORD}")),
        "{journal:#?}"
    );
    // The certificate is reported as failed only after the records are gone:
    // the failure event comes after the cleanup.
    let events = harness.log.json();
    let cleanup_logged = events.iter().position(|record| {
        record.get("step") == Some(&"cleanup".into())
            && record["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("done: "))
    });
    let failure_logged = events
        .iter()
        .position(|record| record.get("event") == Some(&"failure".into()));
    assert!(cleanup_logged.expect("cleanup") < failure_logged.expect("failure"));
}

// dns-validation, "Propagation timeout fails without involving the CA", scenario "Record
// never appears".
#[tokio::test(start_paused = true)]
async fn failed_propagation_check_does_not_involve_the_acme_server() {
    let harness = Harness::new(&[entry("web")]);
    *harness.fakes.propagation.lock().unwrap() = Behaviour::Fail(Error::timeout("not served"));

    harness.run().await;

    assert_eq!(
        harness.journal.count("validate"),
        0,
        "no validation is requested"
    );
    assert_eq!(harness.journal.count("finalize"), 0);
    assert_eq!(
        harness.journal.count("cleanup"),
        1,
        "the published values are removed"
    );
}

// run-orchestration, "Rollback in reverse order".
#[tokio::test(start_paused = true)]
async fn records_are_removed_in_the_reverse_of_the_order_they_were_published() {
    let two_names = "\n[[certificate]]\nname = \"web\"\nnames = [\"a.example.com\", \"b.example.com\", \
                     \"c.example.org\"]\nacme = \"le\"\ndns = \"dns\"\nstores = [\"one\"]\n";
    let harness = Harness::new(&[two_names.to_owned()]);
    *harness.issuer().validate.lock().unwrap() =
        Behaviour::Fail(Error::validation_failed("incorrect TXT record"));

    harness.run().await;

    assert_eq!(
        harness.journal.only(&["present", "cleanup"]),
        [
            "present _acme-challenge.a.example.com",
            "present _acme-challenge.b.example.com",
            "present _acme-challenge.c.example.org",
            "cleanup _acme-challenge.c.example.org",
            "cleanup _acme-challenge.b.example.com",
            "cleanup _acme-challenge.a.example.com",
        ]
    );
}

// run-orchestration, "Rollback in reverse order", scenario "Undo action fails".
#[tokio::test(start_paused = true)]
async fn failing_undo_action_does_not_replace_the_original_failure() {
    let harness = Harness::new(&[entry("web")]);
    *harness.issuer().validate.lock().unwrap() =
        Behaviour::Fail(Error::validation_failed("incorrect TXT record"));
    *harness.dns().cleanup.lock().unwrap() =
        Behaviour::Fail(Error::permanent("the DNS provider refused the deletion"));

    let report = harness.run().await;

    assert_eq!(
        failure_of(&report, "web"),
        (Step::Validate, "incorrect TXT record".to_owned()),
        "the certificate is reported with its original failure"
    );
    assert_eq!(
        harness.records("web", "error"),
        [
            (
                "cleanup".to_owned(),
                format!("failed: removing the challenge record {RECORD}")
            ),
            ("validate".to_owned(), "the certificate failed".to_owned()),
        ],
        "the failure of the undo action is logged"
    );
}

// acme-issuance, "Validation is attempted once per run", scenario "Validation fails".
#[tokio::test(start_paused = true)]
async fn rejected_validation_fails_the_certificate_without_a_second_order() {
    let harness = Harness::new(&[entry("web")]);
    *harness.issuer().validate.lock().unwrap() = Behaviour::Fail(Error::validation_failed(
        "the ACME server le rejected the validation: web.example.com: incorrect TXT record",
    ));

    let report = harness.run().await;

    let (step, reason) = failure_of(&report, "web");
    assert_eq!(step, Step::Validate);
    assert!(
        reason.contains("web.example.com: incorrect TXT record"),
        "{reason}"
    );
    assert_eq!(
        harness.issuer().orders(),
        1,
        "no second order is created in this run"
    );
    assert_eq!(harness.journal.count("validate"), 1);
    assert!(harness.store("one").inner.held("web").is_none());
}

// certificate-stores, "Reading the current certificate", scenario "Store cannot be read".
#[tokio::test(start_paused = true)]
async fn store_that_cannot_be_read_fails_the_certificate_and_nothing_is_issued() {
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one", "two"], "")]);
    *harness.store("two").read.lock().unwrap() = Behaviour::Fail(Error::permanent(
        "/two/web/fullchain.pem is unreadable: not PEM",
    ));

    let report = harness.run().await;

    assert_eq!(
        failure_of(&report, "web"),
        (
            Step::ReadStores,
            "/two/web/fullchain.pem is unreadable: not PEM".to_owned()
        )
    );
    assert_eq!(harness.issuer().orders(), 0, "nothing is issued");
}

#[tokio::test(start_paused = true)]
async fn store_that_fails_for_a_moment_is_read_again() {
    let harness = Harness::new(&[entry("web")]);
    harness.holds("one", "web", 60).await;
    harness
        .store("one")
        .inner
        .fail_reads(Error::transient("connection reset"));

    let report = harness.run().await;

    // The in-memory store keeps failing, so all three attempts are used.
    assert_eq!(failure_of(&report, "web").0, Step::ReadStores);
    assert_eq!(harness.journal.count("read one"), 3);
}

// configuration, "Missing secrets fail only what depends on them".
#[tokio::test(start_paused = true)]
async fn missing_secret_fails_the_certificates_that_depend_on_it_and_no_others() {
    let harness = Harness::new(&[
        entry_with("on-cloudflare", "le", "other-dns", &["one"], ""),
        entry_with("on-azure", "le", "dns", &["one"], ""),
    ]);
    harness.fakes.missing.lock().unwrap().insert(
        "other-dns".to_owned(),
        Error::permanent("the secret dns.other-dns.access_key is not set"),
    );

    let report = harness.run().await;

    assert_eq!(
        failure_of(&report, "on-cloudflare"),
        (
            Step::Configuration,
            "the secret dns.other-dns.access_key is not set".to_owned()
        )
    );
    assert!(
        is_renewed(&report, "on-azure"),
        "the other certificate is processed normally"
    );
    assert_eq!(harness.journal.count("order on-cloudflare"), 0);
}

// acme-issuance, "Account registration on first use", scenario "First run without a vault".
#[tokio::test(start_paused = true)]
async fn certificate_fails_when_its_server_has_no_account() {
    let harness = Harness::new(&[entry("web")]);
    harness.fakes.no_account.lock().unwrap().insert(
        "le".to_owned(),
        Error::permanent("register an account with `butterbot account new --acme le`"),
    );

    let report = harness.run().await;

    assert_eq!(
        failure_of(&report, "web"),
        (
            Step::Account,
            "register an account with `butterbot account new --acme le`".to_owned()
        )
    );
    assert_eq!(
        harness.issuer().orders(),
        0,
        "nothing is ordered without an account"
    );
}

// renewal-policy, "Reissue guard", scenario "Second store holds nothing".
#[tokio::test(start_paused = true)]
async fn held_back_certificate_is_reported_as_failed_with_the_time_issuing_becomes_possible() {
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one", "two"], "")]);
    // Issued one day ago, 89 days left.
    harness.holds("one", "web", 89).await;

    let report = harness.run().await;

    let (step, reason) = failure_of(&report, "web");
    assert_eq!(step, Step::Decide);
    assert_eq!(
        reason,
        "the certificate is due (store two holds no certificate), but store one holds a \
         certificate that was issued less than 2d ago; issuing becomes possible at \
         2026-11-24T02:00:00Z"
    );
    assert_eq!(harness.issuer().orders(), 0, "nothing is issued");
}

// renewal-policy, "Evaluation across several stores", scenario "One store behind".
#[tokio::test(start_paused = true)]
async fn certificate_behind_in_one_store_is_renewed_in_all() {
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one", "two"], "")]);
    harness.holds("one", "web", 60).await;
    harness.holds("two", "web", 10).await;

    let report = harness.run().await;

    assert!(is_renewed(&report, "web"));
    let (one, _) = harness.store("one").inner.held("web").unwrap();
    let (two, _) = harness.store("two").inner.held("web").unwrap();
    assert_eq!(
        one.leaf(),
        two.leaf(),
        "both stores hold the new certificate"
    );
    assert_eq!(
        one.leaf(),
        harness.issuer().issued.lock().unwrap()[0].leaf()
    );
}

// renewal-policy, "Renewal period longer than the certificate": the note is logged in every run.
#[tokio::test(start_paused = true)]
async fn note_about_renew_before_is_logged_in_every_run() {
    let harness = Harness::new(&[entry_with(
        "web",
        "le",
        "dns",
        &["one"],
        "renew_before = \"120d\"",
    )]);
    harness.holds("one", "web", 80).await;

    for _ in 0..2 {
        harness.run().await;
    }

    let notes: Vec<_> = harness
        .records("web", "error")
        .into_iter()
        .filter(|(step, message)| step == "decide" && message.starts_with("renew_before is 120d"))
        .collect();
    assert_eq!(
        notes.len(),
        2,
        "once per run: {:?}",
        harness.records("web", "error")
    );
}

// dns-validation, "Settle delay", scenario "Default settle".
#[tokio::test(start_paused = true)]
async fn validation_is_requested_after_the_settle_delay() {
    let harness = Harness::new(&[entry_with("web", "le", "slow-dns", &["one"], "")]);
    let started = tokio::time::Instant::now();

    let report = harness.run().await;

    assert!(is_renewed(&report, "web"));
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(30),
        "nothing else in this run waits, so the run takes exactly the default settle delay"
    );
}

// dns-validation, "Propagation check mode", scenario "Check switched off": the settle
// delay still applies, with the certificate's own value.
#[tokio::test(start_paused = true)]
async fn settle_delay_of_the_certificate_overrides_the_provider() {
    let harness = Harness::new(&[entry_with(
        "web",
        "le",
        "slow-dns",
        &["one"],
        "settle = \"60s\"",
    )]);
    let started = tokio::time::Instant::now();
    harness.run().await;
    assert_eq!(started.elapsed(), Duration::from_secs(60));
}

// acme-issuance, "Renewal names its predecessor", scenario "Renewal on an ARI-capable server".
#[tokio::test(start_paused = true)]
async fn renewal_names_the_stored_certificate_the_server_knows() {
    let harness = Harness::new(&[entry("web")]);
    let stored = harness.holds("one", "web", 10).await;
    harness.issuer().windows.lock().unwrap().insert(
        stored.leaf().serial.clone(),
        RenewalWindow {
            start: NOW + DAY,
            end: NOW + DAY * 3,
        },
    );

    let report = harness.run().await;

    assert!(is_renewed(&report, "web"));
    let requests = harness.issuer().requests.lock().unwrap();
    assert_eq!(
        requests[0]
            .replaces
            .as_ref()
            .map(|predecessor| predecessor.serial.as_str()),
        Some(stored.leaf().serial.as_str())
    );
}

// acme-issuance, "Renewal names its predecessor", scenario "First issuance"; and a stored
// certificate the server does not know is not named.
#[tokio::test(start_paused = true)]
async fn order_names_no_predecessor_the_server_does_not_know() {
    let harness = Harness::new(&[entry("new"), entry("adopted")]);
    harness.holds("one", "adopted", 10).await; // the fake server knows no serial

    let report = harness.run().await;

    assert!(is_renewed(&report, "new") && is_renewed(&report, "adopted"));
    for request in harness.issuer().requests.lock().unwrap().iter() {
        assert_eq!(request.replaces, None, "{:?}", request.names);
    }
    assert_eq!(
        harness.records("adopted", "error"),
        Vec::<(String, String)>::new(),
        "a failed renewal-information lookup does not fail the certificate"
    );
}

// renewal-policy, "Earlier renewal at the CA's request", scenarios "CA asks for early
// replacement" and "Renewal information switched off".
#[tokio::test(start_paused = true)]
async fn renewal_window_of_the_server_makes_a_certificate_due_unless_switched_off() {
    let harness = Harness::new(&[
        entry("asked"),
        entry_with("not-asked", "le-no-ari", "dns", &["one"], ""),
    ]);
    for name in ["asked", "not-asked"] {
        let stored = harness.holds("one", name, 50).await;
        for issuer in harness.fakes.issuers.values() {
            issuer.windows.lock().unwrap().insert(
                stored.leaf().serial.clone(),
                RenewalWindow {
                    start: NOW - DAY,
                    end: NOW + DAY,
                },
            );
        }
    }

    let report = harness.run().await;

    assert!(is_renewed(&report, "asked"), "the window has started");
    assert!(
        matches!(
            outcome_of(&report, "not-asked"),
            CertificateOutcome::NotDue { .. }
        ),
        "with ari = false only the other rules apply"
    );
    assert_eq!(
        harness.journal.only(&["renewal-info-source"]),
        ["renewal-info-source le"],
        "the server with ari = false is not asked"
    );
}

#[tokio::test(start_paused = true)]
async fn renewal_information_is_not_asked_for_when_nothing_is_stored() {
    let harness = Harness::new(&[entry("web")]);
    harness.run().await;
    assert_eq!(harness.journal.count("renewal-info"), 0);
}

// acme-issuance, "Certificate profile", and configuration, "Defaults and overrides": the
// resolved settings reach the order.
#[tokio::test(start_paused = true)]
async fn order_carries_the_resolved_profile_key_type_and_names() {
    let extra = "profile = \"tlsserver\"\nkey_type = \"ecdsa-p384\"";
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one"], extra)]);

    harness.run().await;

    let requests = harness.issuer().requests.lock().unwrap();
    assert_eq!(requests[0].names, ["web.example.com"]);
    assert_eq!(requests[0].profile.as_deref(), Some("tlsserver"));
    assert_eq!(requests[0].key_type, crate::config::KeyType::EcdsaP384);
}

// certificate-stores, "Store writes are retried", scenario "Transient failure" (task 4.5:
// no second order).
#[tokio::test(start_paused = true)]
async fn store_write_that_fails_once_is_repeated_without_a_new_order() {
    let harness = Harness::new(&[entry("web")]);
    harness
        .store("one")
        .inner
        .fail_next_puts([Error::transient("the vault is busy")]);

    let report = harness.run().await;

    assert!(is_renewed(&report, "web"));
    assert_eq!(
        harness.journal.count("put one"),
        2,
        "the write was made twice"
    );
    assert_eq!(
        harness.issuer().orders(),
        1,
        "no new certificate is ordered"
    );
    assert_eq!(harness.journal.count("finalize"), 1);
}

// run-orchestration, "A certificate is complete only when every store has it", scenario
// "Second store fails".
#[tokio::test(start_paused = true)]
async fn certificate_fails_when_one_store_cannot_be_written_and_the_other_keeps_it() {
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one", "two"], "")]);
    harness.store("two").inner.fail_next_puts([
        Error::transient("busy"),
        Error::transient("busy"),
        Error::transient("busy"),
    ]);

    let report = harness.run().await;

    let (step, reason) = failure_of(&report, "web");
    assert_eq!(step, Step::Store);
    assert!(
        reason.contains("not written to 1 of its 2 stores (store two: busy)"),
        "{reason}"
    );
    assert!(
        harness.store("one").inner.held("web").is_some(),
        "the first store holds the new certificate"
    );
    assert!(harness.store("two").inner.held("web").is_none());
    assert_eq!(
        harness.journal.count("cleanup"),
        1,
        "the challenge records are removed all the same"
    );
}

// renewal-policy, "Forced issuance", scenario "Forced reissue".
#[tokio::test(start_paused = true)]
async fn forced_certificate_is_issued_although_it_is_not_due() {
    let harness = Harness::new(&[entry("web")]);
    let stored = harness.holds("one", "web", 80).await;

    let selection = Selection {
        only: vec!["web".to_owned()],
        force: true,
    };
    let report = harness.run_selected(&selection).await;

    assert!(is_renewed(&report, "web"));
    let (held, _) = harness.store("one").inner.held("web").unwrap();
    assert_ne!(
        held.leaf().serial,
        stored.leaf().serial,
        "a new certificate is stored"
    );
}

// command-line, "Log content": every record about a certificate carries its name and step.
#[tokio::test(start_paused = true)]
async fn every_record_about_a_certificate_carries_its_name_and_a_step() {
    let harness = Harness::new(&[entry("web")]);
    harness.run().await;

    let records = harness.log.json();
    let about_web: Vec<_> = records
        .iter()
        .filter(|record| record.get("certificate") == Some(&"web".into()))
        .collect();
    assert!(about_web.len() >= 6, "{about_web:#?}");
    let steps: Vec<&str> = about_web
        .iter()
        .filter(|record| record.get("event").is_none())
        .map(|record| {
            record["step"]
                .as_str()
                .expect("a record about a certificate has a step")
        })
        .collect();
    for step in [
        "decide", "order", "present", "validate", "finalize", "store", "cleanup",
    ] {
        assert!(steps.contains(&step), "{step} in {steps:?}");
    }
}
