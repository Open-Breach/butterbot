//! Task 2.5: validation. One test per scenario of the validation
//! requirements of the `configuration` spec.

use super::*;

/// Providers that the entries in these tests refer to.
const PROVIDERS: &str = r#"
[acme.letsencrypt]
directory_url = "https://acme.example.com/directory"

[acme.letsencrypt-staging]
directory_url = "https://acme-staging.example.com/directory"

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

[store.local]
type = "directory"
path = "/var/lib/butterbot/certs"
"#;

/// A certificate entry with the given name and extra lines.
fn entry(name: &str, dns: &str, stores: &str, extra: &str) -> String {
    format!(
        "\n[[certificate]]\nname = \"{name}\"\nnames = [\"{name}.example.com\"]\n\
         acme = \"letsencrypt\"\ndns = \"{dns}\"\nstores = {stores}\n{extra}\n"
    )
}

fn with(entries: &[String]) -> String {
    format!("{PROVIDERS}{}", entries.concat())
}

// "Provider instances", scenario "Two instances of one type".
#[test]
fn two_instances_of_one_type_are_both_available() {
    let config = from_toml(&with(&[
        entry("one", "cloudflare", "[\"local\"]", ""),
        entry("two", "cloudflare-eu", "[\"local\"]", ""),
    ]))
    .expect("valid");

    for instance in ["cloudflare", "cloudflare-eu"] {
        assert_eq!(config.dns[instance].provider.type_name(), "cloudflare");
    }
    assert_eq!(config.certificates[0].dns, "cloudflare");
    assert_eq!(
        config.certificates[1].dns, "cloudflare-eu",
        "each certificate uses the one it names"
    );
}

// "Provider instances", scenario "Unknown type".
#[test]
fn unknown_type_names_the_instance_and_the_type() {
    let found = problems(
        r#"
        [dns.route]
        type = "route53"
        hosted_zone = "Z123"

        [store.bucket]
        type = "s3"

        [notify.chat]
        type = "slack"
        events = ["failure", "expiring"]
        "#,
    );
    assert_eq!(
        found,
        [
            "dns.route: unknown type \"route53\" (known types: azure-dns, cloudflare)",
            "store.bucket: unknown type \"s3\" (known types: azure-key-vault, directory)",
            "notify.chat: unknown type \"slack\" (this version has no types of this kind)",
        ]
    );
}

#[test]
fn unknown_event_in_a_notifier_selection_is_reported() {
    let found = problems("[notify.chat]\ntype = \"slack\"\nevents = [\"failure\", \"renewal\"]\n");
    assert_eq!(found.len(), 2, "{found:#?}");
    assert!(
        found[0].starts_with("notify.chat: `events`: unknown event \"renewal\""),
        "{found:#?}"
    );
}

// "Instance and certificate names", scenario "Invalid name".
#[test]
fn invalid_instance_name_is_rejected_and_named() {
    let found = problems("[dns.Cloud_Flare]\ntype = \"cloudflare\"\n");
    assert_eq!(
        found,
        [
            "dns.Cloud_Flare: invalid name: a name consists of lowercase letters, digits and single \
          dashes, and starts and ends with a letter or digit"
        ]
    );
}

#[test]
fn name_alphabet_applies_to_every_kind_of_name() {
    for name in [
        "Cloud_Flare",
        "cloud_flare",
        "CloudFlare",
        "-cloudflare",
        "cloudflare-",
        "cloud--flare",
        "cloud.flare",
        "wolke-ä",
    ] {
        for table in ["acme", "dns", "store", "notify"] {
            let found = problems(&format!("[{table}.\"{name}\"]\ntype = \"cloudflare\"\n"));
            assert!(
                found
                    .iter()
                    .any(|problem| problem.starts_with(&format!("{table}.{name}: invalid name"))),
                "{table}.{name}: {found:#?}"
            );
        }
        let found = problems(&with(&[entry(name, "cloudflare", "[\"local\"]", "")]));
        assert_eq!(found.len(), 1, "{name}: {found:#?}");
        assert!(
            found[0].starts_with(&format!("certificate {name:?}: `name`: invalid name")),
            "{name}: {found:#?}"
        );
    }
    for name in [
        "cloudflare",
        "cloudflare-eu",
        "cf2",
        "2cf",
        "a",
        "a-b-c",
        "0",
    ] {
        from_toml(&format!("[dns.{name}]\ntype = \"cloudflare\"\n")).expect(name);
    }
}

