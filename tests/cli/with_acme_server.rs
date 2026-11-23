//! Command-line tests that need the ACME test server (Pebble) and its
//! challenge DNS server. They are compiled with the `test-support` feature
//! and ignored unless asked for; see `src/testing.rs`.

use butterbot::testing::{AcmeTestServer, PebbleOptions};

use super::*;

pub const CERTIFICATE: &str = "wildcard-example-com";

/// A configuration against the test servers, with a directory store. The
/// ACME server has no account yet.
pub struct Deployment {
    pub acme: AcmeTestServer,
    pub directory: TempDir,
}

impl Deployment {
    /// `acme_name` and `dns_name` are the instance names of the ACME server
    /// and the DNS provider; `acme_settings` are further lines for the ACME
    /// server.
    pub fn new(
        options: &PebbleOptions,
        acme_name: &str,
        dns_name: &str,
        acme_settings: &str,
    ) -> Self {
        let acme = AcmeTestServer::start_with(options);
        let directory = TempDir::new().expect("a temporary directory");
        let text = format!(
            r#"
[acme.{acme_name}]
directory_url = "{directory_url}"
ca_file = "{root}"
{acme_settings}

[dns.{dns_name}]
type = "challtestsrv"
management_url = "{management}"
zones = ["example.com"]
propagation_check = "resolver"
propagation_resolvers = ["{resolver}"]
propagation_interval = "1s"
propagation_timeout = "20s"
settle = "0s"

[store.local]
type = "directory"
path = "{path}"

[[certificate]]
name = "{CERTIFICATE}"
names = ["*.example.com", "example.com"]
acme = "{acme_name}"
dns = "{dns_name}"
stores = ["local"]
"#,
            directory_url = acme.pebble.directory_url,
            root = acme.pebble.root_file().display(),
            management = acme.dns.management_url(),
            resolver = acme.dns.dns,
            path = directory.path().join("certificates").display(),
        );
        fs::write(directory.path().join("butterbot.toml"), text).expect("the configuration");
        Self { acme, directory }
    }

    pub fn config(&self) -> String {
        self.directory
            .path()
            .join("butterbot.toml")
            .display()
            .to_string()
    }

    pub fn fullchain(&self) -> PathBuf {
        self.directory
            .path()
            .join("certificates")
            .join(CERTIFICATE)
            .join("fullchain.pem")
    }

    /// `butterbot --config <the configuration> <args>`.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = butterbot();
        command.arg("--config").arg(self.config()).args(args);
        command
    }
}

/// The credentials in the output of `account new`: its last line.
pub fn credentials_in(output: &str) -> String {
    let line = output.lines().last().expect("output").to_owned();
    let credentials: Value = serde_json::from_str(&line)
        .unwrap_or_else(|error| panic!("the last line is the credentials ({error}): {output}"));
    assert!(credentials["id"].is_string(), "{output}");
    line
}

