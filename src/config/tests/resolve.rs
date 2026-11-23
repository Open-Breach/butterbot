//! Task 2.6: default-and-override resolution.

use std::time::Duration;

use super::*;

/// Builds a configuration with one certificate. Each argument is a block of
/// lines for the ACME server, the DNS provider, both stores and the entry.
fn config(acme: &str, dns: &str, store: &str, certificate: &str) -> Config {
    from_toml(&format!(
        r#"
        [acme.letsencrypt]
        directory_url = "https://acme.example.com/directory"
        {acme}

        [dns.cloudflare]
        type = "cloudflare"
        {dns}

        [store.vault]
        type = "azure-key-vault"
        vault_url = "https://butterbot-certs.vault.example.com/"
        {store}

        [store.local]
        type = "directory"
        path = "/certs"
        {store}

        [[certificate]]
        name = "wildcard-example-com"
        names = ["*.example.com", "example.com"]
        acme = "letsencrypt"
        dns = "cloudflare"
        stores = ["vault", "local"]
        {certificate}
        "#
    ))
    .unwrap_or_else(|error| panic!("{error}"))
}

fn minutes(minutes: u64) -> Duration {
    Duration::from_secs(minutes * 60)
}

// configuration, "Defaults and overrides", scenario "Certificate overrides provider".
#[test]
fn renew_before_of_the_certificate_overrides_the_acme_server() {
    let config = config("renew_before = \"30d\"", "", "", "renew_before = \"20d\"");
    assert_eq!(
        config.resolve(&config.certificates[0]).renew_before,
        days(20)
    );
}

#[test]
fn renew_before_of_the_acme_server_overrides_the_built_in_value() {
    let config = config("renew_before = \"45d\"", "", "", "");
    assert_eq!(
        config.resolve(&config.certificates[0]).renew_before,
        days(45)
    );
}

// configuration, "Defaults and overrides", scenario "Built-in default".
#[test]
fn renew_before_is_30_days_when_nothing_sets_it() {
    let config = config("", "", "", "");
    assert_eq!(
        config.resolve(&config.certificates[0]).renew_before,
        days(30)
    );
}

// acme-issuance, "Key type", scenarios "Default key type" and "Override on the certificate".
#[test]
fn key_type_resolves_certificate_then_server_then_rsa2048() {
    let resolved_key_type = |acme: &str, certificate: &str| {
        let config = config(acme, "", "", certificate);
        config.resolve(&config.certificates[0]).key_type
    };
    assert_eq!(resolved_key_type("", ""), KeyType::Rsa2048);
    assert_eq!(
        resolved_key_type("key_type = \"rsa4096\"", ""),
        KeyType::Rsa4096
    );
    assert_eq!(
        resolved_key_type("", "key_type = \"ecdsa-p256\""),
        KeyType::EcdsaP256
    );
    assert_eq!(
        resolved_key_type("key_type = \"rsa4096\"", "key_type = \"ecdsa-p384\""),
        KeyType::EcdsaP384
    );
}

#[test]
fn profile_resolves_certificate_then_server_then_none() {
    let resolved_profile = |acme: &str, certificate: &str| {
        let config = config(acme, "", "", certificate);
        config
            .resolve(&config.certificates[0])
            .profile
            .map(str::to_owned)
    };
    assert_eq!(
        resolved_profile("", ""),
        None,
        "no profile is requested unless one is set"
    );
    assert_eq!(
        resolved_profile("profile = \"tlsserver\"", "").as_deref(),
        Some("tlsserver")
    );
    assert_eq!(
        resolved_profile("", "profile = \"shortlived\"").as_deref(),
        Some("shortlived")
    );
    assert_eq!(
        resolved_profile("profile = \"tlsserver\"", "profile = \"shortlived\"").as_deref(),
        Some("shortlived")
    );
}

// certificate-stores, "Stored format", scenarios "Key Vault default" and
// "Override on the certificate".
#[test]
fn format_resolves_certificate_then_store_then_the_default_of_the_store_type() {
    let resolved_formats = |store: &str, certificate: &str| {
        let config = config("", "", store, certificate);
        let resolved = config.resolve(&config.certificates[0]);
        let formats: Vec<_> = resolved
            .stores
            .iter()
            .map(|store| (store.name.to_owned(), store.format))
            .collect();
        formats
    };
    let both = |vault, local| vec![("vault".to_owned(), vault), ("local".to_owned(), local)];

    assert_eq!(
        resolved_formats("", ""),
        both(Format::Pfx, Format::Pem),
        "pfx for azure-key-vault and pem for directory"
    );
    assert_eq!(
        resolved_formats("format = \"pem\"", ""),
        both(Format::Pem, Format::Pem)
    );
    assert_eq!(
        resolved_formats("format = \"pfx\"", ""),
        both(Format::Pfx, Format::Pfx)
    );
    assert_eq!(
        resolved_formats("", "format = \"pem\""),
        both(Format::Pem, Format::Pem),
        "the entry's format applies to each of its stores"
    );
    assert_eq!(
        resolved_formats("format = \"pem\"", "format = \"pfx\""),
        both(Format::Pfx, Format::Pfx)
    );
}

#[test]
fn propagation_timeout_resolves_certificate_then_provider_then_5_minutes() {
    let resolved_timeout = |dns: &str, certificate: &str| {
        let config = config("", dns, "", certificate);
        config.resolve(&config.certificates[0]).propagation_timeout
    };
    assert_eq!(resolved_timeout("", ""), minutes(5));
    assert_eq!(
        resolved_timeout("propagation_timeout = \"10m\"", ""),
        minutes(10)
    );
    assert_eq!(
        resolved_timeout("", "propagation_timeout = \"2m\""),
        minutes(2)
    );
    assert_eq!(
        resolved_timeout(
            "propagation_timeout = \"10m\"",
            "propagation_timeout = \"2m\""
        ),
        minutes(2)
    );
}

#[test]
fn settle_resolves_certificate_then_provider_then_30_seconds() {
    let resolved_settle = |dns: &str, certificate: &str| {
        let config = config("", dns, "", certificate);
        config.resolve(&config.certificates[0]).settle
    };
    assert_eq!(resolved_settle("", ""), Duration::from_secs(30));
    assert_eq!(
        resolved_settle("settle = \"60s\"", ""),
        Duration::from_secs(60)
    );
    assert_eq!(resolved_settle("", "settle = \"0s\""), Duration::ZERO);
    assert_eq!(
        resolved_settle("settle = \"60s\"", "settle = \"90s\""),
        Duration::from_secs(90)
    );
}

#[test]
fn resolved_entry_carries_its_providers() {
    let config = config("", "ttl = \"120s\"", "", "");
    let resolved = config.resolve(&config.certificates[0]);
    assert_eq!(resolved.certificate.name, "wildcard-example-com");
    assert_eq!(
        resolved.acme.directory_url,
        "https://acme.example.com/directory"
    );
    assert_eq!(resolved.dns.ttl, 120);
    assert_eq!(resolved.stores[0].store.kind.type_name(), "azure-key-vault");
    assert_eq!(resolved.stores[1].store.kind.type_name(), "directory");
}
