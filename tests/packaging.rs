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
    let sizes: [(&str, &str, &[Labels]); 7] = [
        ("ci.yml", "check", &[AMD64]),
        ("ci.yml", "image", &[AMD64, ARM64]),
        ("ci.yml", "rehearsal", &[SMALL]),
        ("ci.yml", "workflows", &[SMALL]),
        ("ci.yml", "manifest", &[SMALL]),
        ("release.yml", "verify", &[SMALL]),
        ("release.yml", "publish", &[SMALL]),
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
            .is_some_and(|run| run.contains("docker save --output \"$ARCHIVE\" "))
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
    let noting = steps[noted]["run"].as_str().expect("a script");
    assert!(
        noting.ends_with(&format!(
            "docker save --output \"$RUNNER_TEMP/before-the-tests.tar\" {tested}\n\
             echo \"id=$(.github/scripts/image-id.sh \"$RUNNER_TEMP/before-the-tests.tar\")\" >> \"$GITHUB_OUTPUT\"\n"
        )),
        "{noting}"
    );

    // The image is built as one image, without attestations beside it.
    let built = steps
        .iter()
        .find(|step| step["name"] == "Build the image")
        .expect("the step that builds the image");
    assert_eq!(
        built["run"],
        format!("docker build --provenance=false --tag {tested} .")
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

// image-release, "Publishing is rehearsed on every change", scenario
// "Rehearsal", and "Publishing waits behind one approval", scenario "Pull
// request"; task 2.2. The checks of a change rehearse the publishing steps
// with the two images of the same run, and nothing in them is given a
// secret or a deployment environment.
#[test]
fn checks_rehearse_the_publishing_steps_without_any_secret() {
    let text = repository_file(".github/workflows/ci.yml");
    assert!(!text.contains("secrets."), "ci.yml reads no secret");
    assert!(!text.contains("secrets:"), "ci.yml is handed no secret");

    let ci = workflow("ci.yml");
    assert_eq!(ci["permissions"], serde_json::json!({ "contents": "read" }));
    for (name, job) in jobs(&ci) {
        assert!(
            job.get("environment").is_none(),
            "{name} has no environment"
        );
        assert!(job.get("permissions").is_none(), "{name} only reads");
    }

    let rehearsal = &ci["jobs"]["rehearsal"];
    assert_eq!(rehearsal["needs"], "image", "after both image jobs");
    let steps = steps(rehearsal);
    let download = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/download-artifact@"))
        })
        .expect("the step that fetches the images");
    // The artifacts of this run, which are the images its jobs have tested.
    assert_eq!(download["with"]["pattern"], "image-linux-*");
    assert!(download["with"].get("run-id").is_none());
    let rehearse = steps.last().expect("a step");
    assert_eq!(
        rehearse["run"],
        ".github/scripts/rehearse-publish.sh \\\n  \
         \"$RUNNER_TEMP/images/image-linux-amd64.tar\" \\\n  \
         \"$RUNNER_TEMP/images/image-linux-arm64.tar\"\n"
    );
    assert!(rehearse.get("env").is_none(), "no setting from outside");

    // The rehearsal runs the script a release runs, and records no
    // attestation.
    let script = repository_file(".github/scripts/rehearse-publish.sh");
    assert!(script.contains("\"$scripts/publish-image.sh\" \"$@\""));
    for (name, job) in jobs(&ci) {
        for step in self::steps(job) {
            let uses = step["uses"].as_str().unwrap_or_default();
            assert!(!uses.contains("attest"), "{name} attests nothing: {uses}");
        }
    }
}

