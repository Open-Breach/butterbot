# Deployment

Butterbot runs once and exits. Something else has to start it, once a day. This page describes the container image, a Kubernetes CronJob that runs it, and the steps for replacing an existing certificate job.

- [The container image](#the-container-image)
- [Kubernetes](#kubernetes)
- [Replacing an existing job](#replacing-an-existing-job)
- [Verified against](#verified-against)

## The container image

The image is built from the `Dockerfile` in the repository. No image is published yet, so build it and push it to your own registry:

```
docker buildx build --platform linux/amd64,linux/arm64 --tag registry.example.com/butterbot:0.1.0 --push .
```

For one platform, `docker build --tag butterbot .` is enough.

What the image is:

- **One program and nothing else.** `butterbot` is linked statically and is the only file in the image. There is no shell and no package manager. The certificate authorities Butterbot trusts for HTTPS are compiled into the program, so the image needs no certificate store.
- **For `linux/amd64` and `linux/arm64`.** The same `Dockerfile` builds both. Each platform is compiled by a compiler of that platform; nothing is cross-compiled.
- **Unprivileged.** The program runs as user and group `65532`. It needs no capability and no writable root filesystem.

How it starts:

| Started with | Runs |
|---|---|
| no arguments | `butterbot run` |
| `status` | `butterbot status` |
| `check-config` | `butterbot check-config` |
| `account new --acme <name>` | `butterbot account new --acme <name>` |

The configuration is read from `/etc/butterbot/config.toml`. Mount your file there. Secrets come from environment variables or from Azure Key Vault; see [Configuration](configuration.md).

```
docker run --rm \
  --volume "$PWD/butterbot.toml:/etc/butterbot/config.toml:ro" \
  --env BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY \
  --env BUTTERBOT__ACME__LETSENCRYPT__ACCESS_KEY \
  registry.example.com/butterbot:0.1.0 status
```

A `directory` store is the only thing that writes to the filesystem. Mount a volume at its `path`, writable for user `65532`. With Key Vault stores only, nothing is written and the root filesystem can be read-only.

**Stopping.** `docker stop` and Kubernetes send `SIGTERM`. Butterbot then starts no further certificate, removes the challenge records it has published, and exits with code 3. A certificate that the certificate authority has already been asked to issue is still downloaded and written to its stores. Give the program 60 seconds before it is killed (`docker stop --time 60`, or `terminationGracePeriodSeconds: 60`).

If the program is killed without that time, challenge records can stay behind in DNS. They do no harm to later validations. Butterbot does not remove them later, because it cannot tell them from the records of another tool; delete them by hand if they accumulate.

The log is written to standard output, one JSON object per line.

## Kubernetes

The manifest below is a reference. Change the image, the schedule, the configuration and the identity to yours. It contains:

- a ConfigMap with the configuration file;
- a ServiceAccount bound to an Azure workload identity, for the Azure providers and for Key Vault;
- a CronJob.

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: butterbot-config
  namespace: butterbot
data:
  config.toml: |
    # Leave this table out to read secrets from the environment only.
    [secrets]
    key_vault_url = "https://YOUR-CONFIG-VAULT.vault.azure.net/"

    [acme.letsencrypt]
    directory_url = "https://acme-v02.api.letsencrypt.org/directory"

    [dns.azure]
    type = "azure-dns"
    subscription_id = "00000000-0000-0000-0000-000000000000"
    resource_group = "YOUR-DNS-RESOURCE-GROUP"

    [store.vault]
    type = "azure-key-vault"
    vault_url = "https://YOUR-CERTIFICATE-VAULT.vault.azure.net/"

    [[certificate]]
    name = "wildcard-example-com"
    names = ["*.example.com", "example.com"]
    acme = "letsencrypt"
    dns = "azure"
    stores = ["vault"]
---
apiVersion: v1
kind: ServiceAccount
metadata:
  name: butterbot
  namespace: butterbot
  annotations:
    # The client id of the managed identity that has the permissions listed
    # in docs/providers.md.
    azure.workload.identity/client-id: "00000000-0000-0000-0000-000000000000"
---
apiVersion: batch/v1
kind: CronJob
metadata:
  name: butterbot
  namespace: butterbot
spec:
  # Once a day. Pick a minute that is not on the hour.
  schedule: "17 3 * * *"
  # A run that is still going is never joined by a second one.
  concurrencyPolicy: Forbid
  # A run that could not start within an hour is skipped; the next day's run
  # does the work.
  startingDeadlineSeconds: 3600
  successfulJobsHistoryLimit: 3
  failedJobsHistoryLimit: 7
  jobTemplate:
    spec:
      # A run that takes longer than an hour is stopped.
      activeDeadlineSeconds: 3600
      # A failed run is not repeated. Issuing again at once does not help and
      # uses up the limits of the certificate authority. The next day's run
      # is the retry.
      backoffLimit: 0
      template:
        metadata:
          labels:
            app.kubernetes.io/name: butterbot
            azure.workload.identity/use: "true"
        spec:
          serviceAccountName: butterbot
          restartPolicy: Never
          # Time to remove challenge records after SIGTERM.
          terminationGracePeriodSeconds: 60
          securityContext:
            runAsNonRoot: true
            runAsUser: 65532
            runAsGroup: 65532
            seccompProfile:
              type: RuntimeDefault
          containers:
            - name: butterbot
              image: registry.example.com/butterbot:0.1.0
              # No arguments: the image runs `butterbot run`.
              securityContext:
                allowPrivilegeEscalation: false
                readOnlyRootFilesystem: true
                capabilities:
                  drop: ["ALL"]
              # Secrets from the environment. Each key of the Secret is the
              # name of an environment variable, for example
              # BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY. Not needed when every
              # secret comes from Key Vault.
              envFrom:
                - secretRef:
                    name: butterbot-secrets
                    optional: true
              resources:
                requests:
                  cpu: 50m
                  memory: 64Mi
                limits:
                  memory: 256Mi
              volumeMounts:
                - name: config
                  mountPath: /etc/butterbot
                  readOnly: true
          volumes:
            - name: config
              configMap:
                name: butterbot-config
```

Why these settings:

| Setting | Value | Reason |
|---|---|---|
| `schedule` | once a day | Butterbot decides in each run what is due. Running more often gains nothing. |
| `concurrencyPolicy` | `Forbid` | Two runs at the same time would order the same certificates twice. |
| `activeDeadlineSeconds` | `3600` | A run has a deadline. Every wait inside Butterbot has a timeout of its own, so this is a second line of defence. |
| `backoffLimit` | `0` | No automatic retries. A failed certificate is tried again by the next day's run. |
| `restartPolicy` | `Never` | The same, for the container. |
| `terminationGracePeriodSeconds` | `60` | Time to remove challenge records when the pod is stopped. |
| `readOnlyRootFilesystem` | `true` | Butterbot writes nothing outside a `directory` store. |

**Secrets.** There are two ways, and they can be mixed:

- **The environment.** Create a Secret whose keys are the names of the environment variables, and keep the `envFrom` entry:

  ```
  kubectl --namespace butterbot create secret generic butterbot-secrets \
    --from-literal=BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY='<your Cloudflare API token>'
  ```

- **Azure Key Vault.** Set `secrets.key_vault_url` in the configuration. Butterbot reads the secrets from that vault with the workload identity of the pod, and saves the credentials of a newly registered ACME account there. Nothing has to be put into the cluster.

`butterbot check-config` prints the name of every secret in both sources.

**Permissions.** In Kubernetes the pod needs its workload identity binding and nothing else: no Role and no access to the Kubernetes API. The permissions of the identity in Azure, and of a Cloudflare token, are listed in [Providers](providers.md).

**A directory store.** Add a volume for it, mount it at the store's `path`, and set `fsGroup: 65532` in the pod's `securityContext` so that the program may write there.

**Running another command in the cluster.** Start a one-off job from the CronJob and replace its arguments, or run the image from a terminal with the same configuration:

```
kubectl --namespace butterbot create job --from=cronjob/butterbot butterbot-manual
```

**Monitoring.** A run ends with exit code 0, 1, 2 or 3, which the Job shows as succeeded or failed. To notice that the job does not run at all, alert on the absence of the `run_completed` event in the log; see [Events](events.md).

## Replacing an existing job

These steps replace a job that already maintains certificates in a Key Vault. The two programs must never work on the same certificates at the same time.

1. **Grant the permissions.** Give the identity Butterbot runs as the permissions listed in [Providers](providers.md): on the DNS zones, on the vault that stores the certificates, and on the vault that holds the configuration secrets.

2. **Write the configuration.** One entry per existing certificate, so that Butterbot finds what is already there:
   - the entry's `name` is the name the certificate has in the vault today, for example `wildcard-example-com`;
   - `names` are the names in the existing certificate, for example `*.example.com` and `example.com`;
   - `key_type = "rsa2048"` and the Key Vault store with `format = "pfx"`, if that is what the existing certificates are.

3. **Check, without changing anything.** From a terminal, signed in with `az login`:

   ```
   butterbot check-config
   butterbot status
   ```

   Every certificate must be shown as present in its store and not due. If one is shown as due, find out why before going on: the names or the name of the entry probably do not match the stored certificate.

4. **Issue a test certificate.** Add one entry with a name of its own, a zone you can throw away, the staging server of the certificate authority and a store for tests. Then run it:

   ```
   butterbot run --only <the test entry>
   ```

   Do this once for each type of DNS provider in use. A certificate from a staging server must never go to a store that production reads from; that is why the test entry has its own store.

5. **Switch over.** Disable the schedule of the old job and enable Butterbot's CronJob in the same change, so that the two never run against the same certificates.

6. **Watch the first renewal.** When the first certificate becomes due, check the `renewed` event in the log and the certificate in the store. The first renewal of a certificate that the old job issued logs one warning, that the server refused the order that names the certificate it replaces. That is expected: the certificate was issued to the old job's account. Butterbot places the order again and the certificate is issued.

**Going back.** Enable the old job's schedule and suspend Butterbot's CronJob. Certificates written by either program are valid for the other, as long as name, format and key type are the same.

## Verified against

What has been checked, and how:

| What | How | Result |
|---|---|---|
| Issuance, a second run that does nothing, a forced renewal, a failed validation, an interrupted run | Automated tests against the ACME test server Pebble and its challenge DNS server, with the `directory` store. They run on every change. | Pass |
| The container image on `linux/amd64` and `linux/arm64`: default command, other commands, unprivileged user, read-only root filesystem, stopping during a run | Automated tests that start the image with Docker, on a machine of each platform. They run on every change. | Pass |
| The CronJob manifest on this page | `kubectl apply --dry-run=client`, on every change. | Pass |
| Azure DNS, Cloudflare and Azure Key Vault | Automated tests against HTTP exchanges constructed from the providers' API documentation. No real account was used. | Pass |

### Runs against real services, 24 November 2026

The owner ran Butterbot from a terminal against Let's Encrypt, Cloudflare, Azure DNS and an Azure Key Vault. This was the production server of Let's Encrypt, not the staging server the end-to-end check names. The table records what `butterbot status` showed after the runs. The commands of the runs themselves and their logs were not recorded. The names below stand in for the real ones.

| Check | What `butterbot status` showed afterwards | Result |
|---|---|---|
| Issuance through Cloudflare, into Key Vault and a `directory` store | Five entries, each a wildcard and its base name. Every certificate is in both stores with the same expiry, 90 days ahead, and is not due. The five expiry times lie within one minute, as they do when certificates are issued side by side. | Pass |
| Issuance through Azure DNS, into Key Vault | One entry, a wildcard and its base name, whose certificate had been deleted from the vault before the run. The vault holds a new certificate, 90 days ahead, and it is not due. | Pass |
| Certificates of another tool are left alone | Fourteen entries whose certificates another tool had put into the vault. Each is shown with its names and its expiry and as not due. Their expiry was the same before and after the runs. | Pass |

Two entries from that output:

```
wildcard-example-org: not due, becomes due 2027-01-22T03:41:14Z
  names: *.example.org, example.org
  store vault: a certificate for *.example.org, example.org, expires 2027-02-22T00:57:09Z
  store local: a certificate for *.example.org, example.org, expires 2027-02-22T00:57:09Z

wildcard-example-net: not due, becomes due 2027-01-22T16:35:27Z
  names: *.example.net, example.net
  store vault: a certificate for *.example.net, example.net, expires 2027-02-22T13:51:22Z
```

The first went through Cloudflare, the second through Azure DNS.

### Not yet verified

These parts of the end-to-end check have not been run, or their result was not recorded:

| Check | Command | Result |
|---|---|---|
| The configuration is valid and complete | `butterbot check-config` | not recorded |
| Before the first run, the certificates are due | `butterbot status` | not recorded |
| The challenge records are gone after a run | `dig TXT _acme-challenge.<name> +short` prints nothing | not yet run |
| A consumer reads a certificate from the vault and receives the certificate, its chain and the private key | `az keyvault secret show --vault-name <vault> --name <entry> --query value` (the value is the PFX bundle, base64-encoded) | not yet run |
| A second run does nothing | `butterbot run` | not yet run |
| Forced renewal | `butterbot run --only <entry> --force` | not yet run |
| Renewal of a certificate that another tool's account issued: one warning that the server refused the order naming it, then the certificate is issued | `butterbot run --only <entry of the other tool> --force` | not yet run |
| Interruption: `SIGTERM` during the propagation wait gives exit code 3 and leaves no challenge record | `butterbot run --only <entry> --force`, then `SIGTERM` | not yet run |
| The same against the staging server of Let's Encrypt, with a test vault | a test entry of its own, as in step 4 of the cutover | not yet run |
