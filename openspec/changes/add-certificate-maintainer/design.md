# Design

## Context

See `proposal.md` for the motivation. This section records only what shapes the approach.

Butterbot replaces `Foundation.Azure.CertManager`, a .NET console job. Reading that job's source gave these facts, which the design reacts to:

- It is a pipeline of small steps (check expiry, create account, create order, authorise, create TXT record, validate, download, store, remove TXT record) with a rollback step attached to the TXT step. The idea is sound and is kept.
- Let's Encrypt production is hardcoded. Azure DNS is hardcoded, with the challenge record always written at the zone apex. Every certificate is `*.domain` plus `domain`, RSA 2048, imported into one Key Vault as a PFX named `wildcard-<domain with dashes>`.
- The configuration file is compiled into the image.
- The ACME account key is held only in an in-memory cache, so every run that renews something registers a new account.
- The result of the pipeline is ignored, so the process exits 0 when certificates fail.
- The pipeline library enqueues the forward command where it should enqueue the rollback, so a failed run re-creates the TXT record instead of removing it.
- The validation loop has no timeout.
- It renews when 30 or fewer days remain. That rule is kept as the default.

Constraints:

- The program runs once a day from a scheduler and must be safe to restart at any point. It keeps no state of its own; the stores are the state.
- The same binary must work in a cluster (workload identity) and in a developer's terminal (Azure CLI sign-in) without local setup.
- The repository is public. Nothing in it may contain real domain lists, tenant or subscription ids, vault names or secrets. Examples use `example.com`.
- ACME rules that cannot be designed around: a failed validation invalidates its authorisation, so a retry needs a new order; CAs limit failed validations and duplicate certificates; certificate lifetimes are shrinking (Let's Encrypt moves its default to 64 days in February 2027 and 45 days in February 2028).

## Goals / Non-Goals

**Goals:**

- A provider model in which adding a DNS host or a destination is one new module, and adding a CA is configuration only.
- A per-certificate pipeline in which every externally visible change has a registered undo.
- Behaviour that is decided entirely by configuration plus what the stores hold.
- A small, static binary and a minimal container image for amd64 and arm64.
- Tests that exercise the real ACME protocol against a local test server, without touching a public CA.

**Non-Goals:**

- A long-running service, an operator, or a Kubernetes controller.
- HTTP-01 or TLS-ALPN-01 validation. DNS-01 is the only method; it is the only one that covers wildcards.
- A certificate whose names live in different DNS provider instances.
- A plugin system that loads providers at run time. Providers are compiled in.
- Everything listed as "not part of this change" in `proposal.md`.

## Decisions

### D1. Run-once binary, scheduled from outside

The program evaluates, acts and exits. Scheduling, retry-tomorrow and overlap prevention belong to the scheduler (a CronJob with `concurrencyPolicy: Forbid`).

*Why:* restart safety comes for free, Kubernetes already has a reliable scheduler, and there is no state to lose. *Alternative considered:* a daemon with its own timer. Rejected: it needs health checks, state and leader election for no benefit at a once-a-day cadence.

### D2. Three provider kinds; ACME servers are data, DNS and stores are code

- `acme.<name>`: one generic ACME client, parameterised by directory URL, optional EAB, profile and defaults. No trait.
- `dns.<name>`: a `DnsProvider` trait with implementations selected by `type`.
- `store.<name>`: a `CertStore` trait with implementations selected by `type`.

```rust
#[async_trait]
pub trait DnsProvider: Send + Sync {
    /// Zones this instance can write to; used for zone resolution and for the per-zone lock.
    async fn zones(&self) -> Result<Vec<Zone>>;
    /// Add all `values` as TXT at `name`, leaving foreign values alone.
    async fn present(&self, zone: &Zone, name: &str, values: &[String], ttl: u32) -> Result<RecordHandle>;
    /// Remove exactly what `present` added.
    async fn cleanup(&self, handle: RecordHandle) -> Result<()>;
}

#[async_trait]
pub trait CertStore: Send + Sync {
    async fn current(&self, name: &str) -> Result<Option<StoredCert>>; // names, not_before, not_after, issuer, serial, DER
    async fn put(&self, name: &str, cert: &IssuedCert, format: Format) -> Result<()>;
}
```

First-version implementations: `azure-dns`, `cloudflare`; `azure-key-vault`, `directory`.

*Assumption to confirm with the owner:* the provider set. Azure DNS and Key Vault are what the old job uses and Cloudflare is the stated reason for the rewrite. The `directory` store was added by the author of this change because it makes terminal use and integration tests possible without Azure. It can be dropped without affecting the rest.

*Why ACME is not a trait:* servers differ only in data. A trait would invite per-CA code paths that the protocol does not need.

### D3. Layered configuration with secrets addressed by path

Use the `config` crate (config-rs), as the owner's other Rust services do. Layers, lowest to highest: defaults, files, environment, Key Vault.

- **Environment source.** config-rs's built-in environment source cannot express the dash-to-underscore rule for instance names, so a small custom `Source` does the mapping: strip `BUTTERBOT__`, split on `__`, lower-case; for the tables `acme`, `dns`, `store` and `notify` the second segment is an instance name, where `_` is read as `-`; field segments keep `_`. Certificate entries are an array and are not addressable from the environment.
- **Key Vault source.** Enabled only when `secrets.key_vault_url` is present after the lower layers. It lists secret names (metadata only), keeps those whose first `--`-separated segment is a known top-level table, fetches only those, and maps them back to paths (instance segment keeps dashes, field segment turns dashes into underscores). Unrelated secrets in the same vault are never fetched.
- **Validation.** Deserialise into typed structs with unknown fields denied, then run cross-reference checks, collecting every error before reporting.
- **Secret typing.** Secret fields use a wrapper type whose `Debug` and `Display` print a placeholder, so a secret cannot reach a log by accident.

*Why paths as identity:* no secret has to be named in configuration, two instances of one provider type get distinct secrets automatically, and `check-config` can print the exact name each source expects.

*Alternative considered:* explicit references such as `{ store = "…", secret = "…" }` on each provider. Rejected by the owner in favour of the layered model, which also works with Kubernetes secrets and plain environment variables.

### D4. ACME client: `instant-acme`

It supports external account binding, ACME Renewal Information, certificate profiles and serialisable account credentials, and is async and pure Rust (0.8.5 at the time of writing). The serialised `AccountCredentials` JSON is the value of `acme.<name>.access_key`.

*Alternative considered:* writing the protocol by hand on `reqwest`. Rejected: nonce handling, JWS and polling are exactly what a maintained client is for.

*To verify during implementation:* that the crate exposes the directory's profile list (needed for the "profile not offered" error) and the `replaces` field on a new order.

### D5. Account persistence

Credentials are read through configuration like any secret. When they are missing:

- with the Key Vault layer enabled, register and write the secret before placing any order. If the write fails, fail the server's certificates rather than use an account that would be lost;
- without it, do not register during `run`. `butterbot account new` registers and prints the credentials with the names to store them under.

*Why not always auto-register:* without a writable secret store the account would be re-registered on every run, which is the old job's bug.

### D6. Per-certificate pipeline with an undo stack

Each certificate is processed by a fixed sequence of steps sharing a context. A step that changes external state pushes an undo closure onto the context's stack. On any error, on timeout and on shutdown the orchestrator pops and runs the stack (last in, first out), logging but not propagating undo errors.

Steps: read stores → decide → ensure account → create order → collect DNS challenges → resolve zones and take zone locks → present records (undo: cleanup) → propagation check → settle → request validation → await verdict → finalise and download → write stores → cleanup records.

After "finalise and download" the context is marked *issued*. From then on a shutdown request does not cancel the certificate; it lets "write stores" and "cleanup records" finish.

*Why a stack and not a queue:* later steps depend on earlier ones, so they must be undone first. The old library used a queue and, by mistake, queued the forward step.

### D7. The stores are the only state

The decision uses only `CertStore::current` for each store, the configuration, the clock and, when enabled, the server's renewal information. Certificates placed by another tool are indistinguishable from Butterbot's own, which is what makes the cutover a no-op.

The clock is injected so that every rule in `renewal-policy` is unit-tested with fixed times.

### D8. Renewal timing: the earlier of `renew_before` and the CA's suggestion

`renew_before` is the owner's rule and the latest point. With `ari` on (the default), the certificate is also due once the server's suggested window has started. Renewals ordered with `replaces` are coordinated renewals, which Let's Encrypt exempts from its rate limits.

Consequence to be aware of: with Let's Encrypt and `ari` on, a `renew_before` shorter than the CA's suggestion (roughly one third of the lifetime remaining) has no practical effect, because the CA's window opens first. `ari = false` makes `renew_before` the only rule.

A `renew_before` at or above the certificate's lifetime would make the certificate due on every run. It is capped at half the lifetime and logged as an error. *Alternative considered:* refusing to renew such a certificate. Rejected: a typo in configuration must not be able to let a certificate expire.

### D9. Propagation check, and no validation retry in this version

After `present`, find the zone's authoritative nameservers (walk up from the challenge name to the zone cut, follow a CNAME on the challenge name first), resolve them to addresses, and query each directly for the TXT record, over UDP with TCP fallback. Pass when every server returns all expected values; poll every 10 seconds up to `propagation_timeout`; then wait `settle`. The `hickory` DNS crates can address a specific server.

`settle` exists because large DNS hosts answer from many machines behind one address and the CA validates from several places; the check sees only the nearest machine.

A failed CA validation is not retried within the run. The owner's experience is that validation often failed on the first try with the old job, which waited a fixed 60 seconds; the propagation check is expected to remove that cause. The log records propagation time and the CA's failure reason so the decision can be revisited with data. Because the pipeline can restart from "create order", adding retries later is contained.

*Risk accepted:* a network policy that blocks outbound DNS to arbitrary servers makes `authoritative` mode fail. `resolver` mode (which asks the resolvers in `propagation_resolvers` and is subject to their caching) and `off` mode exist for that case.

### D10. Bounded concurrency with a lock per zone

A semaphore limits concurrent certificates to `run.parallel`. A lock keyed by (DNS instance, zone) is held from "present records" to "cleanup records". A certificate spanning several zones takes its locks in sorted order to avoid deadlock.

*Why:* on hosts where a record set is replaced as a whole (Azure DNS), two certificates writing `_acme-challenge` in one zone can overwrite each other's values.

### D11. Error classes drive retries

Every provider error is mapped to one of: `Transient`, `Permanent`, `RateLimited { retry_after }`, `ValidationFailed`, `Timeout`. Only `Transient` is retried (3 attempts, growing pauses). `RateLimited` is waited out once if within `run.retry_wait_max`. The mapping lives next to each provider; the retry policy lives in one place.

| Step | Default | On giving up |
|---|---|---|
| Propagation check | every 10 s, up to 5 min | fail; CA never asked |
| Ask the CA to validate | once per run | fail until the next run |
| Wait for the CA's verdict | every 5 s, up to 2 min | fail as timeout |
| Transient errors | 3 attempts | fail |
| Store write after issuance | 3 attempts | fail; reissue guard applies next run |
| CA rate limit | wait once if ≤ 2 min | fail until the next run |

### D12. Keys and formats

A new key per issuance, generated in memory: ECDSA through the crypto backend already used by the ACME client, RSA through the `rsa` crate or the backend if it offers generation. The CSR carries only the DNS names. PKCS#12 bundles are built in pure Rust (`p12-keystore` is the maintained candidate) with an empty password, which is what the existing consumers already read. Key Vault import uses the matching content type and an exportable key policy.

### D13. Azure access

Use the official Azure SDK for Rust (`azure_identity`, `azure_security_keyvault_certificates`, `azure_security_keyvault_secrets`; all 1.x at the time of writing). There is no maintained management crate for DNS, so Azure DNS is called through the Resource Manager REST API with a token from `azure_identity`.

*To verify during implementation:* which credential types the current `azure_identity` offers. If it has no default chain, compose one explicitly: workload identity when its environment variables are present, otherwise the Azure CLI sign-in.

### D14. TLS and static linking

All HTTP goes through `rustls`, with bundled web PKI roots so that the image needs no system certificate store. Build for `*-unknown-linux-musl` to get a static binary and a `scratch` or distroless image.

*Risk:* a crypto backend with C or assembly parts can complicate musl cross-builds for arm64. If so, switch backend before adding build complexity.

### D15. Events as a small internal bus

An `Event` enum with the four kinds, an `EventSink` trait, a log sink that is always present, and a registry of notifier sinks that is empty in this version. A sink error is logged by the bus and swallowed.

### D16. Two deliberate restrictions in the command line

- `--force` requires `--only`. A blanket reissue of every certificate is never what an operator wants and costs CA quota.
- There is no option to override a certificate's ACME server for one run. Pointing a production entry at a staging server would write an untrusted certificate into a production store. Staging is exercised through its own certificate entry with its own name and store.

### D17. Layout

One crate, modules by capability: `config`, `acme`, `dns` (`azure`, `cloudflare`, `propagation`), `store` (`key_vault`, `directory`), `policy`, `pipeline`, `events`, `cli`. Split into a workspace only when a second binary or a published library appears.

### D18. Testing

- **Unit:** renewal policy with a fixed clock; configuration mapping and validation; error classification; the undo stack.
- **Component:** propagation checker against an in-process DNS server; directory store on a temporary directory; Cloudflare and Azure DNS clients against recorded HTTP exchanges.
- **Integration (CI):** the whole pipeline against Pebble, Let's Encrypt's test ACME server, with its challenge test DNS server standing in as the DNS provider and the directory store as destination. Covers first issuance, renewal with `replaces`, validation failure with rollback, and interruption.
- **End to end (manual, before cutover):** Let's Encrypt staging with a throwaway zone on Azure DNS, one on Cloudflare, and a test vault.

### D19. Required permissions

To be documented in the README and granted no wider than this:

- **Azure DNS:** read the configured zones; create, read and delete TXT record sets in them. A custom role limited to TXT record sets is preferred over the built-in DNS Zone Contributor role, which covers all record types.
- **Key Vault used as a store:** read and import certificates.
- **Key Vault used as a configuration layer:** list secret names, read secrets, and set secrets (only to save a newly registered ACME account).
- **Cloudflare:** a token with zone read and DNS edit on the specific zones only.
- **Kubernetes:** none. The pod needs only its workload identity binding.

## Risks / Trade-offs

- [One identity can rewrite DNS for every configured zone] → narrow roles (D19), TXT-only where the host allows it, and a per-instance token for Cloudflare.
- [A store that fails forever while no store holds a fresh copy] → the reissue guard cannot see it, so the job issues once per run until the CA's duplicate limit stops it. The certificate is reported as failed on every run from the first day. Accepted for this version.
- [Leftover challenge values after a killed run] → they are harmless to validation and are not removed automatically, because the job cannot tell them from another tool's values. Documented; manual cleanup if they accumulate.
- [Stores are trusted as state] → a store that returns a stale answer could cause an unneeded reissue. Limited by the reissue guard and by coordinated renewals being exempt from CA limits.
- [`renew_before` appears to be ignored under ARI] → explained in D8 and to be stated in the README.
- [Crates named here move quickly] → versions are a snapshot. Check current documentation before relying on an API named in this document.
- [No validation retry] → a transient CA-side failure costs a day. Accepted by the owner for this version; revisit with the logged data.

## Migration Plan

Deployment configuration lives outside this repository. The steps for replacing the old job:

1. Grant the identity the permissions in D19.
2. Write the configuration with one entry per existing certificate, using the existing names (`wildcard-<domain with dashes>`), the names `*.domain` and `domain`, `key_type = "rsa2048"` and the Key Vault store with `format = "pfx"`.
3. From a terminal: `butterbot check-config`, then `butterbot status`. Every certificate must show as present and not due. Nothing has been changed so far.
4. Add one test entry with its own name, a throwaway zone, the staging ACME server and a test store. Run `butterbot run --only <test entry>`. Repeat for each DNS provider type in use.
5. Disable the old job's schedule and enable Butterbot's in the same change, so that the two never run against the same certificates.
6. Watch the first natural renewal.

**Rollback:** re-enable the old job's schedule and disable Butterbot's. Certificates written by either tool are valid for the other, because name, format and key type are the same.

## Open Questions

None of these changes the specs or tasks of this change.

- **Notification channel.** Which channel the events are bound to. The owner is deciding; a notifier type is a follow-up change.
- **Fallback ACME server.** Whether a certificate may name a second server to try when the first fails.
- **Image publishing and reference manifests.** Which registry and which CI publish the image, and whether the reference CronJob lives here or only in the deployment repository. This change delivers the Dockerfile and documents the CronJob settings.
- **Validation retries within a run.** To be decided from the logged propagation times and failure reasons.
- **CAA pre-check.** `check-config` could warn when a name's CAA records exclude the configured CA. Becomes relevant with a second CA.
- **Explicit secret name.** An optional override for a secret that already exists under a different name.
- **Licence.** The repository is public and has none yet.
