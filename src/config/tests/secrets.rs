//! Task 2.7: secrets are never printed, and the listing of expected secrets.

use super::*;

const FILE: &str = r#"
[acme.letsencrypt]
directory_url = "https://acme.example.com/directory"
access_key = "account-credentials-in-the-file"

[acme.letsencrypt-staging]
directory_url = "https://acme-staging.example.com/directory"

[acme.bound]
directory_url = "https://acme-bound.example.com/directory"
eab_kid = "kid-1"

[dns.azure]
type = "azure-dns"
subscription_id = "00000000-0000-0000-0000-000000000000"
resource_group = "dns-zones"

[dns.cloudflare]
type = "cloudflare"

[dns.cloudflare-eu]
type = "cloudflare"

[store.vault]
type = "azure-key-vault"
vault_url = "https://butterbot-certs.vault.example.com/"
"#;

fn listing(pairs: &[(&str, &str)]) -> Vec<ExpectedSecret> {
    from_toml_and_env(FILE, pairs)
        .unwrap_or_else(|error| panic!("{error}"))
        .expected_secrets()
}

fn secret<'a>(listing: &'a [ExpectedSecret], path: &str) -> &'a ExpectedSecret {
    listing
        .iter()
        .find(|secret| secret.path == path)
        .unwrap_or_else(|| panic!("{path} is listed"))
}

// configuration, "Source names for a configuration path", both scenarios, and
// "Secret values are never disclosed", scenario "Configuration dump".
#[test]
fn listing_gives_path_source_names_and_whether_found() {
    let listing = listing(&[("BUTTERBOT__DNS__CLOUDFLARE_EU__ACCESS_KEY", "token-two")]);

    assert_eq!(
        listing,
        [
            ExpectedSecret {
                path: "acme.bound.access_key".into(),
                env_name: "BUTTERBOT__ACME__BOUND__ACCESS_KEY".into(),
                vault_name: "acme--bound--access-key".into(),
                found: false,
            },
            ExpectedSecret {
                path: "acme.bound.eab_key".into(),
                env_name: "BUTTERBOT__ACME__BOUND__EAB_KEY".into(),
                vault_name: "acme--bound--eab-key".into(),
                found: false,
            },
            ExpectedSecret {
                path: "acme.letsencrypt.access_key".into(),
                env_name: "BUTTERBOT__ACME__LETSENCRYPT__ACCESS_KEY".into(),
                vault_name: "acme--letsencrypt--access-key".into(),
                found: true,
            },
            ExpectedSecret {
                path: "acme.letsencrypt-staging.access_key".into(),
                env_name: "BUTTERBOT__ACME__LETSENCRYPT_STAGING__ACCESS_KEY".into(),
                vault_name: "acme--letsencrypt-staging--access-key".into(),
                found: false,
            },
            ExpectedSecret {
                path: "dns.cloudflare.access_key".into(),
                env_name: "BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY".into(),
                vault_name: "dns--cloudflare--access-key".into(),
                found: false,
            },
            ExpectedSecret {
                path: "dns.cloudflare-eu.access_key".into(),
                env_name: "BUTTERBOT__DNS__CLOUDFLARE_EU__ACCESS_KEY".into(),
                vault_name: "dns--cloudflare-eu--access-key".into(),
                found: true,
            },
        ]
    );
}

// configuration, "Secrets are identified by configuration path",
// scenario "Two instances, two secrets".
#[test]
fn two_instances_of_one_type_have_distinct_secrets() {
    let listing = listing(&[]);
    let first = secret(&listing, "dns.cloudflare.access_key");
    let second = secret(&listing, "dns.cloudflare-eu.access_key");
    assert_ne!(first.env_name, second.env_name);
    assert_ne!(first.vault_name, second.vault_name);
}

#[test]
fn providers_on_the_ambient_azure_identity_need_no_secret() {
    let listing = listing(&[]);
    assert!(
        !listing.iter().any(
            |secret| secret.path.starts_with("dns.azure.") || secret.path.starts_with("store.")
        ),
        "{listing:#?}"
    );
}

#[test]
fn listed_name_is_the_name_that_supplies_the_secret() {
    for expected in listing(&[]) {
        let after = listing(&[(expected.env_name.as_str(), "supplied")]);
        assert!(
            secret(&after, &expected.path).found,
            "{} via {}",
            expected.path,
            expected.env_name
        );
    }
}

#[test]
fn an_empty_value_is_listed_as_missing() {
    let listing = listing(&[("BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY", "")]);
    assert!(!secret(&listing, "dns.cloudflare.access_key").found);
}

// configuration, "Secret values are never disclosed".
#[test]
fn neither_the_configuration_nor_the_listing_prints_a_secret_value() {
    let pairs = [
        (
            "BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY",
            "token-in-the-environment",
        ),
        (
            "BUTTERBOT__ACME__BOUND__EAB_KEY",
            "binding-key-in-the-environment",
        ),
    ];
    let config = from_toml_and_env(FILE, &pairs).unwrap_or_else(|error| panic!("{error}"));
    let printed = format!(
        "{config:?}\n{config:#?}\n{:?}\n{:#?}",
        config.expected_secrets(),
        config.resolve_all()
    );
    for value in [
        "account-credentials-in-the-file",
        "token-in-the-environment",
        "binding-key-in-the-environment",
    ] {
        assert!(!printed.contains(value), "{value} is printed");
    }
    assert!(printed.contains(PLACEHOLDER));
}
