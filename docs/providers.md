# Providers

Butterbot talks to three kinds of provider. Each is a named instance in the configuration, and a certificate entry names the instances it uses.

- **ACME servers** issue certificates.
- **DNS providers** publish the records that prove control of a domain.
- **Certificate stores** receive the issued certificates.

This document lists the settings of each provider and the permissions the identity Butterbot runs as needs. [configuration.md](configuration.md) explains how settings and secrets reach the program.

Grant no more than what is listed here. The identity can create DNS records in every configured zone and write certificates to every configured store, so its permissions are worth keeping narrow.

## The Azure identity

The providers `azure-dns` and `azure-key-vault`, and Key Vault as a configuration layer, sign in to Azure with the identity Butterbot runs as. Nothing is configured for it:

- In a cluster, the workload identity of the pod is used. It is detected by the environment variable `AZURE_FEDERATED_TOKEN_FILE`, which the cluster sets together with `AZURE_CLIENT_ID` and `AZURE_TENANT_ID`.
- Otherwise, the account signed in to the Azure CLI is used (`az login`). This is for running Butterbot from a terminal.

In Kubernetes the pod needs its workload identity binding and nothing else. Butterbot does not call the Kubernetes API.

## ACME servers: `[acme.<name>]`

Any server that implements ACME (RFC 8555) can be used. A server is defined by its directory URL; adding one needs configuration only.

| Setting | Default | Meaning |
|---|---|---|
| `directory_url` | required | The directory URL of the server. |
| `access_key` | not set | Secret. The account credentials. |
| `eab_kid` | not set | Key identifier for external account binding. |
| `eab_key` | not set | Secret. Key for external account binding. |
| `profile` | not set | Certificate profile to request. Without it no profile is requested. |
| `renew_before` | `"30d"` | How long before expiry a certificate is renewed. |
| `key_type` | `"rsa2048"` | `rsa2048`, `rsa3072`, `rsa4096`, `ecdsa-p256` or `ecdsa-p384`. |
| `ari` | `true` | Whether the server is asked for renewal information. |

`profile`, `renew_before` and `key_type` can be overridden on a certificate entry.

**The account.** An ACME server needs an account. Its credentials are the secret `acme.<name>.access_key`.

- With Key Vault as a configuration layer, Butterbot registers the account on the first run and saves the credentials to the vault.
- Without it, register the account once with `butterbot account new --acme <name>` and supply the printed credentials through the environment.

**External account binding.** Some servers require new accounts to be bound to an existing customer account. Set `eab_kid` and supply `eab_key`; both are used when the account is registered.

**`renew_before` and renewal information.** With `ari = true`, a certificate is renewed at the earlier of two times: when `renew_before` is reached, and when the window the server suggests has started. Let's Encrypt suggests renewal when about one third of the lifetime is left. A shorter `renew_before` then has no practical effect, because the server's window opens first. Set `ari = false` to make `renew_before` the only rule.

**Staging servers.** Define a staging server as its own instance, and test with a certificate entry that has its own name and its own store. A certificate from a staging server is not trusted by clients and must not be written to a store that production reads.

## DNS providers: `[dns.<name>]`

Settings of every DNS provider:

| Setting | Default | Meaning |
|---|---|---|
| `type` | required | `azure-dns` or `cloudflare`. |
| `ttl` | `"60s"` | Time-to-live of the challenge records. |
| `propagation_check` | `"authoritative"` | `authoritative`, `resolver` or `off`. See below. |
| `propagation_resolvers` | `["1.1.1.1", "8.8.8.8"]` | Resolvers asked in `resolver` mode. |
| `propagation_interval` | `"10s"` | Pause between two propagation checks. |
| `propagation_timeout` | `"5m"` | How long to wait for the records to be served. |
| `settle` | `"30s"` | Wait after a successful check before validation is requested. |

`propagation_timeout` and `settle` can be overridden on a certificate entry.

**Zones.** A challenge record is published in the zone of the instance whose name is the longest suffix of the record name. If an instance hosts `example.com` and `dev.example.com`, the record for `*.dev.example.com` goes into `dev.example.com`. If no zone of the instance matches, the certificate fails.

**The propagation check.** Before the ACME server is asked to validate, Butterbot checks that the records are being served:

- `authoritative` asks every authoritative nameserver of the zone directly and goes on only when each returns all published values. If the challenge name is a CNAME into another zone, the nameservers of that zone are asked. This needs outbound DNS (UDP and TCP port 53) to arbitrary servers.
- `resolver` asks the resolvers in `propagation_resolvers`. Use it where outbound DNS is limited to certain resolvers. Their caching can delay the result.
- `off` does not check. Only the `settle` wait takes place.

**What Butterbot removes.** After validation, successful or not, Butterbot removes the values it published and nothing else. A value left behind by a run that was killed does not disturb later runs. Such values are not removed automatically, because Butterbot cannot tell them from the values of another tool; remove them by hand if they accumulate.

### `azure-dns`

