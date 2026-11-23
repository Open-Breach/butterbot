//! Turns the merged configuration tree into the typed model.
//!
//! Reading is strict: a key that is not part of the model, a value of the
//! wrong type and a missing required key are all problems. Every problem is
//! collected, so that one report lists them all.
//!
//! Values from the environment and from Key Vault are always text. For that
//! reason a whole number and `true`/`false` may be written as text; no other
//! conversion takes place.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use config::{Value, ValueKind};

use super::model::*;
use super::{ConfigError, Secret, parse_duration};
use crate::events::EventKind;

/// Every top-level key of the configuration.
const TOP_LEVEL: [&str; 7] = [
    "run",
    "secrets",
    "acme",
    "dns",
    "store",
    "notify",
    "certificate",
];

pub(super) fn parse(root: Value) -> Result<Config, ConfigError> {
    let mut problems = Vec::new();
    let ValueKind::Table(root) = root.kind else {
        return Err(ConfigError::single("the configuration is not a table"));
    };
    let mut root: BTreeMap<String, Value> = root.into_iter().collect();

    let run = parse_run(root.remove("run"), &mut problems);
    let secrets = parse_secrets(root.remove("secrets"), &mut problems);
    let acme = parse_instances("acme", root.remove("acme"), &mut problems, parse_acme);
    let dns = parse_instances("dns", root.remove("dns"), &mut problems, parse_dns);
    let stores = parse_instances("store", root.remove("store"), &mut problems, parse_store);
    parse_instances("notify", root.remove("notify"), &mut problems, parse_notify);
    let declared = Declared {
        acme: &acme.declared,
        dns: &dns.declared,
        stores: &stores.declared,
    };
    let certificates = parse_certificates(root.remove("certificate"), &declared, &mut problems);
    let (acme, dns, stores) = (acme.valid, dns.valid, stores.valid);

    for (key, value) in root {
        problems.push(format!(
            "unknown key `{key}`{} (the top-level keys are: {})",
            origin_note(&value),
            TOP_LEVEL.join(", ")
        ));
    }

    if problems.is_empty() {
        Ok(Config {
            run,
            secrets,
            acme,
            dns,
            stores,
            certificates,
        })
    } else {
        Err(ConfigError::new(problems))
    }
}

fn parse_run(value: Option<Value>, problems: &mut Vec<String>) -> Run {
    let mut run = Run {
        parallel: defaults::PARALLEL,
        min_reissue: defaults::MIN_REISSUE,
        retry_wait_max: defaults::RETRY_WAIT_MAX,
    };
    let Some(mut reader) = value.and_then(|value| Reader::new("run".into(), value, problems))
    else {
        return run;
    };
    if let Some(parallel) = reader.integer_in("parallel", 1, usize::MAX as u64) {
        run.parallel = parallel as usize;
    }
    if let Some(min_reissue) = reader.duration("min_reissue") {
        run.min_reissue = min_reissue;
    }
    if let Some(retry_wait_max) = reader.duration("retry_wait_max") {
        run.retry_wait_max = retry_wait_max;
    }
    reader.finish();
    run
}

fn parse_secrets(value: Option<Value>, problems: &mut Vec<String>) -> Secrets {
    let Some(mut reader) = value.and_then(|value| Reader::new("secrets".into(), value, problems))
    else {
        return Secrets::default();
    };
    let secrets = Secrets {
        key_vault_url: reader.string("key_vault_url"),
    };
    reader.finish();
    secrets
}

/// The instances of one table.
struct Instances<T> {
    /// The instances that have no problem.
    valid: BTreeMap<String, T>,
    /// The name of every instance in the table, whether it has a problem or
    /// not. A reference to an instance with a problem is not a dangling one.
    declared: BTreeSet<String>,
}

/// The instance names a certificate entry may refer to.
struct Declared<'a> {
    acme: &'a BTreeSet<String>,
    dns: &'a BTreeSet<String>,
    stores: &'a BTreeSet<String>,
}

const NAME_RULE: &str = "a name consists of lowercase letters, digits and single dashes, and \
                         starts and ends with a letter or digit";

