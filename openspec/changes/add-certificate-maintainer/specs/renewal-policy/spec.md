# Spec Delta

## Purpose

Defines how Butterbot decides, for each configured certificate, whether a new certificate has to be issued in the current run. The decision is made from what the stores hold, with no state kept between runs.

## ADDED Requirements

### Requirement: Decision from the stores
The system SHALL decide whether a certificate is due by reading what each of its stores currently holds. It SHALL NOT rely on any record of previous runs. A certificate that is not due SHALL cause no order at the ACME server and no change to DNS or to any store.

#### Scenario: Nothing due
- **WHEN** every store of every certificate holds a valid certificate with the configured names that is outside its renewal period
- **THEN** the run orders nothing, changes nothing and ends successfully

### Requirement: Missing certificate is due
A certificate SHALL be due when at least one of its stores holds no certificate under its name.

#### Scenario: New certificate entry
- **WHEN** an entry is added to the configuration and none of its stores holds a certificate under its name
- **THEN** the next run issues the certificate and writes it to all its stores

#### Scenario: New store on an existing entry
- **WHEN** a store is added to an existing entry and does not hold the certificate
- **THEN** the certificate is due

### Requirement: Changed names are due
A certificate SHALL be due when the set of DNS names of a stored certificate differs from the set of names configured for the entry.

#### Scenario: Name added
- **WHEN** an entry's names change from `example.com` to `example.com` and `www.example.com`
- **THEN** the next run issues a certificate covering both names, regardless of the stored certificate's expiry

### Requirement: Renewal period
A certificate SHALL be due when the remaining validity of a stored certificate is less than or equal to its resolved `renew_before`. An expired certificate SHALL be due.

#### Scenario: Inside the renewal period
- **WHEN** `renew_before` resolves to 30 days and the stored certificate expires in 29 days
- **THEN** the certificate is due

#### Scenario: Outside the renewal period
- **WHEN** `renew_before` resolves to 30 days and the stored certificate expires in 45 days
- **THEN** the certificate is not due on account of the renewal period

### Requirement: Earlier renewal at the CA's request
When `ari` is enabled on the ACME server (the default) and the server offers ACME Renewal Information, a certificate SHALL also be due once the renewal window the server suggests for the stored certificate has started. This SHALL only make a certificate due earlier than `renew_before`, never later.

#### Scenario: CA asks for early replacement
- **WHEN** the stored certificate expires in 50 days, `renew_before` is 30 days, and the server's suggested window has already started
- **THEN** the certificate is due

#### Scenario: Window not started
- **WHEN** the server's suggested window starts next week and the certificate expires in 40 days with `renew_before` of 30 days
- **THEN** the certificate is not due

#### Scenario: Renewal information switched off
- **WHEN** the ACME server has `ari = false`
- **THEN** the server is not asked for renewal information and only the other rules apply

### Requirement: Renewal information is best effort
If renewal information cannot be obtained for a stored certificate, for any reason, the system SHALL fall back to the other rules and SHALL NOT fail the certificate.

#### Scenario: Server does not know the certificate
- **WHEN** the server returns an error for the stored certificate's renewal information
- **THEN** the certificate is evaluated by the renewal period alone and is not reported as failed

### Requirement: Renewal period longer than the certificate
When the resolved `renew_before` is greater than or equal to the validity period of the stored certificate, the system SHALL use half of that validity period as the renewal period for that certificate and SHALL log an error naming the certificate and both durations. When `renew_before` is greater than half of the validity period but less than all of it, the system SHALL apply it as configured and SHALL log a warning.

#### Scenario: Setting exceeds lifetime
- **WHEN** `renew_before` is 60 days and the stored certificate is valid for 45 days in total
- **THEN** the certificate becomes due when 22.5 days remain, and each run logs an error about the setting

#### Scenario: Setting exceeds half the lifetime
- **WHEN** `renew_before` is 30 days and the stored certificate is valid for 45 days in total
- **THEN** the certificate becomes due when 30 days remain, and the run logs a warning

### Requirement: Reissue guard
When any store holds a valid certificate with the configured names that was issued less than `run.min_reissue` ago (default 2 days) and is not itself due, the system SHALL NOT issue in this run, even if other stores make the certificate due. It SHALL report the certificate as failed, stating when issuing becomes possible. The guard exists so that a store that keeps failing cannot cause a new certificate on every run.

#### Scenario: Second store holds nothing
- **WHEN** one store holds a matching certificate issued 1 day ago and a second store holds none
- **THEN** nothing is issued, and the certificate is reported as failed with the time at which the guard ends

#### Scenario: Second store is behind
- **WHEN** one store holds a matching certificate issued 1 day ago and a second store still holds the older certificate that is inside its renewal period
- **THEN** nothing is issued, and the certificate is reported as failed with the time at which the guard ends

#### Scenario: Guard elapsed
- **WHEN** one store holds a matching certificate issued 3 days ago and a second store holds none
- **THEN** a new certificate is issued and written to both stores

#### Scenario: CA asks for replacement of a fresh certificate
- **WHEN** the only stored certificate was issued 1 day ago and the CA's renewal information says it must be replaced now
- **THEN** the guard does not apply and a new certificate is issued

### Requirement: Forced issuance
A certificate selected with `--only` and `--force` SHALL be treated as due regardless of every other rule, including the reissue guard.

#### Scenario: Forced reissue
- **WHEN** `butterbot run --only wildcard-example-com --force` is run while the stored certificate is valid for another 80 days
- **THEN** a new certificate is issued and stored

### Requirement: Changing the ACME server is not a reason to issue
Changing the `acme` server of a certificate entry SHALL NOT by itself make the certificate due. The new server SHALL be used the next time the certificate is due.

#### Scenario: Server switched
- **WHEN** an entry's `acme` changes while its stored certificate is valid and outside the renewal period
- **THEN** nothing is issued, and the next renewal is ordered from the new server

### Requirement: Evaluation across several stores
When the stores of a certificate hold different certificates, the system SHALL apply the rules to each store's certificate, and the certificate SHALL be due if it is due for any of them.

#### Scenario: One store behind
- **WHEN** one store holds a certificate expiring in 60 days and another holds one expiring in 10 days, with `renew_before` of 30 days
- **THEN** the certificate is due, and after issuance both stores hold the new certificate
