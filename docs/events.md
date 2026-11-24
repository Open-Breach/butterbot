# Events

A run reports what happened through four kinds of event. Other systems can react to them, or notice that one is missing.

| Event | When |
|---|---|
| `run_completed` | Once at the end of every run that reaches its end. |
| `failure` | Once for every certificate that failed in the run. |
| `renewed` | Once for every certificate that was issued and written to all its stores. |
| `expiring` | Once for every certificate that was due, was not renewed in this run, and is close to expiry. |

The names of the events and of their fields are an interface. They stay the same across versions.

## Where events go

Every event is written to the log, as one record that contains all the fields of the event. Events are written whatever `--log-level` is set to.

Events are also delivered to every configured notifier (`[notify.<name>]`). A notifier receives every event, or only the kinds it selects with `events`. There is one notifier type, `webhook`, which sends each event as an HTTP request, for example to a Microsoft Teams workflow. [Notifications](notifications.md) shows how to set one up, and [Configuration](configuration.md) lists its settings.

Within a run, a notifier receives `run_completed` after every other event of that run.

A notifier that cannot deliver an event is logged and never changes the outcome or the exit code of a run. The record has the name of the notifier, the kind of the event and the reason, and it has no `event` field, so it is never taken for the event itself:

| Field | Meaning |
|---|---|
| `notifier` | The name of the notifier instance. |
| `undelivered_event` | The kind of the event that was not delivered. |
| `reason` | The status the endpoint answered with, or the kind of error. Never the URL. |

An event that could not be delivered is not sent again later. The log always has it.

## Monitoring that the job runs

`run_completed` is the signal that a run finished. It is written when nothing was due and when certificates failed. It is not written when the run was interrupted by a signal or stopped with a configuration error.

So the absence of `run_completed` over a period means that no run finished in that period. Butterbot cannot report that it did not run. Set up an alert outside Butterbot that fires when no `run_completed` record has been seen for longer than the schedule allows, for example for 26 hours with a daily schedule.

## Common fields

Every event has these two fields, in this position:

| Field | Type | Meaning |
|---|---|---|
| `event` | text | The kind: `run_completed`, `failure`, `renewed` or `expiring`. |
| `timestamp` | text | When the event happened, in ISO 8601 format, in UTC, to the second. |

All times in events have this format, for example `2026-11-23T02:00:41Z`. A field that has no value is written as `null`. It is never left out.

## `run_completed`

| Field | Type | Meaning |
|---|---|---|
| `outcome` | text | `success` when every checked certificate was either not due or renewed. `failed` when at least one certificate failed. |
| `checked` | number | How many certificates were checked. |
| `renewed` | number | How many were issued and written to all their stores. |
| `failed` | number | How many failed. |
| `duration_seconds` | number | How long the run took, in whole seconds. |
| `soonest_certificate` | text or `null` | The name of the configured certificate that expires soonest. `null` when no store holds a certificate. |
| `soonest_expiry` | text or `null` | When that certificate expires. |

```json
{
  "event": "run_completed",
  "timestamp": "2026-11-23T02:00:41Z",
  "outcome": "failed",
  "checked": 15,
  "renewed": 2,
  "failed": 1,
  "duration_seconds": 41,
  "soonest_certificate": "api-example-org",
  "soonest_expiry": "2026-12-05T08:15:00Z"
}
```

A run in which nothing is configured or stored yet:

```json
{
  "event": "run_completed",
  "timestamp": "2026-11-23T02:00:03Z",
  "outcome": "success",
  "checked": 0,
  "renewed": 0,
  "failed": 0,
  "duration_seconds": 3,
  "soonest_certificate": null,
  "soonest_expiry": null
}
```

## `failure`

| Field | Type | Meaning |
|---|---|---|
| `certificate` | text | The name of the certificate. |
| `step` | text | The step that failed, for example `propagation` or `validate`. |
| `reason` | text | Why it failed. For a rejected validation this includes the reason the ACME server gave. |
| `stored_expiry` | text or `null` | When the certificate that is currently stored expires. `null` when none is stored. |

```json
{
  "event": "failure",
  "timestamp": "2026-11-23T02:00:38Z",
  "certificate": "api-example-org",
  "step": "validate",
  "reason": "the server rejected the challenge for api.example.org: incorrect TXT record",
  "stored_expiry": "2026-12-05T08:15:00Z"
}
```

A failure of a certificate that has never been issued:

```json
{
  "event": "failure",
  "timestamp": "2026-11-23T02:00:38Z",
  "certificate": "new-example-net",
  "step": "propagation",
  "reason": "the challenge records were not served after 5m",
  "stored_expiry": null
}
```

## `renewed`

| Field | Type | Meaning |
|---|---|---|
| `certificate` | text | The name of the certificate. |
| `names` | list of text | The DNS names the new certificate covers. |
| `expiry` | text | When the new certificate expires. |
| `acme` | text | The ACME server instance that issued it. |
| `stores` | list of text | The store instances it was written to. |

```json
{
  "event": "renewed",
  "timestamp": "2026-11-23T02:00:35Z",
  "certificate": "wildcard-example-com",
  "names": ["*.example.com", "example.com"],
  "expiry": "2027-02-21T01:00:34Z",
  "acme": "letsencrypt",
  "stores": ["vault", "local"]
}
```

## `expiring`

This event is the escalation of a renewal that keeps failing. It is emitted when a certificate was due, was not renewed in this run, and its stored certificate has less than half of its renewal period left or has already expired. With a renewal period of 30 days, that is from 15 days before expiry.

| Field | Type | Meaning |
|---|---|---|
| `certificate` | text | The name of the certificate. |
| `expiry` | text | When the stored certificate expires or expired. |

```json
{
  "event": "expiring",
  "timestamp": "2026-11-23T02:00:38Z",
  "certificate": "api-example-org",
  "expiry": "2026-12-05T08:15:00Z"
}
```

## Events in the log

In the log, an event is a record like any other: it has `timestamp`, `level` and `message`, followed by the fields of the event. The `timestamp` of the record is the timestamp of the event. To find events, select the records that have an `event` field.

| Event | Level |
|---|---|
| `run_completed` | `info` |
| `renewed` | `info` |
| `expiring` | `warn` |
| `failure` | `error` |

With the JSON log format, which is the default when the output is not a terminal, the `run_completed` event above is this line:

```text
{"timestamp":"2026-11-23T02:00:41Z","level":"info","message":"run completed: 15 checked, 2 renewed, 1 failed","event":"run_completed","outcome":"failed","checked":15,"renewed":2,"failed":1,"duration_seconds":41,"soonest_certificate":"api-example-org","soonest_expiry":"2026-12-05T08:15:00Z"}
```

With the text format the same event is this line:

```text
2026-11-23T02:00:41Z INFO  run completed: 15 checked, 2 renewed, 1 failed event=run_completed outcome=failed checked=15 renewed=2 failed=1 duration_seconds=41 soonest_certificate=api-example-org soonest_expiry=2026-12-05T08:15:00Z
```