/// Whether `name` is a valid instance or certificate name.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// Reads one of the tables of named instances.
fn parse_instances<T>(
    table_name: &str,
    value: Option<Value>,
    problems: &mut Vec<String>,
    parse_one: impl Fn(&mut Reader<'_>) -> Option<T>,
) -> Instances<T> {
    let mut instances = Instances {
        valid: BTreeMap::new(),
        declared: BTreeSet::new(),
    };
    let Some(value) = value else {
        return instances;
    };
    let note = origin_note(&value);
    let ValueKind::Table(table) = value.kind else {
        problems.push(format!(
            "{table_name}: expected a table of named instances, found {}{note}",
            kind_name(&value.kind)
        ));
        return instances;
    };
    let table: BTreeMap<String, Value> = table.into_iter().collect();
    for (name, value) in table {
        instances.declared.insert(name.clone());
        let named_well = is_valid_name(&name);
        if !named_well {
            problems.push(format!("{table_name}.{name}: invalid name: {NAME_RULE}"));
        }
        let Some(mut reader) = Reader::new(format!("{table_name}.{name}"), value, problems) else {
            continue;
        };
        let instance = parse_one(&mut reader);
        reader.finish();
        if let Some(instance) = instance.filter(|_| named_well) {
            instances.valid.insert(name, instance);
        }
    }
    instances
}

fn parse_acme(reader: &mut Reader<'_>) -> Option<AcmeServer> {
    let directory_url = reader.required_string("directory_url");
    let access_key = reader.secret("access_key");
    let eab_kid = reader.string("eab_kid");
    let eab_key = reader.secret("eab_key");
    let profile = reader.string("profile");
    let renew_before = reader.duration("renew_before");
    let key_type = reader.parsed("key_type");
    let ari = reader.boolean("ari").unwrap_or(defaults::ARI);
    let extra_root = test_support::acme_extra_root(reader);
    Some(AcmeServer {
        directory_url: directory_url?,
        access_key,
        eab_kid,
        eab_key,
        profile,
        renew_before,
        key_type,
        ari,
        extra_root,
    })
}

fn parse_dns(reader: &mut Reader<'_>) -> Option<DnsInstance> {
    let Some(type_name) = reader.required_string("type") else {
        reader.abandon();
        return None;
    };
    let provider = match type_name.as_str() {
        "azure-dns" => {
            let subscription_id = reader.required_string("subscription_id");
            let resource_group = reader.required_string("resource_group");
            subscription_id
                .zip(resource_group)
                .map(
                    |(subscription_id, resource_group)| DnsProviderConfig::AzureDns {
                        subscription_id,
                        resource_group,
                    },
                )
        }
        "cloudflare" => Some(DnsProviderConfig::Cloudflare {
            access_key: reader.secret("access_key"),
        }),
        other => match test_support::dns_provider(other, reader) {
            Some(provider) => provider,
            None => {
                reader.unknown_type(other, DnsProviderConfig::TYPES);
                return None;
            }
        },
    };
    let ttl = reader
        .duration_in(
            "ttl",
            Duration::from_secs(1),
            Duration::from_secs(u32::MAX.into()),
        )
        .map_or(defaults::TTL_SECONDS, |ttl| ttl.as_secs() as u32);
    let propagation_check = reader
        .parsed("propagation_check")
        .unwrap_or(defaults::PROPAGATION_CHECK);
    let propagation_resolvers = reader
        .resolvers("propagation_resolvers")
        .unwrap_or_else(defaults::propagation_resolvers);
    let propagation_interval = reader
        .duration_in(
            "propagation_interval",
            Duration::from_secs(1),
            Duration::MAX,
        )
        .unwrap_or(defaults::PROPAGATION_INTERVAL);
    let propagation_timeout = reader.duration("propagation_timeout");
    let settle = reader.duration("settle");
    Some(DnsInstance {
        provider: provider?,
        ttl,
        propagation_check,
        propagation_resolvers,
        propagation_interval,
        propagation_timeout,
        settle,
    })
}

fn parse_store(reader: &mut Reader<'_>) -> Option<StoreInstance> {
    let Some(type_name) = reader.required_string("type") else {
        reader.abandon();
        return None;
    };
    let kind = match type_name.as_str() {
        "azure-key-vault" => reader
            .required_string("vault_url")
            .map(|vault_url| StoreKind::AzureKeyVault { vault_url }),
        "directory" => reader
            .required_string("path")
            .map(|path| StoreKind::Directory {
                path: PathBuf::from(path),
            }),
        unknown => {
            reader.unknown_type(unknown, StoreKind::TYPES);
            return None;
        }
    };
    let format = reader.parsed("format");
    Some(StoreInstance {
        kind: kind?,
        format,
    })
}

/// This version has no notifier type, so every notifier instance is a problem.
/// What a notifier has in common, its `type` and its `events` selection, is
/// still read, so that those keys are checked.
fn parse_notify(reader: &mut Reader<'_>) -> Option<()> {
    let type_name = reader.required_string("type");
    if let Some(events) = reader.strings("events") {
        for event in events {
            if let Err(message) = EventKind::from_str(&event) {
                reader.report("events", "", &message);
            }
        }
    }
    if let Some(type_name) = type_name {
        reader.unknown_type(&type_name, &[]);
    } else {
        reader.abandon();
    }
    None
}

fn parse_certificates(
    value: Option<Value>,
    declared: &Declared<'_>,
    problems: &mut Vec<String>,
) -> Vec<Certificate> {
    let mut certificates: Vec<Certificate> = Vec::new();
    let Some(value) = value else {
        return certificates;
    };
    let note = origin_note(&value);
    let ValueKind::Array(entries) = value.kind else {
        problems.push(format!(
            "certificate: expected a list of entries, each written as [[certificate]], \
             found {}{note}",
            kind_name(&value.kind)
        ));
        return certificates;
    };
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let name = match &entry.kind {
            ValueKind::Table(table) => match table.get("name").map(|name| &name.kind) {
                Some(ValueKind::String(name)) => Some(name.clone()),
                _ => None,
            },
            _ => None,
        };
        let location = match &name {
            Some(name) => format!("certificate {name:?}"),
            None => format!("certificate #{}", index + 1),
        };
        if let Some(name) = name
            && !seen.insert(name.clone())
            && duplicates.insert(name.clone())
        {
            problems.push(format!(
                "certificate {name:?} is defined more than once: certificate names are unique"
            ));
        }
        let Some(mut reader) = Reader::new(location, entry, problems) else {
            continue;
        };
        let certificate = parse_certificate(&mut reader, declared);
        reader.finish();
        certificates.extend(certificate);
    }
    certificates
}

