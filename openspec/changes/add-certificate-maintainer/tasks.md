# Tasks

## 1. Project scaffold

- [x] 1.1 Create the Rust binary crate `butterbot` with the module layout from design D17 and a `.gitignore`; verify `cargo build` succeeds and `butterbot --version` prints the crate version
- [x] 1.2 Add formatting and lint configuration; verify `cargo fmt --check` and `cargo clippy -- -D warnings` pass on the empty project
- [x] 1.3 Add a CI workflow that runs fmt, clippy and tests on every push and pull request; verify it passes on the scaffold commit
- [x] 1.4 Define the shared error type with the classes from design D11 (`Transient`, `Permanent`, `RateLimited`, `ValidationFailed`, `Timeout`) and the retry helper (3 attempts, growing pauses, transient only); verify with unit tests that only transient errors are retried and that the third failure is returned

## 2. Configuration

- [x] 2.1 Define the typed configuration model (`run`, `secrets`, `acme.<name>`, `dns.<name>`, `store.<name>`, `notify.<name>`, `certificate[]`) with unknown fields denied and a duration type for `s`/`m`/`h`/`d`; verify with unit tests for a full valid file, an unknown key and an invalid duration
- [x] 2.2 Implement file loading: default path, `--config` (repeatable, later overrides earlier) and `BUTTERBOT_CONFIG`; verify with tests for the default path, two files setting the same key, and a missing file
- [x] 2.3 Implement the environment source with the mapping from design D3; verify with tests that `BUTTERBOT__ACME__LETSENCRYPT_STAGING__ACCESS_KEY` sets `acme.letsencrypt-staging.access_key` and overrides a file value
- [x] 2.4 Implement the Key Vault source, enabled only by `secrets.key_vault_url`, fetching only secrets under known top-level tables; verify with a fake vault client that unrelated secrets are not fetched, that vault overrides environment, and that an unreachable vault is a configuration error
- [x] 2.5 Implement validation: name alphabet and uniqueness, provider types, certificate references, key types, formats, and collection of all errors in one report; verify each scenario of the `configuration` spec's validation requirements with a test
- [x] 2.6 Implement default-and-override resolution for `renew_before`, `key_type`, `profile`, `format`, `propagation_timeout` and `settle`; verify with tests for certificate-level, provider-level and built-in values
- [x] 2.7 Implement the secret wrapper type and the "expected secrets" listing (path, environment name, Key Vault name, found or missing); verify that formatting a secret never prints its value and that the listing matches the spec's naming examples
- [x] 2.8 Write `docs/configuration.md` with a complete annotated example using `example.com` and the source-name rules; verify the example file in the document loads without errors in a test

## 3. Logging and events

- [x] 3.1 Set up logging with JSON and text output, automatic selection by terminal detection, `--log-format` and `--log-level`; verify with tests that non-terminal output is one JSON object per line
- [x] 3.2 Define the `Event` enum (`run_completed`, `failure`, `renewed`, `expiring`) with the fields from the `run-events` spec and stable serialised names; verify with snapshot tests of each event's JSON
- [x] 3.3 Implement the event bus with the always-on log sink and the notifier registry (no notifier types), including per-notifier event selection; verify with a test sink that selection works and that a failing sink is logged and does not propagate
- [x] 3.4 Document the events and their fields in `docs/events.md`; verify the documented examples match the snapshot tests

## 4. Certificate stores

