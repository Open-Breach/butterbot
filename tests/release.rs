//! Tests of the scripts of a release (`.github/scripts/`) that need neither
//! Docker nor a registry. A stand-in for `docker` records how it was called.
//! The publishing steps as a whole run against a registry in the rehearsal
//! of the CI workflow (`.github/scripts/rehearse-publish.sh`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The settings of a registry, as a release reads them from its secrets.
const SETTINGS: [(&str, &str); 3] = [
    ("REGISTRY_HOST", "registry.example.com"),
    ("REGISTRY_USERNAME", "the-user-of-the-pipeline"),
    ("REGISTRY_PASSWORD", "the-password-of-the-pipeline"),
];

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".github/scripts")
        .join(name)
}

/// A directory with a `docker` that does nothing but record its arguments,
/// one call per line, in the file `calls`.
struct Recorder {
    directory: TempDir,
}

impl Recorder {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let directory = TempDir::new().expect("a temporary directory");
        let docker = directory.path().join("docker");
        fs::write(
            &docker,
            "#!/bin/sh\necho \"$@\" >> \"$(dirname \"$0\")/calls\"\n",
        )
        .expect("written");
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).expect("executable");
        Self { directory }
    }

    /// The calls of `docker` so far.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.directory.path().join("calls"))
            .map(|calls| calls.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Runs the publishing steps with the given settings and arguments.
    fn publish(&self, settings: &[(&str, &str)], arguments: &[&str]) -> Finished {
        let path = format!(
            "{}:{}",
            self.directory.path().display(),
            std::env::var("PATH").expect("a PATH")
        );
        let output = Command::new(script("publish-image.sh"))
            .args(arguments)
            .env_clear()
            .env("PATH", path)
            .envs(settings.iter().copied())
            .output()
            .expect("the script can be run");
        Finished::of(output)
    }
}

struct Finished {
    code: Option<i32>,
    /// Standard output and standard error.
    text: String,
}

impl Finished {
    fn of(output: Output) -> Self {
        Self {
            code: output.status.code(),
            text: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    }

    /// No value of a setting is in what the script wrote.
    fn assert_prints_no_value(&self) {
        for (name, value) in SETTINGS {
            assert!(
                !self.text.contains(value),
                "the value of {name} is not printed:\n{}",
                self.text
            );
        }
    }
}

const ARGUMENTS: [&str; 3] = ["0.1.0", "amd64.tar", "arm64.tar"];

// image-release, "A missing registry setting stops the release", scenario
// "Environment not set up"; task 2.1. Each setting in turn is missing, and
// then empty.
#[test]
fn missing_registry_setting_is_named_before_a_registry_is_contacted() {
    for (missing, _) in SETTINGS {
        for empty in [false, true] {
            let settings: Vec<(&str, &str)> = SETTINGS
                .iter()
                .filter_map(|&(name, value)| match (name == missing, empty) {
                    (false, _) => Some((name, value)),
                    (true, true) => Some((name, "")),
                    (true, false) => None,
                })
                .collect();
            let recorder = Recorder::new();
            let finished = recorder.publish(&settings, &ARGUMENTS);

            assert_eq!(finished.code, Some(2), "{missing}:\n{}", finished.text);
            assert_eq!(
                finished.text,
                format!(
                    "publish-image: {missing} is missing or empty. It is one of the three \
                     secrets of the environment \"release\".\n"
                ),
            );
            finished.assert_prints_no_value();
            assert_eq!(
                recorder.calls(),
                Vec::<String>::new(),
                "{missing}: no registry was contacted"
            );
        }
    }
}

// The same when the environment has no secret at all: every one is named.
#[test]
fn every_missing_registry_setting_is_named() {
    let recorder = Recorder::new();
    let finished = recorder.publish(&[], &ARGUMENTS);
    assert_eq!(finished.code, Some(2), "{}", finished.text);
    for (name, _) in SETTINGS {
        assert!(
            finished
                .text
                .contains(&format!("publish-image: {name} is missing or empty.")),
            "{}",
            finished.text
        );
    }
    assert_eq!(recorder.calls(), Vec::<String>::new());
}

// image-release, "A platform's image may have a tag of its own", scenario
// "Version that ends like the tag of a platform's image", and what is not a
// version at all. Nothing logs in for such a version.
#[test]
fn version_that_cannot_be_released_is_refused_before_the_login() {
    for version in [
        "1",
        "0.1",
        "v0.1.0",
        "0.1.0.0",
        "01.1.0",
        "0.1.0-",
        "0.1.0-rc..1",
        "0.1.0-rc.01",
        // Build metadata: an image tag cannot contain `+`.
        "0.1.0+build.5",
        "0.1.0 ",
        "latest",
    ] {
        let recorder = Recorder::new();
        let finished = recorder.publish(&SETTINGS, &[version, "amd64.tar", "arm64.tar"]);
        assert_eq!(finished.code, Some(2), "{version}:\n{}", finished.text);
        assert_eq!(
            finished.text,
            format!(
                "publish-image: \"{version}\" is not a version of the form X.Y.Z or X.Y.Z-<suffix>\n"
            ),
        );
        assert_eq!(recorder.calls(), Vec::<String>::new(), "{version}");
    }
    for version in ["0.1.0-linux-amd64", "0.2.0-rc.1-linux-arm64"] {
        let recorder = Recorder::new();
        let finished = recorder.publish(&SETTINGS, &[version, "amd64.tar", "arm64.tar"]);
        assert_eq!(finished.code, Some(2), "{version}:\n{}", finished.text);
        assert_eq!(
            finished.text,
            format!(
                "publish-image: a version must not end in -linux-amd64 or -linux-arm64: \"{version}\"\n"
            ),
        );
        assert_eq!(recorder.calls(), Vec::<String>::new(), "{version}");
    }
}

// image-release, "Tags of a release", scenario "One platform is missing";
// task 2.6. With all settings and a version, the script logs in, and it
// stops at an archive that is not there before it loads, tags or pushes
// anything. (The rehearsal checks the same against a registry, and a push
// that fails.)
#[test]
fn missing_archive_stops_after_the_login_and_before_any_push() {
    let archives = TempDir::new().expect("a temporary directory");
    let amd64 = archives.path().join("amd64.tar");
    fs::write(&amd64, "").expect("written");
    let amd64 = amd64.to_str().expect("a path");
    let arm64 = archives.path().join("arm64.tar");
    let arm64 = arm64.to_str().expect("a path");

    for version in ["0.1.0", "0.2.0-rc.1", "10.20.30-alpha-1.x"] {
        let recorder = Recorder::new();
        let finished = recorder.publish(&SETTINGS, &[version, amd64, arm64]);
        assert_eq!(finished.code, Some(1), "{version}:\n{}", finished.text);
        assert_eq!(
            finished.text,
            format!(
                "Logging in to the registry.\n\
                 publish-image: the image for linux/arm64 is missing: there is no archive at \
                 {arm64}. Nothing was pushed.\n"
            ),
        );
        finished.assert_prints_no_value();
        // The host and the user are handed to Docker as they are, the
        // password on standard input. Nothing else was called.
        assert_eq!(
            recorder.calls(),
            [
                "login registry.example.com --username the-user-of-the-pipeline --password-stdin",
                "logout registry.example.com",
            ],
            "{version}"
        );
    }
}
