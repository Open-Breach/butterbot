# Tasks

## 1. Configuration

- [x] 1.1 Add the `webhook` type to `parse_notify` with the settings `url`, `method`, `format`, `body`, `content_type`, `headers`, `access_key`, `ca_file`, `timeout` and the existing `events`, returning a typed notifier instance; verify with parser tests for a minimal instance, a full instance, an unknown key and an unknown type (whose message lists `webhook`)
- [x] 1.2 Validate the instance: method is `POST` or `GET`, `format` is `json` or `teams`, `body` and `format` are not both set, header names are valid; verify one test per rule
- [x] 1.3 Treat `url` and `access_key` as secrets by path and add them to the expected-secrets listing, the key only when a template uses it; verify that `check-config` lists `notify.<name>.url` with its environment and Key Vault names, and lists the key only when referenced

## 2. Templates

- [x] 2.1 Implement template parsing with the fixed placeholder list, rejecting unknown placeholders when the configuration is loaded; verify with tests for each placeholder and for an unknown one
- [x] 2.2 Implement rendering with position-aware escaping (URL, header, JSON body, other body) and the unescaped `event_json`; verify with tests using a failure reason that contains a quotation mark, a backslash, a line break and non-ASCII text, checking that a rendered JSON body parses
- [x] 2.3 Implement the `details` and `exit_code` values for all four event kinds; verify against the existing example events, including the empty `exit_code` for events other than `run_completed`

## 3. Webhook sink

- [x] 3.1 Implement `WebhookSink` as an `EventSink`: build the request from method, rendered URL, headers and body, with redirects disabled and the per-attempt timeout; verify against a local test server for the default body, a custom body and a `GET` request
- [x] 3.2 Add retries for network errors, 5xx and 429 (3 attempts, within the bus's time limit) and no retry for other statuses; verify with a test server that fails once, one that always fails, and one that answers 400
- [x] 3.3 Strip the URL from every transport error and log failures with notifier name, event kind and status or error kind only; verify for each failure path that the captured log contains neither the URL nor the key
- [x] 3.4 Support `ca_file` through `http::client_with_roots`; verify delivery to a test server with a private certificate authority succeeds with the file and fails without it
- [x] 3.5 Register a sink per configured instance at start, skip an instance whose URL or referenced key is missing or whose URL has an unsupported scheme, and warn for `http`; verify the three scenarios of "Target URL is a secret" and "Notifier without a URL is skipped", and that the run's exit code is unchanged
- [x] 3.6 Add an integration test of a full run with a webhook instance: a failed certificate produces a `failure` request and then a `run_completed` request, and an unreachable endpoint leaves the exit code as it would be without the notifier

## 4. Teams format

- [x] 4.1 Check Microsoft's current documentation for the payload accepted by a workflow with the "When a Teams webhook request is received" trigger and record the source and date in a comment next to the implementation
- [x] 4.2 Implement `format = "teams"` as a `serde_json` value with one Adaptive Card (summary heading, one fact per event field); verify with snapshot tests for all four event kinds

## 5. Command line

- [x] 5.1 Add `butterbot notify test <name> [--event <kind>]`, delivering one sample event marked as a test through the real sink without writing an event to the log; verify the four scenarios of "The notify test command" against a local test server, including the exit codes
- [x] 5.2 Update `--help` text and the command list in the README; verify the commands shown run as written

## 6. Documents

- [x] 6.1 Replace the "no notifier type" text in `docs/configuration.md` and `docs/events.md` with the `webhook` settings, the placeholder table and the secret names; verify every setting accepted by the parser appears in the document
- [x] 6.2 Write `docs/notifications.md` with a Teams walk-through (create the workflow, store the URL, configure, test), a health-check style example using `{{exit_code}}`, and an example with a key in a header; verify each configuration example in the document loads without errors in a test

## 7. Checks

- [ ] 7.1 Run `openspec validate add-webhook-notifier --strict` and confirm every scenario in the three delta specs has a test; verify by listing the scenario-to-test mapping in the pull request description
- [ ] 7.2 Deliver a test event and a real failure to an actual Teams workflow and confirm the card shows the summary and the fields; verify by adding the result to the "Verified against" table in `docs/deployment.md` (needs a Teams workflow; the owner does this)

## Workflow follow-up

- Archive `add-certificate-maintainer`, or sync its specs, before archiving this change: the `run-events` and `command-line` deltas here apply to specs that change introduces.
