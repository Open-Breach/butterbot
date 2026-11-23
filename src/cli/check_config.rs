//! `butterbot check-config`: validates the configuration as a run does at its
//! start, and lists the secrets it expects. No ACME server, DNS provider or
//! store is contacted.

use std::fmt::Write as _;

use super::{Cli, Exit};
use crate::config::{Config, ConfigError, ExpectedSecret};

pub(super) async fn check_config(cli: &Cli) -> Exit {
    let logger = cli.diagnostics();
    let (text, exit) = match cli.load(&logger).await {
        Ok(loaded) => report(&loaded.config, loaded.vault.is_some()),
        Err(error) => (invalid(&error), Exit::Invalid),
    };
    print!("{text}");
    exit
}

fn invalid(error: &ConfigError) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "The configuration is invalid ({}):",
        counted(error.problems.len(), "problem")
    );
    for problem in &error.problems {
        let _ = writeln!(text, "  - {problem}");
    }
    text
}

/// Whether a secret that was not found keeps a run from working.
///
/// The credentials of an ACME account do not, when the Key Vault layer is
/// enabled: the first run registers an account and saves them there.
fn is_missing(secret: &ExpectedSecret, vault_layer: bool) -> bool {
    !secret.found && !(vault_layer && is_account(secret))
}

fn is_account(secret: &ExpectedSecret) -> bool {
    secret.path.starts_with("acme.") && secret.path.ends_with(".access_key")
}

/// What `check-config` prints for a valid configuration, and how it ends.
/// `vault_layer` says whether the Key Vault layer is enabled.
pub(super) fn report(config: &Config, vault_layer: bool) -> (String, Exit) {
    let secrets = config.expected_secrets();
    let mut text = String::new();
    let _ = writeln!(
        text,
        "The configuration is valid: {}, {}, {}, {}.",
        counted(config.certificates.len(), "certificate"),
        counted(config.acme.len(), "ACME server"),
        counted(config.dns.len(), "DNS provider"),
        counted(config.stores.len(), "store"),
    );
    let _ = writeln!(
        text,
        "Secrets are read from the environment{}.",
        if vault_layer {
            " and from Key Vault"
        } else {
            "; the Key Vault layer is not enabled"
        }
    );

    if secrets.is_empty() {
        text.push_str("\nThe configuration requires no secret.\n");
        return (text, Exit::Success);
    }

    text.push_str("\nSecrets the configuration requires:\n");
    for secret in &secrets {
        let state = if secret.found {
            "found"
        } else if is_missing(secret, vault_layer) {
            "MISSING"
        } else {
            "not found; the first run registers an account and saves it to Key Vault"
        };
        let _ = writeln!(text, "  {}: {state}", secret.path);
        let _ = writeln!(text, "    environment variable: {}", secret.env_name);
        let _ = writeln!(text, "    Key Vault secret:     {}", secret.vault_name);
    }

    let missing: Vec<&ExpectedSecret> = secrets
        .iter()
        .filter(|secret| is_missing(secret, vault_layer))
        .collect();
    text.push('\n');
    if missing.is_empty() {
        text.push_str("Every required secret was found.\n");
        return (text, Exit::Success);
    }
    let _ = writeln!(
        text,
        "{} missing: {}.",
        if missing.len() == 1 {
            "1 secret is".to_owned()
        } else {
            format!("{} secrets are", missing.len())
        },
        missing
            .iter()
            .map(|secret| secret.path.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if missing.iter().any(|secret| is_account(secret)) {
        text.push_str(
            "The credentials of an ACME account are made with `butterbot account new --acme \
             <name>`.\n",
        );
    }
    (text, Exit::Invalid)
}

fn counted(number: usize, what: &str) -> String {
    if number == 1 {
        format!("1 {what}")
    } else {
        format!("{number} {what}s")
    }
}
