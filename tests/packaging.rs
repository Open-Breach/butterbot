//! Checks of the Dockerfile, of `docs/deployment.md` and of the workflows
//! that need neither Docker nor a cluster. The image itself is tested in
//! `tests/container.rs`, the manifest is given to `kubectl` in the CI
//! workflow, and the publishing steps are rehearsed there against a registry.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

fn repository_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The content of the fenced blocks of a document that are marked `language`.
fn blocks(document: &str, language: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in document.lines() {
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

/// The one manifest of the deployment document.
fn manifest() -> String {
    let mut manifests = blocks(&repository_file("docs/deployment.md"), "yaml");
    assert_eq!(manifests.len(), 1, "the document has one manifest");
    manifests.remove(0)
}

// Task 10.1: the version of Rust in the Dockerfile is the pinned one.
#[test]
fn dockerfile_builds_with_the_pinned_rust_version() {
    let toolchain: toml::Table = repository_file("rust-toolchain.toml")
        .parse()
        .expect("TOML");
    let pinned = toolchain["toolchain"]["channel"]
        .as_str()
        .expect("a channel");
    assert!(
        repository_file("Dockerfile").contains(&format!("ARG RUST_VERSION={pinned}\n")),
        "the Dockerfile builds with Rust {pinned}"
    );
}

// container-image: the image is the last stage of the Dockerfile, it runs as
// a user that is not root, and `run` is the default command that arguments
// replace.
#[test]
fn image_is_the_last_stage_with_an_unprivileged_user_and_run_as_default() {
    let dockerfile = repository_file("Dockerfile");
    let last_stage = dockerfile
        .rsplit("\nFROM ")
        .next()
        .expect("the Dockerfile has stages");
    let instructions: Vec<&str> = last_stage
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(
        instructions,
        [
            "scratch",
            "COPY --from=build /butterbot /usr/local/bin/butterbot",
            "USER 65532:65532",
            r#"ENTRYPOINT ["/usr/local/bin/butterbot"]"#,
            r#"CMD ["run"]"#,
        ]
    );
    // The stage it copies from is the build without the test settings.
    let build = dockerfile
        .split("AS build\n")
        .nth(1)
        .and_then(|rest| rest.split("\nFROM ").next())
        .expect("the build stage");
    assert!(build.contains("cargo build --release --locked --bin butterbot \\\n"));
    assert!(!build.contains("--features"), "{build}");
}

// Task 10.3: the reference CronJob has the settings the task names.
#[test]
fn reference_cronjob_has_the_required_settings() {
    let manifest = manifest();
    for setting in [
        "kind: CronJob",
        // A daily schedule: one minute and one hour, every day.
        "  schedule: \"17 3 * * *\"\n",
        "  concurrencyPolicy: Forbid\n",
        // A run deadline.
        "      activeDeadlineSeconds: 3600\n",
        // No automatic retries.
        "      backoffLimit: 0\n",
        "          restartPolicy: Never\n",
        "          terminationGracePeriodSeconds: 60\n",
        // The configuration comes from a ConfigMap, at the default path.
        "kind: ConfigMap",
        "  config.toml: |\n",
        "                  mountPath: /etc/butterbot\n",
        "              configMap:\n                name: butterbot-config\n",
        // Secrets from the environment.
        "              envFrom:\n                - secretRef:\n",
        // Or from the vault, with the workload identity of the pod.
        "    key_vault_url = ",
        "            azure.workload.identity/use: \"true\"\n",
        "    azure.workload.identity/client-id: ",
        // Unprivileged, as the image is.
        "            runAsNonRoot: true\n",
        "            runAsUser: 65532\n",
        "                readOnlyRootFilesystem: true\n",
    ] {
        assert!(
            manifest.contains(setting),
            "the manifest has {setting:?}:\n{manifest}"
        );
    }
    assert!(
        !manifest.contains("args:") && !manifest.contains("command:"),
        "the container has no arguments: the image runs `butterbot run`"
    );
}

// Task 10.3: the configuration in the reference ConfigMap is one the program
// accepts.
#[test]
fn configuration_in_the_reference_configmap_is_valid() {
    let manifest = manifest();
    let configuration: String = manifest
        .split("  config.toml: |\n")
        .nth(1)
        .expect("the configuration file in the ConfigMap")
        .lines()
        .take_while(|line| *line != "---")
        .map(|line| format!("{}\n", line.strip_prefix("    ").unwrap_or(line)))
        .collect();
    // The vault is not there to be read in a test.
    let without_vault =
        configuration.replace("[secrets]\nkey_vault_url", "# [secrets]\n# key_vault_url");
    assert_ne!(without_vault, configuration);
    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let file = directory.path().join("config.toml");
    fs::write(&file, without_vault).expect("written");

    let output = Command::new(env!("CARGO_BIN_EXE_butterbot"))
        .arg("--config")
        .arg(&file)
        .arg("check-config")
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .expect("the program can be started");

    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(
            "The configuration is valid: 1 certificate, 1 ACME server, 1 DNS provider, 1 store."
        ),
        "{text}"
    );
    // The account is the only secret, and the vault would hold it.
    assert_eq!(text.matches(": MISSING").count(), 1, "{text}");
    assert!(
        text.contains("  acme.letsencrypt.access_key: MISSING\n"),
        "{text}"
    );
}

// Task 10.3: the cutover steps of the migration plan are in the document.
#[test]
fn deployment_document_has_the_cutover_steps() {
    let document = repository_file("docs/deployment.md");
    let steps = document
        .split("## Replacing an existing job")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the section about replacing an existing job");
    for (number, step) in [
        "Grant the permissions.",
        "Write the configuration.",
        "Check, without changing anything.",
        "Issue a test certificate.",
        "Switch over.",
        "Watch the first renewal.",
    ]
    .iter()
    .enumerate()
    {
        assert!(
            steps.contains(&format!("{}. **{step}**", number + 1)),
            "step {}: {step}",
            number + 1
        );
    }
    for command in [
        "butterbot check-config",
        "butterbot status",
        "butterbot run --only <the test entry>",
        "key_type = \"rsa2048\"",
        "format = \"pfx\"",
        "**Going back.**",
    ] {
        assert!(steps.contains(command), "{command}");
    }
}

// The documents under `docs/` are linked from the README.
#[test]
fn readme_links_every_document() {
    let readme = repository_file("README.md");
    for entry in fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs")).expect("docs") {
        let name = entry.expect("an entry").file_name();
        let link = format!("(docs/{})", name.to_string_lossy());
        assert!(readme.contains(&link), "the README links {link}");
    }
}

