# Butterbot

Certificates are the bread and butter of the internet: everything runs on them, and looking after them is boring. Butterbot does the boring part.

It is a small program that runs once a day, checks every certificate you have told it about, and gets a new one from an ACME certificate authority (such as Let's Encrypt) when one is missing or about to expire. It proves ownership through DNS and delivers the result to wherever your services pick certificates up.

Everything it talks to is a named provider in the configuration:

- **ACME servers**: where certificates come from.
- **DNS providers**: where the validation records live.
- **Certificate stores**: where certificates go.

Each certificate says which of each it uses, so one Butterbot can serve several certificate authorities, DNS hosts and destinations.

## Status

The first version is implemented, from an [OpenSpec](https://github.com/Fission-AI/OpenSpec) change:

- [Proposal](openspec/changes/add-certificate-maintainer/proposal.md): what and why
- [Design](openspec/changes/add-certificate-maintainer/design.md): how, and what is still open
- [Specs](openspec/changes/add-certificate-maintainer/specs/): the behaviour, one file per capability
- [Tasks](openspec/changes/add-certificate-maintainer/tasks.md): the implementation plan

It has been tested against an ACME test server. It has not yet run against a public certificate authority.

## Quick start

This example gets a wildcard certificate from the Let's Encrypt staging server, proves ownership through Cloudflare, and writes the certificate into a directory. Replace `example.com` with a domain whose DNS zone is at Cloudflare. Certificates from the staging server are for testing and are not trusted by browsers.

Build the program with `cargo build --release` and put `target/release/butterbot` on your `PATH`.

**1. Write the configuration.** Save this as `butterbot.toml`:

```toml
[acme.letsencrypt-staging]
directory_url = "https://acme-staging-v02.api.letsencrypt.org/directory"

[dns.cloudflare]
type = "cloudflare"

[store.local]
type = "directory"
path = "certificates"

[[certificate]]
name = "wildcard-example-com"
names = ["*.example.com", "example.com"]
acme = "letsencrypt-staging"
dns = "cloudflare"
stores = ["local"]
```

Tell Butterbot where the file is. Without this it reads `/etc/butterbot/config.toml`.

```sh
export BUTTERBOT_CONFIG=butterbot.toml
```

**2. Register an account at the ACME server.**

```sh
butterbot account new --acme letsencrypt-staging
```

The last line of the output is the account's credentials. They are a secret. Butterbot never writes secrets into the configuration file; it reads them from the environment (or from Azure Key Vault, see [Configuration](docs/configuration.md)).

**3. Supply the secrets.** The second one is a Cloudflare API token that may edit DNS records in the zone; [Providers](docs/providers.md) lists the exact permissions.

```sh
export BUTTERBOT__ACME__LETSENCRYPT_STAGING__ACCESS_KEY='<the last line of the output of step 2>'
export BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY='<your Cloudflare API token>'
```

**4. Check the configuration.** This lists every secret the configuration requires and says whether it was found. It contacts no server.

```sh
butterbot check-config
```

**5. Look at the state.** This reads the stores and changes nothing. The certificate is reported as due, because the directory holds none.

```sh
butterbot status
```

**6. Run.**

```sh
butterbot run
```

The certificate is now in `certificates/wildcard-example-com/`: `fullchain.pem` holds the certificate and its chain, `privkey.pem` the private key.

Run the same command again and nothing happens, because the certificate is not due. That is the point: schedule `butterbot run` once a day and it issues a new certificate when one is needed.

To get a new certificate now, for example after a private key was exposed, name the certificate and force it:

```sh
butterbot run --only wildcard-example-com --force
```

## Commands

| Command | What it does |
|---|---|
| `run` | Checks every configured certificate and issues the ones that are due. |
| `run --only <name>` | Checks only the named certificate. May be given several times. |
| `run --only <name> --force` | Issues the named certificate even when it is not due. `--force` is accepted only together with `--only`. |
| `status` | Shows what each store holds, whether each certificate is due and why, or when it becomes due. Changes nothing. |
| `status --json` | The same as one JSON document, with one entry per certificate under `certificates`. |
| `check-config` | Validates the configuration as `run` does when it starts, and lists every secret the configuration requires with its name in each source. Contacts no ACME server, DNS provider or store. |
| `account new --acme <name>` | Registers an account at the named ACME server and prints its credentials with the names to store them under. Refuses when the server already has credentials. |

Options for every command:

| Option | Meaning |
|---|---|
| `--config <file>` | A configuration file. May be given several times; later files override earlier ones. |
| `--log-format json` or `text` | How the log is written. Without the option: text in a terminal, one JSON object per line everywhere else. |
| `--log-level error`, `warn`, `info` or `debug` | The least severe records that are written. The default is `info`. |
| `--help`, `--version` | Help for the program or for a command; the version. |

`butterbot run` writes its log to standard output. The other commands print their result on standard output and write log records to standard error, so that the result can be read by another program.

Butterbot never asks for input. It behaves the same in a container and in a terminal.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. For `run`: every certificate was either not due or renewed. |
| 1 | At least one certificate failed. Also: `status` could not read a store, or `account new` could not register the account. |
| 2 | The configuration is invalid or the command line is wrong. Also: `check-config` found a required secret missing, or `account new` was refused because the server already has credentials. No certificate was processed. |
| 3 | The run was stopped by a signal (`SIGTERM` or `SIGINT`). Challenge records were removed before the program ended. A second signal ends the program at once. |

## When a certificate is renewed

A certificate is issued when a store holds none, when the names in the configuration differ from the names in the stored certificate, or when it is close to expiry.

"Close to expiry" has two rules, and the earlier one applies:

- `renew_before`: the certificate is due this long before it expires. The default is 30 days.
- Renewal information from the certificate authority (ARI): the certificate is due once the window the authority suggests has started.

With Let's Encrypt the suggested window opens when about a third of the lifetime is left. A `renew_before` shorter than that has no practical effect, because the authority's window opens first. Set `ari = false` on the ACME server to make `renew_before` the only rule.

## Documentation

- [Configuration](docs/configuration.md): files, environment variables, Key Vault, every setting, a complete example.
- [Providers](docs/providers.md): each ACME, DNS and store type, with the permissions it needs.
- [Events](docs/events.md): the events a run writes to the log, and how to monitor that the job runs.

## Development

```
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Some tests need the ACME test server [Pebble](https://github.com/letsencrypt/pebble) and its challenge server. They are skipped by `cargo test`. To run them, install both and include the ignored tests:

```
go install github.com/letsencrypt/pebble/v2/cmd/pebble@v2.10.3
go install github.com/letsencrypt/pebble/v2/cmd/pebble-challtestsrv@v2.10.3
cargo test --all-features -- --include-ignored
```
