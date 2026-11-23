//! The `butterbot` command line: commands, options, exit codes and log output.

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;

use crate::logging::{Level, LogFormat, Logger};

/// Keeps ACME certificates up to date: checks every configured certificate and
/// issues the ones that are missing or about to expire.
#[derive(Debug, Parser)]
#[command(name = "butterbot", version, about, arg_required_else_help = true)]
pub struct Cli {
    /// How the log is written. Without this option: text on a terminal, JSON
    /// everywhere else.
    #[arg(long, global = true, value_enum, value_name = "FORMAT")]
    pub log_format: Option<LogFormat>,

    /// The least severe records that are written to the log.
    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "LEVEL",
        default_value = "info"
    )]
    pub log_level: Level,
}

impl Cli {
    /// The logger the options ask for. It writes to standard output.
    pub fn logger(&self) -> Logger {
        let format = LogFormat::choose(self.log_format, std::io::stdout().is_terminal());
        Logger::stdout(format, self.log_level)
    }
}

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
    fn log_options_have_their_defaults() {
        let cli = Cli::try_parse_from(["butterbot", "--log-level", "warn"]).expect("valid");
        assert_eq!(
            cli.log_format, None,
            "the format is chosen by terminal detection"
        );

        let cli = Cli::try_parse_from(["butterbot", "--log-format", "json"]).expect("valid");
        assert_eq!(cli.log_level, Level::Info);
    }

    #[test]
    fn log_format_and_level_can_be_set() {
        let cli =
            Cli::try_parse_from(["butterbot", "--log-format", "json", "--log-level", "debug"])
                .expect("valid options");
        assert_eq!(cli.log_format, Some(LogFormat::Json));
        assert_eq!(cli.log_level, Level::Debug);

        let cli = Cli::try_parse_from(["butterbot", "--log-format=text", "--log-level=error"])
            .expect("valid options");
        assert_eq!(cli.log_format, Some(LogFormat::Text));
        assert_eq!(cli.log_level, Level::Error);
    }

    #[test]
    fn unknown_log_format_or_level_is_a_usage_error() {
        for arguments in [
            ["butterbot", "--log-format", "xml"],
            ["butterbot", "--log-level", "loud"],
        ] {
            let error = Cli::try_parse_from(arguments).expect_err("not a known value");
            assert_eq!(error.exit_code(), 2);
        }
    }

    #[test]
    fn version_is_the_crate_version() {
        assert_eq!(
            Cli::command().get_version(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }
}
