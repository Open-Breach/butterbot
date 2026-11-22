# Proposal

## Why

Certificates are the bread and butter of the internet: everything runs on them, and looking after them is boring. The job that does this today (`Foundation.Azure.CertManager`, a .NET console job in `yourAnswer-ch/Foundation`) has done it well, but it is rigid: Let's Encrypt and Azure DNS are hardcoded, every certificate has the same shape, there is one Key Vault for everything, and the configuration is compiled into the container image, so adding a domain means building a new image.

Butterbot replaces it with a Rust rewrite built around providers, so one run can serve several certificate authorities, several DNS hosts and several destinations. The pipeline-with-rollback idea from the old job is kept.

## What Changes

- A new single-binary console program, `butterbot`, that runs once and exits. It is meant to be scheduled (a Kubernetes CronJob, once a day) and can also be run from a terminal. The container image is the default way to run it.
- Three kinds of named provider, defined in configuration:
  - **ACME servers** (where certificates come from). An ACME server is configuration only: a directory URL plus optional account binding, profile and defaults.
  - **DNS providers** (where validation records live). First version: Azure DNS and Cloudflare.
  - **Certificate stores** (where certificates go). First version: Azure Key Vault and a plain directory.
- Each certificate entry wires the three together: which ACME server issues it, which DNS provider proves ownership, and which stores receive it. A certificate may go to several stores.
- Configuration is layered: built-in defaults, config file(s), environment variables, and optionally Azure Key Vault. Secrets are ordinary configuration keys identified by their full path (for example `acme.letsencrypt.access_key`), so they can come from any layer.
- A certificate is issued when it is missing, when its names changed, when it is within `renew_before` of expiry, or when the CA asks for earlier replacement through ACME Renewal Information (ARI). `renew_before`, key type, profile and stored format have a default on the provider and can be overridden per certificate.
- Before asking the CA to validate, the job checks that the challenge record is served by every authoritative nameserver of the zone.
- Each certificate is processed by a pipeline whose steps register rollbacks. Rollbacks run in reverse order on failure and on shutdown. One failed certificate does not stop the others.
- Commands: `run`, `status`, `check-config`, `account new`. Exit codes distinguish success, certificate failures, configuration errors and interruption.
- A defined set of run events (`run_completed`, `failure`, `renewed`, `expiring`) written to the log. `run_completed` is emitted on every run that reaches its end, so that its absence shows the job is not working.
- Fixes for problems in the old job, by design: the ACME account is persisted instead of re-registered on every run, failures set the exit code, rollbacks run the rollback step, and every wait has a timeout.

Explicitly not part of this change:

- Reloading certificates in the services that consume them. Butterbot's responsibility ends when the certificate is in its store.
- Detecting that the job did not run. Butterbot only emits the signal.
- A concrete notification channel. The events and the notifier extension point are defined; no notifier ships in this version.
- Retrying a failed CA validation within the same run. Deferred until logs show whether it is needed.
- A fallback ACME server per certificate.
- Deleting or revoking certificates.

## Capabilities

### New Capabilities

- `configuration`: where configuration comes from, how provider instances and certificates are declared, how secrets are identified and resolved, and how configuration is validated.
- `acme-issuance`: obtaining a certificate from a configured ACME server, including accounts, orders, key types and profiles.
- `dns-validation`: proving domain ownership through DNS challenge records, including publishing, propagation checking and cleanup.
- `certificate-stores`: reading the certificate a destination currently holds and writing a newly issued certificate to it, in the configured format.
- `renewal-policy`: deciding, for each certificate, whether it has to be issued in this run.
- `run-orchestration`: how a run processes certificates, including rollback, parallelism, retries, timeouts, shutdown and the overall result.
- `command-line`: the commands, options, exit codes and log output of the `butterbot` program.
- `run-events`: the events a run emits and how they are delivered.
- `container-image`: the container image through which Butterbot is run by default.

### Modified Capabilities

None. This is the first change in the repository.

## Impact

- **Code**: a new Rust project in this repository. There is no existing code here.
- **External systems the program talks to**: ACME servers (Let's Encrypt first), Azure DNS, the Cloudflare API, Azure Key Vault, and public DNS nameservers (direct queries for the propagation check).
- **Permissions**: the identity Butterbot runs as can create DNS records in every configured zone and write certificates to every configured store. The required permissions are listed in `design.md` and must be granted narrowly.
- **Existing deployment**: Butterbot is meant to replace `Foundation.Azure.CertManager`. It adopts certificates that already exist in a store under the same name, so the cutover does not reissue anything. The cutover steps are in `design.md`. Deployment configuration (the schedule, the config file, the real domain list) lives outside this repository.
- **Dependencies**: Rust crates for ACME, DNS queries, Azure and HTTP. Candidates are named in `design.md`.