// command-line, "The account new command", scenarios "New account" and
// "Account already exists"; the credentials it prints are then used for a
// first issuance, a run that does nothing and a forced renewal.
#[test]
#[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
fn account_new_registers_an_account_whose_credentials_issue_certificates() {
    const VARIABLE: &str = "BUTTERBOT__ACME__PEBBLE__ACCESS_KEY";
    let deployment = Deployment::new(&PebbleOptions::default(), "pebble", "test", "");

    // Without an account the configuration is valid but not complete.
    let finished = finish(deployment.command(&["check-config"]));
    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished
            .stdout
            .contains("  acme.pebble.access_key: MISSING\n"),
        "{}",
        finished.stdout
    );

    let finished = finish(deployment.command(&["account", "new", "--acme", "pebble"]));
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert!(
        finished.stdout.contains(&format!(
            "Registered an account at the ACME server pebble ({}).",
            deployment.acme.pebble.directory_url
        )),
        "{}",
        finished.stdout
    );
    assert!(
        finished
            .stdout
            .contains(&format!("  environment variable: {VARIABLE}\n")),
        "{}",
        finished.stdout
    );
    assert!(
        finished
            .stdout
            .contains("  Key Vault secret:     acme--pebble--access-key\n"),
        "{}",
        finished.stdout
    );
    let credentials = credentials_in(&finished.stdout);
    let with_account = |args: &[&str]| {
        let mut command = deployment.command(args);
        command.env(VARIABLE, &credentials);
        finish(command)
    };

    let finished = with_account(&["check-config"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert!(
        !finished.stdout.contains(&credentials),
        "the credentials are not shown"
    );

    // An account exists now: a second one is refused.
    let finished = with_account(&["account", "new", "--acme", "pebble"]);
    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished
            .stderr
            .contains("an account already exists for the ACME server pebble"),
        "{}",
        finished.stderr
    );
    assert_eq!(finished.stdout, "");

    // First issuance.
    let finished = with_account(&["status"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert!(
        finished.stdout.contains(&format!("{CERTIFICATE}: due\n")),
        "{}",
        finished.stdout
    );
    assert!(!deployment.fullchain().exists(), "status issued nothing");

    let finished = with_account(&["run"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(finished.completed()["renewed"], 1);
    assert_eq!(finished.events("renewed")[0]["certificate"], CERTIFICATE);
    let first = fs::read(deployment.fullchain()).expect("the certificate is in the store");

    // Nothing is due any more.
    let finished = with_account(&["status", "--json"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    let document: Value = serde_json::from_str(&finished.stdout).expect("JSON");
    let entry = &document["certificates"][0];
    assert_eq!(entry["state"], "not_due", "{document:#}");
    assert_eq!(
        entry["stores"][0]["names"],
        serde_json::json!(["*.example.com", "example.com"])
    );

    let finished = with_account(&["run"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(finished.completed()["renewed"], 0);
    assert_eq!(
        fs::read(deployment.fullchain()).expect("still there"),
        first,
        "the second run changed nothing"
    );

    // A forced renewal of the selected certificate.
    let finished = with_account(&["run", "--only", CERTIFICATE, "--force"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(finished.completed()["renewed"], 1);
    assert_ne!(
        fs::read(deployment.fullchain()).expect("still there"),
        first,
        "a new certificate is in the store"
    );
}

// command-line, "The account new command": external account binding is used
// when it is configured. acme-issuance, "External account binding".
#[test]
#[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
fn account_new_uses_the_external_account_binding_of_the_server() {
    const KID: &str = "kid-1";
    const KEY: &str = "zWNDZM6eQGHWpSRTPal5eIUYFTu7EajVIoguysqZ9wG44nMEtx3MUAsUDkMTQ12W";
    const KEY_VARIABLE: &str = "BUTTERBOT__ACME__BOUND__EAB_KEY";
    let options = PebbleOptions {
        external_account: Some((KID.to_owned(), KEY.to_owned())),
        ..PebbleOptions::default()
    };
    let deployment = Deployment::new(&options, "bound", "test", &format!("eab_kid = \"{KID}\""));

    // The key of the binding is a secret, and it is not supplied.
    let finished = finish(deployment.command(&["account", "new", "--acme", "bound"]));
    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished.stderr.contains(KEY_VARIABLE),
        "the message names the variable: {}",
        finished.stderr
    );
    assert_eq!(finished.stdout, "");

    let mut command = deployment.command(&["account", "new", "--acme", "bound"]);
    command.env(KEY_VARIABLE, KEY);
    let finished = finish(command);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    credentials_in(&finished.stdout);
    assert!(
        !finished.stdout.contains(KEY) && !finished.stderr.contains(KEY),
        "the key of the binding is not shown"
    );
}

// The server requires a binding and none is configured: nothing is registered.
#[test]
#[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
fn account_new_that_the_server_refuses_exits_with_1() {
    let options = PebbleOptions {
        external_account: Some(("kid-1".to_owned(), "c2VjcmV0".to_owned())),
        ..PebbleOptions::default()
    };
    let deployment = Deployment::new(&options, "pebble", "test", "");

    let finished = finish(deployment.command(&["account", "new", "--acme", "pebble"]));

    assert_eq!(finished.code, Some(1), "{}", finished.describe());
    assert!(
        finished.stderr.contains("no account was registered"),
        "{}",
        finished.stderr
    );
    assert_eq!(finished.stdout, "", "no credentials are printed");
}

// Task 9.6: every command of the README runs as written. The configuration
// has the instance and certificate names of the README's, and points at the
// test servers in place of Let's Encrypt and Cloudflare.
#[test]
#[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
fn every_command_of_the_readme_runs_as_written() {
    let configuration: toml::Table = readme_configuration().parse().expect("TOML");
    let only_key = |table: &str| -> String {
        let table = configuration[table].as_table().expect("a table");
        assert_eq!(table.len(), 1, "one instance");
        table.keys().next().expect("a key").clone()
    };
    let (acme, dns) = (only_key("acme"), only_key("dns"));
    assert_eq!(only_key("store"), "local");
    let certificate = &configuration["certificate"].as_array().expect("entries")[0];
    assert_eq!(certificate["name"].as_str(), Some(CERTIFICATE));
    assert_eq!(certificate["acme"].as_str(), Some(acme.as_str()));
    assert_eq!(certificate["dns"].as_str(), Some(dns.as_str()));

    let deployment = Deployment::new(&PebbleOptions::default(), &acme, &dns, "");
    let account_variable = butterbot::config::env_name(&format!("acme.{acme}.access_key"));
    let dns_variable = butterbot::config::env_name(&format!("dns.{dns}.access_key"));

    let mut variables: Vec<(String, String)> = Vec::new();
    let mut credentials = None;
    let mut commands = Vec::new();
    for line in readme_lines() {
        match line {
            ReadmeLine::Export { name, value } if name == account_variable => {
                assert!(value.starts_with('<'), "a placeholder: {value}");
                let credentials = credentials
                    .clone()
                    .expect("the README registers the account before it supplies the credentials");
                variables.push((name, credentials));
            }
            // The DNS provider of the test setup needs no secret.
            ReadmeLine::Export { name, value } if name == dns_variable => {
                assert!(value.starts_with('<'), "a placeholder: {value}");
            }
            ReadmeLine::Export { name, value } => {
                assert_eq!(name, "BUTTERBOT_CONFIG");
                variables.push((name, value));
            }
            ReadmeLine::Butterbot { arguments } => {
                let mut command = butterbot();
                command
                    .args(&arguments)
                    .current_dir(deployment.directory.path())
                    .envs(variables.iter().cloned());
                let finished = finish(command);
                let line = arguments.join(" ");
                assert_eq!(
                    finished.code,
                    Some(0),
                    "butterbot {line}\n{}",
                    finished.describe()
                );
                if arguments.starts_with(&["account".to_owned(), "new".to_owned()]) {
                    credentials = Some(credentials_in(&finished.stdout));
                }
                commands.push((line, finished));
            }
        }
    }

    let lines: Vec<&str> = commands.iter().map(|(line, _)| line.as_str()).collect();
    assert_eq!(
        lines,
        [
            format!("account new --acme {acme}").as_str(),
            "check-config",
            "status",
            "run",
            format!("run --only {CERTIFICATE} --force").as_str(),
        ],
        "the commands of the README"
    );
    // They did what the README says they do.
    assert!(
        commands[1]
            .1
            .stdout
            .contains("Every required secret was found.")
    );
    assert!(
        commands[2]
            .1
            .stdout
            .contains(&format!("{CERTIFICATE}: due\n")),
        "{}",
        commands[2].1.stdout
    );
    assert_eq!(commands[3].1.completed()["renewed"], 1);
    assert_eq!(commands[4].1.completed()["renewed"], 1);
    assert!(deployment.fullchain().exists());
    assert!(
        deployment
            .fullchain()
            .with_file_name("privkey.pem")
            .exists()
    );
}
