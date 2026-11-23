//! DNS validation: publishing challenge records through a DNS provider,
//! checking their propagation and removing them afterwards.

use std::collections::BTreeMap;

use async_trait::async_trait;

use crate::{Error, Result};

pub mod azure;
#[cfg(any(test, feature = "test-support"))]
pub mod challtestsrv;
pub mod cloudflare;
pub mod propagation;

/// The label under which ACME looks for the DNS challenge of a name.
pub const CHALLENGE_LABEL: &str = "_acme-challenge";

/// A DNS zone a provider instance can write to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Zone {
    /// The zone's name in lower case, without a trailing dot: `example.com`.
    pub name: String,
    /// What the provider needs to address the zone, for example its
    /// identifier. Providers that address a zone by its name repeat the name.
    pub id: String,
}

impl Zone {
    pub fn new(name: &str, id: &str) -> Self {
        Self {
            name: normalize(name),
            id: id.to_owned(),
        }
    }
}

/// What a call of [`DnsProvider::present`] added, so that
/// [`DnsProvider::cleanup`] can remove exactly that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordHandle {
    pub zone: Zone,
    /// The full record name: `_acme-challenge.example.com`.
    pub name: String,
    /// The TXT values that were added.
    pub values: Vec<String>,
    /// The provider's identifiers of the records it created, for providers
    /// that keep one record per value.
    pub record_ids: Vec<String>,
}

/// A place where validation records are published.
#[async_trait]
pub trait DnsProvider: Send + Sync {
    /// The zones this instance can write to. Used to find the zone of a
    /// challenge record and to keep two certificates out of one zone at the
    /// same time.
    async fn zones(&self) -> Result<Vec<Zone>>;

    /// Adds all `values` as TXT values at the record `name` in `zone`, with a
    /// time-to-live of `ttl` seconds. Values that are already at the name and
    /// that this call did not add are left alone.
    async fn present(
        &self,
        zone: &Zone,
        name: &str,
        values: &[String],
        ttl: u32,
    ) -> Result<RecordHandle>;

    /// Removes exactly what the call of `present` that returned `handle`
    /// added, and nothing else.
    async fn cleanup(&self, handle: RecordHandle) -> Result<()>;
}

/// Opens the DNS provider a `dns.<name>` instance describes. Nothing is
/// contacted here.
///
/// A provider whose credential is supplied by no configuration layer cannot
/// be opened; the error names the missing configuration path and where to
/// supply it.
pub fn open(
    name: &str,
    instance: &crate::config::DnsInstance,
    http: &reqwest::Client,
) -> Result<std::sync::Arc<dyn DnsProvider>> {
    use crate::config::DnsProviderConfig;

    Ok(match &instance.provider {
        DnsProviderConfig::AzureDns {
            subscription_id,
            resource_group,
        } => std::sync::Arc::new(azure::AzureDnsProvider::connect(
            http,
            subscription_id,
            resource_group,
        )?),
        DnsProviderConfig::Cloudflare {
            access_key: Some(token),
        } => std::sync::Arc::new(cloudflare::CloudflareProvider::new(
            http.clone(),
            token.clone(),
        )),
        DnsProviderConfig::Cloudflare { access_key: None } => {
            let path = format!("dns.{name}.access_key");
            return Err(Error::permanent(format!(
                "the secret {path} is not set; supply it as the environment variable {} or as \
                 the Key Vault secret {}",
                crate::config::env_name(&path),
                crate::config::vault_name(&path)
            )));
        }
    })
}

/// A DNS name in the form used for comparisons: lower case, no trailing dot.
pub fn normalize(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// The record at which the challenge of a DNS name is published. A wildcard
/// name and its base name share one record: both `*.example.com` and
/// `example.com` give `_acme-challenge.example.com`.
pub fn challenge_record_name(dns_name: &str) -> String {
    let name = normalize(dns_name);
    let base = name.strip_prefix("*.").unwrap_or(&name);
    format!("{CHALLENGE_LABEL}.{base}")
}

/// Groups challenge values by the record they are published at, so that every
/// value required at a record name is published together.
///
/// `challenges` are pairs of a DNS name of the certificate and the TXT value
/// that proves control of it.
pub fn group_by_record<'a>(
    challenges: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> BTreeMap<String, Vec<String>> {
    let mut records: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (dns_name, value) in challenges {
        let values = records.entry(challenge_record_name(dns_name)).or_default();
        if !values.iter().any(|existing| existing == value) {
            values.push(value.to_owned());
        }
    }
    records
}

/// The zone whose name is the longest suffix of `name`, on a label boundary.
pub fn zone_for<'a>(zones: &'a [Zone], name: &str) -> Option<&'a Zone> {
    let name = normalize(name);
    zones
        .iter()
        .filter(|zone| {
            name == zone.name
                || name
                    .strip_suffix(zone.name.as_str())
                    .is_some_and(|rest| rest.ends_with('.'))
        })
        .max_by_key(|zone| zone.name.len())
}

