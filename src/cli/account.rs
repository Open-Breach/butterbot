//! `butterbot account new`: registers an account at an ACME server and prints
//! its credentials with the names to store them under.

use std::fmt::Write as _;

use super::{Cli, Exit, report_invalid, usage_error};
use crate::acme::AcmeClient;
use crate::config::{Secret, env_name, vault_name};
use crate::pipeline::http_client;
use crate::retry::retry;

pub(super) async fn new(cli: &Cli, acme: &str) -> Exit {
    let logger = cli.diagnostics();
    let config = match cli.load(&logger).await {
        Ok(loaded) => loaded.config,
        Err(error) => {
            report_invalid(&logger, &error);
            return Exit::Invalid;
        }
    };
    let Some(server) = config.acme.get(acme) else {
        let known: Vec<&str> = config.acme.keys().map(String::as_str).collect();
        return usage_error(format!(
            "--acme names an ACME server that is not configured: {acme} (configured: {})",
            known.join(", ")
        ));
    };

    let path = format!("acme.{acme}.access_key");
    if server.access_key.is_some() {
        logger.error(format!(
            "an account already exists for the ACME server {acme}: {path} is set. Nothing was \
             registered"
        ));
        return Exit::Invalid;
    }
    let binding = match (&server.eab_kid, &server.eab_key) {
        (Some(kid), Some(key)) => Some((kid.as_str(), key)),
        (Some(_), None) => {
            let path = format!("acme.{acme}.eab_key");
            logger.error(format!(
                "the ACME server {acme} has an eab_kid but the secret {path} is not set; supply \
                 it as the environment variable {} or as the Key Vault secret {}",
                env_name(&path),
                vault_name(&path)
            ));
            return Exit::Invalid;
        }
        (None, _) => None,
    };

    let http = match http_client(&config) {
        Ok(http) => http,
        Err(error) => {
            logger.error(format!("invalid configuration: {error}"));
            return Exit::Invalid;
        }
    };
    let action = format!("registering an account at the ACME server {acme}");
    let registered = retry(&logger, &action, || {
        AcmeClient::register(acme, &server.directory_url, binding, &http)
    })
    .await;
    match registered {
        Ok((client, credentials)) => {
            print!(
                "{}",
                registered_text(
                    acme,
                    &server.directory_url,
                    client.account_id(),
                    &credentials
                )
            );
            Exit::Success
        }
        Err(error) => {
            logger.error(format!("no account was registered: {error}"));
            Exit::Failed
        }
    }
}

/// What is printed for a new account. The last line is the credentials and
/// nothing else, so that it can be taken from the output as it is.
pub(super) fn registered_text(
    acme: &str,
    directory_url: &str,
    account: &str,
    credentials: &Secret,
) -> String {
    let path = format!("acme.{acme}.access_key");
    let mut text = String::new();
    let _ = writeln!(
        text,
        "Registered an account at the ACME server {acme} ({directory_url})."
    );
    let _ = writeln!(text, "Account: {account}");
    let _ = writeln!(text);
    let _ = writeln!(
        text,
        "Its credentials are the secret {path}. Store the last line of this output under one \
         of these names:"
    );
    let _ = writeln!(text, "  environment variable: {}", env_name(&path));
    let _ = writeln!(text, "  Key Vault secret:     {}", vault_name(&path));
    let _ = writeln!(text);
    let _ = writeln!(text, "{}", credentials.expose());
    text
}
