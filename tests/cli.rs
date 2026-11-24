//! Command-line tests: they start the built `butterbot` executable.
//!
//! None of them needs a network: the certificates in the stores are made
//! here, and the servers that are named in the configurations are never
//! reached, or are a socket of the test that never answers.

use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use time::OffsetDateTime;

#[cfg(feature = "test-support")]
#[path = "cli/with_acme_server.rs"]
mod with_acme_server;

const ACCOUNT_VARIABLE: &str = "BUTTERBOT__ACME__TEST__ACCESS_KEY";
const DNS_VARIABLE: &str = "BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY";

/// A place for a configuration file and a directory store.
struct Setup {
    directory: TempDir,
}

impl Setup {
    /// A configuration with the ACME server `test`, the DNS provider
    /// `cloudflare`, the directory store `local` and one certificate entry
    /// per name in `certificates`, each for `<name>.example.com`.
    fn new(certificates: &[&str]) -> Self {
        Self::with_acme("https://acme.example.com/directory", certificates)
    }

    fn with_acme(directory_url: &str, certificates: &[&str]) -> Self {
        let setup = Self {
            directory: TempDir::new().expect("a temporary directory"),
        };
        let mut text = format!(
            r#"
[acme.test]
directory_url = "{directory_url}"
ari = false

[dns.cloudflare]
type = "cloudflare"

[store.local]
type = "directory"
path = "{}"
"#,
            setup.store().display()
        );
        for name in certificates {
            text.push_str(&format!(
                r#"
[[certificate]]
name = "{name}"
names = ["{name}.example.com"]
acme = "test"
dns = "cloudflare"
stores = ["local"]
"#
            ));
        }
        setup.write("butterbot.toml", &text);
        setup
    }

    fn path(&self, file: &str) -> PathBuf {
        self.directory.path().join(file)
    }

    fn write(&self, file: &str, text: &str) {
        fs::write(self.path(file), text).expect("the file can be written");
    }

    fn config(&self) -> String {
        self.path("butterbot.toml").display().to_string()
    }

    fn store(&self) -> PathBuf {
        self.directory.path().join("certificates")
    }

    /// Puts a certificate for `<name>.example.com` that expires in
    /// `remaining_days` days into the directory store.
    fn holds(&self, name: &str, remaining_days: i64) {
        let key = rcgen::KeyPair::generate().expect("a key");
        let mut parameters = rcgen::CertificateParams::new(vec![format!("{name}.example.com")])
            .expect("certificate parameters");
        let now = OffsetDateTime::now_utc();
        parameters.not_before = now - time::Duration::days(90 - remaining_days);
        parameters.not_after = now + time::Duration::days(remaining_days);
        let certificate = parameters.self_signed(&key).expect("a certificate");
        let folder = self.store().join(name);
        fs::create_dir_all(&folder).expect("the folder of the certificate");
        fs::write(folder.join("fullchain.pem"), certificate.pem()).expect("fullchain.pem");
        fs::write(folder.join("privkey.pem"), key.serialize_pem()).expect("privkey.pem");
    }

    /// Every file under the store, with its content.
    fn store_content(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn collect(folder: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
            let Ok(entries) = fs::read_dir(folder) else {
                return;
            };
            for entry in entries {
                let path = entry.expect("an entry").path();
                if path.is_dir() {
                    collect(&path, files);
                } else {
                    let content = fs::read(&path).expect("readable");
                    files.push((path, content));
                }
            }
        }
        let mut files = Vec::new();
        collect(&self.store(), &mut files);
        files.sort();
        files
    }

    /// `butterbot --config <the configuration> <args>`, with both secrets of
    /// the configuration in the environment.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = self.command_without_secrets(args);
        command
            .env(ACCOUNT_VARIABLE, "account-credentials")
            .env(DNS_VARIABLE, "dns-token");
        command
    }

    fn command_without_secrets(&self, args: &[&str]) -> Command {
        let mut command = butterbot();
        command.arg("--config").arg(self.config()).args(args);
        command
    }
}

/// The executable, with an empty environment and nothing on standard input.
fn butterbot() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_butterbot"));
    command.env_clear().stdin(Stdio::null());
    command
}

fn finish(mut command: Command) -> Finished {
    Finished::of(
        command
            .output()
            .expect("the butterbot executable can be started"),
    )
}

