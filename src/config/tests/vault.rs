//! Task 2.4: the Key Vault layer, against a fake vault.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tempfile::TempDir;

use super::*;
use crate::Error;

const VAULT_URL: &str = "https://butterbot-config.vault.example.com/";

const FILE: &str = r#"
[secrets]
key_vault_url = "https://butterbot-config.vault.example.com/"

[dns.cloudflare]
type = "cloudflare"
"#;

/// A vault held in memory that records what is asked of it.
#[derive(Default)]
struct FakeVault {
    secrets: Mutex<BTreeMap<String, String>>,
    /// Failures returned by the next calls of `list_names`, in order.
    list_failures: Mutex<VecDeque<Error>>,
    /// Failure returned by every call of `get`.
    get_failure: Option<Error>,
    listed: Mutex<usize>,
    fetched: Mutex<Vec<String>>,
}

impl FakeVault {
    fn with(secrets: &[(&str, &str)]) -> Self {
        let secrets = secrets
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()));
        Self {
            secrets: Mutex::new(secrets.collect()),
            ..Self::default()
        }
    }

    fn failing_to_list(failures: impl IntoIterator<Item = Error>) -> Self {
        Self {
            list_failures: Mutex::new(failures.into_iter().collect()),
            ..Self::default()
        }
    }

    fn fetched(&self) -> Vec<String> {
        self.fetched.lock().unwrap().clone()
    }

    fn listed(&self) -> usize {
        *self.listed.lock().unwrap()
    }
}

#[async_trait]
impl SecretVault for FakeVault {
    async fn list_names(&self) -> crate::Result<Vec<String>> {
        *self.listed.lock().unwrap() += 1;
        if let Some(failure) = self.list_failures.lock().unwrap().pop_front() {
            return Err(failure);
        }
        Ok(self.secrets.lock().unwrap().keys().cloned().collect())
    }

    async fn get(&self, name: &str) -> crate::Result<Secret> {
        self.fetched.lock().unwrap().push(name.to_owned());
        if let Some(failure) = &self.get_failure {
            return Err(failure.clone());
        }
        let secrets = self.secrets.lock().unwrap();
        secrets
            .get(name)
            .map(Secret::new)
            .ok_or_else(|| Error::permanent("secret not found"))
    }

    async fn set(&self, name: &str, value: &Secret) -> crate::Result<()> {
        self.secrets
            .lock()
            .unwrap()
            .insert(name.to_owned(), value.expose().to_owned());
        Ok(())
    }
}

struct Fixture {
    _directory: TempDir,
    file: PathBuf,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let directory = TempDir::new().unwrap();
        let file = directory.path().join("config.toml");
        std::fs::write(&file, text).unwrap();
        Self {
            _directory: directory,
            file,
        }
    }

    /// Loads with `vault` standing behind every vault URL.
    async fn load(
        &self,
        pairs: &[(&str, &str)],
        vault: &Arc<FakeVault>,
    ) -> Result<Loaded, ConfigError> {
        let vault: Arc<dyn SecretVault> = vault.clone();
        load(
            &quiet(),
            std::slice::from_ref(&self.file),
            variables(pairs),
            |url| {
                assert_eq!(url, VAULT_URL, "the configured vault is the one opened");
                Ok(vault)
            },
        )
        .await
    }
}

fn cloudflare_token(config: &Config) -> Option<String> {
    match &config.dns["cloudflare"].provider {
        DnsProviderConfig::Cloudflare { access_key } => {
            access_key.as_ref().map(|secret| secret.expose().to_owned())
        }
        other => panic!("unexpected provider {other:?}"),
    }
}

// configuration, "Key Vault layer is optional", scenario "No vault configured".
#[tokio::test]
async fn without_a_vault_url_no_vault_is_contacted() {
    let fixture = Fixture::new("[dns.cloudflare]\ntype = \"cloudflare\"\n");
    let loaded = load(
        &quiet(),
        std::slice::from_ref(&fixture.file),
        variables(&[]),
        |_| panic!("no vault may be opened when secrets.key_vault_url is not set"),
    )
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(loaded.vault.is_none());
    assert_eq!(cloudflare_token(&loaded.config), None);
}

#[tokio::test]
async fn only_configuration_secrets_are_fetched() {
    let vault = Arc::new(FakeVault::with(&[
        ("dns--cloudflare--access-key", "token-from-the-vault"),
        ("run--parallel", "8"),
        ("database-password", "unrelated"),
        ("wildcard-example-com", "unrelated"),
        ("storage--account--key", "unrelated"),
        ("certificate--0--name", "unrelated"),
    ]));
    let loaded = Fixture::new(FILE)
        .load(&[], &vault)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        vault.fetched(),
        ["dns--cloudflare--access-key", "run--parallel"]
    );
    assert_eq!(
        cloudflare_token(&loaded.config).as_deref(),
        Some("token-from-the-vault")
    );
    assert_eq!(loaded.config.run.parallel, 8);
    assert!(
        loaded.vault.is_some(),
        "the vault is handed on for saving account credentials"
    );
}