/// A workflow of the repository, parsed.
fn workflow(name: &str) -> Value {
    let text = repository_file(&format!(".github/workflows/{name}"));
    serde_saphyr::from_str(&text).unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// Every workflow of the repository, with its file name.
fn workflows() -> Vec<(String, Value)> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect(".github/workflows")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let parsed = workflow(&name);
            (name, parsed)
        })
        .collect()
}

/// The jobs of a workflow, with their names.
fn jobs(workflow: &Value) -> Vec<(&String, &Value)> {
    workflow["jobs"]
        .as_object()
        .expect("a workflow has jobs")
        .iter()
        .collect()
}

/// The steps of a job.
fn steps(job: &Value) -> &Vec<Value> {
    job["steps"].as_array().expect("a job has steps")
}

// image-release, "A version tag starts a release", scenarios "Change to a
// branch" and "Another tag"; task 1.1. The checks start for a push to a
// branch and for a pull request, and the release workflow can call them. A
// tag starts nothing here.
#[test]
fn checks_start_for_branches_and_pull_requests_and_can_be_called() {
    let ci = workflow("ci.yml");
    let triggers = ci["on"].as_object().expect("the triggers of ci.yml");
    let mut names: Vec<&str> = triggers.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["pull_request", "push", "workflow_call"]);
    // Every branch, and with a filter on branches a pushed tag starts nothing.
    assert_eq!(triggers["push"], serde_json::json!({ "branches": ["**"] }));
    assert!(triggers["pull_request"].is_null(), "every pull request");
}