/// What a finished program left behind.
struct Finished {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Finished {
    fn of(output: Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).expect("UTF-8 on standard output"),
            stderr: String::from_utf8(output.stderr).expect("UTF-8 on standard error"),
        }
    }

    /// Standard output as JSON log records. Panics when a line is not one.
    fn records(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .map(|line| {
                let record: Value = serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("not a JSON object: {line:?} ({error})"));
                assert!(record.is_object(), "not a JSON object: {line:?}");
                record
            })
            .collect()
    }

    /// The events in the log, by kind.
    fn events(&self, kind: &str) -> Vec<Value> {
        self.records()
            .into_iter()
            .filter(|record| record["event"] == kind)
            .collect()
    }

    fn completed(&self) -> Value {
        let mut events = self.events("run_completed");
        assert_eq!(events.len(), 1, "one run_completed event:\n{}", self.stdout);
        events.remove(0)
    }

    fn describe(&self) -> String {
        format!(
            "exit code {:?}\n--- standard output\n{}--- standard error\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

// ---------------------------------------------------------------------------
// Help and version
// ---------------------------------------------------------------------------

// command-line, "Help and version", scenario "Version".
#[test]
fn version_prints_the_crate_version_and_exits_with_0() {
    let mut command = butterbot();
    command.arg("--version");
    let finished = finish(command);
    assert_eq!(finished.code, Some(0));
    assert_eq!(
        finished.stdout.trim_end(),
        format!("butterbot {}", env!("CARGO_PKG_VERSION"))
    );
}

// command-line, "Help and version".
#[test]
fn help_lists_the_commands_and_options_and_exits_with_0() {
    let mut command = butterbot();
    command.arg("--help");
    let finished = finish(command);
    assert_eq!(finished.code, Some(0));
    for expected in [
        "Usage: butterbot",
        "run ",
        "status ",
        "check-config ",
        "account ",
        "--config <FILE>",
        "--log-format <FORMAT>",
        "--log-level <LEVEL>",
        "--version",
        "Exit codes",
    ] {
        assert!(
            finished.stdout.contains(expected),
            "{expected:?} is in the help:\n{}",
            finished.stdout
        );
    }

    let mut command = butterbot();
    command.args(["run", "--help"]);
    let finished = finish(command);
    assert_eq!(finished.code, Some(0));
    for expected in ["--only <NAME>", "--force"] {
        assert!(finished.stdout.contains(expected), "{}", finished.stdout);
    }
}

#[test]
fn program_without_a_command_shows_the_help_and_is_a_usage_error() {
    let finished = finish(butterbot());
    assert_eq!(finished.code, Some(2));
    assert!(finished.stderr.contains("Usage: butterbot"));
}

// ---------------------------------------------------------------------------
// The run command, and the exit codes
// ---------------------------------------------------------------------------

// command-line, "The run command", scenario "Plain run"; "Exit codes": 0;
// run-orchestration, "Run once and exit", scenario "Nothing to do";
// certificate-stores, "Adoption of existing certificates", scenario "Certificate from
// the previous tool": the certificates in the store were not issued by Butterbot.
#[test]
fn plain_run_evaluates_every_certificate_and_exits_with_0_when_none_is_due() {
    let setup = Setup::new(&["one", "two", "three"]);
    for name in ["one", "two", "three"] {
        setup.holds(name, 60);
    }
    let before = setup.store_content();

    let finished = finish(setup.command(&["run"]));

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    let completed = finished.completed();
    assert_eq!(completed["checked"], 3);
    assert_eq!(completed["renewed"], 0);
    assert_eq!(completed["failed"], 0);
    assert_eq!(completed["outcome"], "success");
    assert_eq!(setup.store_content(), before, "nothing was written");
}

// command-line, "The run command": `--only` limits the run and may be given
// several times. run-orchestration, "Run once and exit", scenario "Selection".
#[test]
fn only_limits_the_run_to_the_named_certificates() {
    let setup = Setup::new(&["one", "two", "three"]);
    for name in ["one", "two", "three"] {
        setup.holds(name, 60);
    }

    let finished = finish(setup.command(&["run", "--only", "two"]));
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(finished.completed()["checked"], 1);
    let named: Vec<Value> = finished
        .records()
        .into_iter()
        .filter_map(|record| record.get("certificate").cloned())
        .collect();
    assert!(
        !named.is_empty() && named.iter().all(|name| name == "two"),
        "only the selected certificate is in the log: {named:?}"
    );

    let finished = finish(setup.command(&["run", "--only", "one", "--only", "three"]));
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(finished.completed()["checked"], 2);
}

// command-line, "The run command", scenario "Force without selection".
#[test]
fn force_without_only_is_a_usage_error_and_nothing_is_processed() {
    let setup = Setup::new(&["one"]);
    setup.holds("one", 60);
    let before = setup.store_content();

    let finished = finish(setup.command(&["run", "--force"]));

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(finished.stderr.contains("--only"), "{}", finished.stderr);
    assert_eq!(finished.stdout, "", "no run was started");
    assert_eq!(setup.store_content(), before);
}

// command-line, "The run command", scenario "Unknown certificate name".
#[test]
fn only_with_an_unknown_name_is_a_usage_error_that_names_it() {
    let setup = Setup::new(&["one"]);
    setup.holds("one", 60);

    let finished = finish(setup.command(&["run", "--only", "one", "--only", "nine"]));

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished
            .stderr
            .contains("--only names a certificate that is not configured: nine"),
        "{}",
        finished.stderr
    );
    assert_eq!(finished.stdout, "", "no run was started");
}

// command-line, "Exit codes", scenario "Certificate failure";
// configuration, "Missing secrets fail only what depends on them".
#[test]
fn run_with_a_failed_certificate_exits_with_1() {
    let setup = Setup::new(&["one"]);

    // The DNS provider has no credential, so the certificate cannot be issued.
    let mut command = setup.command_without_secrets(&["run"]);
    command.env(ACCOUNT_VARIABLE, "account-credentials");
    let finished = finish(command);

    assert_eq!(finished.code, Some(1), "{}", finished.describe());
    let failures = finished.events("failure");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["certificate"], "one");
    assert!(
        failures[0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("dns.cloudflare.access_key")),
        "{}",
        failures[0]
    );
    assert_eq!(finished.completed()["outcome"], "failed");
}