fn parse_certificate(reader: &mut Reader<'_>, declared: &Declared<'_>) -> Option<Certificate> {
    let name = reader.required_string("name").filter(|name| {
        let valid = is_valid_name(name);
        if !valid {
            reader.report("name", "", &format!("invalid name: {NAME_RULE}"));
        }
        valid
    });
    let names = reader.required_strings("names").filter(|names| {
        if names.is_empty() {
            reader.report("names", "", "must list at least one DNS name");
        }
        !names.is_empty()
    });
    let acme = reader
        .required_string("acme")
        .filter(|acme| reader.defined("acme", "ACME server", acme, declared.acme));
    let dns = reader
        .required_string("dns")
        .filter(|dns| reader.defined("dns", "DNS provider", dns, declared.dns));
    let stores = reader.required_strings("stores").filter(|stores| {
        if stores.is_empty() {
            reader.report("stores", "", "must list at least one store");
        }
        // Every store is checked, so that every dangling reference is reported.
        let mut usable = !stores.is_empty();
        for store in stores {
            usable &= reader.defined("stores", "store", store, declared.stores);
        }
        usable
    });
    let renew_before = reader.duration("renew_before");
    let key_type = reader.parsed("key_type");
    let profile = reader.string("profile");
    let format = reader.parsed("format");
    let propagation_timeout = reader.duration("propagation_timeout");
    let settle = reader.duration("settle");
    Some(Certificate {
        name: name?,
        names: names?,
        acme: acme?,
        dns: dns?,
        stores: stores?,
        renew_before,
        key_type,
        profile,
        format,
        propagation_timeout,
        settle,
    })
}

/// Takes the keys of one table, one by one, and reports what is wrong with them.
struct Reader<'p> {
    /// What the table is, for messages: `run`, `dns.cloudflare`, `certificate "name"`.
    location: String,
    table: BTreeMap<String, Value>,
    /// The keys the table had when reading began.
    present: BTreeSet<String>,
    problems: &'p mut Vec<String>,
}

impl<'p> Reader<'p> {
    fn new(location: String, value: Value, problems: &'p mut Vec<String>) -> Option<Self> {
        let note = origin_note(&value);
        match value.kind {
            ValueKind::Table(table) => {
                let table: BTreeMap<String, Value> = table.into_iter().collect();
                let present = table.keys().cloned().collect();
                Some(Self {
                    location,
                    table,
                    present,
                    problems,
                })
            }
            other => {
                problems.push(format!(
                    "{location}: expected a table, found {}{note}",
                    kind_name(&other)
                ));
                None
            }
        }
    }

    fn report(&mut self, key: &str, note: &str, message: &str) {
        self.problems
            .push(format!("{}: `{key}`: {message}{note}", self.location));
    }

