# Spec Delta

## Purpose

Defines how Butterbot proves control of a domain through DNS challenge records: publishing them with a DNS provider, confirming they are being served, and removing them afterwards.

## ADDED Requirements

### Requirement: DNS provider types
The system SHALL support DNS provider instances of type `azure-dns` and `cloudflare`. An `azure-dns` instance SHALL be configured with a subscription and a resource group and SHALL authenticate with the ambient Azure identity. A `cloudflare` instance SHALL authenticate with the API token at `dns.<name>.access_key`.

#### Scenario: Azure DNS instance
- **WHEN** a certificate uses a `dns` instance of type `azure-dns`
- **THEN** its challenge records are created in the matching zone of the configured subscription and resource group

#### Scenario: Cloudflare instance
- **WHEN** a certificate uses a `dns` instance of type `cloudflare`
- **THEN** its challenge records are created through the Cloudflare API with that instance's token

### Requirement: Zone resolution
For each challenge name, the system SHALL publish the record in the zone of the certificate's DNS provider instance whose name is the longest suffix of the challenge name. If no zone of that instance matches, the certificate SHALL fail.

#### Scenario: Sub-zone
- **WHEN** the instance hosts `example.com` and `dev.example.com`, and the name is `*.dev.example.com`
- **THEN** the record is created in the `dev.example.com` zone

#### Scenario: No matching zone
- **WHEN** the instance hosts no zone that is a suffix of the name
- **THEN** the certificate fails with a message naming the name and the DNS provider instance

### Requirement: All values for a name are published together
The system SHALL publish every challenge value required at a record name, so that a wildcard name and its base name, which share one record name, are both satisfied. The record SHALL be published with a time-to-live of 60 seconds unless `ttl` is set on the DNS provider.

#### Scenario: Wildcard and apex
- **WHEN** a certificate covers `*.example.com` and `example.com`
- **THEN** `_acme-challenge.example.com` serves two TXT values, one for each name

### Requirement: Only own values are removed
The system SHALL remove the challenge values it published, after validation succeeds, fails or is abandoned. It SHALL NOT remove or change values it did not publish in the current run.

#### Scenario: Cleanup after success
- **WHEN** a certificate has been issued
- **THEN** the values published for it are no longer served

#### Scenario: Cleanup after failure
- **WHEN** a certificate fails after its challenge values were published
- **THEN** those values are removed before the certificate is reported as failed

#### Scenario: Foreign value at the same name
- **WHEN** the record name already holds a value that this run did not publish
- **THEN** that value is still present after the run

### Requirement: Leftover values do not block validation
A challenge value left at a record name by an earlier, interrupted run SHALL NOT prevent a later run from validating the same name.

#### Scenario: Run after an interrupted run
- **WHEN** a previous run was killed after publishing a value and a new run processes the same certificate
- **THEN** the new run publishes its own values and validates successfully

### Requirement: Propagation check against authoritative nameservers
Before asking the ACME server to validate, the system SHALL query every authoritative nameserver of the zone directly and SHALL proceed only when each of them returns all the values it published. Additional values in the answers SHALL be ignored. The check SHALL repeat every `propagation_interval` (default 10 seconds) until it passes or `propagation_timeout` (default 5 minutes) has elapsed.

#### Scenario: All nameservers agree
- **WHEN** every authoritative nameserver returns all published values
- **THEN** the check passes

#### Scenario: One nameserver lags
- **WHEN** one authoritative nameserver does not yet return a published value
- **THEN** the check does not pass and is repeated

#### Scenario: Stale extra value
- **WHEN** the nameservers return the published values plus one unrelated value
- **THEN** the check passes

### Requirement: Propagation timeout fails without involving the CA
If the propagation check has not passed when `propagation_timeout` elapses, the certificate SHALL fail without the ACME server being asked to validate, and its published values SHALL be removed.

#### Scenario: Record never appears
- **WHEN** a nameserver still does not return a published value after the timeout
- **THEN** the certificate fails, no validation is requested from the ACME server, and the published values are removed

### Requirement: Settle delay
After the propagation check passes, the system SHALL wait for the `settle` duration (default 30 seconds) before asking the ACME server to validate.

#### Scenario: Default settle
- **WHEN** the propagation check passes and `settle` is not configured
- **THEN** validation is requested 30 seconds later

### Requirement: Delegated challenge names
When a challenge name is a CNAME to a name in another zone, the system SHALL run the propagation check against the authoritative nameservers of the zone the CNAME points into.

#### Scenario: CNAME delegation
- **WHEN** `_acme-challenge.example.com` is a CNAME to `example-com.acme.example.net`
- **THEN** the propagation check queries the authoritative nameservers of the zone containing `example-com.acme.example.net`

### Requirement: Propagation check mode
The system SHALL support three values of `propagation_check` on a DNS provider: `authoritative` (the default, described above), `resolver` (the same comparison against the recursive resolvers listed in `propagation_resolvers`, by default two public resolvers) and `off` (no check; the system waits for the `settle` duration only).

#### Scenario: Resolver mode
- **WHEN** `propagation_check = "resolver"` and every configured resolver returns all published values
- **THEN** the check passes without any authoritative nameserver being queried

#### Scenario: Check switched off
- **WHEN** `propagation_check = "off"` and `settle = "60s"`
- **THEN** validation is requested 60 seconds after the records were published, with no DNS queries from the program

### Requirement: Validation diagnostics
For every certificate that reaches the propagation check, the system SHALL record in the log how long propagation took and, on failure, which nameservers were missing which values.

#### Scenario: Slow propagation
- **WHEN** the check passes after 90 seconds
- **THEN** the log records a propagation time of 90 seconds for that certificate

#### Scenario: Failed propagation
- **WHEN** the check times out
- **THEN** the log names each nameserver that did not return the published values
