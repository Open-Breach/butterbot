//! Checks of the Dockerfile and of `docs/deployment.md` that need neither
//! Docker nor a cluster. The image itself is tested in `tests/container.rs`,
//! and the manifest is given to `kubectl` in the CI workflow.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

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
