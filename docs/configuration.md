# Configuration

Butterbot is configured by a TOML file. Single values, and secrets in particular, can also come from environment variables or from Azure Key Vault. This document lists every setting and explains how the sources are combined.

`butterbot check-config` validates a configuration and lists the secrets it expects, with the name each source uses for them.

## Sources and their order

The configuration is built from four layers. Each layer overrides the one before it, key by key:

1. Built-in defaults.
2. Configuration files.
3. Environment variables.
4. Azure Key Vault, when it is enabled.

Any key can come from any layer. A value in the environment replaces the same key from a file, and a value in the vault replaces both.

### Files

Without further instruction Butterbot reads `/etc/butterbot/config.toml`.

- `--config <path>` names another file. The option can be given several times. The files are merged in the order given, and a later file overrides an earlier one.
- `BUTTERBOT_CONFIG` names another file when no `--config` option is given.

A file that is named but does not exist is an error. So is a missing default file.

When files are merged, tables are merged key by key. A list is replaced as a whole. The certificate entries are a list: if a later file contains any `[[certificate]]` entry, its entries replace all entries of the earlier files.

### Environment variables

The variable for a configuration path is `BUTTERBOT__` followed by the segments of the path in upper case, joined by two underscores. A dash in an instance name becomes an underscore.

| Configuration path | Environment variable |
|---|---|
| `acme.letsencrypt-staging.access_key` | `BUTTERBOT__ACME__LETSENCRYPT_STAGING__ACCESS_KEY` |
| `dns.cloudflare.access_key` | `BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY` |
| `run.retry_wait_max` | `BUTTERBOT__RUN__RETRY_WAIT_MAX` |
| `secrets.key_vault_url` | `BUTTERBOT__SECRETS__KEY_VAULT_URL` |

Read in the other direction: in the tables `acme`, `dns`, `store` and `notify` the second segment is an instance name, and an underscore in it stands for a dash. Instance names never contain underscores, so the mapping is unambiguous.

Limits of the environment:

- Certificate entries cannot be set from the environment. They are a list and have no path.
- Settings that are lists (`propagation_resolvers`, and the `events` of a notifier) cannot be set from the environment.
- The `headers` of a notifier are set in a file. A header name with a dash cannot be written as the name of an environment variable.
- A variable that starts with `BUTTERBOT__` but names no configuration key is an error. This catches typing mistakes.

### Azure Key Vault

Key Vault is read only when `secrets.key_vault_url` is set by a file or by the environment. Without it, Butterbot contacts no vault for configuration.

The secret for a configuration path is the segments of the path joined by two dashes. An underscore in a setting name becomes a dash.

| Configuration path | Key Vault secret |
|---|---|
| `dns.cloudflare.access_key` | `dns--cloudflare--access-key` |
| `acme.letsencrypt-staging.access_key` | `acme--letsencrypt-staging--access-key` |
| `run.retry_wait_max` | `run--retry-wait-max` |

Butterbot lists the names of the secrets in the vault, which reads no values. It then fetches only the secrets whose name starts with `run--`, `secrets--`, `acme--`, `dns--`, `store--` or `notify--`. Other secrets in the same vault are never fetched. Disabled secrets are ignored.

A secret that carries one of these prefixes is treated as configuration. If it names a key that does not exist, the configuration is rejected, as it would be for the same mistake in a file.

If the vault is configured but cannot be read, the program stops with a configuration error and processes no certificate.

Butterbot signs in to Azure with the identity it runs as: the workload identity of its pod when the cluster provides one, and otherwise the account signed in to the Azure CLI. There is nothing to configure for this.

## Secrets

A secret is an ordinary configuration key. It is identified by its path, so it can come from any layer and does not have to be named anywhere else. The credential of a provider is always the key `access_key`:

- `acme.<name>.access_key` holds the account credentials for an ACME server.
- `dns.<name>.access_key` holds the API token of a Cloudflare DNS provider.
- `notify.<name>.access_key` holds the key a webhook notifier sends to its endpoint, when it sends one.

A webhook notifier has a second secret: `notify.<name>.url`, the URL its requests go to. A webhook URL carries its own credential, so Butterbot treats the whole URL as a secret.

