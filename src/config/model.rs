//! The typed configuration model, as it is after validation.
//!
//! Settings that can be overridden per certificate stay optional here; the
//! value that applies to a certificate is worked out by resolution.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use super::Secret;
use crate::events::Selection;

/// Built-in defaults, the lowest configuration layer.
pub mod defaults {
    use super::*;

    pub const PARALLEL: usize = 4;
    pub const MIN_REISSUE: Duration = Duration::from_secs(2 * 86_400);
    pub const RETRY_WAIT_MAX: Duration = Duration::from_secs(2 * 60);
    pub const RENEW_BEFORE: Duration = Duration::from_secs(30 * 86_400);
    pub const KEY_TYPE: KeyType = KeyType::Rsa2048;
    pub const ARI: bool = true;
    pub const TTL_SECONDS: u32 = 60;
    pub const PROPAGATION_CHECK: PropagationCheck = PropagationCheck::Authoritative;
    pub const PROPAGATION_INTERVAL: Duration = Duration::from_secs(10);
    pub const PROPAGATION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
    pub const SETTLE: Duration = Duration::from_secs(30);
    pub const DNS_PORT: u16 = 53;
    pub const WEBHOOK_METHOD: WebhookMethod = WebhookMethod::Post;
    pub const WEBHOOK_CONTENT_TYPE: &str = "application/json";
    pub const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

    /// The recursive resolvers asked in `resolver` mode when none are configured.
    pub fn propagation_resolvers() -> Vec<SocketAddr> {
        [Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)]
            .into_iter()
            .map(|address| SocketAddr::new(IpAddr::V4(address), DNS_PORT))
            .collect()
    }
}

/// Defines an enum whose values are written as fixed strings in configuration.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $name:ident, $what:literal { $($variant:ident => $text:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Self::ALL.iter().copied().find(|value| value.as_str() == text).ok_or_else(|| {
                    let known: Vec<_> = Self::ALL.iter().map(|value| value.as_str()).collect();
                    format!("unknown {} {text:?} (expected one of: {})", $what, known.join(", "))
                })
            }
        }
    };
}

string_enum! {
    /// The type of the private key generated for a certificate.
    KeyType, "key type" {
        Rsa2048 => "rsa2048",
        Rsa3072 => "rsa3072",
        Rsa4096 => "rsa4096",
        EcdsaP256 => "ecdsa-p256",
        EcdsaP384 => "ecdsa-p384",
    }
}

string_enum! {
    /// The format in which a store holds a certificate, its chain and its key.
    Format, "format" {
        Pfx => "pfx",
        Pem => "pem",
    }
}

string_enum! {
    /// How the propagation of challenge records is checked.
    PropagationCheck, "propagation check" {
        Authoritative => "authoritative",
        Resolver => "resolver",
        Off => "off",
    }
}

string_enum! {
    /// The HTTP method of a webhook request.
    WebhookMethod, "method" {
        Post => "POST",
        Get => "GET",
    }
}

string_enum! {
    /// A built-in body of a webhook request.
    WebhookFormat, "format" {
        Json => "json",
        Teams => "teams",
    }
}

/// The whole configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub run: Run,
    pub secrets: Secrets,
    /// ACME servers by instance name (`acme.<name>`).
    pub acme: BTreeMap<String, AcmeServer>,
    /// DNS providers by instance name (`dns.<name>`).
    pub dns: BTreeMap<String, DnsInstance>,
    /// Certificate stores by instance name (`store.<name>`).
    pub stores: BTreeMap<String, StoreInstance>,
    /// Notifiers by instance name (`notify.<name>`).
    pub notify: BTreeMap<String, NotifyInstance>,
    /// Certificate entries (`[[certificate]]`), in the order they are written.
    pub certificates: Vec<Certificate>,
}

/// The `run` table.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub parallel: usize,
    pub min_reissue: Duration,
    pub retry_wait_max: Duration,
}

/// The `secrets` table.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Secrets {
    /// When set, Azure Key Vault is read as the highest configuration layer.
    pub key_vault_url: Option<String>,
}

/// An ACME server (`acme.<name>`).
#[derive(Debug, Clone, PartialEq)]
pub struct AcmeServer {
    pub directory_url: String,
    /// The serialised account credentials.
    pub access_key: Option<Secret>,
    pub eab_kid: Option<String>,
    pub eab_key: Option<Secret>,
    pub profile: Option<String>,
    pub renew_before: Option<Duration>,
    pub key_type: Option<KeyType>,
    pub ari: bool,
    /// A file with a root certificate to trust for this server, besides the
    /// bundled roots. Only a build with the `test-support` feature reads this
    /// setting; it is what lets the program talk to the ACME test server.
    pub extra_root: Option<PathBuf>,
}

