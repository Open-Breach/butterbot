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

/// A clone of a repository that holds a program `butterbot`, as the job
/// that checks a tag has it: `origin/main` is the branch of the repository
/// it was cloned from.
struct Clone {
    directory: TempDir,
}

impl Clone {
    /// A repository whose `main` has one commit, with the program at
    /// `version`, and a clone of it.
    fn of_a_program_at(version: &str) -> Self {
        let clone = Self {
            directory: TempDir::new().expect("a temporary directory"),
        };
        let origin = clone.directory.path().join("origin");
        fs::create_dir_all(origin.join("src")).expect("a directory");
        fs::write(origin.join("src/lib.rs"), "").expect("written");
        git(&origin, &["init", "--quiet", "--initial-branch", "main"]);
        write_version(&origin, version);
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "--quiet", "--message", "The program"]);
        git(
            clone.directory.path(),
            &["clone", "--quiet", "origin", "clone"],
        );
        clone
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("clone")
    }

    fn git(&self, arguments: &[&str]) {
        git(&self.path(), arguments);
    }

    /// A commit that sets the version of the program.
    fn commit_version(&self, version: &str) {
        write_version(&self.path(), version);
        self.git(&["commit", "--quiet", "--all", "--message", version]);
    }

    /// Puts `tag` on the commit that is checked out, and checks the tag out
    /// as the release workflow does.
    fn tag(&self, tag: &str) {
        self.git(&["tag", tag]);
        self.git(&["checkout", "--quiet", "--detach", tag]);
    }

    /// Runs the check of a tag. The second value is what the script wrote to
    /// the step's outputs.
    fn check(&self, tag: &str) -> (Finished, String) {
        let outputs = self.directory.path().join("outputs");
        fs::write(&outputs, "").expect("written");
        let output = Command::new(script("check-release-tag.sh"))
            .arg(tag)
            .current_dir(self.path())
            .env("GITHUB_OUTPUT", &outputs)
            .output()
            .expect("the script can be run");
        (
            Finished::of(output),
            fs::read_to_string(outputs).expect("the outputs"),
        )
    }
}

fn write_version(repository: &Path, version: &str) {
    fs::write(
        repository.join("Cargo.toml"),
        format!("[package]\nname = \"butterbot\"\nversion = \"{version}\"\nedition = \"2024\"\n"),
    )
    .expect("written");
}

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=A test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "advice.detachedHead=false",
        ])
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("git can be run");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// image-release, "A version tag starts a release", scenario "Version tag";
// task 3.1. A tag that matches the program's version, on a commit of main,
// passes, and the version is handed on.
#[test]
fn tag_that_matches_the_program_on_main_passes() {
    for (version, tag) in [("0.1.0", "v0.1.0"), ("0.2.0-rc.1", "v0.2.0-rc.1")] {
        let clone = Clone::of_a_program_at(version);
        clone.tag(tag);
        let (finished, outputs) = clone.check(tag);
        assert_eq!(finished.code, Some(0), "{}", finished.text);
        assert!(
            finished.text.starts_with(&format!(
                "The tag {tag} is version {version} of the program, and its commit "
            )) && finished.text.ends_with(" is contained in main.\n"),
            "{}",
            finished.text
        );
        assert_eq!(outputs, format!("version={version}\n"));
    }
}

// An annotated tag is a tag as well, and so is one on an earlier commit of
// main.
#[test]
fn annotated_tag_on_an_earlier_commit_of_main_passes() {
    let clone = Clone::of_a_program_at("0.1.0");
    clone.git(&[
        "tag",
        "--annotate",
        "--message",
        "The first release",
        "v0.1.0",
    ]);
    // Main moves on.
    let origin = clone.directory.path().join("origin");
    write_version(&origin, "0.2.0");
    git(
        &origin,
        &["commit", "--quiet", "--all", "--message", "0.2.0"],
    );
    clone.git(&["fetch", "--quiet", "origin"]);
    clone.git(&["checkout", "--quiet", "--detach", "v0.1.0"]);
    let (finished, outputs) = clone.check("v0.1.0");
    assert_eq!(finished.code, Some(0), "{}", finished.text);
    assert_eq!(outputs, "version=0.1.0\n");
}