// Task 2.8: what docs/releasing.md says the registry holds is what the
// rehearsal finds there, and the commands it shows are the ones the
// publishing steps run.
#[test]
fn releasing_document_matches_the_rehearsal_and_the_publishing_steps() {
    let document = repository_file("docs/releasing.md");
    let rehearsal = repository_file(".github/scripts/rehearse-publish.sh");
    let publish = repository_file(".github/scripts/publish-image.sh");

    // After 0.1.0 and then 0.2.0-rc.1: the tags the rehearsal expects.
    let expected = "\"0.1 0.1.0 0.1.0-linux-amd64 0.1.0-linux-arm64 0.2.0-rc.1 \
                    0.2.0-rc.1-linux-amd64 0.2.0-rc.1-linux-arm64 latest\"";
    assert!(rehearsal.contains(expected), "the rehearsal's tags");
    let holds = document
        .split("## What the registry holds afterwards")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the section about what the registry holds");
    let mut documented: Vec<&str> = holds
        .lines()
        .filter(|line| line.starts_with("| `"))
        .flat_map(|line| {
            line.split('|')
                .nth(1)
                .expect("the first column")
                .split(',')
                .map(|tag| tag.trim().trim_matches('`'))
        })
        .collect();
    documented.sort_unstable();
    assert_eq!(format!("\"{}\"", documented.join(" ")), expected);

    // The tools, with the options the script gives them.
    for (shown, run) in [
        ("docker load --input ", "docker load --input \"$archive\""),
        ("docker tag ", "docker tag \"$loaded\" \"$local_name\""),
        ("docker push ", "docker push \"${local_names[$index]}\""),
        (
            "docker buildx imagetools create \\\n  --tag ",
            "docker buildx imagetools create \"${tag_arguments[@]}\" \"${sources[@]}\"",
        ),
    ] {
        assert!(document.contains(shown), "the document shows `{shown}`");
        assert!(publish.contains(run), "the script runs `{run}`");
    }
    assert!(publish.contains("local_name=\"$image:$version-${platform/\\//-}\""));
    assert!(publish.contains("sources+=(\"$image@$(digest_of "));

    assert_every_registry_is_the_one_for_examples(&document);
}

/// Every registry that a document names is `registry.example.com`.
fn assert_every_registry_is_the_one_for_examples(document: &str) {
    let mut named = 0;
    for line in document.lines() {
        for (position, _) in line.match_indices("/butterbot") {
            let host = line[..position]
                .rsplit(|character: char| character.is_whitespace() || character == '`')
                .next()
                .expect("a word");
            let host = host.strip_prefix("oci://").unwrap_or(host);
            let rest = &line[position + "/butterbot".len()..];
            if host.ends_with("Open-Breach") || host.ends_with("cronjob") {
                // The repository, or the CronJob of the reference manifest.
                continue;
            }
            if rest.starts_with('/') || host.starts_with('/') || host.contains('$') {
                // A path on a filesystem, such as /etc/butterbot.
                continue;
            }
            assert!(
                host == "registry.example.com" || host == "<REGISTRY_HOST>",
                "{line}"
            );
            named += 1;
        }
        if let Some(position) = line.find("docker login ") {
            assert_eq!(
                line[position..].trim_end(),
                "docker login registry.example.com",
                "{line}"
            );
        }
    }
    assert!(named > 0, "the search finds the image where it is named");
}

