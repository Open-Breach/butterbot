# Spec Delta

## Purpose

Defines the container image through which Butterbot is run by default, so that a scheduler can start it with no arguments and get a normal run.

## ADDED Requirements

### Requirement: Default command is a run
The container image SHALL start `butterbot run` when it is started without arguments, reading its configuration from `/etc/butterbot/config.toml`. Arguments given to the container SHALL replace the default command's arguments, so that every `butterbot` command can be run from the image.

#### Scenario: Started without arguments
- **WHEN** the image is started with a configuration file mounted at `/etc/butterbot/config.toml` and no arguments
- **THEN** it performs one run and the container exits with the run's exit code

#### Scenario: Another command
- **WHEN** the image is started with the arguments `status`
- **THEN** it runs `butterbot status`

### Requirement: Supported platforms
The container image SHALL be published for `linux/amd64` and `linux/arm64` under one image name.

#### Scenario: Arm64 node
- **WHEN** the image is pulled on an arm64 node
- **THEN** the arm64 variant is used and the program runs

### Requirement: Unprivileged and self-contained
The container SHALL run as a non-root user. The image SHALL contain everything the program needs at run time, including the certificate authorities it needs to verify the services it calls.

#### Scenario: Non-root
- **WHEN** the container is started with its default settings
- **THEN** the program runs with a non-zero user id

#### Scenario: Read-only root filesystem
- **WHEN** the container is started with a read-only root filesystem and no `directory` store
- **THEN** a run completes normally

### Requirement: Shutdown signal reaches the program
The container SHALL deliver the termination signal to the `butterbot` process, so that the graceful stop defined by the run-orchestration capability takes place when the container is stopped.

#### Scenario: Container stopped during a run
- **WHEN** the container is stopped while a certificate is being validated
- **THEN** the program removes its challenge records and exits with the interrupted exit code
