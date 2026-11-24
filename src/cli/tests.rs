use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::CommandFactory;
use clap::error::ErrorKind;
use time::macros::datetime;

use super::*;
use crate::config::{Config, Layer, Secret};
use crate::pipeline::{CertificateStatus, Holding, Standing, StoreStatus};
use crate::policy::{Note, Reason, Severity};
use crate::store::StoredCert;

fn parse(arguments: &[&str]) -> Cli {
    let mut line = vec!["butterbot"];
    line.extend(arguments);
    Cli::try_parse_from(line).unwrap_or_else(|error| panic!("valid arguments: {error}"))
}

fn refused(arguments: &[&str]) -> clap::Error {
    let mut line = vec!["butterbot"];
    line.extend(arguments);
    Cli::try_parse_from(line).expect_err("not a valid command line")
}

#[test]
fn command_definition_is_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn version_is_the_crate_version() {
    assert_eq!(
        Cli::command().get_version(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

// --- Global options ----------------------------------------------------------

#[test]
fn log_options_have_their_defaults() {
    let cli = parse(&["run"]);
    assert_eq!(
        cli.log_format, None,
        "the format is chosen by terminal detection"
    );
    assert_eq!(cli.log_level, Level::Info);
    assert_eq!(cli.config, Vec::<PathBuf>::new());
}

#[test]
fn log_format_and_level_can_be_set_before_or_after_the_command() {
    let cli = parse(&["--log-format", "json", "--log-level", "debug", "run"]);
    assert_eq!(cli.log_format, Some(LogFormat::Json));
    assert_eq!(cli.log_level, Level::Debug);

    let cli = parse(&["status", "--log-format=text", "--log-level=error"]);
    assert_eq!(cli.log_format, Some(LogFormat::Text));
    assert_eq!(cli.log_level, Level::Error);
}

#[test]
fn unknown_log_format_or_level_is_a_usage_error() {
    for arguments in [
        ["--log-format", "xml", "run"],
        ["--log-level", "loud", "run"],
    ] {
        assert_eq!(refused(&arguments).exit_code(), 2);
    }
}

// configuration, "Configuration file": `--config` may be given several times,
// and the order is kept.
#[test]
fn config_may_be_given_several_times_and_keeps_its_order() {
    let cli = parse(&["--config", "a.toml", "--config", "b.toml", "check-config"]);
    assert_eq!(
        cli.config,
        [PathBuf::from("a.toml"), PathBuf::from("b.toml")]
    );

    let cli = parse(&["run", "--config", "c.toml"]);
    assert_eq!(cli.config, [PathBuf::from("c.toml")]);
}

// --- Commands -------------------------------------------------------------------

// command-line, "The run command".
#[test]
fn run_takes_a_selection_and_force() {
    assert_eq!(
        parse(&["run"]).command,
        Command::Run {
            only: Vec::new(),
            force: false
        }
    );
    assert_eq!(
        parse(&["run", "--only", "a", "--only", "b"]).command,
        Command::Run {
            only: vec!["a".to_owned(), "b".to_owned()],
            force: false
        }
    );
    assert_eq!(
        parse(&["run", "--force", "--only", "a"]).command,
        Command::Run {
            only: vec!["a".to_owned()],
            force: true
        }
    );
}

// command-line, "The run command", scenario "Force without selection".
#[test]
fn force_without_only_is_a_usage_error() {
    let error = refused(&["run", "--force"]);
    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("--only"), "{error}");
}

// design D16: there is no option that points a certificate at another server.
#[test]
fn run_has_no_option_to_override_the_acme_server() {
    for arguments in [
        ["run", "--acme", "staging"],
        ["run", "--server", "staging"],
        ["run", "--staging", "--only"],
    ] {
        assert_eq!(refused(&arguments).kind(), ErrorKind::UnknownArgument);
    }
}

#[test]
fn status_check_config_and_account_new_are_commands() {
    assert_eq!(parse(&["status"]).command, Command::Status { json: false });
    assert_eq!(
        parse(&["status", "--json"]).command,
        Command::Status { json: true }
    );
    assert_eq!(parse(&["check-config"]).command, Command::CheckConfig);
    assert_eq!(
        parse(&["account", "new", "--acme", "letsencrypt"]).command,
        Command::Account {
            command: AccountCommand::New {
                acme: "letsencrypt".to_owned()
            }
        }
    );
}

// command-line, "The notify test command": the sample is a `failure` by
// default, and `--event` chooses another kind (scenario "Another event kind").
#[test]
fn notify_test_takes_a_name_and_an_event_kind() {
    use crate::events::EventKind;

    assert_eq!(
        parse(&["notify", "test", "teams"]).command,
        Command::Notify {
            command: NotifyCommand::Test {
                name: "teams".to_owned(),
                event: EventKind::Failure
            }
        }
    );
    for kind in EventKind::ALL {
        assert_eq!(
            parse(&["notify", "test", "teams", "--event", kind.as_str()]).command,
            Command::Notify {
                command: NotifyCommand::Test {
                    name: "teams".to_owned(),
                    event: kind
                }
            }
        );
    }
}

// command-line, "The notify test command": exit code 2 for a usage error.
#[test]
fn notify_test_with_wrong_arguments_is_a_usage_error() {
    for arguments in [
        vec!["notify"],
        vec!["notify", "test"],
        vec!["notify", "test", "teams", "extra"],
        vec!["notify", "test", "teams", "--event"],
        vec!["notify", "test", "teams", "--event", "renewal"],
        vec!["notify", "send", "teams"],
        vec!["notify", "test", "--name", "teams"],
    ] {
        assert_eq!(
            refused(&arguments).exit_code(),
            2,
            "butterbot {}",
            arguments.join(" ")
        );
    }
    let error = refused(&["notify", "test", "teams", "--event", "renewal"]);
    assert!(
        error
            .to_string()
            .contains("known events: run_completed, failure, renewed, expiring"),
        "{error}"
    );
}

#[test]
fn notify_test_says_what_happened_without_a_url() {
    use crate::events::EventKind;

    assert_eq!(
        notify::delivered_text("teams", EventKind::Failure),
        "A failure test event was delivered to the notifier teams.\n"
    );
    assert_eq!(
        notify::not_delivered_text(
            "teams",
            EventKind::RunCompleted,
            "the endpoint answered with status 400"
        ),
        "A run_completed test event was not delivered to the notifier teams: the endpoint \
         answered with status 400.\n"
    );
}

#[test]
fn incomplete_or_unknown_commands_are_usage_errors() {
    for arguments in [
        vec!["renew"],
        vec!["account"],
        vec!["account", "new"],
        vec!["account", "delete", "--acme", "x"],
        vec!["status", "--only", "x"],
        vec!["check-config", "--json"],
        vec!["run", "extra"],
    ] {
        assert_eq!(
            refused(&arguments).exit_code(),
            2,
            "butterbot {}",
            arguments.join(" ")
        );
    }
}

// command-line, "Exit codes".
#[test]
fn exit_codes_are_0_1_2_and_3() {
    assert_eq!(Exit::Success.code(), 0);
    assert_eq!(Exit::Failed.code(), 1);
    assert_eq!(Exit::Invalid.code(), 2);
    assert_eq!(Exit::Interrupted.code(), 3);
}

// --- status -----------------------------------------------------------------------

fn stored(names: &[&str]) -> StoredCert {
    StoredCert {
        names: names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<_>>(),
        not_before: datetime!(2026-01-01 00:00:00 UTC),
        not_after: datetime!(2026-04-01 00:00:00 UTC),
        issuer: "CN=Example CA".to_owned(),
        serial: "0a1b".to_owned(),
        der: Vec::new(),
    }
}

fn statuses() -> Vec<CertificateStatus> {
    vec![
        CertificateStatus {
            name: "fresh".to_owned(),
            names: vec!["*.example.com".to_owned(), "example.com".to_owned()],
            stores: vec![StoreStatus {
                store: "vault".to_owned(),
                holding: Holding::Certificate(stored(&["*.example.com", "example.com"])),
            }],
            standing: Standing::NotDue {
                due_at: datetime!(2026-03-02 00:00:00 UTC),
            },
            notes: Vec::new(),
        },
        CertificateStatus {
            name: "partial".to_owned(),
            names: vec!["example.org".to_owned()],
            stores: vec![
                StoreStatus {
                    store: "vault".to_owned(),
                    holding: Holding::Certificate(stored(&["example.org"])),
                },
                StoreStatus {
                    store: "local".to_owned(),
                    holding: Holding::Nothing,
                },
            ],
            standing: Standing::Due {
                reasons: vec![Reason::Missing {
                    store: "local".to_owned(),
                }],
            },
            notes: vec![Note {
                severity: Severity::Warning,
                message: "renew_before is more than half the lifetime".to_owned(),
            }],
        },
        CertificateStatus {
            name: "held".to_owned(),
            names: vec!["example.net".to_owned()],
            stores: vec![StoreStatus {
                store: "vault".to_owned(),
                holding: Holding::Certificate(stored(&["www.example.net"])),
            }],
            standing: Standing::HeldBack {
                reasons: vec![Reason::NamesChanged {
                    store: "vault".to_owned(),
                }],
                fresh_store: "vault".to_owned(),
                until: datetime!(2026-01-02 00:00:00 UTC),
            },
            notes: Vec::new(),
        },
        CertificateStatus {
            name: "broken".to_owned(),
            names: vec!["example.edu".to_owned()],
            stores: vec![StoreStatus {
                store: "local".to_owned(),
                holding: Holding::Unreadable("fullchain.pem is not PEM".to_owned()),
            }],
            standing: Standing::Unknown,
            notes: Vec::new(),
        },
    ]
}

// command-line, "The status command": for every certificate and each of its
// stores what the store holds, whether the certificate is due and why, and
// otherwise when it becomes due.
#[test]
fn status_text_shows_every_certificate_with_its_stores_and_its_state() {
    assert_eq!(
        status::to_text(&statuses()),
        "\
fresh: not due, becomes due 2026-03-02T00:00:00Z
  names: *.example.com, example.com
  store vault: a certificate for *.example.com, example.com, expires 2026-04-01T00:00:00Z

partial: due
  names: example.org
  store vault: a certificate for example.org, expires 2026-04-01T00:00:00Z
  store local: nothing
  due because store local holds no certificate
  warning: renew_before is more than half the lifetime

held: due, but not issued before 2026-01-02T00:00:00Z
  names: example.net
  store vault: a certificate for www.example.net, expires 2026-04-01T00:00:00Z
  due because the certificate in store vault does not have the configured names
  held back because store vault holds a certificate that was issued only just

broken: unknown, because a store cannot be read
  names: example.edu
  store local: cannot be read: fullchain.pem is not PEM
"
    );
    assert_eq!(status::to_text(&[]), "No certificate is configured.\n");
}

// command-line, "The status command", scenario "JSON output": the same
// information as JSON.
#[test]
fn status_json_has_the_same_information() {
    let document = status::to_json(&statuses());
    assert_eq!(
        document,
        serde_json::json!({
            "certificates": [
                {
                    "name": "fresh",
                    "names": ["*.example.com", "example.com"],
                    "stores": [{
                        "store": "vault",
                        "state": "present",
                        "names": ["*.example.com", "example.com"],
                        "not_before": "2026-01-01T00:00:00Z",
                        "expiry": "2026-04-01T00:00:00Z",
                        "issuer": "CN=Example CA",
                        "serial": "0a1b",
                    }],
                    "state": "not_due",
                    "due_at": "2026-03-02T00:00:00Z",
                },
                {
                    "name": "partial",
                    "names": ["example.org"],
                    "stores": [
                        {
                            "store": "vault",
                            "state": "present",
                            "names": ["example.org"],
                            "not_before": "2026-01-01T00:00:00Z",
                            "expiry": "2026-04-01T00:00:00Z",
                            "issuer": "CN=Example CA",
                            "serial": "0a1b",
                        },
                        { "store": "local", "state": "empty" },
                    ],
                    "state": "due",
                    "reasons": [{
                        "code": "missing",
                        "text": "store local holds no certificate",
                    }],
                    "notes": [{
                        "severity": "warning",
                        "message": "renew_before is more than half the lifetime",
                    }],
                },
                {
                    "name": "held",
                    "names": ["example.net"],
                    "stores": [{
                        "store": "vault",
                        "state": "present",
                        "names": ["www.example.net"],
                        "not_before": "2026-01-01T00:00:00Z",
                        "expiry": "2026-04-01T00:00:00Z",
                        "issuer": "CN=Example CA",
                        "serial": "0a1b",
                    }],
                    "state": "held_back",
                    "reasons": [{
                        "code": "names_changed",
                        "text": "the certificate in store vault does not have the configured names",
                    }],
                    "held_back_by": "vault",
                    "held_back_until": "2026-01-02T00:00:00Z",
                },
                {
                    "name": "broken",
                    "names": ["example.edu"],
                    "stores": [{
                        "store": "local",
                        "state": "unreadable",
                        "error": "fullchain.pem is not PEM",
                    }],
                    "state": "unknown",
                },
            ]
        })
    );
    assert_eq!(
        status::to_json(&[]),
        serde_json::json!({ "certificates": [] })
    );
}

// --- check-config -----------------------------------------------------------------

fn config(text: &str) -> Config {
    Config::from_layers([Layer::from_toml("test.toml", text).expect("TOML")])
        .unwrap_or_else(|error| panic!("{error}"))
}

const TWO_SECRETS: &str = r#"
[acme.letsencrypt]
directory_url = "https://acme.example.com/directory"

[dns.cloudflare]
type = "cloudflare"

[dns.azure]
type = "azure-dns"
subscription_id = "00000000-0000-0000-0000-000000000000"
resource_group = "dns"

[store.local]
type = "directory"
path = "/var/lib/certificates"

[[certificate]]
name = "web"
names = ["example.com"]
acme = "letsencrypt"
dns = "cloudflare"
stores = ["local"]
"#;

// command-line, "The check-config command", scenario "Valid configuration
// with a missing secret".
#[test]
fn check_config_lists_a_missing_secret_with_its_name_in_each_source() {
    let config = config_with_account();
    let (text, exit) = check_config::report(&config, false);

    assert_eq!(exit, Exit::Invalid);
    assert_eq!(
        text,
        "\
The configuration is valid: 1 certificate, 1 ACME server, 2 DNS providers, 1 store.
Secrets are read from the environment; the Key Vault layer is not enabled.

Secrets the configuration requires:
  acme.letsencrypt.access_key: found
    environment variable: BUTTERBOT__ACME__LETSENCRYPT__ACCESS_KEY
    Key Vault secret:     acme--letsencrypt--access-key
  dns.cloudflare.access_key: MISSING
    environment variable: BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY
    Key Vault secret:     dns--cloudflare--access-key

1 secret is missing: dns.cloudflare.access_key.
"
    );
    assert!(
        !text.contains("credentials-of-the-account"),
        "the value of a secret is not shown"
    );
}

fn config_with_account() -> Config {
    config(&config_text_with_account())
}

// command-line, "The check-config command", scenario "Valid and complete".
#[test]
fn check_config_of_a_complete_configuration_succeeds() {
    let config = config(&config_text_with_account().replace(
        "type = \"cloudflare\"",
        "type = \"cloudflare\"\naccess_key = \"token-of-the-dns-provider\"",
    ));

    let (text, exit) = check_config::report(&config, true);

    assert_eq!(exit, Exit::Success);
    assert!(
        text.contains("Secrets are read from the environment and from Key Vault."),
        "{text}"
    );
    assert!(
        text.contains("  dns.cloudflare.access_key: found\n"),
        "{text}"
    );
    assert!(
        text.ends_with("\nEvery required secret was found.\n"),
        "{text}"
    );
    for secret in ["credentials-of-the-account", "token-of-the-dns-provider"] {
        assert!(!text.contains(secret), "the value of a secret is not shown");
    }
}

fn config_text_with_account() -> String {
    TWO_SECRETS.replace(
        "directory_url = \"https://acme.example.com/directory\"",
        "directory_url = \"https://acme.example.com/directory\"\n\
         access_key = \"credentials-of-the-account\"",
    )
}

#[test]
fn check_config_says_how_missing_account_credentials_are_made() {
    let (text, exit) = check_config::report(&config(TWO_SECRETS), false);

    assert_eq!(exit, Exit::Invalid);
    assert!(
        text.contains(
            "2 secrets are missing: acme.letsencrypt.access_key, dns.cloudflare.access_key."
        ),
        "{text}"
    );
    assert!(
        text.contains("`butterbot account new --acme <name>`"),
        "{text}"
    );
}

// design D5: with the Key Vault layer the first run registers the account
// and saves its credentials, so they are not missing before that run.
#[test]
fn account_credentials_are_not_missing_when_the_first_run_will_register_them() {
    let complete_but_for_the_account = config(&TWO_SECRETS.replace(
        "type = \"cloudflare\"",
        "type = \"cloudflare\"\naccess_key = \"token\"",
    ));

    let (text, exit) = check_config::report(&complete_but_for_the_account, true);
    assert_eq!(exit, Exit::Success, "{text}");
    assert!(
        text.contains(
            "  acme.letsencrypt.access_key: not found; the first run registers an account and \
             saves it to Key Vault\n"
        ),
        "{text}"
    );

    let (text, exit) = check_config::report(&complete_but_for_the_account, false);
    assert_eq!(exit, Exit::Invalid, "{text}");
    assert!(
        text.contains("  acme.letsencrypt.access_key: MISSING\n"),
        "{text}"
    );

    // Any other secret is missing with the layer, too.
    let (_, exit) = check_config::report(&config(TWO_SECRETS), true);
    assert_eq!(exit, Exit::Invalid);
}

#[test]
fn check_config_of_a_configuration_without_secrets_succeeds() {
    let config = config(
        r#"
        [store.local]
        type = "directory"
        path = "/var/lib/certificates"
        "#,
    );

    let (text, exit) = check_config::report(&config, false);

    assert_eq!(exit, Exit::Success);
    assert!(
        text.starts_with(
            "The configuration is valid: 0 certificates, 0 ACME servers, 0 DNS providers, 1 \
             store.\n"
        ),
        "{text}"
    );
    assert!(
        text.ends_with("\nThe configuration requires no secret.\n"),
        "{text}"
    );
}

// --- account new ------------------------------------------------------------------

// command-line, "The account new command", scenario "New account": the
// credentials are printed with the name each source expects them under.
#[test]
fn new_account_is_printed_with_the_names_to_store_its_credentials_under() {
    let text = account::registered_text(
        "letsencrypt",
        "https://acme.example.com/directory",
        "https://acme.example.com/account/7",
        &Secret::new(r#"{"id":"https://acme.example.com/account/7"}"#),
    );

    assert_eq!(
        text,
        "\
Registered an account at the ACME server letsencrypt (https://acme.example.com/directory).
Account: https://acme.example.com/account/7

Its credentials are the secret acme.letsencrypt.access_key. Store the last line of this output \
under one of these names:
  environment variable: BUTTERBOT__ACME__LETSENCRYPT__ACCESS_KEY
  Key Vault secret:     acme--letsencrypt--access-key

{\"id\":\"https://acme.example.com/account/7\"}
"
    );
}
