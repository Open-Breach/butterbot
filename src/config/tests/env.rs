//! Task 2.3: the environment layer.

use std::ffi::OsString;
use std::time::Duration;

use super::*;

const FILE: &str = r#"
[acme.letsencrypt-staging]
directory_url = "https://acme-staging.example.com/directory"
access_key = "from-the-file"

[dns.cloudflare]
type = "cloudflare"

[dns.cloudflare-eu]
type = "cloudflare"
ttl = "60s"
"#;

// configuration, "Layered sources", scenario "Environment overrides file".
#[test]
fn environment_overrides_a_file_value() {
    let config = from_toml_and_env(
        FILE,
        &[(
            "BUTTERBOT__ACME__LETSENCRYPT_STAGING__ACCESS_KEY",
            "from-the-environment",
        )],
    )
    .expect("valid");
    assert_eq!(
        config.acme["letsencrypt-staging"]
            .access_key
            .as_ref()
            .map(Secret::expose),
        Some("from-the-environment")
    );
    assert_eq!(
        config.acme["letsencrypt-staging"].directory_url,
        "https://acme-staging.example.com/directory",
        "the other keys of the instance still come from the file"
    );
}

#[test]
fn without_the_variable_the_file_value_is_used() {
    let config = from_toml_and_env(FILE, &[("PATH", "/usr/bin")]).expect("valid");
    assert_eq!(
        config.acme["letsencrypt-staging"]
            .access_key
            .as_ref()
            .map(Secret::expose),
        Some("from-the-file")
    );
}

// configuration, "Secrets are identified by configuration path",
// scenarios "Two instances, two secrets" and "Secret supplied outside the file".
#[test]
fn secrets_of_two_instances_come_from_two_variables() {
    let config = from_toml_and_env(
        FILE,
        &[
            ("BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY", "token-one"),
            ("BUTTERBOT__DNS__CLOUDFLARE_EU__ACCESS_KEY", "token-two"),
        ],
    )
    .expect("valid");
    let token = |instance: &str| match &config.dns[instance].provider {
        DnsProviderConfig::Cloudflare { access_key } => {
            access_key.as_ref().map(|secret| secret.expose().to_owned())
        }
        other => panic!("unexpected provider {other:?}"),
    };
    assert_eq!(token("cloudflare").as_deref(), Some("token-one"));
    assert_eq!(token("cloudflare-eu").as_deref(), Some("token-two"));
}

#[test]
fn settings_that_are_not_text_can_be_set() {
    let config = from_toml_and_env(
        FILE,
        &[
            ("BUTTERBOT__RUN__PARALLEL", "8"),
            ("BUTTERBOT__RUN__RETRY_WAIT_MAX", "5m"),
            ("BUTTERBOT__ACME__LETSENCRYPT_STAGING__ARI", "false"),
            ("BUTTERBOT__DNS__CLOUDFLARE_EU__PROPAGATION_TIMEOUT", "10m"),
            (
                "BUTTERBOT__SECRETS__KEY_VAULT_URL",
                "https://butterbot-config.vault.example.com/",
            ),
        ],
    )
    .expect("valid");
    assert_eq!(config.run.parallel, 8);
    assert_eq!(config.run.retry_wait_max, Duration::from_secs(300));
    assert!(!config.acme["letsencrypt-staging"].ari);
    assert_eq!(
        config.dns["cloudflare-eu"].propagation_timeout,
        Some(Duration::from_secs(600))
    );
    assert_eq!(
        config.secrets.key_vault_url.as_deref(),
        Some("https://butterbot-config.vault.example.com/")
    );
}

#[test]
fn problem_with_a_value_names_the_variable() {
    let error = from_toml_and_env(FILE, &[("BUTTERBOT__RUN__PARALLEL", "many")])
        .expect_err("not a whole number");
    assert_eq!(
        error.problems,
        ["run: `parallel`: expected a whole number, found a string \
          (set in environment variable BUTTERBOT__RUN__PARALLEL)"]
    );
}

#[test]
fn variable_for_an_unknown_key_is_rejected() {
    let error = from_toml_and_env(FILE, &[("BUTTERBOT__RUN__PARALEL", "8")])
        .expect_err("the key does not exist");
    assert_eq!(
        error.problems,
        ["run: unknown key `paralel` (set in environment variable BUTTERBOT__RUN__PARALEL)"]
    );
}

#[test]
fn variables_that_name_no_key_are_all_reported() {
    let error = env_layer(variables(&[
        ("BUTTERBOT__RUN", "8"),
        ("BUTTERBOT__CERTIFICATE__0__NAME", "wildcard-example-com"),
        ("BUTTERBOT__RUN__PARALLEL", "8"),
    ]))
    .expect_err("two variables are unusable");
    assert_eq!(error.problems.len(), 2, "{error}");
    assert!(
        error.problems[0].contains("BUTTERBOT__CERTIFICATE__0__NAME"),
        "{error}"
    );
    assert!(error.problems[1].contains("BUTTERBOT__RUN "), "{error}");
}

#[cfg(unix)]
#[test]
fn value_that_is_not_utf8_is_reported_without_its_content() {
    use std::os::unix::ffi::OsStringExt;

    let value = OsString::from_vec(vec![b's', b'e', b'c', 0xff]);
    let error = env_layer([(
        "BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY".into(),
        value.clone(),
    )])
    .expect_err("not UTF-8");
    assert_eq!(
        error.problems,
        ["environment variable BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY is not valid UTF-8"]
    );

    // A variable of another program is none of our business.
    env_layer([("LS_COLORS".into(), value)]).expect("ignored");
}