// Task 3.5: docs/releasing.md covers cutting a release, the set-up outside
// the repository, verifying the attestation and a release that fails half
// way. Its commands are spelled as the tools accept them: each one below
// was checked against the tool's own help (git 2, gh 2.105, Docker 29 with
// buildx 0.39) on 2026-11-24.
#[test]
fn releasing_document_covers_a_release_from_the_tag_to_the_checks_afterwards() {
    let document = repository_file("docs/releasing.md");
    for section in [
        "## Cutting a release",
        "## What the workflow does",
        "## Set-up outside the repository",
        "## After a release",
        "## When a release fails",
    ] {
        assert!(document.contains(&format!("\n{section}\n")), "{section}");
    }
    for command in [
        "git tag v0.1.0\n",
        "git push origin v0.1.0\n",
        "gh run list --workflow release.yml\n",
        "gh run watch <run id>\n",
        "gh run rerun <run id> --failed\n",
        "`git tag --delete v0.1.0`",
        "`git push --delete origin v0.1.0`",
        "docker login registry.example.com\n",
        "docker buildx imagetools inspect registry.example.com/butterbot:0.1.0\n",
        "gh attestation verify oci://registry.example.com/butterbot:0.1.0 --repo Open-Breach/butterbot\n",
        "  --signer-workflow Open-Breach/butterbot/.github/workflows/release.yml \\\n",
        "  --source-ref refs/tags/v0.1.0\n",
        "gh api repos/Open-Breach/butterbot/attestations/sha256:<digest>\n",
        "docker pull registry.example.com/butterbot:0.1.0\n",
        "docker run --rm registry.example.com/butterbot:0.1.0 --version\n",
    ] {
        assert!(document.contains(command), "{command}");
    }

    // The set-up names what the workflow refers to.
    let release = workflow("release.yml");
    let environment = release["jobs"]["publish"]["environment"]
        .as_str()
        .expect("an environment");
    assert!(document.contains(&format!("an environment named `{environment}`")));
    for secret in ["REGISTRY_HOST", "REGISTRY_USERNAME", "REGISTRY_PASSWORD"] {
        assert!(document.contains(&format!("| `{secret}` |")), "{secret}");
    }
    let pattern = release["on"]["push"]["tags"][0]
        .as_str()
        .expect("a pattern");
    assert!(document.contains(&format!("one rule for tags, `{pattern}`")));
    assert!(document.contains("**Required reviewers**"));
    assert!(document.contains("**Prevent self-review**: off"));
    assert!(document.contains("**A login for the pipeline.**"));
    // The jobs of the workflow, as the document's table has them.
    for (name, _) in jobs(&release) {
        assert!(document.contains(&format!("\n| `{name}` | ")), "{name}");
    }
    // How long the images of a run are kept.
    let days = workflow("ci.yml")["jobs"]["image"]["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find_map(|step| step["with"]["retention-days"].as_u64())
        .expect("a retention");
    assert_eq!(days, 7);
    assert!(document.contains("They are kept for seven days."));
    // The runners of Open-Breach are recorded as self-hosted.
    assert!(document.contains("Do not add `--deny-self-hosted-runners`"));

    // The page has no address of a registry: the only mention of a scheme
    // is the one that says to leave it out.
    for line in document.lines() {
        assert!(
            !line.contains("://") || line.contains("no `https://`") || line.contains("oci://"),
            "an address in {line}"
        );
    }

    assert_every_registry_is_the_one_for_examples(&document);
}

/// What `needs` names, whether it is one job or a list.
fn needs(job: &Value) -> Vec<&str> {
    match &job["needs"] {
        Value::String(one) => vec![one.as_str()],
        Value::Array(several) => several
            .iter()
            .map(|job| job.as_str().expect("a job"))
            .collect(),
        _ => Vec::new(),
    }
}

// image-release, "A version tag starts a release", scenarios "Version tag",
// "Change to a branch" and "Another tag", and "Only a checked image is
// published", scenario "A check fails"; task 3.2. Only a tag that begins
// with `v` starts the release workflow, no other workflow starts for a tag,
// and its jobs run in the order verify, checks, publish.
#[test]
fn release_starts_for_version_tags_only_and_publishes_after_the_checks() {
    let release = workflow("release.yml");
    assert_eq!(
        release["on"],
        serde_json::json!({ "push": { "tags": ["v*"] } }),
        "nothing but a pushed tag that begins with v"
    );
    for (file, workflow) in workflows() {
        if file != "release.yml" {
            let push = &workflow["on"]["push"];
            assert!(
                push.get("tags").is_none() && push.get("branches").is_some(),
                "{file} does not start for a tag"
            );
        }
    }

    // One release at a time, and a release that is running is not cancelled.
    assert_eq!(
        release["concurrency"],
        serde_json::json!({ "group": "release", "cancel-in-progress": false })
    );

    let jobs = jobs(&release);
    let names: Vec<&str> = jobs.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["verify", "checks", "publish"]);
    let job = |name: &str| &release["jobs"][name];
    assert!(needs(job("verify")).is_empty(), "verify comes first");
    assert_eq!(needs(job("checks")), ["verify"]);
    assert_eq!(needs(job("publish")), ["verify", "checks"]);
    // No job runs although one it needs has failed.
    for (name, job) in &jobs {
        assert!(job.get("if").is_none(), "{name} has no condition");
        assert!(job.get("continue-on-error").is_none(), "{name}");
    }

    // The checks are the CI workflow itself, not a copy of its jobs, and
    // they are handed no secret.
    assert_eq!(job("checks")["uses"], "./.github/workflows/ci.yml");
    assert!(job("checks").get("secrets").is_none());
    assert!(job("checks").get("with").is_none());

    // verify has the history of main, and checks the tag that was pushed.
    let verify = steps(job("verify"));
    assert_eq!(
        verify[0]["with"]["fetch-depth"], 0,
        "not a shallow checkout"
    );
    let check = verify.last().expect("a step");
    assert_eq!(
        check["run"],
        ".github/scripts/check-release-tag.sh \"$TAG\""
    );
    assert_eq!(
        check["env"],
        serde_json::json!({ "TAG": "${{ github.ref_name }}" })
    );
    assert_eq!(
        job("verify")["outputs"],
        serde_json::json!({ "version": "${{ steps.tag.outputs.version }}" })
    );
    assert_eq!(check["id"], "tag");

    // publish takes the version from verify and the images from the checks
    // of the same run.
    let publish = steps(job("publish"));
    let download = publish
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/download-artifact@"))
        })
        .expect("the step that fetches the images");
    assert_eq!(download["with"]["pattern"], "image-linux-*");
    assert!(download["with"].get("run-id").is_none(), "of this run");
    let script = publish
        .iter()
        .find(|step| step["id"] == "publish")
        .expect("the step that publishes");
    assert_eq!(
        script["run"],
        ".github/scripts/publish-image.sh \"$VERSION\" \\\n  \
         \"$RUNNER_TEMP/images/image-linux-amd64.tar\" \\\n  \
         \"$RUNNER_TEMP/images/image-linux-arm64.tar\"\n"
    );
    assert_eq!(
        script["env"]["VERSION"],
        "${{ needs.verify.outputs.version }}"
    );
}

