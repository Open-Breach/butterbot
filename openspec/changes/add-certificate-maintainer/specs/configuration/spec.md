# Spec Delta

## Purpose

Defines where Butterbot's configuration comes from, how provider instances and certificates are declared, how secrets are named and resolved, and what makes a configuration valid.

## ADDED Requirements

### Requirement: Configuration file
The system SHALL read its configuration from one or more TOML files. The default path SHALL be `/etc/butterbot/config.toml`. The path SHALL be replaceable with the `--config` option or the `BUTTERBOT_CONFIG` environment variable. `--config` MAY be given several times, and files SHALL be merged in the order given, with later files overriding earlier ones.

#### Scenario: Default path
- **WHEN** the program starts with no `--config` option and no `BUTTERBOT_CONFIG` variable
- **THEN** it reads `/etc/butterbot/config.toml`

#### Scenario: Several files
- **WHEN** the program starts with `--config a.toml --config b.toml` and both files set the same key
- **THEN** the value from `b.toml` is used

#### Scenario: File missing
- **WHEN** a configuration file named on the command line does not exist
- **THEN** the program reports which file is missing and exits with the configuration-error exit code

### Requirement: Layered sources
The system SHALL build its configuration from layers, each overriding the one before: built-in defaults, configuration files, environment variables, and Azure Key Vault. A key MAY be supplied by any layer.

#### Scenario: Environment overrides file
- **WHEN** a key is set in a configuration file and also in the environment
- **THEN** the value from the environment is used

#### Scenario: Key Vault overrides environment
- **WHEN** the Key Vault layer is enabled and a key is set both in the environment and in the vault
- **THEN** the value from the vault is used

### Requirement: Key Vault layer is optional
The system SHALL read configuration from Azure Key Vault only when `secrets.key_vault_url` is set by an earlier layer. When it is not set, the system SHALL NOT contact any Key Vault for configuration.

#### Scenario: No vault configured
- **WHEN** `secrets.key_vault_url` is not set
- **THEN** the program loads its configuration without contacting a Key Vault

#### Scenario: Vault unreachable
- **WHEN** `secrets.key_vault_url` is set and the vault cannot be read
- **THEN** the program reports the error and exits with the configuration-error exit code without processing any certificate

### Requirement: Provider instances
The system SHALL accept named provider instances in three tables: `acme.<name>` for ACME servers, `dns.<name>` for DNS providers and `store.<name>` for certificate stores. Several instances of the same provider type SHALL be allowed. An instance of a DNS provider or store SHALL declare its `type`.

#### Scenario: Two instances of one type
- **WHEN** the configuration defines `dns.cloudflare` and `dns.cloudflare-eu`, both of type `cloudflare`
- **THEN** both are available and each certificate uses the one it names

#### Scenario: Unknown type
- **WHEN** a DNS provider or store instance declares a `type` the program does not know
- **THEN** the configuration is rejected and the message names the instance and the unknown type

### Requirement: Instance and certificate names
Provider instance names and certificate names SHALL consist of lowercase letters, digits and single dashes, and SHALL start and end with a letter or digit. Names SHALL be unique within their kind.

#### Scenario: Invalid name
- **WHEN** an instance is named `Cloud_Flare`
- **THEN** the configuration is rejected and the message names the offending instance

#### Scenario: Duplicate certificate name
- **WHEN** two certificate entries have the same `name`
- **THEN** the configuration is rejected

### Requirement: Certificate entries
The system SHALL accept a list of certificate entries. Each entry SHALL have a `name`, a non-empty list of DNS `names`, one `acme` server, one `dns` provider and a non-empty list of `stores`. Every referenced instance SHALL exist.

#### Scenario: Valid entry
- **WHEN** an entry names an existing ACME server, an existing DNS provider and two existing stores
- **THEN** the entry is accepted

#### Scenario: Dangling reference
- **WHEN** an entry references a store that is not defined
- **THEN** the configuration is rejected and the message names the certificate and the missing store

