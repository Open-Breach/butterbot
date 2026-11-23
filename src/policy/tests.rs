//! Every scenario of the `renewal-policy` spec, with a fixed clock.

use time::macros::datetime;

use super::*;

const NOW: OffsetDateTime = datetime!(2026-11-23 02:00:00 UTC);
const DAY: Duration = Duration::from_secs(86_400);

fn days(days: u32) -> Duration {
    DAY * days
}

/// A stored certificate for `names` that was issued `age` ago and expires in
/// `remaining`.
fn stored(names: &[&str], age: Duration, remaining: Duration) -> StoredCert {
    StoredCert {
        names: names.iter().map(|name| name.to_string()).collect(),
        not_before: NOW - age,
        not_after: NOW + remaining,
        issuer: "CN=Test Authority".into(),
        serial: format!("{:x}", age.as_secs() ^ remaining.as_secs()),
        der: Vec::new(),
    }
}

/// A certificate with a lifetime of 90 days that expires in `remaining` days.
fn expiring_in(remaining: u32) -> StoredCert {
    stored(&["example.com"], days(90 - remaining), days(remaining))
}

/// A certificate with a lifetime of 90 days that was issued `age` days ago.
fn issued_ago(age: u32) -> StoredCert {
    stored(&["example.com"], days(age), days(90 - age))
}

fn holds<'a>(store: &'a str, certificate: &'a StoredCert) -> StoreState<'a> {
    StoreState {
        store,
        current: Some(certificate),
        renewal_window: None,
    }
}

fn empty(store: &str) -> StoreState<'_> {
    StoreState {
        store,
        current: None,
        renewal_window: None,
    }
}

