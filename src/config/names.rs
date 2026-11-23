//! The name of a configuration path in each source.
//!
//! A path is written with dots, for example `acme.letsencrypt-staging.access_key`.
//! Instance names may contain dashes and never underscores; setting names may
//! contain underscores and never dashes. That is what makes the mappings below
//! reversible.

/// The prefix of every environment variable that sets a configuration key.
pub const ENV_PREFIX: &str = "BUTTERBOT__";

/// The top-level tables whose second path segment is an instance name.
pub(super) const INSTANCE_TABLES: [&str; 4] = ["acme", "dns", "store", "notify"];

/// The environment variable for a configuration path: `BUTTERBOT__`, then the
/// path segments in upper case joined by `__`, with dashes replaced by
/// underscores.
pub fn env_name(path: &str) -> String {
    let segments: Vec<String> = path
        .split('.')
        .map(|segment| segment.to_ascii_uppercase().replace('-', "_"))
        .collect();
    format!("{ENV_PREFIX}{}", segments.join("__"))
}

/// The configuration path an environment variable sets, as segments.
///
/// `None` when the variable does not carry the prefix. An error when it does
/// but cannot name a configuration key.
pub(super) fn path_from_env_name(name: &str) -> Option<Result<Vec<String>, String>> {
    let rest = name.strip_prefix(ENV_PREFIX)?;
    let mut segments: Vec<String> = rest.split("__").map(str::to_ascii_lowercase).collect();
    if segments.len() < 2 || segments.iter().any(String::is_empty) {
        return Some(Err(format!(
            "environment variable {name} does not name a configuration key: expected \
             {ENV_PREFIX}<TABLE>__<KEY>, with the segments of the path separated by `__`"
        )));
    }
    if segments[0] == "certificate" {
        return Some(Err(format!(
            "environment variable {name}: certificate entries cannot be set from the \
             environment, only in a configuration file"
        )));
    }
    if INSTANCE_TABLES.contains(&segments[0].as_str()) {
        segments[1] = segments[1].replace('_', "-");
    }
    Some(Ok(segments))
}

#[cfg(test)]
mod tests {
    use super::*;

    // configuration, "Source names for a configuration path", scenario "Environment name".
    #[test]
    fn environment_name_of_a_path() {
        assert_eq!(
            env_name("acme.letsencrypt-staging.access_key"),
            "BUTTERBOT__ACME__LETSENCRYPT_STAGING__ACCESS_KEY"
        );
        assert_eq!(
            env_name("dns.cloudflare.access_key"),
            "BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY"
        );
        assert_eq!(
            env_name("run.retry_wait_max"),
            "BUTTERBOT__RUN__RETRY_WAIT_MAX"
        );
    }

    #[test]
    fn path_of_an_environment_name() {
        let path = |name| {
            path_from_env_name(name)
                .expect("has the prefix")
                .expect("names a key")
        };
        assert_eq!(
            path("BUTTERBOT__ACME__LETSENCRYPT_STAGING__ACCESS_KEY"),
            ["acme", "letsencrypt-staging", "access_key"]
        );
        assert_eq!(
            path("BUTTERBOT__DNS__CLOUDFLARE_EU__PROPAGATION_TIMEOUT"),
            ["dns", "cloudflare-eu", "propagation_timeout"]
        );
        assert_eq!(
            path("BUTTERBOT__RUN__RETRY_WAIT_MAX"),
            ["run", "retry_wait_max"]
        );
        assert_eq!(
            path("BUTTERBOT__SECRETS__KEY_VAULT_URL"),
            ["secrets", "key_vault_url"]
        );
    }

    #[test]
    fn the_two_mappings_are_inverse() {
        for path in [
            "acme.letsencrypt-staging.access_key",
            "dns.cloudflare-eu.propagation_timeout",
            "store.key-vault-1.vault_url",
            "run.min_reissue",
        ] {
            let segments = path_from_env_name(&env_name(path)).unwrap().unwrap();
            assert_eq!(segments.join("."), path);
        }
    }

    #[test]
    fn other_variables_are_not_ours() {
        assert_eq!(path_from_env_name("PATH"), None);
        assert_eq!(path_from_env_name("BUTTERBOT_CONFIG"), None);
        assert_eq!(path_from_env_name("butterbot__run__parallel"), None);
    }

    #[test]
    fn prefixed_variable_that_names_no_key_is_an_error() {
        for name in [
            "BUTTERBOT__",
            "BUTTERBOT__RUN",
            "BUTTERBOT__RUN__",
            "BUTTERBOT____PARALLEL",
        ] {
            let error = path_from_env_name(name)
                .expect("has the prefix")
                .expect_err(name);
            assert!(
                error.contains(name) && error.contains("does not name"),
                "{error}"
            );
        }
        let error = path_from_env_name("BUTTERBOT__CERTIFICATE__0__NAME")
            .unwrap()
            .unwrap_err();
        assert!(
            error.contains("cannot be set from the environment"),
            "{error}"
        );
    }
}
