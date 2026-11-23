//! Command-line tests: they start the built `butterbot` executable.

use std::process::{Command, Output};

fn butterbot(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_butterbot"))
        .args(args)
        .output()
        .expect("the butterbot executable can be started")
}

// command-line, "Help and version", scenario "Version".
#[test]
fn version_prints_the_crate_version_and_exits_with_0() {
    let output = butterbot(&["--version"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        format!("butterbot {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_exits_with_0() {
    let output = butterbot(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: butterbot"));
}
