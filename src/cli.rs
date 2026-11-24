//! The `butterbot` command line: commands, options, exit codes and log output.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::config::{
    AzureSecretVault, ConfigError, Loaded, PATH_VARIABLE, SecretVault, config_paths,
};
use crate::events::EventKind;
use crate::logging::{Level, LogFormat, Logger};

mod account;
mod check_config;
mod notify;
mod run;
mod status;

/// Keeps ACME certificates up to date: checks every configured certificate and
/// issues the ones that are missing or about to expire.
#[derive(Debug, Parser)]
#[command(
    name = "butterbot",
    version,
    about,
    arg_required_else_help = true,
    after_help = "Exit codes: 0 success, 1 at least one certificate failed or a test event was \
                  not delivered, 2 configuration or usage error, 3 interrupted by a signal."
)]
pub struct Cli {
    /// A configuration file. May be given several times; later files override
    /// earlier ones. Without this option: the file named by BUTTERBOT_CONFIG,
    /// or /etc/butterbot/config.toml.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Vec<PathBuf>,

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

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Check every configured certificate and issue the ones that are due.
    Run {
        /// Process only the named certificate. May be given several times.
        #[arg(long, value_name = "NAME")]
        only: Vec<String>,

        /// Issue the selected certificates even when they are not due.
        /// Accepted only together with --only.
        #[arg(long, requires = "only")]
        force: bool,
    },
    /// Show what each store holds and whether each certificate is due.
    /// Changes nothing.
    Status {
        /// Print the status as one JSON document.
        #[arg(long)]
        json: bool,
    },
    /// Validate the configuration and list the secrets it expects.
    CheckConfig,
    /// Manage the accounts at ACME servers.
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Check the notifiers that receive the events of a run.
    Notify {
        #[command(subcommand)]
        command: NotifyCommand,
    },
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum AccountCommand {
    /// Register an account at an ACME server and print its credentials.
    New {
        /// The name of the ACME server in the configuration.
        #[arg(long, value_name = "NAME")]
        acme: String,
    },
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum NotifyCommand {
    /// Send one sample event through a notifier and say whether it was
    /// delivered. The sample is marked as a test. No certificate is processed.
    Test {
        /// The name of the notifier in the configuration.
        #[arg(value_name = "NAME")]
        name: String,

        /// The kind of the sample event: run_completed, failure, renewed or
        /// expiring.
        #[arg(long, value_name = "KIND", default_value = "failure")]
        event: EventKind,
    },
}

/// How the program ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The command did what it was asked to do.
    Success,
    /// At least one certificate failed, or the command could not do its work.
    Failed,
    /// The configuration is invalid, or the command line asks for something
    /// that cannot be done.
    Invalid,
    /// The run was stopped by a signal.
    Interrupted,
}

impl Exit {
    /// The exit code of the process.
    pub fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Failed => 1,
            Self::Invalid => 2,
            Self::Interrupted => 3,
        }
    }
}

impl Cli {
    /// The log of a run. It is written to standard output.
    pub fn logger(&self) -> Logger {
        let format = LogFormat::choose(self.log_format, std::io::stdout().is_terminal());
        Logger::stdout(format, self.log_level)
    }

    /// The log of a command whose standard output is its result, such as
    /// `status --json`. It is written to standard error.
    pub fn diagnostics(&self) -> Logger {
        let format = LogFormat::choose(self.log_format, std::io::stderr().is_terminal());
        Logger::stderr(format, self.log_level)
    }

    /// Loads the configuration the options and the environment name: the
    /// files, the environment, and Key Vault when it is enabled.
    async fn load(&self, log: &Logger) -> Result<Loaded, ConfigError> {
        let variable = std::env::var_os(PATH_VARIABLE);
        let paths = config_paths(&self.config, variable.as_deref());
        crate::config::load(log, &paths, std::env::vars_os(), |url| {
            let http = crate::http::client()?;
            let vault: Arc<dyn SecretVault> = Arc::new(AzureSecretVault::connect(url, &http)?);
            Ok(vault)
        })
        .await
    }
}

/// Writes every problem of an unusable configuration to the log.
fn report_invalid(log: &Logger, error: &ConfigError) {
    for problem in &error.problems {
        log.error(format!("invalid configuration: {problem}"));
    }
}

/// Reports a command line that asks for something the configuration does not
/// allow, in the same way as any other usage error.
fn usage_error(message: impl std::fmt::Display) -> Exit {
    use clap::CommandFactory;

    let _ = Cli::command()
        .error(clap::error::ErrorKind::ValueValidation, message)
        .print();
    Exit::Invalid
}

/// Performs the command and says how the program ends.
pub async fn execute(cli: &Cli) -> Exit {
    match &cli.command {
        Command::Run { only, force } => run::run(cli, only, *force).await,
        Command::Status { json } => status::status(cli, *json).await,
        Command::CheckConfig => check_config::check_config(cli).await,
        Command::Account {
            command: AccountCommand::New { acme },
        } => account::new(cli, acme).await,
        Command::Notify {
            command: NotifyCommand::Test { name, event },
        } => notify::test(cli, name, *event).await,
    }
}

/// Entry point of the program. Returns the process exit code.
pub fn main() -> ExitCode {
    // A usage error ends the program here, with exit code 2.
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: the program cannot start: {error}");
            return ExitCode::from(Exit::Failed.code());
        }
    };
    let exit = runtime.block_on(execute(&cli));
    // Whatever is still waiting for an answer is not waited for.
    runtime.shutdown_background();
    ExitCode::from(exit.code())
}

#[cfg(test)]
mod tests;
