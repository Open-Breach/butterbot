//! The `butterbot` command line: commands, options, exit codes and log output.

use std::process::ExitCode;

use clap::Parser;

/// Keeps ACME certificates up to date: checks every configured certificate and
/// issues the ones that are missing or about to expire.
#[derive(Debug, Parser)]
#[command(name = "butterbot", version, about, arg_required_else_help = true)]
pub struct Cli {}

/// Entry point of the program. Returns the process exit code.
pub fn main() -> ExitCode {
    let _cli = Cli::parse();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn version_is_the_crate_version() {
        assert_eq!(
            Cli::command().get_version(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }
}
