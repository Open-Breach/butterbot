//! Tests of the container image. They start containers with `docker`.
//!
//! They are compiled only with the `container-tests` feature, and they need:
//!
//! - the image as it is released, named by `BUTTERBOT_IMAGE`:
//!   `docker build -t butterbot:dev .`
//! - an image of a build with the test settings, named by
//!   `BUTTERBOT_TEST_IMAGE`, for the tests that issue a certificate:
//!   `docker build --target test -t butterbot:test .`
//! - the ACME test server and its challenge DNS server; see `src/testing.rs`.
//!
//! ```text
//! BUTTERBOT_IMAGE=butterbot:dev BUTTERBOT_TEST_IMAGE=butterbot:test \
//!     cargo test --features container-tests --test container
//! ```
//!
//! The containers use the network of the host, where the test servers listen.
#![cfg(feature = "container-tests")]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use butterbot::testing::{AcmeTestServer, ChallTestSrv};
use serde_json::Value;
use tempfile::TempDir;
use time::OffsetDateTime;

/// The user the image runs as.
const USER: u32 = 65532;
const CERTIFICATE: &str = "wildcard-example-com";
const RECORD: &str = "_acme-challenge.example.com";

fn image_named_by(variable: &str, build: &str) -> String {
    std::env::var(variable).unwrap_or_else(|_| {
        panic!("{variable} names no image. Build one with `{build}` and set {variable} to its name")
    })
}

/// The image as it is released.
fn image() -> String {
    image_named_by("BUTTERBOT_IMAGE", "docker build -t butterbot:dev .")
}

/// An image of a build with the test settings.
fn test_image() -> String {
    image_named_by(
        "BUTTERBOT_TEST_IMAGE",
        "docker build --target test -t butterbot:test .",
    )
}

fn docker(arguments: &[&str]) -> Finished {
    Finished::of(
        Command::new("docker")
            .args(arguments)
            .output()
            .expect("docker can be run"),
    )
}