| Setting | Default | Meaning |
|---|---|---|
| `subscription_id` | required | The subscription that holds the DNS zones. |
| `resource_group` | required | The resource group that holds the DNS zones. |

Authentication is the Azure identity. There is no `access_key`.

Azure DNS keeps all TXT values of a name in one record set. Butterbot reads the record set, adds its values and writes it back, and on cleanup takes out its own values again. A record set that becomes empty is deleted. Writes are conditional on the version that was read, so a change by another tool in between is not overwritten.

**Permissions.** On each configured zone, or on the resource group:

- read the zones;
- read, write and delete TXT record sets.

A custom role limited to TXT record sets is preferable to the built-in role "DNS Zone Contributor", which allows changing every record type. The actions for such a role:

```json
{
  "Name": "Butterbot DNS challenge writer",
  "Description": "Read DNS zones and manage TXT record sets, for ACME DNS validation.",
  "Actions": [
    "Microsoft.Network/dnsZones/read",
    "Microsoft.Network/dnsZones/TXT/read",
    "Microsoft.Network/dnsZones/TXT/write",
    "Microsoft.Network/dnsZones/TXT/delete"
  ],
  "AssignableScopes": [
    "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/example-dns-zones"
  ]
}
```

### `cloudflare`

| Setting | Default | Meaning |
|---|---|---|
| `access_key` | not set | Secret. The Cloudflare API token. |

Cloudflare keeps one record per TXT value. Butterbot creates one record for each value and deletes exactly those records afterwards.

**Permissions.** Create an API token for each instance with:

- Zone, Zone: Read
- Zone, DNS: Edit

and limit it under "Zone Resources" to the specific zones the instance is used for. Do not use a global API key.

## Certificate stores: `[store.<name>]`

Settings of every store:

| Setting | Default | Meaning |
|---|---|---|
| `type` | required | `azure-key-vault` or `directory`. |
| `format` | by type | `pfx` or `pem`. |

`format` can be overridden on a certificate entry, for all stores of that certificate.

- `pfx` is one PKCS#12 bundle with the certificate, its chain and the private key. The bundle has an empty password.
- `pem` is the same content as PEM text.

A certificate is stored under its entry's `name`. A certificate that is already in a store under that name is treated like one Butterbot issued, whichever program put it there. Butterbot never deletes a certificate from a store and never revokes one.

### `azure-key-vault`

| Setting | Default | Meaning |
|---|---|---|
| `vault_url` | required | URL of the vault, for example `https://YOUR-VAULT-NAME.vault.azure.net/`. |

The default `format` is `pfx`. Authentication is the Azure identity.

The certificate is imported as a Key Vault certificate with an exportable key. A consumer that is allowed to read the secret of the same name receives the certificate, the chain and the private key in the stored format.

**Permissions.** On the vault:

- read certificates;
- import certificates.

With Azure role-based access control these are the data actions:

```json
{
  "Name": "Butterbot certificate importer",
  "Description": "Read and import certificates.",
  "DataActions": [
    "Microsoft.KeyVault/vaults/certificates/read",
    "Microsoft.KeyVault/vaults/certificates/import/action"
  ]
}
```

With vault access policies, grant the certificate permissions Get and Import.

### `directory`

| Setting | Default | Meaning |
|---|---|---|
| `path` | required | The directory that holds the certificates. |

The default `format` is `pem`.

A certificate named `<name>` is written to `<path>/<name>/`:

| Format | Files |
|---|---|
| `pem` | `fullchain.pem` (certificate and chain) and `privkey.pem` (private key) |
| `pfx` | `certificate.pfx` |

Files that hold the private key (`privkey.pem`, `certificate.pfx`) are readable and writable by their owner only. Each file is replaced atomically: a reader sees the complete old file or the complete new file, never a partly written one.

**Permissions.** The user Butterbot runs as needs to create directories and files under `path`.

## Key Vault as a configuration layer

When `secrets.key_vault_url` is set, Butterbot reads configuration from that vault and, on first use of an ACME server, saves the account credentials to it. This vault can be a different one from the vaults used as stores.

**Permissions.** On the vault:

- list secrets (names only);
- read secrets;
- set secrets. This is needed only to save a newly registered ACME account.

With Azure role-based access control these are the data actions:

```json
{
  "Name": "Butterbot configuration reader",
  "Description": "Read configuration secrets and save ACME account credentials.",
  "DataActions": [
    "Microsoft.KeyVault/vaults/secrets/readMetadata/action",
    "Microsoft.KeyVault/vaults/secrets/getSecret/action",
    "Microsoft.KeyVault/vaults/secrets/setSecret/action"
  ]
}
```

With vault access policies, grant the secret permissions List, Get and Set.

Butterbot fetches only secrets whose names start with `run--`, `secrets--`, `acme--`, `dns--`, `store--` or `notify--`. Role assignments apply to the whole vault, though, so the identity is able to read every secret in it. Use a vault that holds nothing else if that matters.