- [x] 4.1 Define the `CertStore` trait, `StoredCert`, `IssuedCert` and `Format`, plus an in-memory store for tests; verify the in-memory store round-trips a certificate
- [x] 4.2 Implement PEM and PKCS#12 encoding of certificate, chain and key for RSA and ECDSA keys; verify by decoding the output with an independent parser in tests, including the empty-password PKCS#12 case
- [x] 4.3 Implement the `directory` store with the layout, owner-only key file and atomic replacement from the spec; verify with tests on a temporary directory, including file modes and a reader during a write
- [x] 4.4 Implement the `azure-key-vault` store (`current` from the certificate's public part, `put` as an import with the matching content type and an exportable key); verify against recorded HTTP exchanges for present, absent, unreadable and import cases
- [x] 4.5 Add store-write retry (3 attempts, transient only) at the call site used by the pipeline; verify with a store that fails once that no second order is created

## 5. Renewal policy

- [x] 5.1 Implement the decision function over (configuration, per-store `StoredCert`, clock, optional renewal information) returning due or not due with a reason and the time it becomes due; verify every scenario of the `renewal-policy` spec with a unit test using a fixed clock
- [x] 5.2 Implement the `renew_before` cap and warning relative to the stored certificate's validity period; verify the two scenarios of that requirement
- [x] 5.3 Implement the reissue guard and the forced-issuance override; verify the guard's four scenarios and the force scenario
- [x] 5.4 Implement the best-effort renewal-information lookup wrapper that turns any error into "no information"; verify a failing lookup leaves the decision to the renewal period and does not fail the certificate

## 6. DNS validation

- [x] 6.1 Define the `DnsProvider` trait, `Zone` and `RecordHandle`, plus zone resolution by longest suffix; verify with tests for a sub-zone match and for no match
- [x] 6.2 Implement the propagation checker (zone cut and nameserver discovery, CNAME following, direct queries with TCP fallback, polling, timeout, `authoritative`, `resolver` and `off` modes, diagnostics); verify against an in-process DNS server for agreement, one lagging server, extra values, delegation and timeout
- [x] 6.3 Implement the `cloudflare` provider (list zones, add TXT values, remove exactly the added records); verify against recorded HTTP exchanges, including a foreign value at the same name that must survive cleanup
- [x] 6.4 Implement the `azure-dns` provider over the Resource Manager REST API (list zones, read-modify-write of the TXT record set, removal of own values, deletion of an emptied record set); verify against recorded HTTP exchanges, including the wildcard-plus-apex case with two values
- [x] 6.5 Implement a test DNS provider that drives the ACME test server's challenge DNS server; verify a TXT value set through it is returned by a DNS query
- [x] 6.6 Document each provider's settings and the required permissions from design D19 in `docs/providers.md`; verify every setting accepted by the configuration model appears in the document

## 7. ACME issuance

- [x] 7.1 Add a test harness that starts the ACME test server and its challenge DNS server for integration tests, locally and in CI; verify a trivial test can fetch the server's directory
- [x] 7.2 Implement the ACME client wrapper: load account from credentials, register with optional external account binding, serialise credentials; verify against the test server that a second start reuses the account
- [x] 7.3 Implement account persistence per design D5 (save to the vault before ordering; fail the server's certificates if saving fails; no registration during `run` without a vault); verify the four scenarios of "Account registration on first use"
- [x] 7.4 Implement key generation for the five key types and CSR creation carrying only the DNS names; verify the issued test certificates have the requested key type and exactly the requested names
- [x] 7.5 Implement order creation with optional profile and optional `replaces`, challenge collection for DNS-01, validation request, verdict polling (5 s, 2 min) and download of certificate and chain; verify against the test server for a wildcard-plus-apex certificate and for a renewal that names its predecessor
- [x] 7.6 Implement the "profile not offered" and "validation failed" error paths with the server's reasons in the message; verify both against the test server

## 8. Run orchestration

- [x] 8.1 Implement the per-certificate pipeline with the step sequence and undo stack from design D6; verify with fake providers that a failure after "present records" runs cleanup, that undo order is last-in-first-out, and that a failing undo does not mask the original error
- [x] 8.2 Implement the run loop: selection with `--only`, failure isolation, the semaphore for `run.parallel` and the per-zone lock; verify with fake providers that one failure does not stop the others and that two certificates in one zone never overlap in their DNS steps
- [x] 8.3 Implement rate-limit handling (`run.retry_wait_max`, one wait, no further attempts) and per-step timeouts; verify with a fake ACME client for the short-wait, long-wait and hanging-service scenarios
- [x] 8.4 Implement graceful stop (no new certificates, cancellation at safe points with undo, completion of store writes for issued certificates, immediate exit on a second signal); verify with tests that send signals at the propagation wait and after issuance
- [x] 8.5 Implement the run outcome, the summary and the emission of `failure`, `renewed`, `expiring` and `run_completed`; verify the outcome and event scenarios, including no `run_completed` on interruption
- [x] 8.6 Add an integration test of a full run against the ACME test server with the directory store: first issuance, an immediate second run that does nothing, a renewal forced with `--only --force`, and a validation failure that leaves no challenge record behind

## 9. Command line

- [x] 9.1 Implement argument parsing for `run`, `status`, `check-config`, `account new`, the global options and usage errors (`--force` without `--only`, unknown certificate name); verify with command-line tests for each usage error
- [x] 9.2 Implement `status` with text and `--json` output and no side effects; verify against fake stores that due and not-due certificates are reported with reasons and that nothing is written
- [x] 9.3 Implement `check-config` using the validation and the expected-secrets listing, with no contact to ACME servers, DNS providers or stores; verify the two scenarios of its requirement
- [x] 9.4 Implement `account new` (register, print credentials with their source names, refuse when credentials exist); verify against the ACME test server
- [x] 9.5 Map outcomes to exit codes 0, 1, 2 and 3; verify each code with a command-line test
- [x] 9.6 Write the README: what Butterbot is, a quick start with the directory store, the commands and exit codes, links to the documents under `docs/`; verify every command shown in the README runs as written against the test setup

## 10. Container image

- [x] 10.1 Add a Dockerfile that builds a static binary for `linux/amd64` and `linux/arm64` and produces a minimal image running as a non-root user with `butterbot run` as the default command; verify both platforms build and that `docker run <image> --version` works on each
- [x] 10.2 Verify container behaviour: default command performs a run with a mounted configuration, arguments replace the default, a read-only root filesystem works, and stopping the container during a run produces exit code 3 with challenge records removed
- [x] 10.3 Write `docs/deployment.md` with a reference CronJob (daily schedule, `concurrencyPolicy: Forbid`, a run deadline, no automatic retries, a 60-second termination grace period, configuration from a ConfigMap, secrets from the environment or the vault) and the cutover steps from the design's migration plan; verify the manifest passes `kubectl apply --dry-run=client`

## 11. Integration checks

- [x] 11.1 Run `openspec validate add-certificate-maintainer --strict` and walk through every scenario in the specs, confirming each has a test or a recorded manual check; verify by adding the scenario-to-test mapping to the pull request description
- [ ] 11.2 Run the end-to-end check against Let's Encrypt staging with a throwaway zone on Azure DNS, a throwaway zone on Cloudflare and a test vault (issuance, second run does nothing, forced renewal, `status`, interruption); verify by recording the commands and results in `docs/deployment.md` under a "verified against" note

## Workflow follow-up

- Decide the items under "Open Questions" in `design.md`; each becomes its own change if taken up.
- Archive this change after the cutover from the old job has gone through one natural renewal.