struct Finished {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Finished {
    fn of(output: Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn describe(&self) -> String {
        format!(
            "exit code {:?}\n--- standard output\n{}--- standard error\n{}",
            self.code, self.stdout, self.stderr
        )
    }

    /// The `run_completed` event in the log on standard output.
    fn completed(&self) -> Value {
        records(&self.stdout)
            .into_iter()
            .find(|record| record["event"] == "run_completed")
            .unwrap_or_else(|| panic!("a run_completed event:\n{}", self.describe()))
    }
}

/// The JSON log records in `log`.
fn records(log: &str) -> Vec<Value> {
    log.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The files of one container: its configuration and a directory store, both
/// mounted into it.
struct Files {
    directory: TempDir,
}

impl Files {
    fn new(configuration: &str) -> Self {
        let files = Self {
            directory: TempDir::new().expect("a temporary directory"),
        };
        fs::write(files.config(), configuration).expect("the configuration");
        fs::create_dir(files.store()).expect("the store");
        // The container runs as another user than the tests.
        fs::set_permissions(files.store(), fs::Permissions::from_mode(0o777)).expect("writable");
        files
    }

    fn config(&self) -> PathBuf {
        self.directory.path().join("config.toml")
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
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).expect("readable");
        fs::write(folder.join("fullchain.pem"), certificate.pem()).expect("fullchain.pem");
        fs::write(folder.join("privkey.pem"), key.serialize_pem()).expect("privkey.pem");
    }

    /// The arguments of `docker run` that every container of these tests
    /// has: the host's network, the configuration at the default path, and
    /// the store at `/certificates`.
    fn mounts(&self) -> Vec<String> {
        vec![
            "--network".to_owned(),
            "host".to_owned(),
            "--volume".to_owned(),
            format!("{}:/etc/butterbot/config.toml:ro", self.config().display()),
            "--volume".to_owned(),
            format!("{}:/certificates", self.store().display()),
        ]
    }

    /// `docker run --rm <mounts> <options> <image> <arguments>`.
    fn run(&self, options: &[&str], image: &str, arguments: &[&str]) -> Finished {
        let mounts = self.mounts();
        let mut line: Vec<&str> = vec!["run", "--rm"];
        line.extend(mounts.iter().map(String::as_str));
        line.extend(options);
        line.push(image);
        line.extend(arguments);
        docker(&line)
    }
}

/// A configuration with one certificate entry per name, each for
/// `<name>.example.com`, in a directory store at `/certificates`. No server
/// it names is reached as long as no certificate is due.
fn offline_configuration(certificates: &[&str]) -> String {
    let mut text = r#"
[acme.test]
directory_url = "https://acme.example.com/directory"
ari = false

[dns.cloudflare]
type = "cloudflare"

[store.local]
type = "directory"
path = "/certificates"
"#
    .to_owned();
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
    text
}

const SECRETS: [&str; 4] = [
    "--env",
    "BUTTERBOT__ACME__TEST__ACCESS_KEY=account-credentials",
    "--env",
    "BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY=dns-token",
];

/// A configuration against the test servers, for an image with the test
/// settings. The DNS provider publishes in `acme`'s challenge DNS server and
/// checks propagation at `checked_at`.
fn configuration_against(
    acme: &AcmeTestServer,
    checked_at: &ChallTestSrv,
    propagation_timeout: &str,
) -> String {
    format!(
        r#"
[acme.pebble]
directory_url = "{directory_url}"
ca_file = "/etc/butterbot/acme-root.pem"

[dns.test]
type = "challtestsrv"
management_url = "{management}"
zones = ["example.com"]
propagation_check = "resolver"
propagation_resolvers = ["{resolver}"]
propagation_interval = "1s"
propagation_timeout = "{propagation_timeout}"
settle = "0s"

[store.local]
type = "directory"
path = "/certificates"

[[certificate]]
name = "{CERTIFICATE}"
names = ["*.example.com", "example.com"]
acme = "pebble"
dns = "test"
stores = ["local"]
"#,
        directory_url = acme.pebble.directory_url,
        management = acme.dns.management_url(),
        resolver = checked_at.dns,
    )
}

/// The mount of the ACME test server's root certificate.
fn root_of(acme: &AcmeTestServer) -> String {
    format!(
        "{}:/etc/butterbot/acme-root.pem:ro",
        acme.pebble.root_file().display()
    )
}

/// Registers an account with the image, and returns the `--env` value that
/// supplies its credentials.
fn account(files: &Files, acme: &AcmeTestServer) -> String {
    let root = root_of(acme);
    let finished = files.run(
        &["--volume", &root],
        &test_image(),
        &["account", "new", "--acme", "pebble"],
    );
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    let credentials = finished.stdout.lines().last().expect("output");
    format!("BUTTERBOT__ACME__PEBBLE__ACCESS_KEY={credentials}")
}

// ---------------------------------------------------------------------------
// The image as it is released
// ---------------------------------------------------------------------------

// Task 10.1: the image runs on the platform it was built for.
// container-image, "Supported platforms".
#[test]
fn image_is_for_this_platform_and_prints_its_version() {
    let image = image();
    let finished = docker(&["run", "--rm", &image, "--version"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(
        finished.stdout.trim_end(),
        format!("butterbot {}", env!("CARGO_PKG_VERSION"))
    );

    let expected = match std::env::consts::ARCH {
        "x86_64" => "linux/amd64",
        "aarch64" => "linux/arm64",
        other => panic!("these tests do not know the platform {other}"),
    };
    let inspected = docker(&[
        "image",
        "inspect",
        "--format",
        "{{.Os}}/{{.Architecture}}",
        &image,
    ]);
    assert_eq!(inspected.stdout.trim_end(), expected);
}

// container-image, "Unprivileged and self-contained", scenario "Non-root".
// That the program really runs as this user is shown by the owner of the
// files it writes, in `certificate_is_issued_with_a_read_only_root_filesystem`.
#[test]
fn image_runs_as_an_unprivileged_user_and_holds_only_the_program() {
    let image = image();
    let inspected = docker(&[
        "image",
        "inspect",
        "--format",
        "{{.Config.User}}|{{json .Config.Entrypoint}}|{{json .Config.Cmd}}",
        &image,
    ]);
    assert_eq!(
        inspected.stdout.trim_end(),
        format!(r#"{USER}:{USER}|["/usr/local/bin/butterbot"]|["run"]"#)
    );

    // The image is the program and nothing else: no shell, no libraries.
    let created = docker(&["create", &image]);
    assert_eq!(created.code, Some(0), "{}", created.describe());
    let container = created.stdout.trim_end().to_owned();
    let exported = Command::new("sh")
        .arg("-c")
        .arg(format!("docker export {container} | tar -t"))
        .output()
        .expect("docker export");
    docker(&["rm", &container]);
    let files: Vec<String> = String::from_utf8_lossy(&exported.stdout)
        .lines()
        .filter(|path| !path.ends_with('/'))
        // What the container runtime puts into every container.
        .filter(|path| {
            !path.starts_with("etc/")
                && !path.starts_with("dev/")
                && !path.starts_with("proc/")
                && !path.starts_with("sys/")
                && *path != ".dockerenv"
        })
        .map(str::to_owned)
        .collect();
    assert_eq!(files, ["usr/local/bin/butterbot"]);
}

// container-image, "Default command is a run", scenario "Started without
// arguments".
#[test]
fn container_without_arguments_performs_a_run_and_exits_with_its_exit_code() {
    let files = Files::new(&offline_configuration(&["one", "two"]));
    files.holds("one", 60);
    files.holds("two", 60);

    let finished = files.run(&SECRETS, &image(), &[]);

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    let completed = finished.completed();
    assert_eq!(completed["checked"], 2);
    assert_eq!(completed["outcome"], "success");
    assert!(
        records(&finished.stdout).len() == finished.stdout.lines().count(),
        "the log is JSON, one object per line:\n{}",
        finished.stdout
    );

    // The DNS provider has no credential: the certificates fail, and the
    // container exits as the run does.
    let finished = files.run(&SECRETS[..2], &image(), &[]);
    assert_eq!(finished.code, Some(1), "{}", finished.describe());
    assert_eq!(finished.completed()["failed"], 2);

    // No configuration at the default path.
    let finished = docker(&["run", "--rm", &image()]);
    assert_eq!(finished.code, Some(2), "{}", finished.describe());
    assert!(
        finished
            .stdout
            .contains("/etc/butterbot/config.toml does not exist"),
        "{}",
        finished.describe()
    );
}

// container-image, "Default command is a run", scenario "Another command".
#[test]
fn arguments_replace_the_default_command() {
    let files = Files::new(&offline_configuration(&["one", "new"]));
    files.holds("one", 60);

    let finished = files.run(&SECRETS, &image(), &["status"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert!(
        finished.stdout.contains("one: not due, becomes due "),
        "{}",
        finished.stdout
    );
    assert!(
        finished.stdout.contains("new: due\n"),
        "{}",
        finished.stdout
    );

    let finished = files.run(&SECRETS, &image(), &["check-config"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert!(
        finished.stdout.contains("Every required secret was found."),
        "{}",
        finished.stdout
    );

    let finished = files.run(&SECRETS, &image(), &["run", "--only", "one"]);
    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(finished.completed()["checked"], 1);
}

// container-image, "Unprivileged and self-contained", scenario "Read-only
// root filesystem", with the image as it is released: the store is read and
// nothing is due.
#[test]
fn run_completes_with_a_read_only_root_filesystem() {
    let files = Files::new(&offline_configuration(&["one"]));
    files.holds("one", 60);

    let finished = files.run(
        &[
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            SECRETS[0],
            SECRETS[1],
            SECRETS[2],
            SECRETS[3],
        ],
        &image(),
        &[],
    );

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(finished.completed()["checked"], 1);
}

// ---------------------------------------------------------------------------
// With the ACME test server
// ---------------------------------------------------------------------------

// container-image, "Unprivileged and self-contained", scenarios "Read-only
// root filesystem" and "Non-root": a certificate is issued by a container
// whose only writable place is the mounted store, and the files in the store
// belong to the unprivileged user.
#[test]
fn certificate_is_issued_with_a_read_only_root_filesystem() {
    let acme = AcmeTestServer::start();
    let files = Files::new(&configuration_against(&acme, &acme.dns, "30s"));
    let root = root_of(&acme);
    let credentials = account(&files, &acme);

    let finished = files.run(
        &[
            "--read-only",
            "--cap-drop",
            "ALL",
            "--volume",
            &root,
            "--env",
            &credentials,
        ],
        &test_image(),
        &[],
    );

    assert_eq!(finished.code, Some(0), "{}", finished.describe());
    assert_eq!(finished.completed()["renewed"], 1);
    for file in ["fullchain.pem", "privkey.pem"] {
        let path = files.store().join(CERTIFICATE).join(file);
        let metadata = fs::metadata(&path)
            .unwrap_or_else(|error| panic!("{} is in the store: {error}", path.display()));
        assert_eq!(
            metadata.uid(),
            USER,
            "{file} was written by the user {USER}"
        );
    }
}

/// Polls until `condition` holds, for at most a minute.
async fn eventually(what: &str, mut condition: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !condition().await {
        assert!(Instant::now() < deadline, "not within a minute: {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Removes the container when the test ends, however it ends.
struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        docker(&["rm", "--force", &self.0]);
    }
}

// Task 10.2; container-image, "Shutdown signal reaches the program", scenario
// "Container stopped during a run".
#[tokio::test]
async fn stopping_the_container_during_a_run_exits_with_3_and_removes_the_challenge_records() {
    let acme = AcmeTestServer::start();
    // Propagation is checked at a server that never has the records, so the
    // run stays in that wait with its challenge records published.
    let elsewhere = ChallTestSrv::start();
    let files = Files::new(&configuration_against(&acme, &elsewhere, "10m"));
    let root = root_of(&acme);
    let credentials = account(&files, &acme);
    let name = format!("butterbot-test-{}", std::process::id());

    let mounts = files.mounts();
    let image = test_image();
    let mut line: Vec<&str> = vec!["run", "--detach", "--name", &name];
    line.extend(mounts.iter().map(String::as_str));
    line.extend(["--volume", &root, "--env", &credentials, &image]);
    let started = docker(&line);
    assert_eq!(started.code, Some(0), "{}", started.describe());
    let container = Container(name);

    eventually("the challenge record is published", async || {
        acme.dns.served_txt_values(RECORD).await > 0
    })
    .await;

    let stopped = docker(&["stop", "--time", "60", &container.0]);
    assert_eq!(stopped.code, Some(0), "{}", stopped.describe());

    let state = docker(&["inspect", "--format", "{{.State.ExitCode}}", &container.0]);
    let log = docker(&["logs", &container.0]);
    assert_eq!(state.stdout.trim_end(), "3", "{}", log.describe());
    assert_eq!(
        acme.dns.served_txt_values(RECORD).await,
        0,
        "the challenge records are removed"
    );
    let records = records(&log.stdout);
    let has = |text: &str| {
        records.iter().any(|record| {
            record["message"]
                .as_str()
                .is_some_and(|message| message.contains(text))
        })
    };
    assert!(has("termination signal received"), "{}", log.stdout);
    assert!(has("run interrupted"), "{}", log.stdout);
    assert!(
        !records
            .iter()
            .any(|record| record["event"] == "run_completed"),
        "an interrupted run has no completion event:\n{}",
        log.stdout
    );
    assert!(
        !files.store().join(CERTIFICATE).exists(),
        "nothing was stored"
    );
}