// image-release, "Registry settings come from the environment's secrets",
// scenario "Repository contents", and "Publishing waits behind one
// approval", scenarios "Release waits" and "Pull request"; task 3.3.
// Exactly one job in any workflow has the environment `release` or a
// registry secret, and every other job only reads.
#[test]
fn only_the_publishing_job_has_the_environment_and_the_registry_secrets() {
    let read_only = serde_json::json!({ "contents": "read" });
    let mut with_environment = Vec::new();
    for (file, workflow) in workflows() {
        assert_eq!(workflow["permissions"], read_only, "{file}");
        let text = repository_file(&format!(".github/workflows/{file}"));
        assert!(!text.contains("secrets: inherit"), "{file}");
        for (name, job) in jobs(&workflow) {
            let publishing = file == "release.yml" && name == "publish";
            if job.get("environment").is_some() {
                with_environment.push((file.clone(), name.clone()));
            }
            let job_text = serde_json::to_string(job).expect("JSON");
            assert_eq!(
                job_text.contains("secrets."),
                publishing,
                "{file}: whether {name} reads a secret"
            );
            if !publishing {
                assert!(
                    job.get("permissions").is_none(),
                    "{file}: {name} only reads"
                );
                assert!(job.get("secrets").is_none(), "{file}: {name}");
            }
        }
    }
    assert_eq!(
        with_environment,
        [("release.yml".to_owned(), "publish".to_owned())]
    );

    let release = workflow("release.yml");
    let publish = &release["jobs"]["publish"];
    assert_eq!(publish["environment"], "release");
    // What an attestation needs, and nothing else.
    assert_eq!(
        publish["permissions"],
        serde_json::json!({ "contents": "read", "id-token": "write", "attestations": "write" })
    );

    // The three secrets, each handed to the one step that publishes, under
    // its own name and unchanged.
    let mut handed = 0;
    for step in steps(publish) {
        let step_text = serde_json::to_string(step).expect("JSON");
        if step["id"] == "publish" {
            for name in ["REGISTRY_HOST", "REGISTRY_USERNAME", "REGISTRY_PASSWORD"] {
                assert_eq!(step["env"][name], format!("${{{{ secrets.{name} }}}}"));
            }
            assert_eq!(step_text.matches("secrets.").count(), 3);
            // The script is given the settings by the environment only: the
            // host is not on the command line, where it would be printed.
            assert!(
                !step["run"]
                    .as_str()
                    .expect("a script")
                    .contains("REGISTRY_")
            );
            handed += 1;
        } else {
            assert!(!step_text.contains("secrets."), "{step_text}");
        }
    }
    assert_eq!(handed, 1);
    let text = repository_file(".github/workflows/release.yml");
    assert_eq!(text.matches("${{ secrets.").count(), 3);
}