    fn mismatch(&mut self, key: &str, expected: &str, found: &ValueKind, note: &str) {
        self.report(
            key,
            note,
            &format!("expected {expected}, found {}", kind_name(found)),
        );
    }

    /// A text value and the note about its origin. The text is not checked.
    fn text(&mut self, key: &str) -> Option<(String, String)> {
        let value = self.table.remove(key)?;
        let note = origin_note(&value);
        match value.kind {
            ValueKind::String(text) => Some((text, note)),
            other => {
                self.mismatch(key, "a string", &other, &note);
                None
            }
        }
    }

    fn string(&mut self, key: &str) -> Option<String> {
        let (text, note) = self.text(key)?;
        if text.is_empty() {
            self.report(key, &note, "must not be empty");
            return None;
        }
        Some(text)
    }

    /// A secret. An empty value counts as not supplied. No message about a
    /// secret ever includes its value.
    fn secret(&mut self, key: &str) -> Option<Secret> {
        let (text, _) = self.text(key)?;
        (!text.is_empty()).then(|| Secret::new(text))
    }

    /// A text value that is read by `parse`.
    fn parsed_with<T>(
        &mut self,
        key: &str,
        parse: impl FnOnce(&str) -> Result<T, String>,
    ) -> Option<T> {
        let (text, note) = self.text(key)?;
        match parse(&text) {
            Ok(value) => Some(value),
            Err(message) => {
                self.report(key, &note, &message);
                None
            }
        }
    }

    fn parsed<T: FromStr<Err = String>>(&mut self, key: &str) -> Option<T> {
        self.parsed_with(key, str::parse)
    }

    fn duration(&mut self, key: &str) -> Option<Duration> {
        self.parsed_with(key, parse_duration)
    }

    fn duration_in(&mut self, key: &str, min: Duration, max: Duration) -> Option<Duration> {
        self.parsed_with(key, |text| {
            let duration = parse_duration(text)?;
            if duration < min {
                Err(format!(
                    "{text:?} is too short: the minimum is {}",
                    super::format_duration(min)
                ))
            } else if duration > max {
                Err(format!(
                    "{text:?} is too long: the maximum is {}",
                    super::format_duration(max)
                ))
            } else {
                Ok(duration)
            }
        })
    }

    fn boolean(&mut self, key: &str) -> Option<bool> {
        let value = self.table.remove(key)?;
        let note = origin_note(&value);
        match value.kind {
            ValueKind::Boolean(flag) => Some(flag),
            ValueKind::String(text) if text == "true" => Some(true),
            ValueKind::String(text) if text == "false" => Some(false),
            other => {
                self.mismatch(key, "true or false", &other, &note);
                None
            }
        }
    }

    /// A whole number between `min` and `max`.
    fn integer_in(&mut self, key: &str, min: u64, max: u64) -> Option<u64> {
        let value = self.table.remove(key)?;
        let note = origin_note(&value);
        let number = match &value.kind {
            ValueKind::I64(number) => i128::from(*number),
            ValueKind::I128(number) => *number,
            ValueKind::U64(number) => i128::from(*number),
            ValueKind::String(text) if text.parse::<i128>().is_ok() => {
                text.parse().unwrap_or_default()
            }
            other => {
                self.mismatch(key, "a whole number", other, &note);
                return None;
            }
        };
        if number < i128::from(min) || number > i128::from(max) {
            let range = if max == u64::MAX {
                format!("at least {min}")
            } else {
                format!("between {min} and {max}")
            };
            self.report(
                key,
                &note,
                &format!("{number} is out of range: expected {range}"),
            );
            return None;
        }
        Some(number as u64)
    }

    /// A list of non-empty text values.
    fn strings(&mut self, key: &str) -> Option<Vec<String>> {
        let value = self.table.remove(key)?;
        let note = origin_note(&value);
        let ValueKind::Array(items) = value.kind else {
            self.mismatch(key, "a list of strings", &value.kind, &note);
            return None;
        };
        let mut strings = Vec::with_capacity(items.len());
        for item in items {
            match item.kind {
                ValueKind::String(text) if !text.is_empty() => strings.push(text),
                ValueKind::String(_) => {
                    self.report(key, &note, "an entry must not be empty");
                    return None;
                }
                other => {
                    self.mismatch(key, "a list of strings", &other, &note);
                    return None;
                }
            }
        }
        Some(strings)
    }