### Requirement: Defaults and overrides
Settings that exist both on a provider and on a certificate SHALL be resolved in this order: the certificate entry, then the provider instance, then the built-in default. This applies to `renew_before`, `key_type` and `profile` (ACME server), `format` (store), and `propagation_timeout` and `settle` (DNS provider).

#### Scenario: Certificate overrides provider
- **WHEN** an ACME server sets `renew_before = "30d"` and a certificate using it sets `renew_before = "20d"`
- **THEN** 20 days is used for that certificate

#### Scenario: Built-in default
- **WHEN** neither the certificate nor its ACME server sets `renew_before`
- **THEN** 30 days is used

### Requirement: Secrets are identified by configuration path
A secret SHALL be an ordinary configuration key, identified by its full path. The conventional key for a provider's credential SHALL be `access_key`: the account credentials for an ACME server, the API token for a DNS provider. A provider that authenticates through the ambient Azure identity SHALL need no `access_key`.

#### Scenario: Two instances, two secrets
- **WHEN** the configuration defines `dns.cloudflare` and `dns.cloudflare-eu`
- **THEN** their credentials are the distinct keys `dns.cloudflare.access_key` and `dns.cloudflare-eu.access_key`

#### Scenario: Secret supplied outside the file
- **WHEN** `dns.cloudflare.access_key` is absent from every configuration file but present in the environment or the vault
- **THEN** the provider is configured with that value

### Requirement: Source names for a configuration path
Each layer SHALL derive its name for a configuration path by a fixed rule. In the environment, the name is `BUTTERBOT__` followed by the path segments in upper case joined by `__`, with dashes replaced by underscores. In Key Vault, the name is the path segments joined by `--`, with underscores replaced by dashes.

#### Scenario: Environment name
- **WHEN** the path is `acme.letsencrypt-staging.access_key`
- **THEN** the environment variable is `BUTTERBOT__ACME__LETSENCRYPT_STAGING__ACCESS_KEY`

#### Scenario: Key Vault name
- **WHEN** the path is `dns.cloudflare.access_key`
- **THEN** the Key Vault secret is `dns--cloudflare--access-key`

### Requirement: Secret values are never disclosed
The system SHALL NOT write secret values to logs, to error messages or to the output of `status` and `check-config`.

#### Scenario: Configuration dump
- **WHEN** `check-config` lists the secrets it expects
- **THEN** it shows each secret's path, its name in each source and whether it was found, and never its value

### Requirement: Durations
Duration settings SHALL be written as a whole number followed by a unit: `s` for seconds, `m` for minutes, `h` for hours, `d` for days.

#### Scenario: Valid duration
- **WHEN** a setting is `"30d"` or `"5m"`
- **THEN** it is read as 30 days or 5 minutes

#### Scenario: Invalid duration
- **WHEN** a setting is `"thirty days"`
- **THEN** the configuration is rejected and the message names the key

### Requirement: Strict validation before work
The system SHALL validate the whole configuration before processing any certificate. Unknown keys, wrong value types, invalid names, dangling references and unknown provider types SHALL be configuration errors. On a configuration error the program SHALL report every error it found and exit with the configuration-error exit code.

#### Scenario: Typo in a key
- **WHEN** a certificate entry contains `renew_befor = "20d"`
- **THEN** the configuration is rejected and the message names the unknown key and the certificate

#### Scenario: Several errors
- **WHEN** the configuration has two independent errors
- **THEN** both are reported in one run

### Requirement: Missing secrets fail only what depends on them
A secret that is required by a provider but supplied by no layer SHALL NOT be a configuration error for `run`. Each certificate that depends on that provider SHALL fail with a message naming the missing configuration path, and certificates that do not depend on it SHALL be processed normally.

#### Scenario: One provider without credential
- **WHEN** `dns.cloudflare.access_key` is missing and the configuration has certificates on Cloudflare and on Azure DNS
- **THEN** the Cloudflare certificates fail with a message naming `dns.cloudflare.access_key` and the Azure DNS certificates are processed
