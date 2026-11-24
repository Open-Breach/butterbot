# Spec Delta

## ADDED Requirements

### Requirement: The notify test command
`butterbot notify test <name>` SHALL send one sample event through the named notifier instance and report whether it was delivered. The sample SHALL carry the certificate name `butterbot-test`, and the reason of a `failure` sample SHALL say that it is a test. `--event <kind>` SHALL choose the kind of the sample, `failure` by default. The command SHALL process no certificate. It SHALL exit with 0 when the sample was delivered, 1 when it was not, and 2 for a configuration or usage error.

#### Scenario: Working notifier
- **WHEN** `butterbot notify test teams` is run and the endpoint accepts the request
- **THEN** the endpoint receives one `failure` event that is recognisable as a test, the program says it was delivered, and the exit code is 0

#### Scenario: Endpoint rejects the request
- **WHEN** the endpoint answers with status 400
- **THEN** the program reports the status without showing the URL and the exit code is 1

#### Scenario: Unknown notifier
- **WHEN** the named notifier is not configured
- **THEN** the program refuses with a usage error naming the notifier and the exit code is 2

#### Scenario: Another event kind
- **WHEN** `butterbot notify test teams --event run_completed` is run
- **THEN** the sample sent is a `run_completed` event
