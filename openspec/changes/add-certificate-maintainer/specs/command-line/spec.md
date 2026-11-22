# Spec Delta

## Purpose

Defines the `butterbot` program as its operator sees it: the commands and options, the exit codes, and the log output, both in a container and in a terminal.

## ADDED Requirements

### Requirement: One program for container and terminal
The system SHALL be a single executable named `butterbot` that behaves the same when started in a container and when started from a terminal. It SHALL never prompt for input.

#### Scenario: No terminal attached
- **WHEN** the program runs without a terminal
- **THEN** it completes without waiting for input

### Requirement: The run command
`butterbot run` SHALL perform one run as defined by the run-orchestration capability. `--only <name>` SHALL limit the run to the named certificate and MAY be given several times. `--force` SHALL treat the selected certificates as due and SHALL be accepted only together with `--only`.

#### Scenario: Plain run
- **WHEN** `butterbot run` is executed
- **THEN** every configured certificate is evaluated

#### Scenario: Force without selection
- **WHEN** `butterbot run --force` is executed without `--only`
- **THEN** the program refuses with a usage error and processes nothing

#### Scenario: Unknown certificate name
- **WHEN** `--only` names a certificate that is not configured
- **THEN** the program refuses with a usage error naming the unknown certificate

### Requirement: The status command
`butterbot status` SHALL show, for every configured certificate and each of its stores, what the store holds (nothing, or the certificate's names and expiry), whether the certificate is due and for which reason, and otherwise when it becomes due. It SHALL change nothing. With `--json` the same information SHALL be printed as JSON.

#### Scenario: Read-only
- **WHEN** `butterbot status` is executed while certificates are due
- **THEN** it reports them as due and issues nothing

#### Scenario: JSON output
- **WHEN** `butterbot status --json` is executed
- **THEN** standard output is a single JSON document with one entry per certificate

### Requirement: The check-config command
`butterbot check-config` SHALL validate the configuration exactly as `run` does at start, and SHALL list every secret the configuration requires, with its configuration path, its name in each source, and whether it was found. It SHALL NOT contact ACME servers, DNS providers or stores.

#### Scenario: Valid configuration with a missing secret
- **WHEN** the configuration is valid and `dns.cloudflare.access_key` is supplied by no source
- **THEN** the output lists that path as missing together with its environment and Key Vault names, and the exit code is the configuration-error code

#### Scenario: Valid and complete
- **WHEN** the configuration is valid and every required secret is found
- **THEN** the program says so and exits successfully

### Requirement: The account new command
`butterbot account new --acme <name>` SHALL register an account at the named ACME server, using external account binding when configured, and SHALL print the account credentials together with the name under which each source expects them. It SHALL refuse when credentials for that server already exist.

#### Scenario: New account
- **WHEN** the command is run for a server with no credentials
- **THEN** an account is registered and its credentials are printed with the environment variable name and the Key Vault secret name to store them under

#### Scenario: Account already exists
- **WHEN** credentials for the server are already configured
- **THEN** the command registers nothing and says that an account already exists

### Requirement: Exit codes
The program SHALL exit with 0 for success, 1 when at least one certificate failed, 2 for a configuration or usage error, and 3 when the run was interrupted by a signal.

#### Scenario: Certificate failure
- **WHEN** a run ends with one failed certificate
- **THEN** the exit code is 1

#### Scenario: Configuration error
- **WHEN** the configuration is invalid
- **THEN** the exit code is 2 and no certificate was processed

#### Scenario: Interrupted
- **WHEN** a run is stopped by a termination signal
- **THEN** the exit code is 3

### Requirement: Log format
The program SHALL write its log to standard output: as one JSON object per line when standard output is not a terminal, and as human-readable text when it is. `--log-format json` and `--log-format text` SHALL override the choice.

#### Scenario: In a container
- **WHEN** the program runs with standard output not attached to a terminal
- **THEN** every log line is a JSON object

#### Scenario: In a terminal
- **WHEN** the program runs in an interactive terminal
- **THEN** the log is human-readable text

### Requirement: Log content
Every log record about a certificate SHALL carry the certificate's name and the step being performed. A failure record SHALL carry the reason. The level SHALL be selectable with `--log-level` (default `info`).

#### Scenario: Failed step
- **WHEN** a certificate fails in the propagation check
- **THEN** the log contains a record with that certificate's name, the step `propagation` and the reason

### Requirement: Help and version
`butterbot --help` SHALL list the commands and options, and `butterbot --version` SHALL print the program's version.

#### Scenario: Version
- **WHEN** `butterbot --version` is executed
- **THEN** the version is printed and the exit code is 0