Two instances of the same provider type have two different secrets, for example `dns.cloudflare.access_key` and `dns.cloudflare-eu.access_key`.

Providers that sign in with the Azure identity (`azure-dns`, `azure-key-vault`) have no `access_key`.

Further rules:

- Keep secrets out of the configuration file. Supply them through the environment or the vault.
- A secret with an empty value counts as not supplied.
- A missing secret does not stop a run. The certificates that need it fail with a message that names the missing path, and all other certificates are processed. A notifier whose secret is missing is reported once and receives no events.
- Secret values are never written to the log, to error messages or to the output of any command.

## Value formats

**Durations** are a whole number followed by a unit: `s` for seconds, `m` for minutes, `h` for hours, `d` for days. Examples: `"30d"`, `"5m"`, `"90s"`. Combinations such as `"1h30m"` are not accepted.

**Names** of provider instances and certificates consist of lowercase letters, digits and single dashes. They start and end with a letter or digit. Certificate names are unique. Instance names are unique within their table.

**Numbers and switches from the environment or the vault.** Values from these sources are always text. A whole number and `true` or `false` may therefore be written as text. No other conversion takes place: a number where text is expected is an error.

## Settings

### `[run]`

| Key | Default | Meaning |
|---|---|---|
| `parallel` | `4` | How many certificates are processed at the same time. At least 1. |
| `min_reissue` | `"2d"` | A certificate issued less than this long ago is not issued again because another of its stores lacks it. |
| `retry_wait_max` | `"2m"` | The longest wait an ACME server may ask for in a rate-limit answer before the certificate is given up for this run. |

### `[secrets]`

| Key | Default | Meaning |
|---|---|---|
| `key_vault_url` | not set | URL of the Azure Key Vault to read as the highest configuration layer. |

### `[acme.<name>]`: ACME servers

An ACME server is where certificates come from. Adding one needs configuration only. A staging server is defined the same way as a production server.

| Key | Default | Meaning |
|---|---|---|
| `directory_url` | required | The directory URL of the ACME server. |
| `access_key` | not set | Secret. The account credentials. Create them with `butterbot account new --acme <name>`. |
| `eab_kid` | not set | Key identifier for external account binding, for servers that require it. |
| `eab_key` | not set | Secret. The key for external account binding. |
| `profile` | not set | The certificate profile to request. Without it no profile is requested. Can be overridden per certificate. |
| `renew_before` | `"30d"` | How long before expiry a certificate is renewed. Can be overridden per certificate. |
| `key_type` | `"rsa2048"` | The key type: `rsa2048`, `rsa3072`, `rsa4096`, `ecdsa-p256` or `ecdsa-p384`. Can be overridden per certificate. |
| `ari` | `true` | Whether the server is asked for renewal information, so that a certificate is renewed earlier when the server suggests it. |

### `[dns.<name>]`: DNS providers

A DNS provider is where the validation records are published. Every instance has a `type`.

Settings for every type:

| Key | Default | Meaning |
|---|---|---|
| `type` | required | `azure-dns` or `cloudflare`. |
| `ttl` | `"60s"` | Time-to-live of the challenge records. At least `"1s"`. |
| `propagation_check` | `"authoritative"` | How Butterbot checks that the records are served before it asks the ACME server to validate: `authoritative` asks every authoritative nameserver of the zone, `resolver` asks the resolvers in `propagation_resolvers`, `off` does not check. |
| `propagation_resolvers` | `["1.1.1.1", "8.8.8.8"]` | The resolvers asked in `resolver` mode. Each is an IP address, optionally with a port. |
| `propagation_interval` | `"10s"` | The pause between two propagation checks. At least `"1s"`. |
| `propagation_timeout` | `"5m"` | How long to wait for the records to be served. Can be overridden per certificate. |
| `settle` | `"30s"` | The wait after a successful propagation check before the ACME server is asked to validate. Can be overridden per certificate. |

Settings for `type = "azure-dns"`:

