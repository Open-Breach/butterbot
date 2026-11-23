//! Task 2.2: which configuration files are read, and how they are merged.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;

fn write(directory: &TempDir, name: &str, text: &str) -> PathBuf {
    let path = directory.path().join(name);
    fs::write(&path, text).expect("the test file can be written");
    path
}

fn load(paths: &[PathBuf]) -> Result<Config, ConfigError> {
    Config::from_layers(read_files(paths)?)
}

// configuration, "Configuration file", scenario "Default path".
#[test]
fn default_path_is_used_without_option_and_variable() {
    assert_eq!(
        config_paths(&[], None),
        [Path::new("/etc/butterbot/config.toml")]
    );
    assert_eq!(
        config_paths(&[], Some(OsStr::new(""))),
        [Path::new("/etc/butterbot/config.toml")]
    );
}

#[test]
fn variable_replaces_the_default_path() {
    assert_eq!(
        config_paths(&[], Some(OsStr::new("/srv/butterbot.toml"))),
        [Path::new("/srv/butterbot.toml")]
    );
}

#[test]
fn options_replace_the_variable_and_keep_their_order() {
    let options = [PathBuf::from("a.toml"), PathBuf::from("b.toml")];
    assert_eq!(
        config_paths(&options, Some(OsStr::new("/srv/butterbot.toml"))),
        options
    );
}

// configuration, "Configuration file", scenario "Several files".
#[test]
fn later_file_overrides_the_same_key_of_an_earlier_file() {
    let directory = TempDir::new().unwrap();
    let a = write(
        &directory,
        "a.toml",
        r#"
        [run]
        parallel = 2
        min_reissue = "3d"

        [acme.letsencrypt]
        directory_url = "https://acme.example.com/directory"
        renew_before = "30d"
        "#,
    );
    let b = write(
        &directory,
        "b.toml",
        r#"
        [run]
        parallel = 8

        [acme.letsencrypt]
        renew_before = "20d"
        "#,
    );

    let config = load(&[a.clone(), b.clone()]).expect("both files are valid");
    assert_eq!(config.run.parallel, 8, "the value from b.toml is used");
    assert_eq!(
        config.run.min_reissue,
        days(3),
        "a key only a.toml sets is kept"
    );
    let letsencrypt = &config.acme["letsencrypt"];
    assert_eq!(letsencrypt.renew_before, Some(days(20)));
    assert_eq!(
        letsencrypt.directory_url,
        "https://acme.example.com/directory"
    );

    let config = load(&[b, a]).expect("both files are valid");
    assert_eq!(
        config.run.parallel, 2,
        "in the other order the value from a.toml is used"
    );
}

#[test]
fn certificate_list_of_a_later_file_replaces_the_earlier_list() {
    let providers = r#"
        [acme.letsencrypt]
        directory_url = "https://acme.example.com/directory"
        [dns.cloudflare]
        type = "cloudflare"
        [store.local]
        type = "directory"
        path = "/certs"
    "#;
    let entry = |name: &str| {
        format!(
            "[[certificate]]\nname = \"{name}\"\nnames = [\"{name}.example.com\"]\n\
             acme = \"letsencrypt\"\ndns = \"cloudflare\"\nstores = [\"local\"]\n"
        )
    };
    let directory = TempDir::new().unwrap();
    let a = write(
        &directory,
        "a.toml",
        &format!("{providers}\n{}{}", entry("one"), entry("two")),
    );
    let b = write(&directory, "b.toml", &entry("three"));

    let config = load(&[a, b]).expect("both files are valid");
    let names: Vec<_> = config
        .certificates
        .iter()
        .map(|certificate| &certificate.name)
        .collect();
    assert_eq!(
        names,
        ["three"],
        "a list is replaced as a whole, not merged"
    );
}

// configuration, "Configuration file", scenario "File missing".
#[test]
fn missing_file_is_reported_by_name() {
    let directory = TempDir::new().unwrap();
    let present = write(&directory, "present.toml", "");
    let missing = directory.path().join("missing.toml");

    let error = load(&[present, missing.clone()]).expect_err("a named file must exist");
    assert_eq!(
        error.problems,
        [format!(
            "configuration file {} does not exist",
            missing.display()
        )]
    );
}

#[test]
fn every_unusable_file_is_reported() {
    let directory = TempDir::new().unwrap();
    let broken = write(&directory, "broken.toml", "[run\nparallel = 2\n");
    let missing = directory.path().join("missing.toml");

    let error = load(&[broken.clone(), missing.clone()]).expect_err("neither file is usable");
    assert_eq!(error.problems.len(), 2, "{error}");
    assert!(
        error.problems[0].starts_with(&format!(
            "configuration file {} is not valid TOML: line 1, ",
            broken.display()
        )),
        "{error}"
    );
    assert!(
        error.problems[1].ends_with("missing.toml does not exist"),
        "{error}"
    );
}

#[test]
fn problem_in_a_file_names_the_file() {
    let directory = TempDir::new().unwrap();
    let path = write(&directory, "config.toml", "[run]\nparallel = 0\n");

    let error = load(std::slice::from_ref(&path)).expect_err("parallel must be at least 1");
    assert_eq!(
        error.problems,
        [format!(
            "run: `parallel`: 0 is out of range: expected at least 1 (set in {})",
            path.display()
        )]
    );
}