// image-release, "The tag matches the program and lies on main", scenario
// "Version differs"; task 3.1. The message names both versions.
#[test]
fn tag_with_another_version_than_the_program_fails_naming_both() {
    let clone = Clone::of_a_program_at("0.1.0");
    clone.tag("v0.2.0");
    let (finished, outputs) = clone.check("v0.2.0");
    assert_eq!(finished.code, Some(1), "{}", finished.text);
    assert_eq!(
        finished.text,
        "check-release-tag: the versions differ: the tag \"v0.2.0\" says 0.2.0, but the program \
         at the tagged commit is version 0.1.0. Set the version in Cargo.toml, or tag the commit \
         that has version 0.2.0.\n"
    );
    assert_eq!(outputs, "", "no version is handed on");
}

// image-release, "The tag matches the program and lies on main", scenario
// "Commit outside main"; task 3.1. The message says that it is this, and
// not the version.
#[test]
fn tag_on_a_commit_outside_main_fails_saying_so() {
    let clone = Clone::of_a_program_at("0.1.0");
    clone.git(&["checkout", "--quiet", "-b", "not-merged"]);
    clone.git(&[
        "commit",
        "--quiet",
        "--allow-empty",
        "--message",
        "Not merged",
    ]);
    clone.tag("v0.1.0");
    let (finished, outputs) = clone.check("v0.1.0");
    assert_eq!(finished.code, Some(1), "{}", finished.text);
    assert!(
        finished.text.starts_with(
            "check-release-tag: the commit is outside main: the tag \"v0.1.0\" is on commit "
        ) && finished.text.ends_with(
            ", which is not contained in the branch main. Merge into main first, then tag a \
             commit of main.\n"
        ),
        "{}",
        finished.text
    );
    assert!(!finished.text.contains("the versions differ"));
    assert_eq!(outputs, "");

    // A commit that main does not have yet is outside main as well, even on
    // a local branch of that name.
    let clone = Clone::of_a_program_at("0.1.0");
    clone.git(&[
        "commit",
        "--quiet",
        "--allow-empty",
        "--message",
        "Not pushed",
    ]);
    clone.tag("v0.1.0");
    let (finished, _) = clone.check("v0.1.0");
    assert_eq!(finished.code, Some(1), "{}", finished.text);
    assert!(finished.text.contains("the commit is outside main"));
}

// Both at once: each has its message.
#[test]
fn tag_with_another_version_outside_main_fails_with_both_messages() {
    let clone = Clone::of_a_program_at("0.1.0");
    clone.git(&["checkout", "--quiet", "-b", "not-merged"]);
    clone.commit_version("0.3.0");
    clone.tag("v0.2.0");
    let (finished, outputs) = clone.check("v0.2.0");
    assert_eq!(finished.code, Some(1), "{}", finished.text);
    assert!(
        finished.text.contains(
            "the tag \"v0.2.0\" says 0.2.0, but the program at the tagged commit is version 0.3.0"
        ),
        "{}",
        finished.text
    );
    assert!(
        finished.text.contains("the commit is outside main"),
        "{}",
        finished.text
    );
    assert_eq!(outputs, "");
}

// image-release, "A version tag starts a release", scenario "Tag that only
// looks like a version"; task 3.1. The workflow's pattern admits every tag
// that begins with `v`; the check refuses what is not a version.
#[test]
fn tag_that_is_not_a_version_tag_fails() {
    let clone = Clone::of_a_program_at("0.1.0");
    for tag in [
        "v1",
        "vfoo",
        "v0.1",
        "v",
        "0.1.0",
        "version-0.1.0",
        "v0.1.0.0",
        "v01.1.0",
        "experiment-1",
    ] {
        let (finished, outputs) = clone.check(tag);
        assert_eq!(finished.code, Some(1), "{tag}: {}", finished.text);
        assert_eq!(
            finished.text,
            format!(
                "check-release-tag: the tag \"{tag}\" is not a version tag. A release is started \
                 by a tag of the form v<semantic version>, such as v0.1.0 or v0.2.0-rc.1.\n"
            )
        );
        assert_eq!(outputs, "");
    }

    // Build metadata is part of a semantic version, but not of an image tag.
    let (finished, _) = clone.check("v0.1.0+build.5");
    assert_eq!(finished.code, Some(1), "{}", finished.text);
    assert!(
        finished.text.ends_with(
            "Build metadata (+...) is not accepted, because an image tag cannot contain +.\n"
        ),
        "{}",
        finished.text
    );

    // image-release, "A platform's image may have a tag of its own".
    let (finished, _) = clone.check("v0.1.0-linux-arm64");
    assert_eq!(finished.code, Some(1), "{}", finished.text);
    assert_eq!(
        finished.text,
        "check-release-tag: the tag \"v0.1.0-linux-arm64\" ends like the tag of one platform's \
         image. A version must not end in -linux-amd64 or -linux-arm64.\n"
    );
}