// "Instance and certificate names", scenario "Duplicate certificate name".
#[test]
fn duplicate_certificate_name_is_rejected() {
    let found = problems(&with(&[
        entry("wildcard", "cloudflare", "[\"local\"]", ""),
        entry("other", "cloudflare", "[\"local\"]", ""),
        entry("wildcard", "azure", "[\"vault\"]", ""),
        entry("wildcard", "azure", "[\"vault\"]", ""),
    ]));
    assert_eq!(
        found,
        ["certificate \"wildcard\" is defined more than once: certificate names are unique"]
    );
}

#[test]
fn instance_names_need_only_be_unique_within_their_kind() {
    from_toml(
        r#"
        [dns.main]
        type = "cloudflare"

        [store.main]
        type = "directory"
        path = "/certs"

        [acme.main]
        directory_url = "https://acme.example.com/directory"
        "#,
    )
    .expect("the same name in different kinds is not a clash");
}

// "Certificate entries", scenario "Valid entry".
#[test]
fn entry_with_existing_references_is_accepted() {
    let config = from_toml(&with(&[entry(
        "wildcard",
        "azure",
        "[\"vault\", \"local\"]",
        "",
    )]))
    .expect("valid");
    let certificate = &config.certificates[0];
    assert_eq!(certificate.acme, "letsencrypt");
    assert_eq!(certificate.dns, "azure");
    assert_eq!(certificate.stores, ["vault", "local"]);
}

// "Certificate entries", scenario "Dangling reference".
#[test]
fn dangling_store_reference_names_the_certificate_and_the_store() {
    let found = problems(&with(&[entry(
        "wildcard",
        "azure",
        "[\"vault\", \"backup\"]",
        "",
    )]));
    assert_eq!(
        found,
        ["certificate \"wildcard\": `stores`: the store \"backup\" is not defined"]
    );
}

#[test]
fn every_dangling_reference_is_reported() {
    let text = with(&[entry(
        "wildcard",
        "route53",
        "[\"backup\", \"vault\", \"offsite\"]",
        "",
    )])
    .replace("acme = \"letsencrypt\"", "acme = \"zerossl\"");
    assert_eq!(
        problems(&text),
        [
            "certificate \"wildcard\": `acme`: the ACME server \"zerossl\" is not defined",
            "certificate \"wildcard\": `dns`: the DNS provider \"route53\" is not defined",
            "certificate \"wildcard\": `stores`: the store \"backup\" is not defined",
            "certificate \"wildcard\": `stores`: the store \"offsite\" is not defined",
        ]
    );
}

#[test]
fn reference_to_an_instance_with_a_problem_is_not_reported_as_dangling() {
    let text = with(&[entry("wildcard", "cloudflare", "[\"local\"]", "")]).replace(
        "path = \"/var/lib/butterbot/certs\"",
        "pathh = \"/var/lib/butterbot/certs\"",
    );
    assert_eq!(
        problems(&text),
        [
            "store.local: missing key `path`",
            "store.local: unknown key `pathh` (set in test.toml)",
        ]
    );
}

#[test]
fn entry_needs_at_least_one_name_and_one_store() {
    let text = with(&[entry("wildcard", "cloudflare", "[]", "")])
        .replace("names = [\"wildcard.example.com\"]", "names = []");
    assert_eq!(
        problems(&text),
        [
            "certificate \"wildcard\": `names`: must list at least one DNS name",
            "certificate \"wildcard\": `stores`: must list at least one store",
        ]
    );
}