/// The runner labels of a job: Depot's for Open-Breach, and GitHub's for any
/// other owner of the repository.
fn runner_labels(file: &str, name: &str, job: &Value) -> Vec<(String, String)> {
    let runs_on = job["runs-on"]
        .as_str()
        .unwrap_or_else(|| panic!("{file}: the job {name} names one runner, not a list"));
    let choice = runs_on
        .strip_prefix("${{ github.repository_owner == 'Open-Breach' && ")
        .and_then(|rest| rest.strip_suffix(" }}"))
        .and_then(|rest| rest.split_once(" || "))
        .unwrap_or_else(|| {
            panic!("{file}: the job {name} chooses its runner by the repository's owner: {runs_on}")
        });
    if choice == ("matrix.depot", "matrix.github") {
        return job["strategy"]["matrix"]["include"]
            .as_array()
            .unwrap_or_else(|| panic!("{file}: the job {name} has a matrix"))
            .iter()
            .map(|entry| {
                let label = |key: &str| {
                    entry[key]
                        .as_str()
                        .unwrap_or_else(|| panic!("{file}: {name}: a matrix entry without {key}"))
                        .to_owned()
                };
                (label("depot"), label("github"))
            })
            .collect();
    }
    let literal = |label: &str| {
        label
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap_or_else(|| panic!("{file}: {name}: {label} is not a label"))
            .to_owned()
    };
    vec![(literal(choice.0), literal(choice.1))]
}

// Design D11, task 1.4: every job of every workflow runs on a Depot runner
// when the repository belongs to Open-Breach, and on GitHub's runner of the
// same platform for any other owner. One label per job. The jobs that
// compile Rust or build the image get four processors.
#[test]
fn every_job_runs_on_depot_for_open_breach_and_on_github_otherwise() {
    /// A Depot label and the GitHub label of the same platform.
    type Labels = (&'static str, &'static str);
    const AMD64: Labels = ("depot-ubuntu-24.04-4", "ubuntu-24.04");
    const ARM64: Labels = ("depot-ubuntu-24.04-arm-4", "ubuntu-24.04-arm");
    const SMALL: Labels = ("depot-ubuntu-24.04", "ubuntu-24.04");
    // Which runners each job gets. A new job is added here.
    let sizes: [(&str, &str, &[Labels]); 4] = [
        ("ci.yml", "check", &[AMD64]),
        ("ci.yml", "image", &[AMD64, ARM64]),
        ("ci.yml", "workflows", &[SMALL]),
        ("ci.yml", "manifest", &[SMALL]),
    ];
    let mut seen = 0;
    for (file, workflow) in workflows() {
        for (name, job) in jobs(&workflow) {
            if job.get("uses").is_some() {
                // A call of another workflow: its jobs name their runners.
                assert!(job.get("runs-on").is_none(), "{file}: {name}");
                continue;
            }
            let expected = sizes
                .iter()
                .find(|(in_file, job, _)| *in_file == file && job == name)
                .unwrap_or_else(|| panic!("{file}: the job {name} has no size in this test"))
                .2;
            let labels = runner_labels(&file, name, job);
            let labels: Vec<(&str, &str)> = labels
                .iter()
                .map(|(depot, github)| (depot.as_str(), github.as_str()))
                .collect();
            assert_eq!(labels, expected, "{file}: the runners of {name}");
            seen += 1;
        }
    }
    assert_eq!(seen, sizes.len(), "every job in the table exists");
}

