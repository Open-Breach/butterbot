//! The state of the configured certificates, as `butterbot status` shows it.

use super::*;
use crate::policy::Reason;

/// Everything the fakes recorded that is not a read.
fn changes(harness: &Harness) -> Vec<String> {
    harness
        .journal
        .only(&["account", "order", "present", "put", "cleanup"])
}

fn of<'a>(statuses: &'a [CertificateStatus], name: &str) -> &'a CertificateStatus {
    statuses
        .iter()
        .find(|status| status.name == name)
        .unwrap_or_else(|| panic!("{name} is in the status"))
}

// command-line, "The status command": what each store holds, whether the
// certificate is due and why, and otherwise when it becomes due.
#[tokio::test(start_paused = true)]
async fn due_and_not_due_certificates_are_reported_with_their_reasons() {
    let harness = Harness::new(&[entry("fresh"), entry("old"), entry("new")]);
    let fresh = harness.holds("one", "fresh", 60).await;
    let old = harness.holds("one", "old", 10).await;

    let statuses = status(&harness.context, &harness.config).await;

    assert_eq!(
        statuses
            .iter()
            .map(|status| status.name.as_str())
            .collect::<Vec<_>>(),
        ["fresh", "old", "new"],
        "one entry per certificate, in the order of the configuration"
    );

    let status = of(&statuses, "fresh");
    assert_eq!(status.names, ["fresh.example.com"]);
    assert_eq!(
        status.stores,
        [StoreStatus {
            store: "one".to_owned(),
            holding: Holding::Certificate(fresh.leaf().clone()),
        }]
    );
    assert_eq!(
        status.standing,
        Standing::NotDue {
            due_at: fresh.leaf().not_after - DAY * 30
        }
    );

    let status = of(&statuses, "old");
    assert_eq!(
        status.stores[0].holding,
        Holding::Certificate(old.leaf().clone())
    );
    assert_eq!(
        status.standing,
        Standing::Due {
            reasons: vec![Reason::RenewalPeriod {
                store: "one".to_owned(),
                expiry: old.leaf().not_after,
                renewal_period: DAY * 30,
            }]
        }
    );

    let status = of(&statuses, "new");
    assert_eq!(status.stores[0].holding, Holding::Nothing);
    assert_eq!(
        status.standing,
        Standing::Due {
            reasons: vec![Reason::Missing {
                store: "one".to_owned()
            }]
        }
    );
}

// command-line, "The status command", scenario "Read-only".
#[tokio::test(start_paused = true)]
async fn status_of_due_certificates_issues_nothing_and_writes_nothing() {
    let harness = Harness::new(&[
        entry("old"),
        entry_with("new", "le", "dns", &["one", "two"], ""),
    ]);
    let old = harness.holds("one", "old", 10).await;

    let statuses = status(&harness.context, &harness.config).await;

    assert!(
        statuses
            .iter()
            .all(|status| matches!(status.standing, Standing::Due { .. })),
        "both are reported as due: {statuses:?}"
    );
    assert_eq!(
        changes(&harness),
        Vec::<String>::new(),
        "no account, no order, no change to DNS or to any store"
    );
    assert_eq!(harness.issuer().orders(), 0);
    assert_eq!(harness.events(), Vec::new(), "and no event");
    assert_eq!(
        harness
            .store("one")
            .inner
            .current("old", Format::Pem)
            .await
            .expect("readable"),
        Some(old.leaf().clone()),
        "the stored certificate is the one that was there"
    );
    for store in ["one", "two"] {
        assert_eq!(
            harness
                .store(store)
                .inner
                .current("new", Format::Pem)
                .await
                .expect("readable"),
            None
        );
    }
}

#[tokio::test(start_paused = true)]
async fn certificate_with_a_store_that_cannot_be_read_has_an_unknown_state() {
    let harness = Harness::new(&[
        entry_with("web", "le", "dns", &["one", "two"], ""),
        entry("other"),
    ]);
    let stored = harness.holds("one", "web", 60).await;
    harness.holds("one", "other", 60).await;
    *harness.store("two").read.lock().unwrap() = Behaviour::Fail(Error::permanent(
        "/two/web/fullchain.pem is unreadable: not PEM",
    ));

    let statuses = status(&harness.context, &harness.config).await;

    let web = of(&statuses, "web");
    assert_eq!(
        web.stores,
        [
            StoreStatus {
                store: "one".to_owned(),
                holding: Holding::Certificate(stored.leaf().clone()),
            },
            StoreStatus {
                store: "two".to_owned(),
                holding: Holding::Unreadable(
                    "/two/web/fullchain.pem is unreadable: not PEM".to_owned()
                ),
            },
        ],
        "the store that can be read is still shown"
    );
    assert_eq!(web.standing, Standing::Unknown);
    assert!(!web.is_known());
    assert!(
        of(&statuses, "other").is_known(),
        "the other certificate is not affected"
    );
}

// renewal-policy: the server's renewal window counts for the status as it
// does for a run.
#[tokio::test(start_paused = true)]
async fn renewal_window_of_the_server_is_a_reason_in_the_status() {
    let harness = Harness::new(&[entry("web")]);
    let stored = harness.holds("one", "web", 50).await;
    harness.issuer().windows.lock().unwrap().insert(
        stored.leaf().serial.clone(),
        RenewalWindow {
            start: NOW - DAY,
            end: NOW + DAY,
        },
    );

    let statuses = status(&harness.context, &harness.config).await;

    assert_eq!(
        of(&statuses, "web").standing,
        Standing::Due {
            reasons: vec![Reason::RenewalInfo {
                store: "one".to_owned(),
                window_start: NOW - DAY,
            }]
        }
    );
    assert_eq!(changes(&harness), Vec::<String>::new());
}

#[tokio::test(start_paused = true)]
async fn certificate_that_a_run_would_hold_back_is_reported_as_held_back() {
    let harness = Harness::new(&[entry_with("web", "le", "dns", &["one", "two"], "")]);
    // Issued just now, and missing in the second store.
    harness.holds("one", "web", 90).await;

    let statuses = status(&harness.context, &harness.config).await;

    match &of(&statuses, "web").standing {
        Standing::HeldBack {
            reasons,
            fresh_store,
            until,
        } => {
            assert_eq!(
                reasons,
                &[Reason::Missing {
                    store: "two".to_owned()
                }]
            );
            assert_eq!(fresh_store, "one");
            assert_eq!(*until, NOW + harness.config.run.min_reissue);
        }
        other => panic!("not held back: {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn note_about_renew_before_is_part_of_the_status() {
    let harness = Harness::new(&[entry_with(
        "web",
        "le",
        "dns",
        &["one"],
        "renew_before = \"80d\"",
    )]);
    harness.holds("one", "web", 85).await;

    let statuses = status(&harness.context, &harness.config).await;

    let notes = &of(&statuses, "web").notes;
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].message.contains("renew_before"), "{notes:?}");
}
