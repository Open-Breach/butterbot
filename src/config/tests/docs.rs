//! Task 2.8: `docs/configuration.md` agrees with the program.

use super::*;

const DOCUMENT: &str = include_str!("../../../docs/configuration.md");

/// The TOML block under the heading "Complete example".
fn complete_example() -> &'static str {
    let section = DOCUMENT
        .split("\n## Complete example\n")
        .nth(1)
        .expect("the section exists");
    let block = section
        .split("```toml\n")
        .nth(1)
        .expect("the section has a TOML block");
    block.split("\n```").next().expect("the block is closed")
}

#[test]
fn complete_example_loads_without_errors() {
    let config = from_toml(complete_example()).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(config.certificates.len(), 3);
    for certificate in &config.certificates {
        config.resolve(certificate);
    }
}

#[test]
fn complete_example_sets_every_setting() {
    let config = from_toml(complete_example()).unwrap_or_else(|error| panic!("{error}"));
    let text = complete_example();

    // Settings with a built-in value cannot be told from the default once
    // loaded, so they are looked for in the text.
    for key in [
        "parallel",
        "min_reissue",
        "retry_wait_max",
        "ari",
        "ttl",
        "propagation_check",
        "propagation_resolvers",
        "propagation_interval",
    ] {
        assert!(
            text.contains(&format!("\n{key} = ")),
            "the example sets `{key}`"
        );
    }

    assert!(config.secrets.key_vault_url.is_some());
    let any_acme = |set: fn(&AcmeServer) -> bool| config.acme.values().any(set);
    assert!(any_acme(|server| server.eab_kid.is_some()));
    assert!(any_acme(|server| server.profile.is_some()));
    assert!(any_acme(|server| server.renew_before.is_some()));
    assert!(any_acme(|server| server.key_type.is_some()));
    let any_dns = |set: fn(&DnsInstance) -> bool| config.dns.values().any(set);
    assert!(any_dns(|instance| instance.propagation_timeout.is_some()));
    assert!(any_dns(|instance| instance.settle.is_some()));
    assert!(any_dns(
        |instance| instance.provider.type_name() == "azure-dns"
    ));
    assert!(any_dns(
        |instance| instance.provider.type_name() == "cloudflare"
    ));
    assert!(
        config
            .stores
            .values()
            .any(|store| store.kind.type_name() == "azure-key-vault")
    );
    assert!(
        config
            .stores
            .values()
            .any(|store| store.kind.type_name() == "directory")
    );
    assert!(config.stores.values().any(|store| store.format.is_some()));
    let any_certificate = |set: fn(&Certificate) -> bool| config.certificates.iter().any(set);
    assert!(any_certificate(|certificate| certificate
        .renew_before
        .is_some()));
    assert!(any_certificate(|certificate| certificate
        .key_type
        .is_some()));
    assert!(any_certificate(|certificate| certificate.profile.is_some()));
    assert!(any_certificate(|certificate| certificate.format.is_some()));
    assert!(any_certificate(|certificate| certificate
        .propagation_timeout
        .is_some()));
    assert!(any_certificate(|certificate| certificate.settle.is_some()));
}

#[test]
fn complete_example_keeps_secrets_out_of_the_file() {
    let config = from_toml(complete_example()).unwrap_or_else(|error| panic!("{error}"));
    let expected = config.expected_secrets();
    assert!(!expected.is_empty());
    assert!(expected.iter().all(|secret| !secret.found), "{expected:#?}");
    // The names the example gives in its comments are the names the program uses.
    let mut mentioned = 0;
    for line in complete_example().lines() {
        if let Some(name) = line.strip_prefix("#   environment:") {
            assert!(
                expected.iter().any(|secret| secret.env_name == name.trim()),
                "{line}"
            );
            mentioned += 1;
        } else if let Some(name) = line.strip_prefix("#   Key Vault:") {
            assert!(
                expected
                    .iter()
                    .any(|secret| secret.vault_name == name.trim()),
                "{line}"
            );
            mentioned += 1;
        }
    }
    assert_eq!(mentioned, 8, "four secrets are named, each in both sources");
}

/// The rows of the form "| `path` | `name` |" in the document.
fn name_rows() -> Vec<(&'static str, &'static str)> {
    DOCUMENT
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            match cells.as_slice() {
                ["", path, name, ""] => Some((
                    path.strip_prefix('`')?.strip_suffix('`')?,
                    name.strip_prefix('`')?.strip_suffix('`')?,
                )),
                _ => None,
            }
        })
        .collect()
}

#[test]
fn source_names_in_the_tables_follow_the_rules() {
    let rows = name_rows();
    let environment: Vec<_> = rows
        .iter()
        .filter(|(_, name)| name.starts_with("BUTTERBOT__"))
        .collect();
    let vault: Vec<_> = rows
        .iter()
        .filter(|(_, name)| name.contains("--"))
        .collect();
    assert_eq!(environment.len(), 4, "the environment table has four rows");
    assert_eq!(vault.len(), 3, "the Key Vault table has three rows");
    for (path, name) in environment {
        assert_eq!(env_name(path), *name);
    }
    for (path, name) in vault {
        assert_eq!(vault_name(path), *name);
    }
}

#[test]
fn documented_defaults_are_the_built_in_values() {
    let config = from_toml("[dns.d]\ntype = \"cloudflare\"\n").expect("valid");
    let dns = &config.dns["d"];
    for (documented, actual) in [
        ("| `parallel` | `4` |", config.run.parallel.to_string()),
        (
            "| `min_reissue` | `\"2d\"` |",
            format_duration(config.run.min_reissue),
        ),
        (
            "| `retry_wait_max` | `\"2m\"` |",
            format_duration(config.run.retry_wait_max),
        ),
        (
            "| `renew_before` | `\"30d\"` |",
            format_duration(defaults::RENEW_BEFORE),
        ),
        (
            "| `key_type` | `\"rsa2048\"` |",
            defaults::KEY_TYPE.to_string(),
        ),
        ("| `ari` | `true` |", defaults::ARI.to_string()),
        ("| `ttl` | `\"60s\"` |", format!("{}s", dns.ttl)),
        (
            "| `propagation_check` | `\"authoritative\"` |",
            dns.propagation_check.to_string(),
        ),
        (
            "| `propagation_interval` | `\"10s\"` |",
            format_duration(dns.propagation_interval),
        ),
        (
            "| `propagation_timeout` | `\"5m\"` |",
            format_duration(defaults::PROPAGATION_TIMEOUT),
        ),
        (
            "| `settle` | `\"30s\"` |",
            format_duration(defaults::SETTLE),
        ),
    ] {
        assert!(
            DOCUMENT.contains(documented),
            "the document has the row {documented}"
        );
        assert!(
            documented.contains(&actual),
            "{documented} states the built-in value {actual}"
        );
    }
    assert!(DOCUMENT.contains("| `propagation_resolvers` | `[\"1.1.1.1\", \"8.8.8.8\"]` |"));
    assert_eq!(dns.propagation_resolvers, defaults::propagation_resolvers());
    assert!(DOCUMENT.contains(&format!("reads `{DEFAULT_PATH}`")));
    assert!(DOCUMENT.contains(&format!("`{PATH_VARIABLE}` names another file")));
}