fn with_window(state: StoreState<'_>, start: OffsetDateTime) -> StoreState<'_> {
    StoreState {
        renewal_window: Some(RenewalWindow {
            start,
            end: start + days(2),
        }),
        ..state
    }
}

/// Evaluates an entry for `example.com` with a `renew_before` of 30 days.
fn decide(stores: &[StoreState<'_>]) -> Decision {
    decide_with(days(30), false, stores)
}

fn decide_with(renew_before: Duration, forced: bool, stores: &[StoreState<'_>]) -> Decision {
    evaluate_with(renew_before, forced, stores).decision
}

fn evaluate_with(renew_before: Duration, forced: bool, stores: &[StoreState<'_>]) -> Evaluation {
    let names = ["example.com".to_owned()];
    evaluate(&Input {
        names: &names,
        renew_before,
        min_reissue: days(2),
        forced,
        stores,
        now: NOW,
    })
}

fn reasons(decision: &Decision) -> Vec<&'static str> {
    match decision {
        Decision::Due { reasons } | Decision::HeldBack { reasons, .. } => {
            reasons.iter().map(Reason::code).collect()
        }
        Decision::NotDue { .. } => Vec::new(),
    }
}

// --- Decision from the stores -------------------------------------------------

// "Decision from the stores", scenario "Nothing due".
#[test]
fn valid_certificate_outside_its_renewal_period_is_not_due() {
    let (first, second) = (expiring_in(60), expiring_in(60));
    let decision = decide(&[holds("vault", &first), holds("local", &second)]);
    assert_eq!(
        decision,
        Decision::NotDue {
            due_at: NOW + days(30)
        }
    );
    assert!(!decision.is_due());
}

// --- Missing certificate is due -------------------------------------------------

// "Missing certificate is due", scenario "New certificate entry".
#[test]
fn entry_whose_stores_hold_nothing_is_due() {
    let decision = decide(&[empty("vault"), empty("local")]);
    assert_eq!(
        decision,
        Decision::Due {
            reasons: vec![
                Reason::Missing {
                    store: "vault".into()
                },
                Reason::Missing {
                    store: "local".into()
                }
            ]
        }
    );
}

// "Missing certificate is due", scenario "New store on an existing entry".
#[test]
fn store_added_to_an_existing_entry_makes_it_due() {
    let existing = expiring_in(60);
    let decision = decide(&[holds("vault", &existing), empty("local")]);
    assert_eq!(
        decision,
        Decision::Due {
            reasons: vec![Reason::Missing {
                store: "local".into()
            }]
        }
    );
}

// --- Changed names are due ---------------------------------------------------------

// "Changed names are due", scenario "Name added".
#[test]
fn name_added_to_the_entry_makes_it_due_regardless_of_expiry() {
    let existing = expiring_in(80);
    let names = ["example.com".to_owned(), "www.example.com".to_owned()];
    let stores = [holds("vault", &existing)];
    let evaluation = evaluate(&Input {
        names: &names,
        renew_before: days(30),
        min_reissue: days(2),
        forced: false,
        stores: &stores,
        now: NOW,
    });
    assert_eq!(
        evaluation.decision,
        Decision::Due {
            reasons: vec![Reason::NamesChanged {
                store: "vault".into()
            }]
        }
    );
}

#[test]
fn name_removed_from_the_entry_makes_it_due() {
    let existing = stored(&["example.com", "www.example.com"], days(10), days(80));
    assert_eq!(
        reasons(&decide(&[holds("vault", &existing)])),
        ["names_changed"]
    );
}

#[test]
fn names_are_compared_as_sets_without_regard_to_case_and_order() {
    let existing = stored(&["www.example.com", "example.com"], days(10), days(80));
    let names = ["Example.COM".to_owned(), "WWW.example.com".to_owned()];
    let stores = [holds("vault", &existing)];
    let evaluation = evaluate(&Input {
        names: &names,
        renew_before: days(30),
        min_reissue: days(2),
        forced: false,
        stores: &stores,
        now: NOW,
    });
    assert!(
        matches!(evaluation.decision, Decision::NotDue { .. }),
        "{:?}",
        evaluation.decision
    );
}

// --- Renewal period ------------------------------------------------------------------

// "Renewal period", scenario "Inside the renewal period".
#[test]
fn certificate_inside_the_renewal_period_is_due() {
    let existing = expiring_in(29);
    assert_eq!(
        decide(&[holds("vault", &existing)]),
        Decision::Due {
            reasons: vec![Reason::RenewalPeriod {
                store: "vault".into(),
                expiry: NOW + days(29),
                renewal_period: days(30),
            }]
        }
    );
}

// "Renewal period", scenario "Outside the renewal period".
#[test]
fn certificate_outside_the_renewal_period_is_not_due() {
    let existing = expiring_in(45);
    assert_eq!(
        decide(&[holds("vault", &existing)]),
        Decision::NotDue {
            due_at: NOW + days(15)
        }
    );
}

#[test]
fn remaining_validity_equal_to_the_renewal_period_is_due() {
    let exactly = expiring_in(30);
    assert_eq!(
        reasons(&decide(&[holds("vault", &exactly)])),
        ["renewal_period"]
    );
    let one_second_more = stored(
        &["example.com"],
        days(60),
        days(30) + Duration::from_secs(1),
    );
    assert!(reasons(&decide(&[holds("vault", &one_second_more)])).is_empty());
}

#[test]
fn expired_certificate_is_due() {
    let expired = StoredCert {
        not_after: NOW - days(3),
        ..expiring_in(1)
    };
    assert_eq!(
        decide(&[holds("vault", &expired)]),
        Decision::Due {
            reasons: vec![Reason::Expired {
                store: "vault".into(),
                expiry: NOW - days(3)
            }]
        }
    );
    let just_now = StoredCert {
        not_after: NOW,
        ..expiring_in(1)
    };
    assert_eq!(reasons(&decide(&[holds("vault", &just_now)])), ["expired"]);
}

// --- Earlier renewal at the CA's request ----------------------------------------------

// "Earlier renewal at the CA's request", scenario "CA asks for early replacement".
#[test]
fn started_renewal_window_makes_the_certificate_due_before_the_renewal_period() {
    let existing = expiring_in(50);
    let state = with_window(holds("vault", &existing), NOW - days(1));
    assert_eq!(
        decide(&[state]),
        Decision::Due {
            reasons: vec![Reason::RenewalInfo {
                store: "vault".into(),
                window_start: NOW - days(1)
            }]
        }
    );
}

// "Earlier renewal at the CA's request", scenario "Window not started".
#[test]
fn renewal_window_that_has_not_started_does_not_make_the_certificate_due() {
    let existing = expiring_in(40);
    let state = with_window(holds("vault", &existing), NOW + days(7));
    assert_eq!(
        decide(&[state]),
        Decision::NotDue {
            due_at: NOW + days(7)
        },
        "it becomes due when the window starts, which is before the renewal period"
    );
}

#[test]
fn renewal_window_never_makes_a_certificate_due_later_than_the_renewal_period() {
    // The window starts in 20 days, the renewal period started yesterday.
    let existing = expiring_in(29);
    let state = with_window(holds("vault", &existing), NOW + days(20));
    assert_eq!(reasons(&decide(&[state])), ["renewal_period"]);

    // The window starts after the renewal period would: the period decides.
    let existing = expiring_in(45);
    let state = with_window(holds("vault", &existing), NOW + days(40));
    assert_eq!(
        decide(&[state]),
        Decision::NotDue {
            due_at: NOW + days(15)
        }
    );
}

#[test]
fn renewal_window_that_starts_now_makes_the_certificate_due() {
    let existing = expiring_in(50);
    assert_eq!(
        reasons(&decide(&[with_window(holds("vault", &existing), NOW)])),
        ["renewal_info"]
    );
}

// "Earlier renewal at the CA's request", scenario "Renewal information switched off",
// and "Renewal information is best effort", scenario "Server does not know the certificate":
// without a window only the other rules apply. The lookup itself is in `lookup` below.
#[test]
fn without_renewal_information_only_the_other_rules_apply() {
    let existing = expiring_in(50);
    assert_eq!(
        decide(&[holds("vault", &existing)]),
        Decision::NotDue {
            due_at: NOW + days(20)
        }
    );
}

// --- Renewal period longer than the certificate ------------------------------------------

// "Renewal period longer than the certificate", scenario "Setting exceeds lifetime".
#[test]
fn renew_before_at_or_above_the_lifetime_uses_half_the_lifetime_and_is_an_error() {
    let half = Duration::from_secs(45 * 86_400 / 2);
    assert_eq!(
        renewal_period(days(60), days(45)),
        (half, Some(Severity::Error))
    );
    assert_eq!(
        renewal_period(days(45), days(45)),
        (half, Some(Severity::Error)),
        "equal counts too"
    );

    // 23 days remain of 45: not yet due, because the period is 22.5 days.
    let not_yet = stored(&["example.com"], days(22), days(23));
    let evaluation = evaluate_with(days(60), false, &[holds("vault", &not_yet)]);
    assert_eq!(
        evaluation.decision,
        Decision::NotDue {
            due_at: NOW + days(23) - half
        }
    );
    assert_eq!(evaluation.renewal_periods, [Some(half)]);
    assert_eq!(
        evaluation.notes,
        [Note {
            severity: Severity::Error,
            message: "renew_before is 60d, which is not shorter than the lifetime of the stored \
                      certificate (45.0 days); half the lifetime, 22.5 days, is used as the \
                      renewal period"
                .into()
        }],
        "the error names both durations, in every run"
    );

    // 22.5 days remain: due.
    let due = stored(&["example.com"], half, half);
    let evaluation = evaluate_with(days(60), false, &[holds("vault", &due)]);
    assert_eq!(
        evaluation.decision,
        Decision::Due {
            reasons: vec![Reason::RenewalPeriod {
                store: "vault".into(),
                expiry: NOW + half,
                renewal_period: half
            }]
        }
    );
    assert_eq!(
        evaluation.notes.len(),
        1,
        "the error is logged whether due or not"
    );
}

// "Renewal period longer than the certificate", scenario "Setting exceeds half the lifetime".
#[test]
fn renew_before_above_half_the_lifetime_is_applied_and_is_a_warning() {
    assert_eq!(
        renewal_period(days(30), days(45)),
        (days(30), Some(Severity::Warning))
    );

    let not_yet = stored(&["example.com"], days(14), days(31));
    let evaluation = evaluate_with(days(30), false, &[holds("vault", &not_yet)]);
    assert_eq!(
        evaluation.decision,
        Decision::NotDue {
            due_at: NOW + days(1)
        }
    );
    assert_eq!(
        evaluation.notes,
        [Note {
            severity: Severity::Warning,
            message: "renew_before is 30d, which is more than half the lifetime of the stored \
                      certificate (45.0 days)"
                .into()
        }]
    );

    let due = stored(&["example.com"], days(15), days(30));
    let evaluation = evaluate_with(days(30), false, &[holds("vault", &due)]);
    assert_eq!(
        reasons(&evaluation.decision),
        ["renewal_period"],
        "due when 30 days remain"
    );
}

#[test]
fn renew_before_up_to_half_the_lifetime_needs_no_note() {
    assert_eq!(renewal_period(days(30), days(90)), (days(30), None));
    assert_eq!(
        renewal_period(days(45), days(90)),
        (days(45), None),
        "exactly half is fine"
    );
    let existing = expiring_in(60);
    assert!(
        evaluate_with(days(30), false, &[holds("vault", &existing)])
            .notes
            .is_empty()
    );
}

#[test]
fn note_about_the_same_setting_is_given_once_for_several_stores() {
    let (first, second) = (
        stored(&["example.com"], days(5), days(40)),
        stored(&["example.com"], days(5), days(40)),
    );
    let evaluation = evaluate_with(
        days(60),
        false,
        &[holds("vault", &first), holds("local", &second)],
    );
    assert_eq!(evaluation.notes.len(), 1);
}

// --- Reissue guard ----------------------------------------------------------------------

// "Reissue guard", scenario "Second store holds nothing".
#[test]
fn fresh_certificate_in_one_store_holds_back_issuing_for_an_empty_store() {
    let fresh = issued_ago(1);
    let decision = decide(&[holds("vault", &fresh), empty("local")]);
    assert_eq!(
        decision,
        Decision::HeldBack {
            reasons: vec![Reason::Missing {
                store: "local".into()
            }],
            fresh_store: "vault".into(),
            until: NOW + days(1),
        },
        "nothing is issued, and the time at which the guard ends is stated"
    );
    assert!(!decision.is_due());
}

// "Reissue guard", scenario "Second store is behind".
#[test]
fn fresh_certificate_in_one_store_holds_back_issuing_for_a_store_that_is_behind() {
    let fresh = issued_ago(1);
    let behind = expiring_in(10);
    let decision = decide(&[holds("vault", &fresh), holds("local", &behind)]);
    assert_eq!(
        decision,
        Decision::HeldBack {
            reasons: vec![Reason::RenewalPeriod {
                store: "local".into(),
                expiry: NOW + days(10),
                renewal_period: days(30)
            }],
            fresh_store: "vault".into(),
            until: NOW + days(1),
        }
    );
}

// "Reissue guard", scenario "Guard elapsed".
#[test]
fn certificate_issued_longer_ago_than_min_reissue_does_not_hold_back() {
    let older = issued_ago(3);
    let decision = decide(&[holds("vault", &older), empty("local")]);
    assert_eq!(
        decision,
        Decision::Due {
            reasons: vec![Reason::Missing {
                store: "local".into()
            }]
        }
    );

    let exactly = issued_ago(2);
    assert!(
        decide(&[holds("vault", &exactly), empty("local")]).is_due(),
        "the guard ends at 2 days"
    );
}

// "Reissue guard", scenario "CA asks for replacement of a fresh certificate".
#[test]
fn guard_does_not_apply_when_the_fresh_certificate_is_itself_due() {
    let fresh = issued_ago(1);
    let decision = decide(&[with_window(
        holds("vault", &fresh),
        NOW - Duration::from_secs(3600),
    )]);
    assert_eq!(reasons(&decision), ["renewal_info"]);
    assert!(decision.is_due(), "a new certificate is issued");

    // The same with a second store that holds nothing.
    let decision = decide(&[
        with_window(holds("vault", &fresh), NOW - Duration::from_secs(3600)),
        empty("local"),
    ]);
    assert!(decision.is_due());
}

#[test]
fn guard_needs_a_fresh_certificate_with_the_configured_names() {
    let other_names = stored(&["other.example.com"], days(1), days(89));
    let decision = decide(&[holds("vault", &other_names), empty("local")]);
    assert!(
        decision.is_due(),
        "a certificate for other names does not hold back"
    );
    assert_eq!(reasons(&decision), ["names_changed", "missing"]);
}

#[test]
fn guard_does_not_apply_when_nothing_is_due() {
    let fresh = issued_ago(1);
    assert!(matches!(
        decide(&[holds("vault", &fresh)]),
        Decision::NotDue { .. }
    ));
}

#[test]
fn guard_ends_when_the_most_recent_fresh_certificate_allows() {
    let one_day = issued_ago(1);
    let half_a_day = stored(&["example.com"], DAY / 2, days(90) - DAY / 2);
    let decision = decide(&[holds("a", &one_day), holds("b", &half_a_day), empty("c")]);
    assert_eq!(
        decision,
        Decision::HeldBack {
            reasons: vec![Reason::Missing { store: "c".into() }],
            fresh_store: "b".into(),
            until: NOW + days(2) - DAY / 2,
        }
    );
}

// --- Forced issuance ------------------------------------------------------------------

// "Forced issuance", scenario "Forced reissue".
#[test]
fn forced_certificate_is_due_whatever_the_stores_hold() {
    let existing = expiring_in(80);
    assert_eq!(
        decide_with(days(30), true, &[holds("vault", &existing)]),
        Decision::Due {
            reasons: vec![Reason::Forced]
        }
    );
}

#[test]
fn forcing_overrides_the_reissue_guard() {
    let fresh = issued_ago(1);
    let decision = decide_with(days(30), true, &[holds("vault", &fresh), empty("local")]);
    assert_eq!(
        decision,
        Decision::Due {
            reasons: vec![
                Reason::Forced,
                Reason::Missing {
                    store: "local".into()
                }
            ]
        }
    );
}

// --- Changing the ACME server is not a reason to issue ---------------------------------

// "Changing the ACME server is not a reason to issue", scenario "Server switched".
#[test]
fn acme_server_of_the_entry_is_no_input_of_the_decision() {
    use crate::config::{Config, Layer};

    let entry = |acme: &str| {
        let text = format!(
            r#"
            [acme.letsencrypt]
            directory_url = "https://acme.example.com/directory"
            [acme.other]
            directory_url = "https://acme.example.net/directory"
            [dns.cloudflare]
            type = "cloudflare"
            [store.local]
            type = "directory"
            path = "/certs"
            [[certificate]]
            name = "example-com"
            names = ["example.com"]
            acme = "{acme}"
            dns = "cloudflare"
            stores = ["local"]
            "#
        );
        Config::from_layers([Layer::from_toml("test.toml", &text).unwrap()]).unwrap()
    };
    let existing = expiring_in(60);
    let decide_for = |config: &Config| {
        let resolved = config.resolve(&config.certificates[0]);
        let stores = [holds("local", &existing)];
        evaluate(&Input::for_entry(
            &resolved,
            config.run.min_reissue,
            false,
            &stores,
            NOW,
        ))
        .decision
    };

    let before = decide_for(&entry("letsencrypt"));
    let after = decide_for(&entry("other"));
    assert_eq!(
        before,
        Decision::NotDue {
            due_at: NOW + days(30)
        }
    );
    assert_eq!(
        after, before,
        "switching the server changes nothing about the decision"
    );
}

// --- Evaluation across several stores ------------------------------------------------------

// "Evaluation across several stores", scenario "One store behind".
#[test]
fn certificate_is_due_when_it_is_due_for_any_store() {
    let (ahead, behind) = (expiring_in(60), expiring_in(10));
    let decision = decide(&[holds("vault", &ahead), holds("local", &behind)]);
    assert_eq!(
        decision,
        Decision::Due {
            reasons: vec![Reason::RenewalPeriod {
                store: "local".into(),
                expiry: NOW + days(10),
                renewal_period: days(30)
            }]
        }
    );
}

#[test]
fn time_at_which_a_certificate_becomes_due_is_the_earliest_of_its_stores() {
    let (later, sooner) = (expiring_in(80), expiring_in(50));
    let decision = decide(&[holds("vault", &later), holds("local", &sooner)]);
    assert_eq!(
        decision,
        Decision::NotDue {
            due_at: NOW + days(20)
        }
    );
}

#[test]
fn every_store_that_makes_the_certificate_due_gives_its_reason() {
    let expired = StoredCert {
        not_after: NOW - days(1),
        ..expiring_in(1)
    };
    let other_names = stored(&["old.example.com"], days(10), days(80));
    let decision = decide(&[empty("a"), holds("b", &expired), holds("c", &other_names)]);
    assert_eq!(reasons(&decision), ["missing", "expired", "names_changed"]);
}

// --- Wording ---------------------------------------------------------------------------------

#[test]
fn reasons_read_as_sentences() {
    let texts: Vec<String> = [
        Reason::Forced,
        Reason::Missing {
            store: "vault".into(),
        },
        Reason::NamesChanged {
            store: "vault".into(),
        },
        Reason::Expired {
            store: "vault".into(),
            expiry: NOW - days(1),
        },
        Reason::RenewalPeriod {
            store: "vault".into(),
            expiry: NOW + days(29),
            renewal_period: days(30),
        },
        Reason::RenewalInfo {
            store: "vault".into(),
            window_start: NOW - days(1),
        },
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        texts,
        [
            "issuance was forced",
            "store vault holds no certificate",
            "the certificate in store vault does not have the configured names",
            "the certificate in store vault expired on 2026-11-22T02:00:00Z",
            "the certificate in store vault expires on 2026-12-22T02:00:00Z, within the renewal \
             period of 30.0 days",
            "the ACME server asks for the certificate in store vault to be replaced from \
             2026-11-22T02:00:00Z on",
        ]
    );
}

#[test]
fn days_are_written_with_one_decimal() {
    assert_eq!(format_days(days(30)), "30.0 days");
    assert_eq!(
        format_days(Duration::from_secs(45 * 86_400 / 2)),
        "22.5 days"
    );
    assert_eq!(format_days(DAY), "1.0 day");
    assert_eq!(format_days(days(90) - Duration::from_secs(1)), "90.0 days");
}

// --- The `expiring` rule of run-events ----------------------------------------------------------

// run-events, "Expiring event", both scenarios.
#[test]
fn certificate_is_expiring_with_less_than_half_its_renewal_period_left() {
    assert!(
        is_expiring(NOW + days(12), days(30), NOW),
        "12 of 30 days left"
    );
    assert!(
        !is_expiring(NOW + days(25), days(30), NOW),
        "25 of 30 days left"
    );
    assert!(
        !is_expiring(NOW + days(15), days(30), NOW),
        "exactly half is not less than half"
    );
    assert!(is_expiring(
        NOW + days(15) - Duration::from_secs(1),
        days(30),
        NOW
    ));
    assert!(is_expiring(NOW - days(1), days(30), NOW), "already expired");
}

// --- Best-effort lookup of renewal information ----------------------------------------------------

mod lookup {
    use std::sync::Mutex;

    use super::*;
    use crate::logging::{Capture, Level, LogFormat};
    use crate::{Error, Result};

    enum Answer {
        Window(RenewalWindow),
        NotOffered,
        Fails(Error),
        Hangs,
    }

    struct Source {
        answer: Answer,
        asked: Mutex<u32>,
    }

    impl Source {
        fn new(answer: Answer) -> Self {
            Self {
                answer,
                asked: Mutex::new(0),
            }
        }

        fn asked(&self) -> u32 {
            *self.asked.lock().unwrap()
        }
    }

    #[async_trait]
    impl RenewalInfoSource for Source {
        async fn renewal_window(&self, _certificate: &StoredCert) -> Result<Option<RenewalWindow>> {
            *self.asked.lock().unwrap() += 1;
            match &self.answer {
                Answer::Window(window) => Ok(Some(*window)),
                Answer::NotOffered => Ok(None),
                Answer::Fails(error) => Err(error.clone()),
                Answer::Hangs => std::future::pending().await,
            }
        }
    }

    fn log() -> (Capture, StepLog) {
        let capture = Capture::new();
        let logger = capture.logger(LogFormat::Json, Level::Info, NOW);
        (capture, logger.step("example-com", "decide"))
    }

    #[tokio::test]
    async fn window_the_server_gives_is_returned() {
        let window = RenewalWindow {
            start: NOW - days(1),
            end: NOW + days(1),
        };
        let source = Source::new(Answer::Window(window));
        let (capture, log) = log();

        let found = lookup_renewal_window(Some(&source), &expiring_in(50), &log).await;

        assert_eq!(found, Some(window));
        assert!(capture.lines().is_empty());
    }

    // "Earlier renewal at the CA's request", scenario "Renewal information switched off".
    #[tokio::test]
    async fn server_is_not_asked_when_renewal_information_is_switched_off() {
        let (capture, log) = log();
        // No source is what `ari = false` gives; there is nothing that could be asked.
        assert_eq!(
            lookup_renewal_window(None, &expiring_in(50), &log).await,
            None
        );
        assert!(capture.lines().is_empty());
    }

    #[tokio::test]
    async fn server_that_offers_no_renewal_information_gives_none() {
        let source = Source::new(Answer::NotOffered);
        let (capture, log) = log();
        assert_eq!(
            lookup_renewal_window(Some(&source), &expiring_in(50), &log).await,
            None
        );
        assert_eq!(source.asked(), 1);
        assert!(
            capture.lines().is_empty(),
            "that is not a failure and needs no record"
        );
    }

    // "Renewal information is best effort", scenario "Server does not know the certificate".
    #[tokio::test]
    async fn failing_lookup_leaves_the_decision_to_the_renewal_period() {
        for failure in [
            Error::permanent("the server does not know the certificate"),
            Error::transient("connection reset"),
            Error::rate_limited("too many requests", Some(Duration::from_secs(60))),
        ] {
            let source = Source::new(Answer::Fails(failure.clone()));
            let (capture, log) = log();
            let existing = expiring_in(50);

            let window = lookup_renewal_window(Some(&source), &existing, &log).await;

            assert_eq!(window, None, "any error is no information");
            assert_eq!(source.asked(), 1, "the lookup is not repeated");
            let state = StoreState {
                store: "vault",
                current: Some(&existing),
                renewal_window: window,
            };
            assert_eq!(
                decide(&[state]),
                Decision::NotDue {
                    due_at: NOW + days(20)
                },
                "the certificate is judged by the renewal period alone and does not fail"
            );
            let records = capture.json();
            assert_eq!(records.len(), 1);
            assert_eq!(
                records[0]["level"], "info",
                "not an error: the certificate is fine"
            );
            assert_eq!(records[0]["reason"], failure.to_string());
            assert_eq!(records[0]["certificate"], "example-com");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn server_that_does_not_answer_gives_none_after_the_time_limit() {
        let source = Source::new(Answer::Hangs);
        let (capture, log) = log();
        let started = tokio::time::Instant::now();

        let window = lookup_renewal_window(Some(&source), &expiring_in(50), &log).await;

        assert_eq!(window, None);
        assert_eq!(started.elapsed(), RENEWAL_INFO_TIMEOUT);
        assert_eq!(capture.json()[0]["reason"], "no answer after 30s");
    }
}

#[test]
fn stores_are_listed_with_their_renewal_periods() {
    let existing = expiring_in(60);
    let evaluation = evaluate_with(
        days(30),
        false,
        &[holds("vault", &existing), empty("local")],
    );
    assert_eq!(evaluation.renewal_periods, [Some(days(30)), None]);
}
