# Spec Delta

## MODIFIED Requirements

### Requirement: Notifier extension point
The system SHALL deliver every event to each configured notifier instance (`notify.<name>`). A notifier SHALL be able to select the event kinds it receives. Notifier types are defined by their own capabilities; a `notify` instance whose `type` is not one of them SHALL be a configuration error.

#### Scenario: No notifier configured
- **WHEN** the configuration has no `notify` instance
- **THEN** events are written to the log only and the run is unaffected

#### Scenario: Notifier with a filter
- **WHEN** a notifier instance is configured to receive only `failure` and `expiring`
- **THEN** it receives those events and no others

#### Scenario: Unknown notifier type
- **WHEN** a `notify` instance declares `type = "carrier-pigeon"`
- **THEN** the configuration is rejected and the message names the instance and the notifier types that exist