/// A DNS provider instance (`dns.<name>`).
#[derive(Debug, Clone, PartialEq)]
pub struct DnsInstance {
    pub provider: DnsProviderConfig,
    /// Time-to-live of challenge records, in seconds.
    pub ttl: u32,
    pub propagation_check: PropagationCheck,
    pub propagation_resolvers: Vec<SocketAddr>,
    pub propagation_interval: Duration,
    pub propagation_timeout: Option<Duration>,
    pub settle: Option<Duration>,
}

/// The settings that depend on a DNS provider's `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum DnsProviderConfig {
    AzureDns {
        subscription_id: String,
        resource_group: String,
    },
    Cloudflare {
        access_key: Option<Secret>,
    },
    /// The challenge DNS server of the ACME test server. Only a build with
    /// the `test-support` feature accepts this type.
    ChallTestSrv {
        management_url: String,
        zones: Vec<String>,
    },
}

impl DnsProviderConfig {
    pub const TYPES: &'static [&'static str] = &["azure-dns", "cloudflare"];

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::AzureDns { .. } => "azure-dns",
            Self::Cloudflare { .. } => "cloudflare",
            Self::ChallTestSrv { .. } => "challtestsrv",
        }
    }
}

/// A certificate store instance (`store.<name>`).
#[derive(Debug, Clone, PartialEq)]
pub struct StoreInstance {
    pub kind: StoreKind,
    pub format: Option<Format>,
}

/// The settings that depend on a store's `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreKind {
    AzureKeyVault { vault_url: String },
    Directory { path: PathBuf },
}

impl StoreKind {
    pub const TYPES: &'static [&'static str] = &["azure-key-vault", "directory"];

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::AzureKeyVault { .. } => "azure-key-vault",
            Self::Directory { .. } => "directory",
        }
    }

    /// The format a store of this type uses when none is configured.
    pub fn default_format(&self) -> Format {
        match self {
            Self::AzureKeyVault { .. } => Format::Pfx,
            Self::Directory { .. } => Format::Pem,
        }
    }
}

/// A notifier instance (`notify.<name>`).
#[derive(Debug, Clone, PartialEq)]
pub struct NotifyInstance {
    /// The event kinds the notifier receives.
    pub events: Selection,
    pub kind: NotifierKind,
}

/// The settings that depend on a notifier's `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum NotifierKind {
    Webhook(Webhook),
}

impl NotifierKind {
    pub const TYPES: &'static [&'static str] = &["webhook"];

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Webhook(_) => "webhook",
        }
    }
}

/// A notifier that sends each event as an HTTP request (`type = "webhook"`).
#[derive(Debug, Clone, PartialEq)]
pub struct Webhook {
    /// Where the request goes. A secret: a webhook URL carries its own
    /// credential. `None` when no layer supplies it.
    pub url: Option<Secret>,
    pub method: WebhookMethod,
    pub body: WebhookBody,
    /// The headers sent with every request, by name. The values are kept as
    /// secrets, because a key may be written into one.
    pub headers: BTreeMap<String, Secret>,
    /// The value of the `access_key` placeholder.
    pub access_key: Option<Secret>,
    /// A PEM file with certificates to trust for the endpoint, besides the
    /// bundled roots.
    pub ca_file: Option<PathBuf>,
    /// The time limit of one attempt.
    pub timeout: Duration,
}

/// What a webhook request with a body carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookBody {
    /// The event as a JSON object.
    Json,
    /// The message a Microsoft Teams workflow accepts.
    Teams,
    /// The configured text, with its placeholders replaced.
    Custom {
        template: String,
        content_type: String,
    },
}

impl Webhook {
    /// Whether any template of the instance uses the `access_key`
    /// placeholder, which makes the secret required.
    pub fn uses_access_key(&self) -> bool {
        const PLACEHOLDER: &str = "{{access_key}}";
        self.url
            .as_ref()
            .is_some_and(|url| url.expose().contains(PLACEHOLDER))
            || self
                .headers
                .values()
                .any(|value| value.expose().contains(PLACEHOLDER))
            || matches!(&self.body, WebhookBody::Custom { template, .. } if template.contains(PLACEHOLDER))
    }
}

/// A certificate entry (`[[certificate]]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Certificate {
    pub name: String,
    /// The DNS names the certificate covers.
    pub names: Vec<String>,
    /// The ACME server instance that issues it.
    pub acme: String,
    /// The DNS provider instance that proves control of its names.
    pub dns: String,
    /// The store instances that receive it.
    pub stores: Vec<String>,
    pub renew_before: Option<Duration>,
    pub key_type: Option<KeyType>,
    pub profile: Option<String>,
    pub format: Option<Format>,
    pub propagation_timeout: Option<Duration>,
    pub settle: Option<Duration>,
}
