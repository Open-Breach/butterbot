# Spec Delta

## Purpose

Defines how a Butterbot run processes its certificates from start to exit: isolation between certificates, rollback, parallelism, retries, timeouts, shutdown and the overall outcome of the run.

## ADDED Requirements

### Requirement: Run once and exit
A run SHALL evaluate every configured certificate, or the certificates selected with `--only`, issue those that are due, and then exit. The program SHALL NOT keep running or schedule itself.

#### Scenario: Nothing to do
- **WHEN** no certificate is due
- **THEN** the run ends successfully without ordering anything

#### Scenario: Selection
- **WHEN** the run is started with `--only wildcard-example-com`
- **THEN** only that certificate is evaluated

### Requirement: Runs are repeatable and restartable
A run SHALL be safe to repeat at any time and safe to kill at any point. A run that was killed SHALL NOT leave anything behind that makes a later run fail or issue a certificate that is not due.

#### Scenario: Run twice in a row
- **WHEN** a successful run is immediately followed by a second run
- **THEN** the second run issues nothing

#### Scenario: Killed run
- **WHEN** a run is killed while a certificate is waiting for validation and a new run starts
- **THEN** the new run processes that certificate from the beginning and can complete it

### Requirement: Failures are isolated per certificate
The failure of one certificate SHALL NOT prevent the other certificates from being processed.

#### Scenario: One certificate fails
- **WHEN** the configuration has three due certificates and the second fails validation
- **THEN** the first and third are issued and stored, and the run reports one failure

### Requirement: Rollback in reverse order
Every step that changes something outside the program SHALL register how to undo it. When a certificate fails, the system SHALL run the registered undo actions for that certificate in the reverse of the order in which they were registered.

#### Scenario: Failure after publishing challenge records
- **WHEN** a certificate fails after its challenge records were published
- **THEN** the records are removed before the certificate is reported as failed

#### Scenario: Undo action fails
- **WHEN** an undo action itself fails
- **THEN** the failure is logged, the remaining undo actions still run, and the certificate is reported with its original failure

### Requirement: Bounded parallelism
The system SHALL process up to `run.parallel` certificates at the same time (default 4). It SHALL NOT process at the same time two certificates whose challenge records are in the same DNS zone.

#### Scenario: Different zones
- **WHEN** four due certificates are in four different zones and `run.parallel` is 4
- **THEN** they are processed concurrently

#### Scenario: Same zone
- **WHEN** two due certificates have challenge records in the same zone
- **THEN** the second starts its DNS step only after the first has removed its challenge records

### Requirement: Transient errors are retried
An operation that fails with a transient error (a network error, a server error, or a temporary failure of a DNS or store service) SHALL be attempted up to 3 times in total, with an increasing pause between attempts. A failed CA validation, a permission error and a rejected name SHALL NOT be retried.

#### Scenario: Temporary network failure
- **WHEN** a request fails with a network error and succeeds on the second attempt
- **THEN** the certificate continues normally

#### Scenario: Permission denied
- **WHEN** a DNS provider answers that the credential is not allowed to change the zone
- **THEN** the certificate fails at once, without a retry

### Requirement: Rate limits are obeyed
When an ACME server answers with a rate-limit error, the system SHALL wait for the time the server states and retry once, provided that time is at most `run.retry_wait_max` (default 2 minutes). Otherwise, or if the retry is also rate limited, the certificate SHALL fail for this run and SHALL NOT be attempted again in this run.

#### Scenario: Short wait
- **WHEN** the server answers with a rate-limit error and a wait of 30 seconds
- **THEN** the request is repeated after 30 seconds

#### Scenario: Long wait
- **WHEN** the server answers with a rate-limit error and a wait of 3 hours
- **THEN** the certificate fails, the log records when the limit ends, and the next run tries again

### Requirement: Every wait has a timeout
Every step that waits for something outside the program SHALL have a time limit. When the limit is reached, the certificate SHALL fail and its undo actions SHALL run.

#### Scenario: Service hangs
- **WHEN** a store or DNS service does not answer
- **THEN** the certificate fails after the step's time limit instead of blocking the run

### Requirement: A certificate is complete only when every store has it
A certificate SHALL count as renewed only when it has been written to all its stores. If writing to any store finally fails, the certificate SHALL be reported as failed. Stores that were already written SHALL keep the new certificate.

#### Scenario: Second store fails
- **WHEN** a certificate is issued, written to its first store, and the second store fails after all retries
- **THEN** the certificate is reported as failed and the first store holds the new certificate

### Requirement: Graceful stop
On an interrupt or termination signal the system SHALL start no further certificates, SHALL stop certificates in progress at their next safe point and run their undo actions, and SHALL then exit with the interrupted outcome. A certificate that has already been issued SHALL first be written to its stores. A second signal SHALL make the program exit immediately.

#### Scenario: Signal during validation
- **WHEN** the program receives a termination signal while a certificate waits for propagation
- **THEN** that certificate's challenge records are removed and the program exits with the interrupted outcome

#### Scenario: Signal after issuance
- **WHEN** the program receives a termination signal after a certificate was issued but before it was stored
- **THEN** the certificate is written to its stores before the program exits

#### Scenario: Second signal
- **WHEN** a second signal arrives while undo actions are running
- **THEN** the program exits immediately

### Requirement: Run outcome
A run SHALL end with exactly one outcome: `success` when every evaluated certificate was either not due or renewed; `failed` when at least one certificate failed; `interrupted` when the run was stopped by a signal. The outcome SHALL determine the exit code.

#### Scenario: Mixed results
- **WHEN** two certificates are renewed and one fails
- **THEN** the outcome is `failed`

#### Scenario: All not due
- **WHEN** no certificate is due
- **THEN** the outcome is `success`

### Requirement: Run summary
At the end of a run the system SHALL report how many certificates were evaluated, renewed and failed, and which configured certificate expires soonest and when.

#### Scenario: Summary after a run
- **WHEN** a run evaluates 15 certificates, renews 2 and fails none
- **THEN** the summary states 15 evaluated, 2 renewed, 0 failed, and the name and expiry of the certificate that expires soonest