/// The files under `.github`: the workflows and their scripts.
fn workflow_files() -> Vec<(String, String)> {
    let mut files = Vec::new();
    for directory in [".github/workflows", ".github/scripts"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(directory);
        for entry in fs::read_dir(path).expect(directory) {
            let name = entry.expect("an entry").file_name();
            let file = format!("{directory}/{}", name.to_string_lossy());
            let text = repository_file(&file);
            files.push((file, text));
        }
    }
    files.push((
        ".github/actionlint.yaml".to_owned(),
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/actionlint.yaml"))
            .unwrap_or_default(),
    ));
    files
}

/// The word that ends at `end` in `line`: what a shell would hand on as one
/// argument, without quotation marks.
fn word_before(line: &str, end: usize) -> &str {
    line[..end]
        .rsplit(|character: char| {
            character.is_whitespace() || matches!(character, '"' | '\'' | '`' | '(' | '[' | '=')
        })
        .next()
        .unwrap_or_default()
}

// image-release, "Nothing else names the registry" and "Registry settings
// come from the environment's secrets", scenario "Repository contents", and
// "The project publishes no image", scenario "Workflows"; task 3.3. No
// workflow and no script names a registry: not as the host of an image, not
// in an address, and not as one of the public registries.
#[test]
fn no_workflow_or_script_names_a_registry() {
    // Hosts that an address may name: the one for examples, the machine
    // itself, and where the linter is downloaded from.
    const ADDRESSES: [&str; 4] = ["registry.example.com", "localhost", "$host", "github.com"];
    // What may stand before `/butterbot`: a variable that holds the host
    // that was given, or the host for examples.
    const IMAGE_HOSTS: [&str; 4] = [
        "$REGISTRY_HOST",
        "<REGISTRY_HOST>",
        "$host",
        "registry.example.com",
    ];
    const PUBLIC_REGISTRIES: [&str; 9] = [
        "docker.io",
        "ghcr.io",
        "quay.io",
        "gcr.io",
        "pkg.dev",
        "azurecr.io",
        "amazonaws.com",
        "mcr.microsoft.com",
        "registry.gitlab.com",
    ];
    let mut images = 0;
    for (file, text) in workflow_files() {
        for (number, line) in text.lines().enumerate() {
            let place = format!("{file}:{}: {line}", number + 1);
            for registry in PUBLIC_REGISTRIES {
                assert!(!line.contains(registry), "a public registry in {place}");
            }
            // An image `<host>/butterbot`.
            for (position, _) in line.match_indices("/butterbot") {
                let rest = &line[position + "/butterbot".len()..];
                if rest.starts_with(['/', '-', '.']) && !rest.starts_with(".git") {
                    // A path such as /butterbot/tags/list of the registry's
                    // interface, or a file name.
                    continue;
                }
                let host = word_before(line, position);
                if host.ends_with("github.com/Open-Breach")
                    || host.ends_with("/v2")
                    || host.is_empty()
                {
                    continue;
                }
                assert!(IMAGE_HOSTS.contains(&host), "the host {host} in {place}");
                images += 1;
            }
            // An address: a scheme, `://` and a host.
            for (position, _) in line.match_indices("://") {
                if !line[..position].ends_with(|character: char| character.is_ascii_alphabetic()) {
                    continue;
                }
                let host: String = line[position + 3..]
                    .chars()
                    .take_while(|character| !matches!(character, '/' | '"' | '\'' | ' ' | ':'))
                    .collect();
                assert!(
                    ADDRESSES.contains(&host.as_str()),
                    "the address {host} in {place}"
                );
            }
            // A login names its registry as an argument.
            for command in ["docker login ", "docker logout "] {
                if let Some(position) = line.find(command) {
                    let argument = line[position + command.len()..]
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .trim_matches('"');
                    assert!(
                        argument == "$REGISTRY_HOST" || argument == "$host",
                        "the registry of a login in {place}"
                    );
                }
            }
        }
    }
    assert!(images > 0, "the search finds the image where it is named");
}