// configuration, "Layered sources", scenario "Key Vault overrides environment".
#[tokio::test]
async fn vault_overrides_the_environment() {
    let vault = Arc::new(FakeVault::with(&[(
        "dns--cloudflare--access-key",
        "from-the-vault",
    )]));
    let pairs = [(
        "BUTTERBOT__DNS__CLOUDFLARE__ACCESS_KEY",
        "from-the-environment",
    )];

    let loaded = Fixture::new(FILE)
        .load(&pairs, &vault)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        cloudflare_token(&loaded.config).as_deref(),
        Some("from-the-vault")
    );

    let empty = Arc::new(FakeVault::default());
    let loaded = Fixture::new(FILE)
        .load(&pairs, &empty)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        cloudflare_token(&loaded.config).as_deref(),
        Some("from-the-environment"),
        "a key the vault does not hold keeps its value from the environment"
    );
}

#[tokio::test]
async fn vault_url_from_the_environment_enables_the_layer() {
    let vault = Arc::new(FakeVault::with(&[(
        "dns--cloudflare--access-key",
        "from-the-vault",
    )]));
    let fixture = Fixture::new("[dns.cloudflare]\ntype = \"cloudflare\"\n");
    let loaded = fixture
        .load(&[("BUTTERBOT__SECRETS__KEY_VAULT_URL", VAULT_URL)], &vault)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        cloudflare_token(&loaded.config).as_deref(),
        Some("from-the-vault")
    );
}

// configuration, "Key Vault layer is optional", scenario "Vault unreachable".
#[tokio::test(start_paused = true)]
async fn unreachable_vault_is_a_configuration_error() {
    let vault = Arc::new(FakeVault::failing_to_list([
        Error::transient("connection refused"),
        Error::transient("connection refused"),
        Error::transient("connection refused"),
    ]));
    let error = Fixture::new(FILE)
        .load(&[], &vault)
        .await
        .err()
        .expect("the vault cannot be read");
    assert_eq!(
        error.problems,
        [format!(
            "Key Vault {VAULT_URL} cannot be read: connection refused"
        )]
    );
    assert_eq!(
        vault.listed(),
        3,
        "a transient failure is retried before giving up"
    );
}

#[tokio::test(start_paused = true)]
async fn vault_that_recovers_is_read() {
    let vault = Arc::new(FakeVault {
        list_failures: Mutex::new([Error::transient("connection reset")].into()),
        ..FakeVault::with(&[("dns--cloudflare--access-key", "from-the-vault")])
    });
    let loaded = Fixture::new(FILE)
        .load(&[], &vault)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        cloudflare_token(&loaded.config).as_deref(),
        Some("from-the-vault")
    );
    assert_eq!(vault.listed(), 2);
}

#[tokio::test]
async fn vault_that_refuses_access_is_a_configuration_error_at_once() {
    let vault = Arc::new(FakeVault::failing_to_list([Error::permanent(
        "403 Forbidden",
    )]));
    let error = Fixture::new(FILE)
        .load(&[], &vault)
        .await
        .err()
        .expect("access is refused");
    assert_eq!(
        error.problems,
        [format!(
            "Key Vault {VAULT_URL} cannot be read: 403 Forbidden"
        )]
    );
    assert_eq!(vault.listed(), 1);
}

#[tokio::test]
async fn secret_that_cannot_be_fetched_is_a_configuration_error() {
    let vault = Arc::new(FakeVault {
        get_failure: Some(Error::permanent("403 Forbidden")),
        ..FakeVault::with(&[("dns--cloudflare--access-key", "from-the-vault")])
    });
    let error = Fixture::new(FILE)
        .load(&[], &vault)
        .await
        .err()
        .expect("the secret is unreadable");
    assert_eq!(
        error.problems,
        [format!(
            "Key Vault {VAULT_URL} cannot be read: 403 Forbidden"
        )]
    );
}

#[tokio::test]
async fn vault_that_cannot_be_opened_is_a_configuration_error() {
    let fixture = Fixture::new(FILE);
    let error = load(
        &quiet(),
        std::slice::from_ref(&fixture.file),
        variables(&[]),
        |_| Err(Error::permanent("no Azure identity is available")),
    )
    .await
    .err()
    .expect("the vault cannot be opened");
    assert_eq!(
        error.problems,
        [format!(
            "Key Vault {VAULT_URL} cannot be read: no Azure identity is available"
        )]
    );
}

#[tokio::test]
async fn problem_with_a_vault_value_names_the_secret_and_not_its_value() {
    let vault = Arc::new(FakeVault::with(&[
        ("run--parallel", "many"),
        ("dns--cloudflare--acces-key", "hunter2"),
    ]));
    let error = Fixture::new(FILE)
        .load(&[], &vault)
        .await
        .err()
        .expect("two problems");
    assert_eq!(
        error.problems,
        [
            "run: `parallel`: expected a whole number, found a string \
             (set in Key Vault secret run--parallel)",
            "dns.cloudflare: unknown key `acces_key` \
             (set in Key Vault secret dns--cloudflare--acces-key)",
        ]
    );
}

#[tokio::test]
async fn problems_of_files_and_environment_are_reported_before_the_vault_is_opened() {
    let fixture = Fixture::new(FILE);
    let missing = fixture.file.with_file_name("missing.toml");
    let error = load(
        &quiet(),
        &[fixture.file.clone(), missing],
        variables(&[("BUTTERBOT__RUN", "x")]),
        |_| panic!("the vault is not opened when the lower layers are unusable"),
    )
    .await
    .err()
    .expect("two problems");
    assert_eq!(error.problems.len(), 2, "{error}");
}