// image-release, "Only a checked image is published"; task 1.2. Each image
// job saves the image its container tests ran against, after those tests,
// and hands it on as an artifact that is named after the platform.
#[test]
fn image_jobs_hand_on_the_image_the_container_tests_ran_against() {
    let ci = workflow("ci.yml");
    let image = &ci["jobs"]["image"];
    let artifacts: Vec<(&str, &str)> = image["strategy"]["matrix"]["include"]
        .as_array()
        .expect("a matrix")
        .iter()
        .map(|entry| {
            (
                entry["platform"].as_str().expect("a platform"),
                entry["artifact"].as_str().expect("an artifact"),
            )
        })
        .collect();
    assert_eq!(
        artifacts,
        [
            ("linux/amd64", "image-linux-amd64"),
            ("linux/arm64", "image-linux-arm64"),
        ]
    );

    let steps = steps(image);
    let position = |what: &str, found: &dyn Fn(&Value) -> bool| {
        steps
            .iter()
            .position(found)
            .unwrap_or_else(|| panic!("the image job has a step that {what}"))
    };
    let tests = position("runs the container tests", &|step| {
        step["run"]
            .as_str()
            .is_some_and(|run| run.contains("--test container"))
    });
    let tested = steps[tests]["env"]["BUTTERBOT_IMAGE"]
        .as_str()
        .expect("the image under test");
    let save = position("saves the image", &|step| {
        step["run"]
            .as_str()
            .is_some_and(|run| run.contains("docker save "))
    });
    let upload = position("uploads the image", &|step| {
        step["uses"]
            .as_str()
            .is_some_and(|uses| uses.starts_with("actions/upload-artifact@"))
    });
    assert!(tests < save && save < upload, "tested, saved, uploaded");

    // What is saved is the image the tests ran against, and the step fails
    // when the archive holds an image with another identifier.
    let script = steps[save]["run"].as_str().expect("a script");
    assert!(
        script.contains(&format!("docker save --output \"$ARCHIVE\" {tested}\n")),
        "{script}"
    );
    assert!(
        script.contains("saved=\"$(.github/scripts/image-id.sh \"$ARCHIVE\")\"\n"),
        "{script}"
    );
    assert!(
        script.ends_with("test \"$saved\" = \"$TESTED\"\n"),
        "{script}"
    );
    assert_eq!(
        steps[save]["env"]["TESTED"],
        "${{ steps.image.outputs.id }}"
    );
    let noted = steps
        .iter()
        .position(|step| step["id"] == "image")
        .expect("the step that notes the identifier");
    assert!(noted < tests, "the identifier is noted before the tests");
    assert!(
        steps[noted]["run"].as_str().expect("a script").contains(
            &format!("echo \"id=$(docker image inspect --format '{{{{.Id}}}}' {tested})\" >> \"$GITHUB_OUTPUT\"")
        ),
        "{}",
        steps[noted]["run"]
    );

    let with = &steps[upload]["with"];
    assert_eq!(with["name"], "${{ matrix.artifact }}");
    assert_eq!(with["path"], steps[save]["env"]["ARCHIVE"]);
    assert_eq!(with["if-no-files-found"], "error");
    let days = with["retention-days"].as_u64().expect("a retention");
    assert!((1..=14).contains(&days), "a short retention: {days} days");
}

/// Writes an archive as `docker save` writes it, with one configuration.
/// `named` is the name the archive gives the configuration.
fn image_archive(directory: &Path, configuration: &str, named: &str) -> std::path::PathBuf {
    let content = directory.join("content");
    fs::create_dir_all(content.join("blobs/sha256")).expect("a directory");
    fs::write(content.join("blobs/sha256").join(named), configuration).expect("written");
    fs::write(
        content.join("manifest.json"),
        format!(
            r#"[{{"Config":"blobs/sha256/{named}","RepoTags":["butterbot:dev"],"Layers":[]}}]"#
        ),
    )
    .expect("written");
    let archive = directory.join("image.tar");
    let status = Command::new("tar")
        .arg("-cf")
        .arg(&archive)
        .arg("-C")
        .arg(&content)
        .args(["manifest.json", "blobs"])
        .status()
        .expect("tar can be run");
    assert!(status.success());
    archive
}

fn script(name: &str) -> Command {
    Command::new(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".github/scripts")
            .join(name),
    )
}

// Task 1.2: the identifier of a saved image is the digest of its
// configuration, read from the archive.
#[test]
fn identifier_of_a_saved_image_is_the_digest_of_its_configuration() {
    // The SHA-256 digest of `{"architecture":"arm64","os":"linux"}`.
    let configuration = r#"{"architecture":"arm64","os":"linux"}"#;
    let digest = "d6f56bc20064075ce319ac2e6fcef5de9ea21773b0a8a4398c4405222971f9c0";
    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let archive = image_archive(directory.path(), configuration, digest);
    let output = script("image-id.sh")
        .arg(&archive)
        .output()
        .expect("the script can be run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("sha256:{digest}\n")
    );
}

// An archive that names its configuration by another digest than its own is
// refused, and so is a file that is not there.
#[test]
fn identifier_of_a_saved_image_is_refused_when_the_archive_is_wrong() {
    let directory = tempfile::TempDir::new().expect("a temporary directory");
    let other = "0".repeat(64);
    let archive = image_archive(directory.path(), "{}", &other);
    let output = script("image-id.sh")
        .arg(&archive)
        .output()
        .expect("the script can be run");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
        message.contains(&format!("is named {other}, but its digest is ")),
        "{message}"
    );

    let output = script("image-id.sh")
        .arg(directory.path().join("missing.tar"))
        .output()
        .expect("the script can be run");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no image archive at "),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
