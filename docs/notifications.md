# Notifications

Butterbot writes every event of a run to the log. A notifier also sends the events somewhere, so that a failed renewal reaches a person without anyone reading logs.

There is one notifier type, `webhook`. For every event it selects, it sends one HTTP request to a URL. This document shows three uses of it:

- [Microsoft Teams](#microsoft-teams): a card in a channel or chat.
- [A job monitor](#a-job-monitor): a request per run that says whether the run succeeded.
- [An endpoint that wants a key](#an-endpoint-that-wants-a-key): a body of your own and a key in a header.

[Configuration](configuration.md) lists every setting, and [Events](events.md) describes the events and their fields.

## What every webhook has in common

- **The URL is a secret.** A webhook URL carries its own credential: whoever has it can post. Keep it out of the configuration file. Supply it as the environment variable `BUTTERBOT__NOTIFY__<NAME>__URL` or as the Key Vault secret `notify--<name>--url`. Butterbot never writes the URL to the log or to the output of a command.
- **A notifier never changes a run.** When a delivery fails, the log has a record with the name of the notifier, the kind of the event and the reason. The certificates are processed as before, and the exit code is the same.
- **Choose the events.** Without `events` a notifier receives all four kinds. A run that renews nothing still sends `run_completed`, every day. For a channel that people read, `failure` and `expiring` are usually enough.
- **Test it.** `butterbot notify test <name>` sends one sample event through the notifier and says whether it was delivered. The sample is recognisable: its certificate is called `butterbot-test`.

## Microsoft Teams

Teams receives webhook requests through a workflow of its Workflows app. The workflow has a URL, and what is sent to the URL is posted as a card.

### 1. Create the workflow

Microsoft describes the steps in [Create incoming webhooks with Workflows for Microsoft Teams](https://support.microsoft.com/en-us/office/create-incoming-webhooks-with-workflows-for-microsoft-teams-8ae491c7-0394-4861-ba59-055e33f75498) and in the [incoming webhook article](https://learn.microsoft.com/en-us/microsoftteams/platform/webhooks-and-connectors/how-to/add-incoming-webhook). In short, as those pages had it on 24 November 2026:

1. In Teams, go to the team and channel that is to receive the messages.
2. Select **More options (...)** next to the channel, then **Workflows**.
3. Search for the template **Send webhook alerts to a channel** and select it. For a chat instead of a channel, the template is **Send webhook alerts to a chat**.
4. Choose the team and the channel, or the chat, and select **Save**.
5. Teams shows the details of the new workflow. Copy the webhook URL from there.

Things to know:

- **Anyone must be allowed to trigger the workflow.** The trigger of the workflow, "When a Teams webhook request is received", can be limited to users of your organisation. Such a workflow expects a Microsoft sign-in with every request, which Butterbot cannot present. There are also templates named "… from specific people …" and "… from people in an org …". Microsoft says that each template has a different authentication type and does not say which has which, so use the templates named in step 3, and check the trigger if the test in step 4 is refused.
- **The workflow belongs to the person who created it**, not to the team or the channel. When that person leaves, the workflow can stop working. Microsoft recommends adding co-owners.
- **The URL can be looked up again** by editing the workflow in the Power Automate designer.

### 2. Store the URL

The instance below is called `teams`, so its URL is the secret `notify.teams.url`.

| Source | Name |
|---|---|
| Environment | `BUTTERBOT__NOTIFY__TEAMS__URL` |
| Key Vault | `notify--teams--url` |

In a terminal, for a first test:

```sh
export BUTTERBOT__NOTIFY__TEAMS__URL='<the URL of the workflow>'
```

For the scheduled job, store it where the job's other secrets are; see [Deployment](deployment.md).

### 3. Configure the notifier

```toml
[notify.teams]
type = "webhook"
format = "teams"
events = ["failure", "expiring"]
```

`format = "teams"` sends the message a workflow expects: one card whose heading is the summary of the event, for example "certificate api-example-org failed at step validate", followed by every field of the event as a name and a value.

### 4. Test it

```sh
butterbot check-config
butterbot notify test teams
```

`check-config` lists `notify.teams.url` and says whether it was found. `notify test` sends a sample `failure` and prints one of:

```text
A failure test event was delivered to the notifier teams.
A failure test event was not delivered to the notifier teams: the endpoint answered with status 400.
```

When it was delivered, the channel shows a card with the heading "certificate butterbot-test failed at step validate" and a reason that says it is a test. The exit code is 0 when the sample was delivered, 1 when it was not, and 2 when the notifier is not configured or its URL is missing.

To see the other cards:

```sh
butterbot notify test teams --event expiring
butterbot notify test teams --event renewed
butterbot notify test teams --event run_completed
```

Run the test again whenever the workflow was changed or replaced.

## A job monitor

Services that watch scheduled jobs often have one URL per job, with the result of the job at the end of the URL. The placeholder `{{exit_code}}` is made for this: for `run_completed` it is `0` when no certificate failed and `1` when one did.

```toml
[notify.monitor]
type = "webhook"
method = "GET"
events = ["run_completed"]
```

The URL is the secret `notify.monitor.url`, with the placeholder in it:

```text
https://monitor.example.com/ping/YOUR-CHECK-ID/{{exit_code}}
```

Select `run_completed` and nothing else. For every other event the placeholder is empty, and the request would go to the URL that ends in `/`.

`butterbot notify test monitor --event run_completed` sends the sample of a successful run, so it calls the URL that ends in `/0`.

A run that is interrupted or stops with a configuration error has no `run_completed` event and sends nothing. A monitor that expects a request every day notices that.

## An endpoint that wants a key

Some receivers want a body in their own shape and a key in a header. This example also trusts a private certificate authority, as a device in your own network may need.

```toml
[notify.console]
type = "webhook"
events = ["failure", "expiring"]
body = '{"title": "{{summary}}", "text": "{{details}}"}'
ca_file = "/etc/butterbot/console-ca.pem"

[notify.console.headers]
X-API-KEY = "{{access_key}}"
```

Two secrets belong to this instance:

| Secret | Environment | Key Vault |
|---|---|---|
| `notify.console.url` | `BUTTERBOT__NOTIFY__CONSOLE__URL` | `notify--console--url` |
| `notify.console.access_key` | `BUTTERBOT__NOTIFY__CONSOLE__ACCESS_KEY` | `notify--console--access-key` |

The key is required because a template uses `{{access_key}}`. Write the placeholder into the header, never the key itself: the configuration file is not a place for secrets.

About the body:

- Write it in single quotation marks, so that TOML leaves the double quotation marks of the JSON alone.
- Put quotation marks around a placeholder yourself, as in `"title": "{{summary}}"`. Butterbot escapes the value, so a reason that contains a quotation mark or a line break does not break the JSON.
- `{{event_json}}` is the whole event as a JSON object and is written without quotation marks: `'{"source": "butterbot", "event": {{event_json}}}'`.
- With another `content_type`, for example `text/plain`, values are inserted as they are.

Without `body` and `format`, a webhook sends the event itself as JSON, which is what a receiver that you write yourself wants:

```toml
[notify.archive]
type = "webhook"
```

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

## When a delivery fails

The log has one record for a delivery that failed:

```text
{"timestamp":"2026-11-23T02:01:00.000Z","level":"error","message":"an event could not be delivered to a notifier","notifier":"teams","undelivered_event":"failure","reason":"the endpoint answered with status 404"}
```

| Reason | What to check |
|---|---|
| `the endpoint answered with status 4xx` | The URL is wrong or no longer valid, or the receiver does not accept the body. For Teams: the workflow was deleted or switched off, or its trigger does not allow anyone. |
| `the endpoint answered with status 3xx; redirects are not followed` | The URL is not the final address of the receiver. Use the address it redirects to. |
| `the endpoint answered with status 5xx` or `429` | The receiver had a problem or was busy. Butterbot tried three times. |
| `the connection failed` | The host name is wrong, the receiver is down, or the network does not let Butterbot reach it. |
| `the connection failed: the certificate of the endpoint was not accepted` | The certificate of the receiver is not from a public authority. Set `ca_file`. |
| `the endpoint did not answer within 10s` | The receiver is slow. `timeout` sets the limit of one attempt. |

A notifier that cannot work at all is reported once, at the start of the run, and receives no events:

```text
{"timestamp":"2026-11-23T02:00:00.000Z","level":"error","message":"a notifier is misconfigured and receives no events","notifier":"teams","reason":"notify.teams.url is not set; supply it as the environment variable BUTTERBOT__NOTIFY__TEAMS__URL or as the Key Vault secret notify--teams--url"}
```

An event that was not delivered is not sent again later. It is in the log.
