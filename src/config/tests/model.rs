//! Task 2.1: the typed model, unknown keys and the duration type.

use std::path::PathBuf;
use std::time::Duration;

use super::*;

#[test]
fn full_valid_file_is_read_into_the_model() {
    let config = from_toml(FULL).expect("the full example is valid");

    assert_eq!(
        config.run,
        Run {
            parallel: 2,
            min_reissue: days(3),
            retry_wait_max: Duration::from_secs(90)
        }
    );
    assert_eq!(
        config.secrets.key_vault_url.as_deref(),
        Some("https://butterbot-config.vault.example.com/")
    );

    assert_eq!(
        config.acme.keys().collect::<Vec<_>>(),
        ["letsencrypt", "letsencrypt-staging"]
    );
    let letsencrypt = &config.acme["letsencrypt"];
    assert_eq!(
        letsencrypt.directory_url,
        "https://acme.example.com/directory"
    );
    assert_eq!(
        letsencrypt.access_key.as_ref().map(Secret::expose),
        Some(r#"{"id":"https://acme.example.com/acct/1","key_pkcs8":"..."}"#)
    );
    assert_eq!(letsencrypt.eab_kid.as_deref(), Some("kid-1"));
    assert_eq!(
        letsencrypt.eab_key.as_ref().map(Secret::expose),
        Some("eab-secret")
    );
    assert_eq!(letsencrypt.profile.as_deref(), Some("tlsserver"));
    assert_eq!(letsencrypt.renew_before, Some(days(20)));
    assert_eq!(letsencrypt.key_type, Some(KeyType::EcdsaP256));
    assert!(!letsencrypt.ari);

    assert_eq!(
        config.dns["azure"],
        DnsInstance {
            provider: DnsProviderConfig::AzureDns {
                subscription_id: "00000000-0000-0000-0000-000000000000".into(),
                resource_group: "dns-zones".into(),
            },
            ttl: 120,
            propagation_check: PropagationCheck::Resolver,
            propagation_resolvers: vec![
                "192.0.2.1:53".parse().unwrap(),
                "192.0.2.2:5353".parse().unwrap(),
                "[2001:db8::1]:53".parse().unwrap(),
            ],
            propagation_interval: Duration::from_secs(5),
            propagation_timeout: Some(Duration::from_secs(600)),
            settle: Some(Duration::from_secs(45)),
        }
    );
    assert_eq!(
        config.dns["cloudflare"].provider,
        DnsProviderConfig::Cloudflare {
            access_key: Some(Secret::new("cloudflare-token"))
        }
    );

    assert_eq!(
        config.stores["vault"],
        StoreInstance {
            kind: StoreKind::AzureKeyVault {
                vault_url: "https://butterbot-certs.vault.example.com/".into()
            },
            format: Some(Format::Pem),
        }
    );
    assert_eq!(
        config.stores["local"],
        StoreInstance {
            kind: StoreKind::Directory {
                path: PathBuf::from("/var/lib/butterbot/certs")
            },
            format: None,
        }
    );

    assert_eq!(
        config.certificates,
        [
            Certificate {
                name: "wildcard-example-com".into(),
                names: vec!["*.example.com".into(), "example.com".into()],
                acme: "letsencrypt".into(),
                dns: "azure".into(),
                stores: vec!["vault".into(), "local".into()],
                renew_before: Some(days(15)),
                key_type: Some(KeyType::Rsa4096),
                profile: Some("shortlived".into()),
                format: Some(Format::Pfx),
                propagation_timeout: Some(Duration::from_secs(180)),
                settle: Some(Duration::from_secs(60)),
            },
            Certificate {
                name: "api-example-org".into(),
                names: vec!["api.example.org".into()],
                acme: "letsencrypt-staging".into(),
                dns: "cloudflare".into(),
                stores: vec!["local".into()],
                renew_before: None,
                key_type: None,
                profile: None,
                format: None,
                propagation_timeout: None,
                settle: None,
            },
        ]
    );
}

#[test]
fn settings_that_are_left_out_take_the_built_in_values() {
    let config = from_toml(
        r#"
        [acme.letsencrypt]
        directory_url = "https://acme.example.com/directory"

        [dns.cloudflare]
        type = "cloudflare"
        "#,
    )
    .expect("a minimal configuration is valid");

    assert_eq!(
        config.run,
        Run {
            parallel: 4,
            min_reissue: days(2),
            retry_wait_max: Duration::from_secs(120)
        }
    );
    assert_eq!(config.secrets.key_vault_url, None);
    let letsencrypt = &config.acme["letsencrypt"];
    assert!(
        letsencrypt.ari,
        "renewal information is on unless switched off"
    );
    assert_eq!(letsencrypt.access_key, None);
    assert_eq!(
        config.dns["cloudflare"],
        DnsInstance {
            provider: DnsProviderConfig::Cloudflare { access_key: None },
            ttl: 60,
            propagation_check: PropagationCheck::Authoritative,
            propagation_resolvers: vec![
                "1.1.1.1:53".parse().unwrap(),
                "8.8.8.8:53".parse().unwrap()
            ],
            propagation_interval: Duration::from_secs(10),
            propagation_timeout: None,
            settle: None,
        }
    );
    assert!(config.certificates.is_empty());
}

#[test]
fn an_empty_configuration_is_valid() {
    let config = from_toml("").expect("nothing configured is not an error");
    assert!(config.acme.is_empty() && config.dns.is_empty() && config.stores.is_empty());
}

// configuration, "Strict validation before work", scenario "Typo in a key".
#[test]
fn unknown_key_in_a_certificate_names_the_key_and_the_certificate() {
    let text = FULL.replace("renew_before = \"15d\"", "renew_befor = \"15d\"");
    assert_eq!(
        problems(&text),
        ["certificate \"wildcard-example-com\": unknown key `renew_befor` (set in test.toml)"]
    );
}

#[test]
fn unknown_key_is_rejected_in_every_table() {
    let found = problems(
        r#"
        colour = "yellow"

        [run]
        paralel = 2

        [secrets]
        vault = "x"

        [acme.letsencrypt]
        directory_url = "https://acme.example.com/directory"
        directory = "x"

        [dns.cloudflare]
        type = "cloudflare"
        subscription_id = "x"

        [store.local]
        type = "directory"
        path = "/certs"
        vault_url = "x"
        "#,
    );
    assert_eq!(
        found,
        [
            "run: unknown key `paralel` (set in test.toml)",
            "secrets: unknown key `vault` (set in test.toml)",
            "acme.letsencrypt: unknown key `directory` (set in test.toml)",
            "dns.cloudflare: unknown key `subscription_id` (set in test.toml)",
            "store.local: unknown key `vault_url` (set in test.toml)",
            "unknown key `colour` (set in test.toml) (the top-level keys are: run, secrets, \
             acme, dns, store, notify, certificate)",
        ]
    );
}

// configuration, "Durations", scenario "Invalid duration".
#[test]
fn invalid_duration_names_the_key() {
    let text = FULL.replace("renew_before = \"20d\"", "renew_before = \"thirty days\"");
    assert_eq!(
        problems(&text),
        [
            "acme.letsencrypt: `renew_before`: invalid duration \"thirty days\": expected a whole \
          number followed by s, m, h or d, for example \"30d\" (set in test.toml)"
        ]
    );
}

#[test]
fn every_duration_setting_is_checked() {
    let mut text = FULL.to_owned();
    for (from, to) in [
        ("min_reissue = \"3d\"", "min_reissue = \"3 days\""),
        ("retry_wait_max = \"90s\"", "retry_wait_max = \"90\""),
        ("ttl = \"120s\"", "ttl = \"2min\""),
        (
            "propagation_interval = \"5s\"",
            "propagation_interval = \"fast\"",
        ),
        (
            "propagation_timeout = \"10m\"",
            "propagation_timeout = \"1.5h\"",
        ),
        ("settle = \"45s\"", "settle = \"-45s\""),
        ("propagation_timeout = \"3m\"", "propagation_timeout = \"\""),
        ("settle = \"1m\"", "settle = \"1M\""),
    ] {
        assert!(text.contains(from), "{from}");
        text = text.replace(from, to);
    }
    let found = problems(&text);
    assert_eq!(found.len(), 8, "{found:#?}");
    assert!(
        found
            .iter()
            .all(|problem| problem.contains("invalid duration")),
        "{found:#?}"
    );
}

#[test]
fn wrong_value_types_are_rejected() {
    let found = problems(
        r#"
        [run]
        parallel = "four"
        min_reissue = 2

        [acme.letsencrypt]
        directory_url = ["https://acme.example.com/directory"]
        ari = "yes"

        [dns.cloudflare]
        type = "cloudflare"
        access_key = 12345
        propagation_resolvers = "192.0.2.1"

        [[certificate]]
        name = "wildcard-example-com"
        names = "example.com"
        acme = "letsencrypt"
        dns = "cloudflare"
        stores = [1, 2]
        "#,
    );
    assert_eq!(
        found,
        [
            "run: `parallel`: expected a whole number, found a string (set in test.toml)",
            "run: `min_reissue`: expected a string, found a whole number (set in test.toml)",
            "acme.letsencrypt: `directory_url`: expected a string, found a list (set in test.toml)",
            "acme.letsencrypt: `ari`: expected true or false, found a string (set in test.toml)",
            "dns.cloudflare: `access_key`: expected a string, found a whole number (set in test.toml)",
            "dns.cloudflare: `propagation_resolvers`: expected a list of strings, found a string \
             (set in test.toml)",
            "certificate \"wildcard-example-com\": `names`: expected a list of strings, found a \
             string (set in test.toml)",
            "certificate \"wildcard-example-com\": `stores`: expected a list of strings, found a \
             whole number (set in test.toml)",
        ]
    );
}

#[test]
fn numbers_and_booleans_may_be_written_as_text() {
    // Values from the environment and from Key Vault are always text.
    let config = from_toml(
        r#"
        [run]
        parallel = "8"

        [acme.letsencrypt]
        directory_url = "https://acme.example.com/directory"
        ari = "false"
        "#,
    )
    .expect("text is converted");
    assert_eq!(config.run.parallel, 8);
    assert!(!config.acme["letsencrypt"].ari);
}

#[test]
fn values_out_of_range_are_rejected() {
    let found = problems(
        r#"
        [run]
        parallel = 0

        [dns.cloudflare]
        type = "cloudflare"
        ttl = "0s"
        propagation_interval = "0s"
        propagation_resolvers = []

        [dns.other]
        type = "cloudflare"
        propagation_resolvers = ["dns.example.com"]
        "#,
    );
    assert_eq!(
        found,
        [
            "run: `parallel`: 0 is out of range: expected at least 1 (set in test.toml)",
            "dns.cloudflare: `ttl`: \"0s\" is too short: the minimum is 1s (set in test.toml)",
            "dns.cloudflare: `propagation_resolvers`: must list at least one resolver \
             (set in test.toml)",
            "dns.cloudflare: `propagation_interval`: \"0s\" is too short: the minimum is 1s \
             (set in test.toml)",
            "dns.other: `propagation_resolvers`: invalid resolver \"dns.example.com\": expected \
             an IP address, optionally with a port (set in test.toml)",
        ]
    );
}

#[test]
fn missing_required_keys_are_all_reported() {
    let found = problems(
        r#"
        [acme.letsencrypt]

        [dns.azure]
        type = "azure-dns"

        [dns.untyped]
        ttl = "60s"

        [store.vault]
        type = "azure-key-vault"

        [store.local]
        type = "directory"

        [[certificate]]
        "#,
    );
    assert_eq!(
        found,
        [
            "acme.letsencrypt: missing key `directory_url`",
            "dns.azure: missing key `subscription_id`",
            "dns.azure: missing key `resource_group`",
            "dns.untyped: missing key `type`",
            "store.local: missing key `path`",
            "store.vault: missing key `vault_url`",
            "certificate #1: missing key `name`",
            "certificate #1: missing key `names`",
            "certificate #1: missing key `acme`",
            "certificate #1: missing key `dns`",
            "certificate #1: missing key `stores`",
        ]
    );
}

#[test]
fn tables_of_the_wrong_shape_are_rejected() {
    let found = problems(
        r#"
        run = "fast"
        acme = ["letsencrypt"]

        [dns]
        cloudflare = "token"

        [certificate]
        name = "wildcard-example-com"
        "#,
    );
    assert_eq!(
        found,
        [
            "run: expected a table, found a string (set in test.toml)",
            "acme: expected a table of named instances, found a list (set in test.toml)",
            "dns.cloudflare: expected a table, found a string (set in test.toml)",
            // A table is merged from all layers and so has no single origin.
            "certificate: expected a list of entries, each written as [[certificate]], found a \
             table",
        ]
    );
}

#[test]
fn an_empty_secret_counts_as_not_supplied() {
    let config = from_toml(
        r#"
        [dns.cloudflare]
        type = "cloudflare"
        access_key = ""
        "#,
    )
    .expect("an empty secret is not an error");
    assert_eq!(
        config.dns["cloudflare"].provider,
        DnsProviderConfig::Cloudflare { access_key: None }
    );
}

#[test]
fn a_problem_with_a_secret_does_not_show_its_value() {
    let found = problems(
        r#"
        [dns.cloudflare]
        type = "cloudflare"
        access_key = ["hunter2"]
        "#,
    );
    assert_eq!(
        found,
        ["dns.cloudflare: `access_key`: expected a string, found a list (set in test.toml)"]
    );
}