/// Like [`zone_for`], with the error a certificate fails with when no zone of
/// the DNS provider instance matches.
pub fn resolve_zone<'a>(instance: &str, zones: &'a [Zone], name: &str) -> Result<&'a Zone> {
    zone_for(zones, name).ok_or_else(|| {
        let hosted: Vec<&str> = zones.iter().map(|zone| zone.name.as_str()).collect();
        let hosted = if hosted.is_empty() {
            "none".to_owned()
        } else {
            hosted.join(", ")
        };
        Error::permanent(format!(
            "the DNS provider {instance} hosts no zone for {name} (its zones: {hosted})"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zones(names: &[&str]) -> Vec<Zone> {
        names
            .iter()
            .map(|name| Zone::new(name, &format!("id-of-{name}")))
            .collect()
    }

    // dns-validation, "Zone resolution", scenario "Sub-zone".
    #[test]
    fn longest_matching_zone_is_chosen() {
        let hosted = zones(&["example.com", "dev.example.com"]);
        let record = challenge_record_name("*.dev.example.com");
        assert_eq!(record, "_acme-challenge.dev.example.com");
        assert_eq!(
            zone_for(&hosted, &record).map(|zone| zone.name.as_str()),
            Some("dev.example.com")
        );

        let record = challenge_record_name("www.example.com");
        assert_eq!(
            zone_for(&hosted, &record).map(|zone| zone.name.as_str()),
            Some("example.com")
        );
        assert_eq!(
            zone_for(&hosted, &record).map(|zone| zone.id.as_str()),
            Some("id-of-example.com"),
            "the zone comes with what the provider needs to address it"
        );
    }

    // dns-validation, "Zone resolution", scenario "No matching zone".
    #[test]
    fn name_outside_every_zone_fails_and_names_the_name_and_the_provider() {
        let hosted = zones(&["example.com", "dev.example.com"]);
        let record = challenge_record_name("www.example.org");
        assert_eq!(zone_for(&hosted, &record), None);

        let error = resolve_zone("cloudflare", &hosted, &record).expect_err("no zone");
        assert_eq!(error.class(), "permanent");
        assert_eq!(
            error.to_string(),
            "the DNS provider cloudflare hosts no zone for _acme-challenge.www.example.org \
             (its zones: example.com, dev.example.com)"
        );
        let error = resolve_zone("azure", &[], &record).expect_err("no zone");
        assert!(error.to_string().ends_with("(its zones: none)"), "{error}");
    }

    #[test]
    fn zone_matches_only_on_a_label_boundary() {
        let hosted = zones(&["example.com"]);
        assert_eq!(zone_for(&hosted, "_acme-challenge.notexample.com"), None);
        assert_eq!(zone_for(&hosted, "example.com.evil.net"), None);
        assert!(zone_for(&hosted, "_acme-challenge.example.com").is_some());
        assert!(
            zone_for(&hosted, "example.com").is_some(),
            "the zone's own name is in the zone"
        );
    }

    #[test]
    fn names_are_compared_without_regard_to_case_and_trailing_dot() {
        let hosted = vec![Zone::new("Example.COM.", "z1")];
        assert_eq!(hosted[0].name, "example.com");
        assert!(zone_for(&hosted, "_ACME-Challenge.WWW.Example.com.").is_some());
        assert!(resolve_zone("cloudflare", &hosted, "_acme-challenge.example.com").is_ok());
    }

    #[test]
    fn challenge_record_of_a_wildcard_is_that_of_its_base_name() {
        assert_eq!(
            challenge_record_name("example.com"),
            "_acme-challenge.example.com"
        );
        assert_eq!(
            challenge_record_name("*.example.com"),
            "_acme-challenge.example.com"
        );
        assert_eq!(
            challenge_record_name("API.Example.Org."),
            "_acme-challenge.api.example.org"
        );
    }

    // dns-validation, "All values for a name are published together", scenario
    // "Wildcard and apex": the two names share one record with two values.
    #[test]
    fn values_of_a_wildcard_and_its_base_name_are_grouped_at_one_record() {
        let records = group_by_record([
            ("*.example.com", "value-for-the-wildcard"),
            ("example.com", "value-for-the-apex"),
            ("api.example.org", "value-for-api"),
        ]);
        assert_eq!(
            records,
            BTreeMap::from([
                (
                    "_acme-challenge.example.com".to_owned(),
                    vec![
                        "value-for-the-wildcard".to_owned(),
                        "value-for-the-apex".to_owned()
                    ]
                ),
                (
                    "_acme-challenge.api.example.org".to_owned(),
                    vec!["value-for-api".to_owned()]
                ),
            ])
        );
    }

    #[test]
    fn same_value_is_published_once() {
        let records = group_by_record([("example.com", "v"), ("EXAMPLE.com", "v")]);
        assert_eq!(records["_acme-challenge.example.com"], ["v"]);
    }

    // configuration, "Missing secrets fail only what depends on them": the
    // message names the missing configuration path.
    #[test]
    fn provider_without_its_credential_cannot_be_opened_and_the_path_is_named() {
        let config = crate::config::Config::from_layers([crate::config::Layer::from_toml(
            "test.toml",
            "[dns.cloudflare-eu]\ntype = \"cloudflare\"\n\n[dns.azure]\ntype = \"azure-dns\"\n\
             subscription_id = \"00000000-0000-0000-0000-000000000000\"\nresource_group = \"dns-zones\"\n",
        )
        .unwrap()])
        .unwrap();
        let http = crate::http::client().expect("an HTTP client");

        let error = open("cloudflare-eu", &config.dns["cloudflare-eu"], &http)
            .err()
            .expect("no token is configured");
        assert_eq!(error.class(), "permanent");
        assert_eq!(
            error.to_string(),
            "the secret dns.cloudflare-eu.access_key is not set; supply it as the environment \
             variable BUTTERBOT__DNS__CLOUDFLARE_EU__ACCESS_KEY or as the Key Vault secret \
             dns--cloudflare-eu--access-key"
        );

        assert!(
            open("azure", &config.dns["azure"], &http).is_ok(),
            "no secret is needed"
        );
    }

    #[test]
    fn provider_with_its_credential_is_opened() {
        let config = crate::config::Config::from_layers([crate::config::Layer::from_toml(
            "test.toml",
            "[dns.cloudflare]\ntype = \"cloudflare\"\naccess_key = \"token\"\n",
        )
        .unwrap()])
        .unwrap();
        let http = crate::http::client().expect("an HTTP client");
        assert!(open("cloudflare", &config.dns["cloudflare"], &http).is_ok());
    }

    /// Task 6.6: `docs/providers.md` lists every provider setting the
    /// configuration model accepts.
    mod docs {
        const DOCUMENT: &str = include_str!("../docs/providers.md");
        const PARSER: &str = include_str!("config/parse.rs");

        /// Every key and type name the parser reads for ACME servers, DNS
        /// providers and stores: the text literals in those functions that
        /// are written like a setting.
        fn settings_the_model_accepts() -> Vec<&'static str> {
            let start = PARSER.find("fn parse_acme(").expect("the ACME parser");
            let end = PARSER
                .find("fn parse_notify(")
                .expect("the end of the store parser");
            let mut settings: Vec<&str> = PARSER[start..end]
                .split('"')
                .skip(1)
                .step_by(2)
                .filter(|literal| {
                    !literal.is_empty()
                        && literal.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || b"_-".contains(&byte)
                        })
                })
                .collect();
            settings.sort_unstable();
            settings.dedup();
            settings
        }

        #[test]
        fn every_setting_the_model_accepts_is_in_the_document() {
            let settings = settings_the_model_accepts();
            // A check of the check: the list is what is expected to be found.
            assert_eq!(
                settings,
                [
                    "access_key",
                    "ari",
                    "azure-dns",
                    "azure-key-vault",
                    "cloudflare",
                    "directory",
                    "directory_url",
                    "eab_key",
                    "eab_kid",
                    "format",
                    "key_type",
                    "path",
                    "profile",
                    "propagation_check",
                    "propagation_interval",
                    "propagation_resolvers",
                    "propagation_timeout",
                    "renew_before",
                    "resource_group",
                    "settle",
                    "subscription_id",
                    "ttl",
                    "type",
                    "vault_url",
                ]
            );
            for setting in settings {
                assert!(
                    DOCUMENT.contains(&format!("`{setting}`")),
                    "`{setting}` is documented"
                );
            }
        }

        #[test]
        fn every_setting_has_a_row_in_a_settings_table() {
            for setting in settings_the_model_accepts() {
                if setting.contains('-') || ["cloudflare", "directory"].contains(&setting) {
                    continue; // a provider type, not a setting
                }
                assert!(
                    DOCUMENT
                        .lines()
                        .any(|line| line.starts_with(&format!("| `{setting}` |"))),
                    "`{setting}` has a row"
                );
            }
        }

        #[test]
        fn documented_values_are_the_ones_the_model_knows() {
            use crate::config::{Format, KeyType, PropagationCheck};

            for key_type in KeyType::ALL {
                assert!(DOCUMENT.contains(&format!("`{key_type}`")), "{key_type}");
            }
            for format in Format::ALL {
                assert!(DOCUMENT.contains(&format!("`{format}`")), "{format}");
            }
            for check in PropagationCheck::ALL {
                assert!(DOCUMENT.contains(&format!("`{check}`")), "{check}");
            }
            for file in [
                crate::store::directory::FULLCHAIN_FILE,
                crate::store::directory::PRIVATE_KEY_FILE,
                crate::store::directory::BUNDLE_FILE,
            ] {
                assert!(DOCUMENT.contains(&format!("`{file}`")), "{file}");
            }
            assert!(DOCUMENT.contains(&format!("`{}`", crate::azure::FEDERATED_TOKEN_FILE)));
        }
    }
}