| Key | Default | Meaning |
|---|---|---|
| `subscription_id` | required | The subscription that holds the DNS zones. |
| `resource_group` | required | The resource group that holds the DNS zones. |

Settings for `type = "cloudflare"`:

| Key | Default | Meaning |
|---|---|---|
| `access_key` | not set | Secret. The Cloudflare API token. |

### `[store.<name>]`: certificate stores

A store is where an issued certificate is delivered. Every instance has a `type`.

| Key | Default | Meaning |
|---|---|---|
| `type` | required | `azure-key-vault` or `directory`. |
| `format` | by type | `pfx` (certificate, chain and key in one PKCS#12 bundle) or `pem`. The default is `pfx` for `azure-key-vault` and `pem` for `directory`. Can be overridden per certificate. |

Settings for `type = "azure-key-vault"`:

| Key | Default | Meaning |
|---|---|---|
| `vault_url` | required | URL of the vault that receives the certificates. |

Settings for `type = "directory"`:

| Key | Default | Meaning |
|---|---|---|
| `path` | required | The directory. A certificate is written to `<path>/<certificate name>/`. |

### `[notify.<name>]`: notifiers

A notifier receives the events of a run; [Events](events.md) describes them. There is one notifier type, `webhook`, which sends each event as one HTTP request. [Notifications](notifications.md) has complete examples, Microsoft Teams among them.

| Key | Default | Meaning |
|---|---|---|
| `type` | required | `webhook`. |
| `events` | every kind | The kinds of event the notifier receives, as a list: any of `run_completed`, `failure`, `renewed` and `expiring`. |

Settings for `type = "webhook"`:

| Key | Default | Meaning |
|---|---|---|
| `url` | secret | Where the request goes. It starts with `https://` or `http://` and may contain placeholders. With `http://` the request is not encrypted, and Butterbot logs a warning. |
| `method` | `"POST"` | `POST` or `GET`. A `GET` request has no body. |
| `format` | `"json"` | The built-in body: `json` is the event as a JSON object, `teams` is a message for a Microsoft Teams workflow. Cannot be set together with `body`. |
| `body` | none | A body of your own, as text with placeholders. Cannot be set together with `format`. |
| `content_type` | `"application/json"` | The content type of `body`. It has no effect without `body`. |
| `headers` | none | A table of headers that are sent with every request. A value may contain placeholders. |
| `access_key` | secret | The value of the placeholder `{{access_key}}`. Required when the URL, a header or the body uses the placeholder, and not otherwise. |
| `ca_file` | none | A PEM file with certificates to trust for the endpoint, in addition to the public certificate authorities. For an endpoint with a certificate from a private authority. |
| `timeout` | `"10s"` | The time limit of one attempt. At least `"1s"`. |

The secrets of a webhook, for an instance called `teams`:

| Secret | Environment variable | Key Vault secret |
|---|---|---|
| `notify.teams.url` | `BUTTERBOT__NOTIFY__TEAMS__URL` | `notify--teams--url` |
| `notify.teams.access_key` | `BUTTERBOT__NOTIFY__TEAMS__ACCESS_KEY` | `notify--teams--access-key` |

The URL, the header values and `body` are templates. A placeholder is written as `{{name}}`, exactly so, without spaces. A name that is not in this list is a configuration error.

| Placeholder | Value |
|---|---|
| `{{event}}` | The kind of the event, for example `failure`. |
| `{{summary}}` | The event in one sentence, for example "certificate api-example-org failed at step validate". |
| `{{details}}` | The fields of the event except its kind, one per line as `name: value`. A list is written as its items separated by commas, a field without a value as `none`. |
| `{{exit_code}}` | For `run_completed`: `0` when no certificate failed, `1` when one did. Empty for every other event. |
| `{{event_json}}` | The event as a JSON object, as [Events](events.md) shows it. |
| `{{access_key}}` | The secret `notify.<name>.access_key`. |

A value is inserted so that it cannot break what surrounds it:

| Where | How a value is inserted |
|---|---|
| `url` | Percent-encoded. Only letters, digits and `-`, `.`, `_`, `~` are left as they are. |
| A header value | As it is. A value with a line break makes the delivery fail, so `{{details}}` does not belong in a header. |
| `body` with a JSON content type | Escaped as the content of a JSON string. Write the quotation marks yourself: `"text": "{{summary}}"`. |
| `body` with another content type | As it is. |

`{{event_json}}` is the exception: in a body it is inserted as a JSON object, without quotation marks around it: `"event": {{event_json}}`.

How a delivery behaves:

- A 2xx answer is a success.
- After a network error, a 5xx answer or status 429, Butterbot tries again: three attempts in all, each limited by `timeout`. A delivery is given up after 30 seconds whatever its state.
- Every other answer fails at once. Redirects are not followed, because a redirect would send the headers to a host the configuration does not name.
- A delivery that fails is written to the log with the name of the notifier, the kind of the event, and the status or the kind of error. It never changes the outcome or the exit code of a run.
- The URL, the key and the header values are never written to the log, to error messages or to the output of a command.
- Certificate verification cannot be switched off. Use `ca_file` for an endpoint with a private certificate authority.

A notifier whose `url` is supplied by no source is reported once at the start of a run and receives no events. So is one that uses `{{access_key}}` without the key being supplied, and one whose URL has another scheme than `https` or `http`. `butterbot check-config` lists a missing URL or key as a missing secret, and `butterbot notify test <name>` sends a sample event through one notifier.

### `[[certificate]]`: certificate entries

Each entry describes one certificate and names the providers it uses.

| Key | Default | Meaning |
|---|---|---|
| `name` | required | The name of the certificate. It is also the name under which every store holds it. |
| `names` | required | The DNS names the certificate covers. At least one. Wildcard names such as `*.example.com` are allowed. |
| `acme` | required | The ACME server instance that issues it. |
| `dns` | required | The DNS provider instance that proves control of its names. |
| `stores` | required | The store instances that receive it. At least one. |
| `renew_before` | from the ACME server | Overrides the server's `renew_before`. |
| `key_type` | from the ACME server | Overrides the server's `key_type`. |
| `profile` | from the ACME server | Overrides the server's `profile`. |
| `format` | from each store | Overrides the `format` of every store of this certificate. |
| `propagation_timeout` | from the DNS provider | Overrides the provider's `propagation_timeout`. |
| `settle` | from the DNS provider | Overrides the provider's `settle`. |

## Defaults and overrides

Six settings exist both on a provider and on a certificate. The value that applies is the first one that is set, in this order: the certificate entry, the provider instance, the built-in default.

| Setting | Provider | Built-in default |
|---|---|---|
| `renew_before` | ACME server | `"30d"` |
| `key_type` | ACME server | `"rsa2048"` |
| `profile` | ACME server | none |
| `format` | store | `pfx` for `azure-key-vault`, `pem` for `directory` |
| `propagation_timeout` | DNS provider | `"5m"` |
| `settle` | DNS provider | `"30s"` |

## Validation

The whole configuration is validated before any certificate is processed. These are errors:

- a key that does not exist, in any table;
- a value of the wrong type, an invalid duration, a value out of range;
- a missing required key;
- an invalid instance or certificate name, or a certificate name used twice;
- an unknown provider `type`, key type, format or propagation check;
- an unknown notifier `type`, method or format, an invalid header name, a notifier with both `body` and `format`, or a placeholder that does not exist;
- a certificate entry that refers to an ACME server, DNS provider or store that is not defined, or that lists no DNS name or no store.

Every problem is reported, not only the first. Each message names the table, the key and, where known, the file, environment variable or vault secret that set the value. On a configuration error the program exits with code 2.

## Complete example

This example sets every setting. Most of them can be left out; the values shown for `[run]`, `ttl` and the `propagation_` settings are the defaults.

```toml
[run]
parallel = 4
min_reissue = "2d"
retry_wait_max = "2m"

# Read secrets from this vault as the highest configuration layer.
# Leave the table out to use files and the environment only.
[secrets]
key_vault_url = "https://YOUR-VAULT-NAME.vault.azure.net/"

# --- ACME servers: where certificates come from ---------------------------

[acme.letsencrypt]
directory_url = "https://acme-v02.api.letsencrypt.org/directory"
renew_before = "30d"
key_type = "rsa2048"
ari = true
# The account credentials are the secret acme.letsencrypt.access_key:
#   environment: BUTTERBOT__ACME__LETSENCRYPT__ACCESS_KEY
#   Key Vault:   acme--letsencrypt--access-key

[acme.letsencrypt-staging]
directory_url = "https://acme-staging-v02.api.letsencrypt.org/directory"
profile = "tlsserver"
#   environment: BUTTERBOT__ACME__LETSENCRYPT_STAGING__ACCESS_KEY
#   Key Vault:   acme--letsencrypt-staging--access-key

# A server that requires external account binding.
[acme.bound]
directory_url = "https://acme.example.com/directory"
eab_kid = "example-key-identifier"
# The binding key is the secret acme.bound.eab_key:
#   environment: BUTTERBOT__ACME__BOUND__EAB_KEY
#   Key Vault:   acme--bound--eab-key

# --- DNS providers: where validation records are published ----------------

[dns.azure]
type = "azure-dns"
subscription_id = "00000000-0000-0000-0000-000000000000"
resource_group = "example-dns-zones"
ttl = "60s"
propagation_check = "authoritative"
propagation_interval = "10s"
propagation_timeout = "5m"
settle = "30s"

[dns.cloudflare]
type = "cloudflare"
# The API token is the secret dns.cloudflare.access_key:
#   environment: BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY
#   Key Vault:   dns--cloudflare--access-key

# A second instance of the same type has its own secret,
# dns.cloudflare-eu.access_key. This one checks propagation through
# resolvers instead of the authoritative nameservers.
[dns.cloudflare-eu]
type = "cloudflare"
propagation_check = "resolver"
propagation_resolvers = ["1.1.1.1", "8.8.8.8"]

# --- Stores: where certificates are delivered -----------------------------

[store.vault]
type = "azure-key-vault"
vault_url = "https://YOUR-CERTIFICATE-VAULT-NAME.vault.azure.net/"
format = "pfx"

[store.local]
type = "directory"
path = "/var/lib/butterbot/certs"
format = "pem"

# A separate store for test certificates, so that a certificate from a
# staging server never lands where production certificates are picked up.
[store.staging]
type = "directory"
path = "/var/lib/butterbot/staging-certs"

# --- Notifiers: who is told about the events of a run ---------------------

# A Microsoft Teams workflow that is told about failures.
[notify.teams]
type = "webhook"
format = "teams"
events = ["failure", "expiring"]
# The URL of the workflow is the secret notify.teams.url:
#   environment: BUTTERBOT__NOTIFY__TEAMS__URL
#   Key Vault:   notify--teams--url

# An endpoint with a body of its own, a key in a header and a certificate
# from a private authority. Its URL is the secret notify.console.url.
[notify.console]
type = "webhook"
method = "POST"
body = '{"title": "{{summary}}", "text": "{{details}}"}'
content_type = "application/json"
ca_file = "/etc/butterbot/console-ca.pem"
timeout = "10s"
# The key is the secret notify.console.access_key:
#   environment: BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY
#   Key Vault:   notify--console--access-key

[notify.console.headers]
X-API-KEY = "{{access_key}}"

# --- Certificates ---------------------------------------------------------

# A wildcard certificate delivered to two stores.
[[certificate]]
name = "wildcard-example-com"
names = ["*.example.com", "example.com"]
acme = "letsencrypt"
dns = "azure"
stores = ["vault", "local"]

# A certificate that overrides every setting it can.
[[certificate]]
name = "api-example-org"
names = ["api.example.org", "www.example.org"]
acme = "letsencrypt"
dns = "cloudflare"
stores = ["local"]
renew_before = "20d"
key_type = "ecdsa-p256"
profile = "tlsserver"
format = "pfx"
propagation_timeout = "10m"
settle = "60s"

# A test entry on the staging server, with its own name and its own store.
[[certificate]]
name = "staging-test-example-net"
names = ["test.example.net"]
acme = "letsencrypt-staging"
dns = "cloudflare-eu"
stores = ["staging"]
```
