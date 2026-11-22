# Spec Delta

## Purpose

Defines the events a Butterbot run emits about its outcome and about individual certificates, what each event contains, and how events are delivered, so that other systems can react to them or notice their absence.

## ADDED Requirements

### Requirement: Event model
The system SHALL emit four kinds of event: `run_completed`, `failure`, `renewed` and `expiring`. Every event SHALL carry its kind in an `event` field and a UTC timestamp. Field names SHALL stay stable across versions.

#### Scenario: Event shape
- **WHEN** any event is emitted
- **THEN** it has an `event` field naming its kind and a timestamp in ISO 8601 UTC format

### Requirement: Run completed on every finished run
The system SHALL emit exactly one `run_completed` event at the end of every run that reaches its end, including runs in which nothing was due and runs in which certificates failed. The event SHALL state the run outcome, the number of certificates checked, renewed and failed, the run's duration, and the name and expiry of the configured certificate that expires soonest.

#### Scenario: Nothing was due
- **WHEN** a run finds no certificate due
- **THEN** a `run_completed` event is emitted with `renewed` 0 and `failed` 0

#### Scenario: Run with failures
- **WHEN** a run ends with two failed certificates
- **THEN** a `run_completed` event is emitted with `failed` 2

### Requirement: No completion event for an unfinished run
The system SHALL NOT emit `run_completed` when a run is interrupted by a signal or ends because of a configuration error. The absence of the event SHALL therefore mean that no run finished.

#### Scenario: Interrupted run
- **WHEN** a run is stopped by a termination signal
- **THEN** no `run_completed` event is emitted

#### Scenario: Invalid configuration
- **WHEN** the program exits with a configuration error
- **THEN** no `run_completed` event is emitted

### Requirement: Failure event
The system SHALL emit one `failure` event for every certificate that fails in a run. The event SHALL name the certificate, the step that failed, the reason, and the expiry of the certificate currently stored, if any.

#### Scenario: Validation failure
- **WHEN** a certificate fails because the ACME server rejected a challenge
- **THEN** a `failure` event names the certificate, the step `validate` and the server's reason

### Requirement: Renewed event
The system SHALL emit one `renewed` event for every certificate that was issued and written to all its stores. The event SHALL name the certificate, its DNS names, its new expiry, the ACME server that issued it and the stores it was written to.

#### Scenario: Successful renewal
- **WHEN** a certificate is issued and stored in both of its stores
- **THEN** a `renewed` event names the certificate, both stores and the new expiry

### Requirement: Expiring event
The system SHALL emit one `expiring` event for every certificate that was due, was not renewed in this run, and whose stored certificate has less than half of its renewal period left or has expired. The event SHALL name the certificate and state its expiry.

#### Scenario: Renewal keeps failing
- **WHEN** a certificate with a renewal period of 30 days has failed to renew and its stored certificate expires in 12 days
- **THEN** the run emits an `expiring` event for it in addition to the `failure` event

#### Scenario: Early in the renewal period
- **WHEN** a certificate with a renewal period of 30 days fails to renew and its stored certificate expires in 25 days
- **THEN** only a `failure` event is emitted for it

### Requirement: Events are written to the log
The system SHALL write every event to the log output as a structured record that contains all the event's fields.

#### Scenario: JSON log
- **WHEN** the log format is JSON and a run completes
- **THEN** the log contains a line whose `event` field is `run_completed` together with the event's other fields

### Requirement: Notifier extension point
The system SHALL deliver every event to each configured notifier instance (`notify.<name>`). A notifier SHALL be able to select the event kinds it receives. This version SHALL ship no notifier type; the extension point exists so that a notification channel can be added without changing how events are produced.

#### Scenario: No notifier configured
- **WHEN** the configuration has no `notify` instance
- **THEN** events are written to the log only and the run is unaffected

#### Scenario: Notifier with a filter
- **WHEN** a notifier instance is configured to receive only `failure` and `expiring`
- **THEN** it receives those events and no others

### Requirement: Delivery failures never change the run
A failure to deliver an event to a notifier SHALL be logged and SHALL NOT change the outcome or the exit code of the run.

#### Scenario: Notifier unreachable
- **WHEN** a notifier cannot deliver the `run_completed` event of a successful run
- **THEN** the delivery failure is logged and the exit code is still 0