// image-release, "The release is attested", scenario "Attestation
// recorded", and "Nothing else names the registry"; task 3.4. The last step
// of the publishing job attests the digest that the publishing step handed
// on, under the name `butterbot`, and does not push the attestation to the
// registry. Nothing else attests.
#[test]
fn release_attests_the_published_digest_under_the_bare_image_name() {
    let release = workflow("release.yml");
    let publish = steps(&release["jobs"]["publish"]);
    let attest = publish.last().expect("a step");
    assert!(
        attest["uses"]
            .as_str()
            .expect("an action")
            .starts_with("actions/attest@"),
        "{}",
        attest["uses"]
    );
    assert_eq!(
        attest["with"],
        serde_json::json!({
            "subject-name": "butterbot",
            "subject-digest": "${{ steps.publish.outputs.digest }}",
            "push-to-registry": false,
        })
    );
    assert!(
        attest.get("env").is_none(),
        "it is given no registry setting"
    );
    assert!(
        attest.get("if").is_none(),
        "it runs when the image is published"
    );
    let published = publish
        .iter()
        .position(|step| step["id"] == "publish")
        .expect("the step that publishes");
    assert_eq!(
        published + 2,
        publish.len(),
        "right after the publishing step"
    );
    // The digest it attests is the one the script wrote to its outputs.
    assert!(
        repository_file(".github/scripts/publish-image.sh")
            .contains("printf 'digest=%s\\n' \"$digest\" >> \"${GITHUB_OUTPUT:-/dev/null}\"")
    );

    let mut attesting = 0;
    for (file, workflow) in workflows() {
        for (name, job) in jobs(&workflow) {
            if job.get("uses").is_some() {
                continue;
            }
            for step in steps(job) {
                if step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.contains("attest"))
                {
                    assert_eq!((file.as_str(), name.as_str()), ("release.yml", "publish"));
                    attesting += 1;
                }
            }
        }
    }
    assert_eq!(attesting, 1);
}

// image-release, "The project publishes no image", scenario "Reader of the
// README"; task 4.1. The README says how to build the program from source,
// says that no image is published, and points to the commands for building
// one.
#[test]
fn readme_says_that_the_program_is_built_from_source_and_no_image_is_published() {
    let readme = repository_file("README.md");
    let getting = readme
        .split("\n## Getting Butterbot\n")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the section about getting Butterbot");
    assert!(
        getting.contains("Butterbot is a console application, and it is built from source."),
        "{getting}"
    );
    assert!(
        getting.contains("\n```\ncargo build --release\n```\n"),
        "{getting}"
    );
    assert!(
        getting.contains("The project publishes no container image, and no prebuilt program."),
        "{getting}"
    );
    // The note that it packs into a container, with the pointer.
    assert!(
        getting.contains("Butterbot packs easily into a container")
            && getting.contains(
                "[Deployment](docs/deployment.md) has the commands for building an image from it."
            ),
        "{getting}"
    );
    // The section comes before the example that uses the program.
    assert!(
        readme.find("## Getting Butterbot").expect("the section")
            < readme.find("## Quick start").expect("the example")
    );
    assert!(readme.contains("(docs/releasing.md)"));
    // Nothing in the README offers an image to pull.
    assert!(!readme.contains("docker pull"), "no image is offered");
}

// image-release, "The project publishes no image"; task 4.2. The deployment
// document says that the project publishes no image, and still has the
// commands for building one from the Dockerfile.
#[test]
fn deployment_document_says_that_no_image_is_published_and_how_to_build_one() {
    let document = repository_file("docs/deployment.md");
    let image = document
        .split("\n## The container image\n")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the section about the container image");
    assert!(
        image.trim_start().starts_with(
            "The project publishes no container image. Build one from the `Dockerfile` in the \
             repository, and push it to your own registry:\n"
        ),
        "{image}"
    );
    assert!(!document.contains("No image is published yet"));
    assert!(
        image.contains(
            "\n```\ndocker buildx build --platform linux/amd64,linux/arm64 --tag \
             registry.example.com/butterbot:0.1.0 --push .\n```\n"
        ),
        "{image}"
    );
    assert!(
        image.contains("`docker build --tag butterbot .`"),
        "{image}"
    );
    assert!(image.contains("[Releasing](releasing.md)"), "{image}");
    assert_every_registry_is_the_one_for_examples(&document);
}