// configuration, "Missing secrets fail only what depends on them", scenario "One
// provider without credential", with the providers of the program.
#[test]
fn missing_dns_credential_fails_its_certificates_and_the_others_are_processed() {
    let setup = Setup::new(&[]);
    let mut text = fs::read_to_string(setup.path("butterbot.toml")).expect("the configuration");
    text.push_str(
        r#"
[dns.azure]
type = "azure-dns"
subscription_id = "00000000-0000-0000-0000-000000000000"
resource_group = "dns-zones"
"#,
    );
    for (name, dns) in [
        ("first-on-cloudflare", "cloudflare"),
        ("on-azure", "azure"),
        ("second-on-cloudflare", "cloudflare"),
    ] {
        text.push_str(&format!(
            r#"
[[certificate]]
name = "{name}"
names = ["{name}.example.com"]
acme = "test"
dns = "{dns}"
stores = ["local"]
"#
        ));
        setup.holds(name, 60);
    }
    setup.write("butterbot.toml", &text);

    // `dns.cloudflare.access_key` is supplied by no source.
    let mut command = setup.command_without_secrets(&["run"]);
    command.env(ACCOUNT_VARIABLE, "account-credentials");
    let finished = finish(command);

    assert_eq!(finished.code, Some(1), "{}", finished.describe());
    let failures = finished.events("failure");
    // Certificates are processed side by side, so the order is not fixed.
    let mut failed: Vec<&str> = failures
        .iter()
        .map(|failure| failure["certificate"].as_str().unwrap_or_default())
        .collect();
    failed.sort_unstable();
    assert_eq!(failed, ["first-on-cloudflare", "second-on-cloudflare"]);
    for failure in &failures {
        assert!(
            failure["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("dns.cloudflare.access_key")),
            "the message names the missing path: {failure}"
        );
    }
    let completed = finished.completed();
    assert_eq!(completed["checked"], 3);
    assert_eq!(completed["failed"], 2);
    assert!(
        finished.records().iter().any(|record| {
            record["certificate"] == "on-azure" && record["message"] == "the certificate is not due"
        }),
        "the certificate on Azure DNS is processed:\n{}",
        finished.stdout
    );
}

// command-line, "Exit codes", scenario "Configuration error"; run-events, "No
// completion event for an unfinished run", scenario "Invalid configuration";
// configuration, "Strict validation before work".
#[test]
fn invalid_configuration_exits_with_2_before_any_certificate_is_processed() {
    let setup = Setup::new(&["one"]);
    let mut text = fs::read_to_string(setup.path("butterbot.toml")).expect("the configuration");
    text.push_str("renew_befor = \"30d\"\nacme_server = \"test\"\n");
    setup.write("butterbot.toml", &text);

    let finished = finish(setup.command(&["run"]));

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    let records = finished.records();
    assert!(
        records.iter().all(|record| record["level"] == "error"),
        "nothing but the problems is logged: {}",
        finished.stdout
    );
    for key in ["renew_befor", "acme_server"] {
        assert!(
            records.iter().any(|record| {
                record["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(key))
            }),
            "the problem with {key} is reported: {}",
            finished.stdout
        );
    }
    assert!(finished.events("run_completed").is_empty());
    assert!(!setup.store().exists(), "the store was not touched");
}

#[test]
fn missing_configuration_file_exits_with_2() {
    let mut command = butterbot();
    command.args(["--config", "/nonexistent/butterbot.toml", "run"]);
    let finished = finish(command);
    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished
            .stdout
            .contains("/nonexistent/butterbot.toml does not exist")
    );
}

/// Account credentials that are well formed, for an account at
/// `directory_url`. No server knows the account.
fn well_formed_credentials(directory_url: &str) -> String {
    use base64::Engine;

    let key = rcgen::KeyPair::generate().expect("a key");
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.serialize_der());
    serde_json::json!({
        "id": format!("{directory_url}/account/1"),
        "key_pkcs8": key,
        "directory": directory_url,
    })
    .to_string()
}

/// A server that accepts connections and never answers.
struct SilentServer {
    listener: TcpListener,
    connections: Vec<std::net::TcpStream>,
}

