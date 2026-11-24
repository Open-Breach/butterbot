# Spec Delta

## Purpose

Defines the `webhook` notifier: how Butterbot delivers its run events as HTTP requests to a configured endpoint, what those requests contain, and how delivery behaves when the endpoint fails.

## ADDED Requirements

### Requirement: Webhook notifier type
The system SHALL support notifier instances of type `webhook`. For every event such an instance selects, the system SHALL send one HTTP request to the instance's `url`. Several webhook instances MAY be configured, each with its own URL and event selection.

#### Scenario: One notifier
- **WHEN** a `webhook` instance selects `failure` and a certificate fails
- **THEN** one request is sent to that instance's URL for that failure

#### Scenario: Two notifiers
- **WHEN** two `webhook` instances both select `run_completed` and a run finishes
- **THEN** each instance's URL receives one request

### Requirement: Target URL is a secret
The `url` of a webhook instance SHALL be treated as a secret identified by its configuration path `notify.<name>.url`, and MAY be supplied by any configuration layer. It SHALL use the `https` or `http` scheme. An `http` URL SHALL be accepted, with a warning in the log that the request is not encrypted.

#### Scenario: URL from the vault
- **WHEN** `notify.teams.url` is absent from the configuration files and the vault holds the secret `notify--teams--url`
- **THEN** the notifier delivers to the URL from the vault

#### Scenario: Unsupported scheme
- **WHEN** the URL of a webhook instance starts with `ftp://`
- **THEN** the notifier is reported as misconfigured and receives no events

### Requirement: Notifier without a URL is skipped
When the `url` of a webhook instance is supplied by no layer, the system SHALL log an error naming the configuration path at the start of the run, SHALL deliver no events to that instance, and SHALL otherwise run normally. `check-config` SHALL list the path as a missing secret.

#### Scenario: Run with a missing URL
- **WHEN** `notify.teams.url` is supplied by no layer and a run starts
- **THEN** the log names `notify.teams.url` as missing, certificates are processed normally, and the exit code depends only on the certificates

### Requirement: Method and default body
The system SHALL send the request with the method given by `method`, `POST` by default or `GET`. With `POST` and no other body setting, the body SHALL be the event as a JSON object with all its fields and the content type `application/json`. With `GET`, no body SHALL be sent.

#### Scenario: Default request
- **WHEN** a webhook instance has only `type` and `url` configured and a `renewed` event is delivered
- **THEN** a `POST` request is sent whose body is the JSON object of that event, including its `event` field

#### Scenario: GET request
- **WHEN** a webhook instance has `method = "GET"`
- **THEN** each delivery is a `GET` request without a body