// acme-issuance, "Key type", scenario "Unknown key type".
#[test]
fn unknown_key_type_is_rejected() {
    let text = with(&[entry(
        "wildcard",
        "cloudflare",
        "[\"local\"]",
        "key_type = \"rsa1024\"",
    )])
    .replace(
        "directory_url = \"https://acme.example.com/directory\"",
        "directory_url = \"https://acme.example.com/directory\"\nkey_type = \"ed25519\"",
    );
    assert_eq!(
        problems(&text),
        [
            "acme.letsencrypt: `key_type`: unknown key type \"ed25519\" (expected one of: rsa2048, \
             rsa3072, rsa4096, ecdsa-p256, ecdsa-p384) (set in test.toml)",
            "certificate \"wildcard\": `key_type`: unknown key type \"rsa1024\" (expected one of: \
             rsa2048, rsa3072, rsa4096, ecdsa-p256, ecdsa-p384) (set in test.toml)",
        ]
    );
}

#[test]
fn every_key_type_of_the_spec_is_accepted() {
    for key_type in ["rsa2048", "rsa3072", "rsa4096", "ecdsa-p256", "ecdsa-p384"] {
        let extra = format!("key_type = \"{key_type}\"");
        let config = from_toml(&with(&[entry(
            "wildcard",
            "cloudflare",
            "[\"local\"]",
            &extra,
        )]))
        .expect(key_type);
        assert_eq!(
            config.certificates[0].key_type.map(KeyType::as_str),
            Some(key_type)
        );
    }
}

#[test]
fn unknown_format_is_rejected() {
    let text = with(&[entry(
        "wildcard",
        "cloudflare",
        "[\"local\"]",
        "format = \"der\"",
    )])
    .replace(
        "path = \"/var/lib/butterbot/certs\"",
        "path = \"/certs\"\nformat = \"jks\"",
    );
    assert_eq!(
        problems(&text),
        [
            "store.local: `format`: unknown format \"jks\" (expected one of: pfx, pem) (set in test.toml)",
            "certificate \"wildcard\": `format`: unknown format \"der\" (expected one of: pfx, pem) \
             (set in test.toml)",
        ]
    );
}

#[test]
fn unknown_propagation_check_is_rejected() {
    let found =
        problems("[dns.cloudflare]\ntype = \"cloudflare\"\npropagation_check = \"public\"\n");
    assert_eq!(
        found,
        [
            "dns.cloudflare: `propagation_check`: unknown propagation check \"public\" (expected one \
          of: authoritative, resolver, off) (set in test.toml)"
        ]
    );
}

// "Strict validation before work", scenario "Several errors".
#[test]
fn independent_errors_are_all_reported_in_one_run() {
    let text = with(&[
        entry(
            "wildcard",
            "cloudflare",
            "[\"backup\"]",
            "renew_befor = \"20d\"\nsettle = \"soon\"",
        ),
        entry("Api", "cloudflare", "[\"local\"]", ""),
    ])
    .replace(
        "resource_group = \"dns-zones\"",
        "resource_group = \"dns-zones\"\nttl = \"1 minute\"",
    ) + "\n[run]\nparallel = 0\n";
    let error = from_toml(&text).expect_err("six problems");
    assert_eq!(
        error.problems,
        [
            "run: `parallel`: 0 is out of range: expected at least 1 (set in test.toml)",
            "dns.azure: `ttl`: invalid duration \"1 minute\": expected a whole number followed by \
             s, m, h or d, for example \"30d\" (set in test.toml)",
            "certificate \"wildcard\": `stores`: the store \"backup\" is not defined",
            "certificate \"wildcard\": `settle`: invalid duration \"soon\": expected a whole number \
             followed by s, m, h or d, for example \"30d\" (set in test.toml)",
            "certificate \"wildcard\": unknown key `renew_befor` (set in test.toml)",
            "certificate \"Api\": `name`: invalid name: a name consists of lowercase letters, \
             digits and single dashes, and starts and ends with a letter or digit",
        ]
    );
    let report = error.to_string();
    assert!(
        report.starts_with("invalid configuration (6 problems):\n  - run: "),
        "{report}"
    );
}

#[test]
fn single_problem_is_reported_on_one_line() {
    let error = from_toml("[run]\nparallel = 0\n").expect_err("one problem");
    assert_eq!(
        error.to_string(),
        "invalid configuration: run: `parallel`: 0 is out of range: expected at least 1 \
         (set in test.toml)"
    );
}
