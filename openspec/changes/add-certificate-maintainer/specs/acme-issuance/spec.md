# Spec Delta

## Purpose

Defines how Butterbot obtains a certificate from a configured ACME server: how servers and accounts are set up, what is requested, and what an issued certificate consists of.

## ADDED Requirements

### Requirement: Any ACME server by configuration
The system SHALL be able to use any ACME (RFC 8555) server that is defined by a directory URL. Adding a server SHALL require configuration only. A staging server SHALL be defined the same way as a production server.

#### Scenario: Production and staging side by side
- **WHEN** the configuration defines `acme.letsencrypt` and `acme.letsencrypt-staging` with their directory URLs
- **THEN** a certificate entry can use either by name

#### Scenario: Directory unreachable
- **WHEN** the directory URL of a certificate's ACME server cannot be fetched after the allowed retries
- **THEN** that certificate fails with a message naming the server, and certificates on other servers are unaffected

### Requirement: Account reuse
The system SHALL use the account credentials found at `acme.<name>.access_key` for every request to that server, and SHALL NOT register a new account when credentials are present.

#### Scenario: Existing account
- **WHEN** `acme.letsencrypt.access_key` holds account credentials
- **THEN** all certificates on that server are ordered with that account and no account is registered

### Requirement: Account registration on first use
When no account credentials exist for an ACME server and the Key Vault layer is enabled, the system SHALL register an account, save its credentials to the vault under the name derived from `acme.<name>.access_key`, and continue. When the Key Vault layer is not enabled, the system SHALL NOT register an account during `run`; each certificate on that server SHALL fail with a message pointing to `butterbot account new`.

#### Scenario: First run with a vault
- **WHEN** no credentials exist for `acme.letsencrypt` and `secrets.key_vault_url` is set
- **THEN** an account is registered, its credentials are saved as the vault secret `acme--letsencrypt--access-key`, and the run continues

#### Scenario: Second run with a vault
- **WHEN** a later run starts after such a registration
- **THEN** it reuses the saved account and registers nothing

#### Scenario: First run without a vault
- **WHEN** no credentials exist for `acme.letsencrypt` and no vault is configured
- **THEN** certificates on that server fail with a message that says to run `butterbot account new --acme letsencrypt`

#### Scenario: Credentials cannot be saved
- **WHEN** an account is registered but saving its credentials to the vault fails
- **THEN** the certificates on that server fail in this run and no certificate is ordered with the unsaved account

### Requirement: External account binding
When an ACME server instance has `eab_kid` and `eab_key` configured, the system SHALL use them to bind the account at registration.

#### Scenario: Server that requires binding
- **WHEN** an account is registered for a server instance with `eab_kid` and `eab_key` set
- **THEN** the registration request carries the external account binding

### Requirement: Order for the configured names
The system SHALL request one certificate covering exactly the DNS names of the certificate entry, including wildcard names, and SHALL prove control of every name through the DNS challenge.

#### Scenario: Wildcard and apex
- **WHEN** an entry lists `*.example.com` and `example.com`
- **THEN** the issued certificate covers exactly those two names

#### Scenario: Several unrelated names
- **WHEN** an entry lists `api.example.com` and `www.example.org`
- **THEN** one certificate covering both names is issued

### Requirement: Key type
The system SHALL generate the certificate key according to `key_type`, one of `rsa2048`, `rsa3072`, `rsa4096`, `ecdsa-p256` or `ecdsa-p384`. The built-in default SHALL be `rsa2048`.

#### Scenario: Default key type
- **WHEN** neither the certificate nor its ACME server sets `key_type`
- **THEN** the certificate has an RSA 2048-bit key

#### Scenario: Override on the certificate
- **WHEN** the certificate sets `key_type = "ecdsa-p256"`
- **THEN** the certificate has an ECDSA P-256 key

#### Scenario: Unknown key type
- **WHEN** `key_type` is set to a value outside the list
- **THEN** the configuration is rejected

### Requirement: Fresh private key per issuance
The system SHALL generate a new private key for every issued certificate. The private key SHALL exist only in memory until it is written to the certificate's stores, and SHALL NOT appear in logs.

#### Scenario: Renewal
- **WHEN** a certificate is renewed
- **THEN** the new certificate has a different private key from the one it replaces

### Requirement: Certificate profile
When `profile` is set, the system SHALL request that profile from the ACME server. When it is not set, the system SHALL NOT request any profile.

#### Scenario: Profile set
- **WHEN** a certificate resolves to `profile = "tlsserver"` and the server offers that profile
- **THEN** the order requests the `tlsserver` profile

#### Scenario: Profile not offered
- **WHEN** a certificate resolves to a profile the server does not offer
- **THEN** the certificate fails with a message naming the profile and the profiles the server offers

### Requirement: Renewal names its predecessor
When the ACME server supports ACME Renewal Information and a stored certificate from that server is being replaced, the system SHALL identify the certificate being replaced in the new order.

#### Scenario: Renewal on an ARI-capable server
- **WHEN** a stored certificate is renewed on a server that supports renewal information
- **THEN** the new order names the stored certificate as the one it replaces

#### Scenario: First issuance
- **WHEN** no certificate exists yet in any store
- **THEN** the order names no predecessor

### Requirement: Validation is attempted once per run
The system SHALL ask the ACME server to validate a certificate's challenges once per run. If validation fails, the certificate SHALL fail for this run and SHALL be attempted again by the next run. The failure message SHALL include the reason the server gave.

#### Scenario: Validation fails
- **WHEN** the server reports a challenge as invalid
- **THEN** the certificate fails, the log records the server's reason for each failed name, and no second order is created in this run

### Requirement: Bounded wait for the server's verdict
After asking the ACME server to validate or to issue, the system SHALL poll for the result every 5 seconds for at most 2 minutes. If no result is reached in that time, the certificate SHALL fail with a timeout.

#### Scenario: Verdict never arrives
- **WHEN** the order is still pending 2 minutes after validation was requested
- **THEN** the certificate fails with a timeout and its challenge records are removed

### Requirement: Issued certificate with chain
The system SHALL obtain the issued certificate together with the intermediate certificates supplied by the server, and SHALL hand the certificate, the chain and the private key to the stores as one unit.

#### Scenario: Successful issuance
- **WHEN** the server issues the certificate
- **THEN** each of the certificate's stores receives the leaf certificate, its chain and the private key