impl SilentServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        listener.set_nonblocking(true).expect("non-blocking");
        Self {
            listener,
            connections: Vec::new(),
        }
    }

    fn url(&self) -> String {
        format!(
            "https://127.0.0.1:{}/directory",
            self.listener.local_addr().expect("an address").port()
        )
    }

    /// Takes the connections that have arrived, and says how many there are
    /// so far.
    fn connections(&mut self) -> usize {
        while let Ok((connection, _)) = self.listener.accept() {
            self.connections.push(connection);
        }
        self.connections.len()
    }

    /// Waits until a client has connected.
    fn wait_for_a_connection(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.connections() == 0 {
            assert!(Instant::now() < deadline, "no client connected");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

// command-line, "Exit codes", scenario "Interrupted"; run-events, "No
// completion event for an unfinished run", scenario "Interrupted run";
// run-orchestration, "Graceful stop".
#[test]
fn run_that_is_stopped_by_a_termination_signal_exits_with_3() {
    let mut server = SilentServer::start();
    let setup = Setup::with_acme(&server.url(), &["one"]);
    let mut command = setup.command(&["run"]);
    command
        .env(ACCOUNT_VARIABLE, well_formed_credentials(&server.url()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("the executable can be started");

    // The certificate is due, and the run now waits for the ACME server.
    server.wait_for_a_connection();
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("kill can be run");
    assert!(killed.success());

    let finished = Finished::of(child.wait_with_output().expect("the program ends"));
    assert_eq!(finished.code, Some(3), "{}", finished.describe());
    assert!(
        finished.events("run_completed").is_empty(),
        "an interrupted run has no completion event:\n{}",
        finished.stdout
    );
    let messages: Vec<String> = finished
        .records()
        .iter()
        .filter_map(|record| record["message"].as_str().map(str::to_owned))
        .collect();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("termination signal received")),
        "{messages:#?}"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.starts_with("run interrupted")),
        "{messages:#?}"
    );
    assert!(!setup.store().join("one").exists(), "nothing was stored");
}

// ---------------------------------------------------------------------------
// The log
// ---------------------------------------------------------------------------

// command-line, "Log format", scenario "In a container"; "One program for
// container and terminal", scenario "No terminal attached".
#[test]
fn log_is_one_json_object_per_line_when_standard_output_is_not_a_terminal() {
    let setup = Setup::new(&["one", "two"]);
    setup.holds("one", 60);

    let finished = finish(setup.command_without_secrets(&["run"]));

    assert_eq!(finished.code, Some(1), "{}", finished.describe());
    let records = finished.records();
    assert!(records.len() >= 4, "{}", finished.stdout);
    for record in &records {
        for field in ["timestamp", "level", "message"] {
            assert!(record[field].is_string(), "{field} is in {record}");
        }
    }
    assert_eq!(finished.stderr, "", "the log is on standard output only");
}

// command-line, "Log format": `--log-format` overrides the choice.
#[test]
fn log_format_text_overrides_the_choice() {
    let setup = Setup::new(&["one"]);
    setup.holds("one", 60);

    let finished = finish(setup.command(&["--log-format", "text", "run"]));

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert!(
        finished.stdout.lines().all(|line| !line.starts_with('{')),
        "{}",
        finished.stdout
    );
    assert!(
        finished.stdout.contains("INFO  run started certificates=1"),
        "{}",
        finished.stdout
    );
}

/// Runs the program with a terminal as its standard output, and returns what
/// the terminal received.
fn in_a_terminal(setup: &Setup, args: &[&str]) -> String {
    let mut line = vec![
        env!("CARGO_BIN_EXE_butterbot").to_owned(),
        "--config".to_owned(),
        setup.config(),
    ];
    line.extend(args.iter().map(|argument| (*argument).to_owned()));
    let output = Command::new("script")
        .args(["-q", "-e", "-c", &line.join(" "), "/dev/null"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/sh")
        .env(ACCOUNT_VARIABLE, "account-credentials")
        .env(DNS_VARIABLE, "dns-token")
        .stdin(Stdio::null())
        .output()
        .expect("`script` (util-linux) can be run");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    String::from_utf8(output.stdout).expect("UTF-8")
}

// command-line, "Log format", scenario "In a terminal".
#[test]
fn log_is_text_in_a_terminal_unless_json_is_asked_for() {
    let setup = Setup::new(&["one"]);
    setup.holds("one", 60);

    let text = in_a_terminal(&setup, &["run"]);
    assert!(text.contains("INFO  run started certificates=1"), "{text}");
    assert!(text.lines().all(|line| !line.starts_with('{')), "{text}");

    let json = in_a_terminal(&setup, &["--log-format", "json", "run"]);
    for line in json.lines() {
        let record: Value = serde_json::from_str(line.trim_end())
            .unwrap_or_else(|error| panic!("not JSON: {line:?} ({error})"));
        assert!(record.is_object());
    }
    assert!(json.contains("\"message\":\"run started\""), "{json}");
}

// command-line, "Log content": the level is selectable, and `info` is the
// default. Events are written at any level.
#[test]
fn log_level_selects_what_is_written() {
    let setup = Setup::new(&["one"]);
    setup.holds("one", 60);
    let levels = |args: &[&str]| -> Vec<String> {
        let finished = finish(setup.command(args));
        assert_eq!(finished.code, Some(0), "{}", finished.describe());
        finished
            .records()
            .iter()
            .filter(|record| record.get("event").is_none())
            .map(|record| record["level"].as_str().unwrap_or_default().to_owned())
            .collect()
    };

    let default = levels(&["run"]);
    assert!(default.contains(&"info".to_owned()));
    assert!(!default.contains(&"debug".to_owned()));
    assert_eq!(default, levels(&["--log-level", "info", "run"]));

    assert_eq!(
        levels(&["run", "--log-level", "error"]),
        Vec::<String>::new()
    );
    let finished = finish(setup.command(&["--log-level", "error", "run"]));
    assert_eq!(
        finished.completed()["checked"],
        1,
        "the completion event is written at any level"
    );
}

// command-line, "Log content", scenario "Failed step" (here the step is
// `configuration`; the propagation step is covered with fake providers).
#[test]
fn record_about_a_failed_certificate_carries_its_name_the_step_and_the_reason() {
    let setup = Setup::new(&["one"]);

    let finished = finish(setup.command_without_secrets(&["run"]));

    let records = finished.records();
    let failed = records
        .iter()
        .find(|record| record["message"] == "the certificate failed")
        .unwrap_or_else(|| panic!("a record about the failure:\n{}", finished.stdout));
    assert_eq!(failed["certificate"], "one");
    assert_eq!(failed["step"], "configuration");
    assert!(
        failed["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );
}

// ---------------------------------------------------------------------------
// The configuration files
// ---------------------------------------------------------------------------

// configuration, "Configuration file": `BUTTERBOT_CONFIG` names the file, and
// `--config` takes precedence and may be given several times.
#[test]
fn configuration_is_read_from_the_variable_or_from_the_options() {
    let setup = Setup::new(&["one"]);
    setup.holds("one", 60);
    setup.write("override.toml", "[run]\nparallel = 7\n");
    let parallel = |finished: &Finished| -> Value {
        finished
            .records()
            .iter()
            .find(|record| record["message"] == "run started")
            .unwrap_or_else(|| panic!("the run started:\n{}", finished.describe()))["parallel"]
            .clone()
    };

    let mut command = butterbot();
    command
        .arg("run")
        .env("BUTTERBOT_CONFIG", setup.config())
        .env(ACCOUNT_VARIABLE, "account-credentials")
        .env(DNS_VARIABLE, "dns-token");
    let finished = finish(command);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(parallel(&finished), 4, "the built-in default");

    let override_file = setup.path("override.toml").display().to_string();
    let mut command = setup.command(&["--config", &override_file, "run"]);
    command.env("BUTTERBOT_CONFIG", "/nonexistent/ignored.toml");
    let finished = finish(command);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(parallel(&finished), 7, "the later file overrides");
}

// ---------------------------------------------------------------------------
// The status command
// ---------------------------------------------------------------------------

// command-line, "The status command", scenario "Read-only".
#[test]
fn status_reports_due_certificates_and_changes_nothing() {
    let setup = Setup::new(&["fresh", "old", "new"]);
    setup.holds("fresh", 60);
    setup.holds("old", 10);
    let before = setup.store_content();

    let finished = finish(setup.command(&["status"]));

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    let text = &finished.stdout;
    assert!(text.contains("fresh: not due, becomes due "), "{text}");
    assert!(
        text.contains("  store local: a certificate for fresh.example.com, expires "),
        "{text}"
    );
    assert!(text.contains("old: due\n"), "{text}");
    assert!(
        text.contains("  due because the certificate in store local expires on "),
        "{text}"
    );
    assert!(text.contains("new: due\n"), "{text}");
    assert!(text.contains("  store local: nothing\n"), "{text}");
    assert!(
        text.contains("  due because store local holds no certificate\n"),
        "{text}"
    );
    assert_eq!(
        setup.store_content(),
        before,
        "nothing was issued or written"
    );
    assert!(!setup.store().join("new").exists());
}

// command-line, "The status command", scenario "JSON output".
#[test]
fn status_json_is_a_single_document_with_one_entry_per_certificate() {
    let setup = Setup::new(&["fresh", "old", "new"]);
    setup.holds("fresh", 60);
    setup.holds("old", 10);
    let before = setup.store_content();

    // Even with every log record asked for, standard output is the document.
    let finished = finish(setup.command(&["status", "--json", "--log-level", "debug"]));

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    let document: Value = serde_json::from_str(&finished.stdout)
        .unwrap_or_else(|error| panic!("one JSON document ({error}):\n{}", finished.stdout));
    let certificates = document["certificates"].as_array().expect("an array");
    assert_eq!(
        certificates
            .iter()
            .map(|certificate| certificate["name"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["fresh", "old", "new"]
    );

    let fresh = &certificates[0];
    assert_eq!(fresh["names"], serde_json::json!(["fresh.example.com"]));
    assert_eq!(fresh["state"], "not_due");
    assert!(fresh["due_at"].is_string());
    assert_eq!(fresh["stores"][0]["store"], "local");
    assert_eq!(fresh["stores"][0]["state"], "present");
    assert_eq!(
        fresh["stores"][0]["names"],
        serde_json::json!(["fresh.example.com"])
    );
    assert!(fresh["stores"][0]["expiry"].is_string());

    let old = &certificates[1];
    assert_eq!(old["state"], "due");
    assert_eq!(old["reasons"][0]["code"], "renewal_period");

    let new = &certificates[2];
    assert_eq!(new["state"], "due");
    assert_eq!(new["stores"][0]["state"], "empty");
    assert_eq!(new["reasons"][0]["code"], "missing");
    assert_eq!(
        new["reasons"][0]["text"],
        "store local holds no certificate"
    );

    assert_eq!(setup.store_content(), before);
}

#[test]
fn status_of_a_store_that_cannot_be_read_says_so_and_exits_with_1() {
    let setup = Setup::new(&["one", "two"]);
    setup.holds("one", 60);
    setup.holds("two", 60);
    fs::write(setup.store().join("two/fullchain.pem"), "not a certificate").expect("written");

    let finished = finish(setup.command(&["status"]));

    assert_eq!(finished.code, Some(1), "{}", finished.describe());
    assert!(finished.stdout.contains("one: not due"));
    assert!(
        finished
            .stdout
            .contains("two: unknown, because a store cannot be read"),
        "{}",
        finished.stdout
    );
    assert!(finished.stdout.contains("  store local: cannot be read: "));
}

#[test]
fn status_with_an_invalid_configuration_exits_with_2_and_prints_no_status() {
    let setup = Setup::new(&["one"]);
    setup.write("butterbot.toml", "[run]\nparalel = 2\n");

    let finished = finish(setup.command(&["status", "--json"]));

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert_eq!(finished.stdout, "");
    assert!(finished.stderr.contains("paralel"), "{}", finished.stderr);
}

// ---------------------------------------------------------------------------
// The check-config command
// ---------------------------------------------------------------------------

// command-line, "The check-config command", scenario "Valid configuration
// with a missing secret".
#[test]
fn check_config_lists_a_missing_secret_with_its_names_and_exits_with_2() {
    let setup = Setup::new(&["one"]);
    let mut command = setup.command_without_secrets(&["check-config"]);
    command.env(ACCOUNT_VARIABLE, "account-credentials");

    let finished = finish(command);

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    let text = &finished.stdout;
    assert!(text.contains("The configuration is valid"), "{text}");
    assert!(text.contains("  acme.test.access_key: found\n"), "{text}");
    assert!(
        text.contains(
            "  dns.cloudflare.access_key: MISSING\n    environment variable: \
             BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY\n    Key Vault secret:     \
             dns--cloudflare--access-key\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("1 secret is missing: dns.cloudflare.access_key."),
        "{text}"
    );
}

// command-line, "The check-config command", scenario "Valid and complete";
// configuration, "Secret values are never disclosed".
#[test]
fn check_config_of_a_complete_configuration_says_so_and_exits_with_0() {
    let setup = Setup::new(&["one"]);

    let finished = finish(setup.command(&["check-config"]));

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    let text = &finished.stdout;
    assert!(
        text.contains(
            "The configuration is valid: 1 certificate, 1 ACME server, 1 DNS provider, 1 store."
        ),
        "{text}"
    );
    assert!(text.contains("Every required secret was found."), "{text}");
    for secret in ["account-credentials", "dns-token"] {
        assert!(
            !text.contains(secret) && !finished.stderr.contains(secret),
            "the value of a secret is not shown"
        );
    }
}

// command-line, "The check-config command": no ACME server, DNS provider or
// store is contacted.
#[test]
fn check_config_contacts_no_server_and_no_store() {
    let mut server = SilentServer::start();
    let setup = Setup::with_acme(&server.url(), &["one"]);
    let mut command = setup.command(&["check-config"]);
    command.env(ACCOUNT_VARIABLE, well_formed_credentials(&server.url()));

    let finished = finish(command);

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(server.connections(), 0, "the ACME server was not contacted");
    assert!(!setup.store().exists(), "the store was not touched");
}

#[test]
fn check_config_of_an_invalid_configuration_lists_every_problem_and_exits_with_2() {
    let setup = Setup::new(&["one"]);
    setup.write(
        "butterbot.toml",
        "[run]\nparalel = 2\n\n[store.local]\ntype = \"cupboard\"\n",
    );

    let finished = finish(setup.command_without_secrets(&["check-config"]));

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    let text = &finished.stdout;
    assert!(
        text.contains("The configuration is invalid (2 problems):"),
        "{text}"
    );
    assert!(text.contains("paralel"), "{text}");
    assert!(text.contains("cupboard"), "{text}");
}

// ---------------------------------------------------------------------------
// Webhook notifiers
// ---------------------------------------------------------------------------

const WEBHOOK_URL_VARIABLE: &str = "BUTTERBOT__NOTIFY__CONSOLE__URL";
const WEBHOOK_KEY_VARIABLE: &str = "BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY";

impl Setup {
    /// Adds text to the end of the configuration file.
    fn append(&self, text: &str) {
        let mut configuration =
            fs::read_to_string(self.path("butterbot.toml")).expect("the configuration file");
        configuration.push_str(text);
        self.write("butterbot.toml", &configuration);
    }
}

// webhook-notifier, "Notifier without a URL is skipped": `check-config` lists
// the path as a missing secret. Task 1.3: with its name in each source, and
// the key only when a template uses it.
#[test]
fn check_config_lists_the_url_of_a_webhook_and_its_key_only_when_referenced() {
    let setup = Setup::new(&["one"]);
    setup.append("\n[notify.console]\ntype = \"webhook\"\naccess_key = \"not-referenced\"\n");

    let finished = finish(setup.command(&["check-config"]));

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    let text = &finished.stdout;
    assert!(
        text.contains(
            "  notify.console.url: MISSING\n    environment variable: \
             BUTTERBOT__NOTIFY__CONSOLE__URL\n    Key Vault secret:     notify--console--url\n"
        ),
        "{text}"
    );
    assert!(!text.contains("notify.console.access_key"), "{text}");
    assert!(
        text.contains("1 secret is missing: notify.console.url."),
        "{text}"
    );
}

// webhook-notifier, "Headers and access key", scenario "Key referenced but
// missing": `check-config` lists the key as missing.
#[test]
fn check_config_lists_a_referenced_key_that_is_missing() {
    let setup = Setup::new(&["one"]);
    setup.append(
        "\n[notify.console]\ntype = \"webhook\"\n\n[notify.console.headers]\nX-API-KEY = \
         \"{{access_key}}\"\n",
    );
    let mut command = setup.command(&["check-config"]);
    command.env(
        WEBHOOK_URL_VARIABLE,
        "https://console.example.com/hook/credential",
    );

    let finished = finish(command);

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    let text = &finished.stdout;
    assert!(text.contains("  notify.console.url: found\n"), "{text}");
    assert!(
        text.contains(
            "  notify.console.access_key: MISSING\n    environment variable: \
             BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY\n    Key Vault secret:     \
             notify--console--access-key\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("1 secret is missing: notify.console.access_key."),
        "{text}"
    );
    assert!(
        !text.contains("console.example.com") && !finished.stderr.contains("console.example.com"),
        "the URL is not shown"
    );
}

// webhook-notifier, "Nothing secret in the output": the output of `check-config`.
#[test]
fn check_config_of_a_complete_webhook_shows_neither_the_url_nor_the_key() {
    let setup = Setup::new(&["one"]);
    setup.append(
        "\n[notify.console]\ntype = \"webhook\"\n\n[notify.console.headers]\nX-API-KEY = \
         \"{{access_key}}\"\n",
    );
    let mut command = setup.command(&["check-config"]);
    command
        .env(
            WEBHOOK_URL_VARIABLE,
            "https://console.example.com/hook/credential",
        )
        .env(WEBHOOK_KEY_VARIABLE, "console-key");

    let finished = finish(command);

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    let text = &finished.stdout;
    assert!(text.contains("  notify.console.url: found\n"), "{text}");
    assert!(
        text.contains("  notify.console.access_key: found\n"),
        "{text}"
    );
    assert!(text.contains("Every required secret was found."), "{text}");
    for secret in ["console.example.com", "credential", "console-key"] {
        assert!(
            !text.contains(secret) && !finished.stderr.contains(secret),
            "{secret} is not shown"
        );
    }
}

/// A local server that answers every request with one status and keeps the
/// requests the program sends to it.
struct WebhookServer {
    runtime: tokio::runtime::Runtime,
    server: wiremock::MockServer,
}

/// The path of the URLs given to the program. It stands for the credential a
/// webhook URL carries.
const WEBHOOK_PATH: &str = "/hook/credential-in-the-path";

impl WebhookServer {
    fn answering(status: u16) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let server = runtime.block_on(async {
            let server = wiremock::MockServer::start().await;
            wiremock::Mock::given(wiremock::matchers::any())
                .respond_with(wiremock::ResponseTemplate::new(status))
                .mount(&server)
                .await;
            server
        });
        Self { runtime, server }
    }

    fn url(&self) -> String {
        format!("{}{WEBHOOK_PATH}", self.server.uri())
    }

    /// The host and port of the server, as they are written in its URL.
    fn authority(&self) -> String {
        self.server.address().to_string()
    }

    fn requests(&self) -> Vec<wiremock::Request> {
        self.runtime
            .block_on(self.server.received_requests())
            .expect("the server records its requests")
    }

    /// The bodies of the requests, as JSON, in the order they arrived.
    fn bodies(&self) -> Vec<Value> {
        self.requests()
            .iter()
            .map(|request| serde_json::from_slice(&request.body).expect("a JSON body"))
            .collect()
    }
}

/// The address of a port on which nothing listens.
fn closed_port() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("its address")
}

/// The records of the log about notifiers: the ones with a `notifier` field.
fn notifier_records(finished: &Finished) -> Vec<Value> {
    finished
        .records()
        .into_iter()
        .filter(|record| record.get("notifier").is_some())
        .collect()
}

// Task 3.6. webhook-notifier, "Webhook notifier type", scenario "One notifier";
// "Completion is delivered last", scenario "Failure and completion".
#[test]
fn run_with_a_failed_certificate_sends_the_failure_and_then_the_completion() {
    let server = WebhookServer::answering(200);
    let setup = Setup::new(&["one"]);
    setup.append("\n[notify.console]\ntype = \"webhook\"\n");

    // The DNS provider has no credential, so the certificate fails.
    let mut command = setup.command_without_secrets(&["run"]);
    command
        .env(ACCOUNT_VARIABLE, "account-credentials")
        .env(WEBHOOK_URL_VARIABLE, server.url());
    let finished = finish(command);

    assert_eq!(finished.code, Some(1), "{}", finished.describe());
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{}", finished.describe());
    for request in &requests {
        assert_eq!(request.method.as_str(), "POST");
        assert_eq!(request.url.path(), WEBHOOK_PATH);
    }
    let bodies = server.bodies();
    assert_eq!(bodies[0]["event"], "failure", "the failure comes first");
    assert_eq!(bodies[0]["certificate"], "one");
    assert_eq!(bodies[0]["step"], "configuration");
    assert_eq!(
        bodies[1]["event"], "run_completed",
        "the completion is last"
    );
    assert_eq!(bodies[1]["outcome"], "failed");
    assert_eq!(bodies[1]["failed"], 1);

    // What was sent is what the log has.
    let mut logged_failure = finished.events("failure").remove(0);
    let mut logged_completion = finished.completed();
    for (logged, sent) in [
        (&mut logged_failure, &bodies[0]),
        (&mut logged_completion, &bodies[1]),
    ] {
        let logged = logged.as_object_mut().expect("an object");
        for added_by_the_log in ["level", "message"] {
            logged.remove(added_by_the_log);
        }
        assert_eq!(&Value::Object(logged.clone()), sent);
    }

    // The URL is in no output. Its use of http is warned about.
    for output in [&finished.stdout, &finished.stderr] {
        assert!(!output.contains(WEBHOOK_PATH), "{output}");
        assert!(!output.contains(&server.authority()), "{output}");
    }
    let about_notifiers = notifier_records(&finished);
    assert_eq!(about_notifiers.len(), 1, "{}", finished.describe());
    assert_eq!(about_notifiers[0]["level"], "warn");
    assert_eq!(about_notifiers[0]["notifier"], "console");
}

// Task 3.6. run-events, "Delivery failures never change the run", with the
// notifier type of the program: an endpoint that cannot be reached leaves the
// exit code as it is without the notifier.
#[test]
fn run_with_an_unreachable_endpoint_ends_as_it_does_without_the_notifier() {
    let run = |notifier: &str| {
        let setup = Setup::new(&["one"]);
        setup.holds("one", 60);
        setup.append(notifier);
        let mut command = setup.command(&["run"]);
        if !notifier.is_empty() {
            command.env(
                WEBHOOK_URL_VARIABLE,
                format!("http://{}{WEBHOOK_PATH}", closed_port()),
            );
        }
        finish(command)
    };

    let without = run("");
    // The instance selects one event, so that the test waits for the retries
    // of one delivery only.
    let with = run("\n[notify.console]\ntype = \"webhook\"\nevents = [\"run_completed\"]\n");

    assert_eq!(without.code, Some(0), "{}", without.describe());
    assert_eq!(with.code, without.code, "{}", with.describe());
    assert_eq!(with.completed()["outcome"], "success");
    assert_eq!(with.completed()["checked"], 1);

    let about_notifiers = notifier_records(&with);
    let undelivered: Vec<&Value> = about_notifiers
        .iter()
        .filter(|record| record["level"] == "error")
        .collect();
    assert_eq!(undelivered.len(), 1, "{}", with.describe());
    assert_eq!(undelivered[0]["notifier"], "console");
    assert_eq!(undelivered[0]["undelivered_event"], "run_completed");
    assert_eq!(undelivered[0]["reason"], "the connection failed");
    assert!(!with.stdout.contains(WEBHOOK_PATH), "{}", with.stdout);
}

// webhook-notifier, "Notifier without a URL is skipped", scenario "Run with a
// missing URL".
#[test]
fn run_with_a_notifier_without_a_url_names_the_path_and_is_otherwise_normal() {
    let run = |notifier: &str| {
        let setup = Setup::new(&["one", "two"]);
        setup.holds("one", 60);
        setup.holds("two", 60);
        setup.append(notifier);
        finish(setup.command(&["run"]))
    };

    let without = run("");
    let finished = run("\n[notify.teams]\ntype = \"webhook\"\nformat = \"teams\"\n");

    assert_eq!(without.code, Some(0), "{}", without.describe());
    assert_eq!(
        finished.code,
        without.code,
        "the exit code depends on the certificates only: {}",
        finished.describe()
    );
    let records = finished.records();
    let about_notifiers = notifier_records(&finished);
    assert_eq!(about_notifiers.len(), 1, "{}", finished.describe());
    assert_eq!(about_notifiers[0]["level"], "error");
    assert_eq!(about_notifiers[0]["notifier"], "teams");
    assert!(
        about_notifiers[0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.starts_with("notify.teams.url is not set")),
        "{}",
        about_notifiers[0]
    );
    let error_at = records
        .iter()
        .position(|record| record.get("notifier").is_some())
        .expect("the error");
    let run_started_at = records
        .iter()
        .position(|record| record["message"] == "run started")
        .expect("the run starts");
    assert!(
        error_at < run_started_at,
        "the error is logged at the start"
    );

    let completed = finished.completed();
    assert_eq!(
        completed["checked"], 2,
        "certificates are processed normally"
    );
    assert_eq!(completed["failed"], 0);
    assert_eq!(completed["outcome"], without.completed()["outcome"]);
}

// The same configuration without the notifier ends the same way.
#[test]
fn run_without_a_notifier_is_as_before() {
    let setup = Setup::new(&["one"]);
    setup.holds("one", 60);

    let finished = finish(setup.command(&["run"]));

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert!(notifier_records(&finished).is_empty());
}

// ---------------------------------------------------------------------------
// The account new command
// ---------------------------------------------------------------------------

// command-line, "The account new command", scenario "Account already exists".
#[test]
fn account_new_refuses_when_the_server_has_credentials_and_registers_nothing() {
    let mut server = SilentServer::start();
    let setup = Setup::with_acme(&server.url(), &["one"]);

    let finished = finish(setup.command(&["account", "new", "--acme", "test"]));

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished
            .stderr
            .contains("an account already exists for the ACME server test"),
        "{}",
        finished.stderr
    );
    assert_eq!(finished.stdout, "", "no credentials are printed");
    assert_eq!(server.connections(), 0, "the ACME server was not contacted");
}

#[test]
fn account_new_for_an_unknown_server_is_a_usage_error() {
    let setup = Setup::new(&["one"]);

    let finished = finish(setup.command(&["account", "new", "--acme", "elsewhere"]));

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished
            .stderr
            .contains("--acme names an ACME server that is not configured: elsewhere"),
        "{}",
        finished.stderr
    );

    let finished = finish(setup.command(&["account", "new"]));
    assert_eq!(finished.code, Some(2), "--acme is required");
}

// command-line, "One program for container and terminal": the program never
// waits for input. Standard input is closed here, and a pipe that is never
// written to would be the same to the program.
#[test]
fn no_command_reads_standard_input() {
    let setup = Setup::new(&["one"]);
    setup.holds("one", 60);
    for args in [
        vec!["run"],
        vec!["status"],
        vec!["check-config"],
        vec!["account", "new", "--acme", "test"],
    ] {
        let mut command = setup.command(&args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("started");
        // The pipe stays open and empty until the program has ended.
        let stdin = child.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(30);
        while child
            .try_wait()
            .expect("the state of the program")
            .is_none()
        {
            assert!(
                Instant::now() < deadline,
                "`butterbot {}` waits for input",
                args.join(" ")
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(stdin);
        let mut output = String::new();
        child
            .stdout
            .take()
            .expect("piped")
            .read_to_string(&mut output)
            .expect("readable");
    }
}

// ---------------------------------------------------------------------------
// The README
// ---------------------------------------------------------------------------

/// The content of the fenced blocks of the README that are marked `language`.
fn readme_blocks(language: &str) -> Vec<String> {
    let readme =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).expect("the README");
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in readme.lines() {
        match &mut current {
            None if line.trim() == format!("```{language}") => current = Some(String::new()),
            None => {}
            Some(block) if line.trim() == "```" => {
                blocks.push(std::mem::take(block));
                current = None;
            }
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            }
        }
    }
    blocks
}

/// What the quick start of the README asks the reader to type, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReadmeLine {
    /// `export NAME=value`.
    Export { name: String, value: String },
    /// `butterbot <arguments>`.
    Butterbot { arguments: Vec<String> },
}

fn readme_lines() -> Vec<ReadmeLine> {
    readme_blocks("sh")
        .iter()
        .flat_map(|block| block.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if let Some(assignment) = line.strip_prefix("export ") {
                let (name, value) = assignment.split_once('=').expect("NAME=value");
                ReadmeLine::Export {
                    name: name.to_owned(),
                    value: value.trim_matches('\'').to_owned(),
                }
            } else if let Some(arguments) = line.strip_prefix("butterbot ") {
                ReadmeLine::Butterbot {
                    arguments: arguments.split_whitespace().map(str::to_owned).collect(),
                }
            } else {
                panic!("a line of the README that is neither an export nor a command: {line}")
            }
        })
        .collect()
}

/// The configuration of the quick start.
fn readme_configuration() -> String {
    let mut blocks = readme_blocks("toml");
    assert_eq!(blocks.len(), 1, "the README has one configuration");
    blocks.remove(0)
}

// Task 9.6: the configuration of the quick start is valid, the secrets it
// requires are the ones the README tells the reader to supply, and it is
// complete once they are supplied.
#[test]
fn configuration_of_the_readme_is_valid_and_its_secrets_are_the_ones_it_names() {
    let directory = TempDir::new().expect("a temporary directory");
    fs::write(
        directory.path().join("butterbot.toml"),
        readme_configuration(),
    )
    .expect("written");
    let exports: Vec<(String, String)> = readme_lines()
        .into_iter()
        .filter_map(|line| match line {
            ReadmeLine::Export { name, value } => Some((name, value)),
            ReadmeLine::Butterbot { .. } => None,
        })
        .collect();
    assert_eq!(
        exports[0],
        ("BUTTERBOT_CONFIG".to_owned(), "butterbot.toml".to_owned())
    );
    let check_config = |variables: &[(String, String)]| {
        let mut command = butterbot();
        command
            .arg("check-config")
            .current_dir(directory.path())
            .envs(variables.iter().cloned());
        finish(command)
    };

    let finished = check_config(&exports[..1]);
    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished.stdout.contains("The configuration is valid"),
        "{}",
        finished.stdout
    );
    for (name, _) in &exports[1..] {
        assert!(
            finished
                .stdout
                .contains(&format!("environment variable: {name}\n")),
            "{name} is a secret the configuration requires:\n{}",
            finished.stdout
        );
    }
    assert_eq!(
        finished.stdout.matches(": MISSING").count(),
        exports.len() - 1,
        "the README names every secret:\n{}",
        finished.stdout
    );

    let finished = check_config(&exports);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
}

// Task 9.6: every command and option the README describes exists.
#[test]
fn commands_and_options_of_the_readme_exist() {
    let readme =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).expect("the README");
    let section = readme
        .split("## Commands")
        .nth(1)
        .and_then(|rest| rest.split("### Exit codes").next())
        .expect("the section about the commands");
    let described: Vec<&str> = section
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|line| line.split('`').next())
        .collect();
    assert_eq!(
        described,
        [
            "run",
            "run --only <name>",
            "run --only <name> --force",
            "status",
            "status --json",
            "check-config",
            "account new --acme <name>",
            "--config <file>",
            "--log-format json",
            "--log-level error",
            "--help",
        ],
        "the commands and options the README describes"
    );

    let setup = Setup::new(&["one"]);
    setup.holds("one", 60);
    for (line, expected) in [
        ("run", 0),
        ("run --only one", 0),
        ("status", 0),
        ("status --json", 0),
        ("check-config", 0),
        // The server has credentials, so this is refused; it is run for
        // real with the ACME test server.
        ("account new --acme test", 2),
        ("run --log-format json", 0),
        ("run --log-format text", 0),
        ("run --log-level error", 0),
        ("run --log-level warn", 0),
        ("run --log-level info", 0),
        ("run --log-level debug", 0),
        ("--help", 0),
        ("run --help", 0),
        ("--version", 0),
    ] {
        let arguments: Vec<&str> = line.split_whitespace().collect();
        let finished = finish(setup.command(&arguments));
        assert_eq!(
            finished.code,
            Some(expected),
            "butterbot {line}\n{}",
            finished.describe()
        );
    }
}

// The settings that exist for the tests only are not part of the program.
#[cfg(not(feature = "test-support"))]
#[test]
fn settings_for_the_tests_are_unknown_to_the_program() {
    let setup = Setup::new(&[]);
    setup.write(
        "butterbot.toml",
        r#"
[acme.test]
directory_url = "https://acme.example.com/directory"
ca_file = "/tmp/root.pem"

[dns.test]
type = "challtestsrv"
management_url = "http://127.0.0.1:8055"
zones = ["example.com"]
"#,
    );

    let finished = finish(setup.command_without_secrets(&["check-config"]));

    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished.stdout.contains("unknown key `ca_file`"),
        "{}",
        finished.stdout
    );
    assert!(
        finished.stdout.contains("unknown type \"challtestsrv\""),
        "{}",
        finished.stdout
    );
}