    /// A list of resolver addresses, each an IP address with an optional port.
    fn resolvers(&mut self, key: &str) -> Option<Vec<SocketAddr>> {
        let note = self.table.get(key).map(origin_note).unwrap_or_default();
        let entries = self.strings(key)?;
        if entries.is_empty() {
            self.report(key, &note, "must list at least one resolver");
            return None;
        }
        let mut resolvers = Vec::with_capacity(entries.len());
        for entry in entries {
            let address = entry.parse::<SocketAddr>().or_else(|_| {
                entry
                    .parse::<IpAddr>()
                    .map(|ip| SocketAddr::new(ip, defaults::DNS_PORT))
            });
            match address {
                Ok(address) => resolvers.push(address),
                Err(_) => {
                    let message = format!(
                        "invalid resolver {entry:?}: expected an IP address, optionally with a port"
                    );
                    self.report(key, &note, &message);
                    return None;
                }
            }
        }
        Some(resolvers)
    }

    /// Reports a missing required key, unless the key was present, in which
    /// case its problem has already been reported.
    fn required<T>(&mut self, key: &str, value: Option<T>) -> Option<T> {
        if value.is_none() && !self.present.contains(key) {
            self.problems
                .push(format!("{}: missing key `{key}`", self.location));
        }
        value
    }

    fn required_string(&mut self, key: &str) -> Option<String> {
        let value = self.string(key);
        self.required(key, value)
    }

    fn required_strings(&mut self, key: &str) -> Option<Vec<String>> {
        let value = self.strings(key);
        self.required(key, value)
    }

    /// Checks that `name` is one of the `declared` instances and reports a
    /// dangling reference otherwise.
    fn defined(&mut self, key: &str, what: &str, name: &str, declared: &BTreeSet<String>) -> bool {
        let defined = declared.contains(name);
        if !defined {
            self.report(key, "", &format!("the {what} {name:?} is not defined"));
        }
        defined
    }

    /// Reports an unknown `type` and stops reading the table: its other keys
    /// cannot be judged without knowing the type.
    fn unknown_type(&mut self, type_name: &str, known: &[&str]) {
        let known = if known.is_empty() {
            "this version has no types of this kind".to_owned()
        } else {
            format!("known types: {}", known.join(", "))
        };
        self.problems.push(format!(
            "{}: unknown type {type_name:?} ({known})",
            self.location
        ));
        self.abandon();
    }

    fn abandon(&mut self) {
        self.table.clear();
    }

    /// Reports every key that was not taken as unknown.
    fn finish(self) {
        for (key, value) in &self.table {
            self.problems.push(format!(
                "{}: unknown key `{key}`{}",
                self.location,
                origin_note(value)
            ));
        }
    }
}

fn kind_name(kind: &ValueKind) -> &'static str {
    match kind {
        ValueKind::Nil => "nothing",
        ValueKind::Boolean(_) => "a boolean",
        ValueKind::I64(_) | ValueKind::I128(_) | ValueKind::U64(_) | ValueKind::U128(_) => {
            "a whole number"
        }
        ValueKind::Float(_) => "a number",
        ValueKind::String(_) => "a string",
        ValueKind::Table(_) => "a table",
        ValueKind::Array(_) => "a list",
    }
}

/// Where a value was set, for the end of a message.
fn origin_note(value: &Value) -> String {
    value
        .origin()
        .map(|origin| format!(" (set in {origin})"))
        .unwrap_or_default()
}

/// Settings that exist only for tests. A normal build accepts none of them:
/// they are unknown keys and an unknown type there.
mod test_support {
    use std::path::PathBuf;

    use super::{DnsProviderConfig, Reader};

    /// Whether this build reads the test settings.
    const ENABLED: bool = cfg!(any(test, feature = "test-support"));

    /// `acme.<name>.ca_file`: a root certificate to trust for the server.
    pub(super) fn acme_extra_root(reader: &mut Reader<'_>) -> Option<PathBuf> {
        if !ENABLED {
            return None;
        }
        reader.string("ca_file").map(PathBuf::from)
    }

    /// The DNS provider type `challtestsrv`. `None` when `type_name` is not a
    /// test type or this build does not have them; `Some(None)` when it is
    /// but a setting of it has a problem.
    pub(super) fn dns_provider(
        type_name: &str,
        reader: &mut Reader<'_>,
    ) -> Option<Option<DnsProviderConfig>> {
        if !ENABLED || type_name != "challtestsrv" {
            return None;
        }
        let management_url = reader.required_string("management_url");
        let zones = reader.required_strings("zones");
        Some(management_url.zip(zones).map(|(management_url, zones)| {
            DnsProviderConfig::ChallTestSrv {
                management_url,
                zones,
            }
        }))
    }
}
