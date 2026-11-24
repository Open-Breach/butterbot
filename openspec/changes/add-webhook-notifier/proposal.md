# Proposal

## Why

Butterbot emits run events but can only write them to the log, so a failed renewal is noticed only by someone reading logs or watching the job's exit code. The owner wants to be told. A single generic webhook notifier covers the targets in view (a Microsoft Teams workflow first, possibly a UniFi alarm trigger or a job-monitoring service later) without Butterbot knowing any of them.

## What Changes

- A first notifier type, `webhook`, for `notify.<name>` instances. For each event it selects, it sends one HTTP request to a configured URL.
- The URL is a secret, addressed by its configuration path like every other secret (`notify.<name>.url`), so it can come from the environment or the vault.
- The request body is the event as JSON by default. A built-in `teams` format produces the message a Microsoft Teams workflow expects. A custom body template covers other receivers.
- The URL, the headers and a custom body may contain a small fixed set of placeholders, such as the event's one-line summary or the run's exit code.
- Optional request headers, with an optional `access_key` secret for receivers that want a key in a header.
- Transient delivery failures are retried within the existing delivery time limit. As before, a delivery failure never changes the run.
- A new command, `butterbot notify test <name>`, sends a sample event through one notifier so that the wiring can be checked without waiting for a real failure.
- The statement that Butterbot ships no notifier type is removed.

Not part of this change:

- Sending e-mail or SMS through a provider API. A webhook receiver can do that.
- Noticing that no run happened. The owner has decided not to pursue this for now.
- Checking the certificate that a public endpoint actually serves. That is an external monitor's job.
- Receiver-specific formats other than Teams.

## Capabilities

### New Capabilities

- `webhook-notifier`: delivering run events as HTTP requests, including target, body formats, placeholders, headers, retries and secrecy.

### Modified Capabilities

- `run-events`: the notifier extension point no longer says that no notifier type ships.
- `command-line`: a `notify test` command is added.

## Impact

- **Code**: the configuration parser (the `notify` table gains its first type), a new event sink, a small template renderer, one new command, and the documents under `docs/`.
- **Behaviour for existing installations**: none. A configuration without a `notify` instance behaves exactly as before.
- **External systems**: whatever the configured URLs point at. The first intended target is a Teams workflow with the "When a Teams webhook request is received" trigger.
- **Ordering with the first change**: `run-events` and `command-line` come from `add-certificate-maintainer`, which is not archived yet. That change has to be archived, or its specs synced, before this one is archived. Implementation does not depend on that order.