### Requirement: Placeholders
The `url`, the header values and a custom `body` MAY contain placeholders written as `{{name}}`. The system SHALL support these names: `event` (the event kind), `summary` (the event in one sentence), `details` (the event's fields as lines of name and value), `exit_code` (for `run_completed`, the exit code its outcome maps to), `event_json` (the event as a JSON object) and `access_key`.

#### Scenario: Exit code in the URL
- **WHEN** the URL ends in `/{{exit_code}}` and a run completes with one failed certificate
- **THEN** the request goes to the URL ending in `/1`

#### Scenario: Successful run
- **WHEN** the URL ends in `/{{exit_code}}` and a run completes with no failed certificate
- **THEN** the request goes to the URL ending in `/0`

### Requirement: Missing and unknown placeholders
A placeholder that has no value for the event being delivered SHALL be replaced by nothing. A placeholder whose name is not in the supported list SHALL be a configuration error.

#### Scenario: No value for this event
- **WHEN** the URL ends in `/{{exit_code}}` and a `renewed` event is delivered
- **THEN** the request goes to the URL ending in `/`

#### Scenario: Unknown placeholder
- **WHEN** a body template contains `{{certificat}}`
- **THEN** the configuration is rejected and the message names the notifier and the placeholder

### Requirement: Placeholder values are escaped for their position
The system SHALL insert a placeholder value so that it cannot change the structure of what surrounds it: percent-encoded in a URL, and escaped as the content of a JSON string in a body whose content type is JSON. `event_json` SHALL be inserted into a body as a JSON object, unescaped.

#### Scenario: Reason with quotation marks
- **WHEN** a JSON body template contains `"text": "{{details}}"` and the failure reason contains a quotation mark and a line break
- **THEN** the body sent is valid JSON and the text contains the quotation mark and the line break

### Requirement: Custom body
When `body` is set on a webhook instance, the system SHALL send that text as the request body after replacing its placeholders, with the content type given by `content_type`, `application/json` by default. Setting both `body` and `format` SHALL be a configuration error.

#### Scenario: Custom JSON body
- **WHEN** `body` is `{"message": "{{summary}}"}` and a certificate named `wildcard-example-com` fails at step `validate`
- **THEN** the body sent is a JSON object whose `message` says that `wildcard-example-com` failed at step `validate`

#### Scenario: Body and format together
- **WHEN** a webhook instance sets both `body` and `format`
- **THEN** the configuration is rejected

### Requirement: Teams format
With `format = "teams"`, the system SHALL send a body that a Microsoft Teams workflow started by a webhook request accepts: a message carrying one Adaptive Card that shows the event's summary as its heading and each field of the event as a name and a value. `format = "json"` SHALL select the default body.

#### Scenario: Failure posted to Teams
- **WHEN** a webhook instance has `format = "teams"` and a certificate fails
- **THEN** the body is a JSON message with one Adaptive Card attachment whose heading is the failure's summary and whose fields include the certificate, the step and the reason

### Requirement: Headers and access key
The system SHALL send the headers listed in the instance's `headers` table with every request. A header value MAY contain the `access_key` placeholder, whose value is the secret at `notify.<name>.access_key`. When any template of an instance uses `access_key`, that secret SHALL be required for the instance.

#### Scenario: Key in a header
- **WHEN** `headers` contains `X-API-KEY = "{{access_key}}"` and `notify.console.access_key` is supplied by the environment
- **THEN** every request carries the header `X-API-KEY` with that key as its value

#### Scenario: Key referenced but missing
- **WHEN** a header uses `{{access_key}}` and `notify.console.access_key` is supplied by no layer
- **THEN** the instance is treated like a notifier without a URL, and `check-config` lists `notify.console.access_key` as missing

### Requirement: Success and retries
A delivery SHALL count as successful when the endpoint answers with a 2xx status. On a network error, a 5xx status or status 429, the system SHALL try again, up to 3 attempts in total and within the delivery time limit of the event bus. Any other status SHALL be a failed delivery without a retry. Each attempt SHALL be limited by the instance's `timeout`, 10 seconds by default.

#### Scenario: Endpoint fails once
- **WHEN** the endpoint answers the first attempt with status 503 and the second with status 200
- **THEN** the event counts as delivered and nothing is logged as an error

#### Scenario: Request rejected
- **WHEN** the endpoint answers with status 400
- **THEN** the delivery fails after that one attempt and the failure is logged

### Requirement: Redirects are not followed
The system SHALL NOT follow a redirect returned by the endpoint. A 3xx answer SHALL be a failed delivery.

#### Scenario: Endpoint redirects
- **WHEN** the endpoint answers with status 302 and a `Location` header
- **THEN** no request is sent to that location and the delivery fails

### Requirement: Completion is delivered last
The `run_completed` event of a run SHALL be delivered to a webhook instance after every other event of that run that the instance selects.

#### Scenario: Failure and completion
- **WHEN** a run has one failed certificate and an instance selects all events
- **THEN** the endpoint receives the `failure` request before the `run_completed` request

### Requirement: Additional trusted certificates
When `ca_file` is set on a webhook instance, the system SHALL trust the certificates in that PEM file in addition to the public certificate authorities when it connects to the instance's URL. The system SHALL NOT offer a way to switch certificate verification off.

#### Scenario: Endpoint with a private certificate authority
- **WHEN** the endpoint's certificate is issued by a private authority whose certificate is in `ca_file`
- **THEN** the delivery succeeds

#### Scenario: Untrusted endpoint
- **WHEN** the endpoint's certificate is issued by an authority that is neither public nor in `ca_file`
- **THEN** the delivery fails and the failure is logged

### Requirement: Nothing secret in the output
The system SHALL NOT write the URL of a webhook instance, any part of it, its `access_key` or its header values to the log, to error messages or to the output of any command. A delivery failure SHALL be logged with the notifier's name, the event kind and either the HTTP status or the kind of error.

#### Scenario: Failed delivery
- **WHEN** a delivery fails with status 404
- **THEN** the log names the notifier, the event kind and status 404, and contains neither the URL nor the key

#### Scenario: Connection error
- **WHEN** the endpoint's host cannot be resolved
- **THEN** the logged error says that the connection failed and does not contain the URL
