# Spec Delta

## Purpose

Defines the destinations certificates are delivered to: how Butterbot reads what a store currently holds and how it writes a newly issued certificate, in the configured format.

## ADDED Requirements

### Requirement: Store types
The system SHALL support store instances of type `azure-key-vault` and `directory`. An `azure-key-vault` instance SHALL be configured with the vault URL and SHALL authenticate with the ambient Azure identity. A `directory` instance SHALL be configured with a filesystem path.

#### Scenario: Key Vault store
- **WHEN** a certificate lists a store of type `azure-key-vault`
- **THEN** the issued certificate is imported into that vault under the certificate's name

#### Scenario: Directory store
- **WHEN** a certificate lists a store of type `directory`
- **THEN** the issued certificate is written under `<path>/<certificate name>/`

### Requirement: Several stores per certificate
The system SHALL deliver an issued certificate to every store the certificate entry lists. All stores of a certificate SHALL receive the same certificate, chain and private key.

#### Scenario: Two stores
- **WHEN** a certificate lists two stores and is issued
- **THEN** both stores hold the same certificate afterwards

### Requirement: Reading the current certificate
The system SHALL be able to read, from each store, the certificate currently held under a certificate name: its DNS names, validity period, issuer and serial number. A store that holds nothing under that name SHALL be reported as holding no certificate, which is not an error.

#### Scenario: Certificate present
- **WHEN** a store holds a certificate under the name
- **THEN** its names and expiry are available for the renewal decision

#### Scenario: Nothing stored
- **WHEN** a store holds nothing under the name
- **THEN** the store is reported as holding no certificate and the run continues

#### Scenario: Store cannot be read
- **WHEN** a store cannot be reached or returns content that cannot be read as a certificate
- **THEN** the certificate fails for this run and nothing is issued for it

### Requirement: Adoption of existing certificates
The system SHALL treat a certificate that already exists in a store under the configured name the same as one it issued itself, regardless of which program put it there.

#### Scenario: Certificate from the previous tool
- **WHEN** a store holds a valid certificate under the configured name with the configured DNS names, placed there by another tool
- **THEN** the certificate is not reissued until it becomes due

### Requirement: Stored format
The system SHALL write certificates in the store's `format`: `pfx` (certificate, chain and private key in one PKCS#12 bundle) or `pem` (the same content as PEM text). The default SHALL be `pfx` for `azure-key-vault` and `pem` for `directory`. A certificate entry MAY override the format.

#### Scenario: Key Vault default
- **WHEN** a certificate is written to a Key Vault store with no `format` set
- **THEN** it is imported as a PKCS#12 bundle

#### Scenario: Override on the certificate
- **WHEN** a certificate entry sets `format = "pem"` and lists a Key Vault store
- **THEN** it is imported into the vault as PEM

### Requirement: Private key is retrievable from Key Vault
A certificate imported into an `azure-key-vault` store SHALL be stored so that an authorised consumer can retrieve the certificate together with its private key.

#### Scenario: Consumer reads the certificate
- **WHEN** an authorised consumer reads the stored certificate's secret from the vault
- **THEN** it receives the certificate, the chain and the private key in the stored format

### Requirement: Directory layout
A `directory` store SHALL write, under `<path>/<certificate name>/`, the files `fullchain.pem` and `privkey.pem` for the `pem` format, or `certificate.pfx` for the `pfx` format. The private key file SHALL be readable only by its owner. Files SHALL be replaced atomically, so that a reader never sees a partly written file.

#### Scenario: PEM files
- **WHEN** a certificate named `wildcard-example-com` is written in `pem` format to a store with path `/certs`
- **THEN** `/certs/wildcard-example-com/fullchain.pem` and `/certs/wildcard-example-com/privkey.pem` exist and the key file is readable only by its owner

#### Scenario: Reader during a write
- **WHEN** a process reads the certificate files while a renewal is being written
- **THEN** it sees either the complete old files or the complete new files

### Requirement: Store writes are retried
When writing an issued certificate to a store fails with a transient error, the system SHALL retry that write up to 3 times in total, without contacting the ACME server again.

#### Scenario: Transient failure
- **WHEN** the first write to a store fails with a transient error and the second succeeds
- **THEN** the certificate is stored and no new certificate is ordered

### Requirement: No destructive actions
The system SHALL NOT delete a certificate from a store, and SHALL NOT revoke a certificate.

#### Scenario: Entry removed from configuration
- **WHEN** a certificate entry is removed from the configuration
- **THEN** the certificates stored under its name are left untouched and are no longer renewed
