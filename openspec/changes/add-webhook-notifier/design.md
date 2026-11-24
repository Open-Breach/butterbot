# Design

## Context

`add-certificate-maintainer` left the notification channel open. It defined the four run events, an `EventSink` trait, an `EventBus` that always writes events to the log, and a registry for notifier instances that no type could fill. The configuration parser already reads a notifier's `type` and `events` and rejects every `type`.

The owner's decisions that shape this change (2026-11-24):

- The channel is a webhook. Microsoft Teams is the first target. A UniFi alarm trigger is a possible second one, as an experiment.
- No e-mail or SMS provider integration. Sending text messages to US numbers needs sender registration with any provider, which is out of proportion for a few alerts.
- Detecting that no run happened is not pursued for now. It has not been a problem in the owner's experience, and the infrastructure is not yet critical.
- A separate idea, outside Butterbot: an availability monitor that checks the certificate a public endpoint serves and raises an alarm below about 15 days of validity.

What the code already provides and this change builds on:

- `Event::to_json` and `Event::summary` (`src/events.rs`).
- `EventBus::emit`, which delivers to each notifier in turn and gives each delivery `DELIVERY_TIMEOUT` (30 seconds).
- `http::client_with_roots` (`src/http.rs`), which builds a client that trusts additional root certificates.
- The secret wrapper and the expected-secrets listing in `src/config`.

## Goals / Non-Goals

**Goals:**

- One notifier type that can reach Teams out of the box and most other HTTP receivers with a few lines of configuration.
- No new place where a secret can leak.
- A way to check a notifier's wiring without waiting for a real failure.

**Non-Goals:**

- A general template language. There are no conditions, loops or filters.
- Receiver-specific formats beyond Teams. Others use the custom body.
- Queuing events across runs, or delivering them after the run has ended.
- Switching certificate verification off for an endpoint.
- Anything listed as "not part of this change" in `proposal.md`.

## Decisions

### D1. The notifier is one more `EventSink`

`parse_notify` gains the type `webhook` and returns a typed instance. At start, each instance becomes a `WebhookSink` registered on the `EventBus` with its event selection. Nothing in the pipeline changes.

Delivery stays inline in `EventBus::emit`, as it is today. A slow endpoint can therefore delay the step that emitted the event by up to the bus's 30-second limit. *Alternative considered:* a background queue per notifier that is drained before exit. Rejected for now: a run emits a handful of events, and inline delivery keeps ordering trivial (`run_completed` is emitted after all certificates are done, so it is delivered last) and needs no shutdown handling.

### D2. A fixed set of placeholders, rendered by a small function

Templates are plain text with `{{name}}` placeholders from a fixed list: `event`, `summary`, `details`, `exit_code`, `event_json`, `access_key`. Rendering is a single pass that knows where the template is used:

| Position | How a value is inserted |
|---|---|
| URL | percent-encoded |
| Header value | as is; a value with control characters makes the delivery fail |
| Body with a JSON content type | escaped as the content of a JSON string, without the surrounding quotation marks |
| Body with another content type | as is |

`event_json` is the exception in a body: it is inserted as a JSON object.

Templates are parsed when the configuration is loaded, so an unknown placeholder is a configuration error and not a runtime surprise.

*Alternative considered:* a template crate such as `minijinja` or `handlebars`. Rejected: position-aware escaping is the part that matters, a general engine does not give it, and the fixed list keeps the configuration checkable.

`details` is the event's fields as `name: value` lines in the order of the event's JSON, without `event`. `exit_code` is `0` or `1` for `run_completed` and empty for other events, which makes a health-check style URL (`…/{{exit_code}}`) work when the instance selects only `run_completed`.

### D3. The URL and the key are secrets by path

`notify.<name>.url` and `notify.<name>.access_key` use the existing secret wrapper and appear in the expected-secrets listing: the URL always, the key only when a template of the instance uses `{{access_key}}`. Their source names follow the existing rule, for example `BUTTERBOT__NOTIFY__TEAMS__URL` and `notify--teams--url`.

The URL is a secret because webhook URLs, including a Teams workflow URL, carry their own credential.

A notifier with a missing or unusable URL is disabled for the run with one error in the log. This follows the existing rule that a missing secret fails only what depends on it, and the rule that notifiers never change a run.

### D4. Teams is a built-in format

`format = "teams"` renders a message with one Adaptive Card attachment: a bold, wrapping text block with the summary, and a fact set with one fact per event field. The card is built as a `serde_json` value, not from a text template, so escaping cannot go wrong.

*Why built in:* it is the first target, and the wrapper Teams requires is boilerplate nobody should have to paste into a configuration file.

*To verify during implementation:* the exact payload against Microsoft's current documentation for workflows started by "When a Teams webhook request is received". The older connector-style incoming webhooks are retired, so examples written for them may not apply. The flow has to allow anyone to trigger it, because Butterbot cannot present a Microsoft sign-in.

### D5. Retries inside the bus's time limit

Up to 3 attempts for network errors, 5xx and 429, with a short growing pause, each attempt limited by `timeout` (default 10 seconds). The bus's 30-second limit cuts the sequence off if it runs long. Other statuses fail at once. The existing retry helper and error classes are reused.

### D6. No redirects, no unverified TLS

The client for webhook sinks is built with redirects disabled, because a redirect would send custom headers, possibly with a key, to a host the configuration never named. Certificate verification cannot be switched off. An endpoint with a private authority is handled with `ca_file`, passed to `http::client_with_roots`.

### D7. Redaction at the source

`reqwest` errors include the request URL. Every error from a webhook delivery is stripped of its URL before it becomes a Butterbot error, and the sink formats its own messages from the notifier's name, the event kind and the status or error kind. A test asserts that neither the URL nor the key appears in the captured log for each failure path.

### D8. `notify test` reuses the real path

The command builds the same sink the run would build and delivers one sample event from the existing examples, with the certificate name `butterbot-test` and a reason that says it is a test. It prints "delivered" or the status and error kind. It does not go through the event bus's log sink, so no real-looking event is written to the log.

## Risks / Trade-offs

- [A slow endpoint delays a certificate's step by up to 30 seconds per event] → accepted (D1). The per-attempt timeout can be lowered per instance.
- [Teams changes the payload it accepts] → the format is one function with its own tests; the custom body is the fallback until it is updated.
- [An endpoint that keeps failing produces an error line per event] → intended. The run's outcome is unaffected.
- [Events are lost when delivery fails] → accepted. The log always has them.
- [A leftover notifier with a deleted Teams flow fails silently for the reader of Teams] → `notify test` after any change to the flow, and the delivery failure is in the log.

## Migration Plan

Nothing to migrate. The feature is off until a `notify` instance is configured. To enable Teams:

1. In Teams, create a workflow from the template that posts to a channel when a webhook request is received, and allow anyone to trigger it.
2. Store the workflow's URL as the secret `notify.<name>.url`.
3. Add the instance with `type = "webhook"` and `format = "teams"`.
4. Run `butterbot notify test <name>`.

Rollback is removing the instance.

## Open Questions

Neither changes the specs or tasks of this change.

- **UniFi as a target.** A UniFi console's alarm trigger is called on the console itself with a key in a header. Whether the cluster can reach the console, and whether the console's certificate matches the name it is called by, decide if this works with `ca_file` alone.
- **A job-monitoring service.** The `exit_code` placeholder makes one usable later if the owner decides to pursue detection of missing runs.
