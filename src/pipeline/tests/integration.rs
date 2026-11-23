//! Task 8.6: a full run against the ACME test server, with the configured
//! providers and the directory store. Ignored unless the test servers are
//! installed; see `src/testing.rs`.

use std::path::Path;

use tempfile::TempDir;

use super::*;
use crate::acme::{AcmeClient, Polling};
use crate::clock::SystemClock;
use crate::store::encode;
use crate::testing::{AcmeTestServer, ChallTestSrv};

const NAME: &str = "wildcard-example-com";
const RECORD: &str = "_acme-challenge.example.com";

/// A deployment against the test servers: a configuration file's worth of
/// settings, an account, and a directory for the certificates.
struct Deployment {
    acme: AcmeTestServer,
    /// The challenge DNS server the DNS provider publishes in. It is the one
    /// the ACME server validates against, unless a test separates the two.
    published_in: Option<ChallTestSrv>,
    certificates: TempDir,
    config: Arc<Config>,
}

impl Deployment {
    async fn new(separate_dns: bool) -> Self {
        let acme = AcmeTestServer::start();
        let published_in = separate_dns.then(ChallTestSrv::start);
        let dns = published_in.as_ref().unwrap_or(&acme.dns);
        let certificates = TempDir::new().expect("a directory for the certificates");
        let (_, credentials) = AcmeClient::register(
            "pebble",
            &acme.pebble.directory_url,
            None,
            &acme.pebble.http(),
        )
        .await
        .expect("an account at the test server");

        let text = format!(
            r#"
            [acme.pebble]
            directory_url = "{directory_url}"
            ca_file = "{root}"
            access_key = '{credentials}'

            [dns.test]
            type = "challtestsrv"
            management_url = "{management}"
            zones = ["example.com"]
            propagation_check = "resolver"
            propagation_resolvers = ["{resolver}"]
            propagation_interval = "1s"
            propagation_timeout = "20s"
            settle = "0s"

            [store.local]
            type = "directory"
            path = "{path}"

            [[certificate]]
            name = "{NAME}"
            names = ["*.example.com", "example.com"]
            acme = "pebble"
            dns = "test"
            stores = ["local"]
            "#,
            directory_url = acme.pebble.directory_url,
            root = acme.pebble.root_file().display(),
            credentials = credentials.expose(),
            management = dns.management_url(),
            resolver = dns.dns,
            path = certificates.path().display(),
        );
        let config = Config::from_layers([Layer::from_toml("test.toml", &text).expect("TOML")])
            .unwrap_or_else(|error| panic!("{error}"));
        Self {
            acme,
            published_in,
            certificates,
            config: Arc::new(config),
        }
    }

    /// Performs one run, as the program does, with the real clock.
    async fn run(&self, selection: &Selection) -> (RunReport, Capture) {
        let providers = ConfiguredProviders::new(self.config.clone(), None)
            .expect("the providers of the configuration")
            .with_polling(Polling {
                interval: Duration::from_millis(100),
                timeout: Duration::from_secs(30),
            });
        let log = Capture::new();
        let logger = log.logger(LogFormat::Json, Level::Info, NOW);
        let context = Arc::new(RunContext {
            providers: Arc::new(providers),
            clock: Arc::new(SystemClock),
            logger: logger.clone(),
            shutdown: Shutdown::new(),
            limits: Limits::default(),
            min_reissue: self.config.run.min_reissue,
            retry_wait_max: self.config.run.retry_wait_max,
            zone_locks: ZoneLocks::default(),
        });
        let report = run(&context, &self.config, selection, &EventBus::new(logger)).await;
        (report, log)
    }

    fn folder(&self) -> std::path::PathBuf {
        self.certificates.path().join(NAME)
    }

    /// The certificate in the directory store.
    fn stored(&self) -> StoredCert {
        let text =
            std::fs::read_to_string(self.folder().join("fullchain.pem")).expect("fullchain.pem");
        encode::certificate_from_pem(&text).expect("a certificate")
    }

    /// Whether the challenge DNS server the provider publishes in still
    /// serves any value at the challenge record.
    async fn challenge_record_is_served(&self) -> bool {
        let dns = self.published_in.as_ref().unwrap_or(&self.acme.dns);
        served_values(dns, RECORD).await > 0
    }

    /// Checks the check: a value published through the provider is counted.
    async fn assert_served_values_are_seen(&self) {
        use crate::dns::DnsProvider;
        use crate::dns::challtestsrv::ChallTestSrvProvider;

        let dns = self.published_in.as_ref().unwrap_or(&self.acme.dns);
        let provider = ChallTestSrvProvider::new(
            self.acme.pebble.http(),
            &dns.management_url(),
            &["example.com"],
        );
        let probe = "_acme-challenge.probe.example.com";
        let zone = crate::dns::Zone::new("example.com", "example.com");
        let handle = provider
            .present(&zone, probe, &["probe-value".to_owned()], 60)
            .await
            .expect("published");
        assert_eq!(
            served_values(dns, probe).await,
            1,
            "a published value is seen"
        );
        provider.cleanup(handle).await.expect("removed");
        assert_eq!(
            served_values(dns, probe).await,
            0,
            "and a removed one is not"
        );
    }
}

/// How many TXT values the challenge DNS server returns for `record`.
async fn served_values(dns: &ChallTestSrv, record: &str) -> usize {
    use hickory_proto::op::{Message, Query};
    use hickory_proto::rr::{Name, RData, RecordType};
    use tokio::net::UdpSocket;

    let mut question = Message::query();
    let mut name = Name::from_utf8(record).expect("a name");
    name.set_fqdn(true);
    question.add_query(Query::query(name, RecordType::TXT));
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("a socket");
    socket
        .send_to(&question.to_vec().expect("encoded"), dns.dns)
        .await
        .expect("sent");
    let mut buffer = vec![0_u8; 4096];
    let (length, _) = tokio::time::timeout(Duration::from_secs(5), socket.recv_from(&mut buffer))
        .await
        .expect("the challenge DNS server answers")
        .expect("received");
    let answer = Message::from_vec(&buffer[..length]).expect("a DNS message");
    answer
        .answers
        .iter()
        .filter(|record| matches!(record.data, RData::TXT(_)))
        .count()
}

fn only_forced() -> Selection {
    Selection {
        only: vec![NAME.to_owned()],
        force: true,
    }
}

fn files_in(folder: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(folder)
        .expect("the folder exists")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    files
}

#[tokio::test]
#[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
async fn full_run_issues_then_does_nothing_then_renews_when_forced() {
    let deployment = Deployment::new(false).await;
    deployment.assert_served_values_are_seen().await;

    // --- First run: the certificate is missing, so it is issued -------------
    let (first, log) = deployment.run(&Selection::default()).await;

    assert_eq!(
        first.outcome,
        RunOutcome::Success,
        "{:#?}\n{}",
        first.certificates,
        log.text()
    );
    assert_eq!(
        (first.evaluated(), first.renewed(), first.failed()),
        (1, 1, 0)
    );
    assert_eq!(
        files_in(&deployment.folder()),
        ["fullchain.pem", "privkey.pem"]
    );
    let issued = deployment.stored();
    assert_eq!(
        issued.names.iter().map(String::as_str).collect::<Vec<_>>(),
        ["*.example.com", "example.com"],
        "exactly the configured names"
    );
    let key = std::fs::read_to_string(deployment.folder().join("privkey.pem")).unwrap();
    let key = encode::private_key_from_pem(&key).expect("a private key");
    assert!(
        encode::key_belongs_to(&issued, &key),
        "the stored key belongs to the certificate"
    );
    assert_eq!(
        encode::key_algorithm(&issued),
        Ok(encode::KeyAlgorithm::Rsa { bits: 2048 }),
        "the default key type"
    );
    assert!(
        !deployment.challenge_record_is_served().await,
        "the challenge records are removed"
    );
    let events: Vec<String> = log
        .json()
        .iter()
        .filter_map(|record| record.get("event")?.as_str().map(str::to_owned))
        .collect();
    assert_eq!(events, ["renewed", "run_completed"]);

    // --- Second run, right away: nothing is due ---------------------------------
    let (second, log) = deployment.run(&Selection::default()).await;

    assert_eq!(second.outcome, RunOutcome::Success);
    assert_eq!(
        (second.evaluated(), second.renewed(), second.failed()),
        (1, 0, 0)
    );
    assert_eq!(
        deployment.stored().serial,
        issued.serial,
        "the stored certificate is untouched"
    );
    assert!(
        !log.text().contains("the order is placed"),
        "nothing is ordered:\n{}",
        log.text()
    );

    // --- Third run, forced: a new certificate replaces the stored one -------------
    let (third, log) = deployment.run(&only_forced()).await;

    assert_eq!(
        third.outcome,
        RunOutcome::Success,
        "{:#?}\n{}",
        third.certificates,
        log.text()
    );
    assert_eq!(third.renewed(), 1);
    let renewed = deployment.stored();
    assert_ne!(renewed.serial, issued.serial, "a new certificate is stored");
    assert_eq!(renewed.names, issued.names);
    let new_key = std::fs::read_to_string(deployment.folder().join("privkey.pem")).unwrap();
    let new_key = encode::private_key_from_pem(&new_key).expect("a private key");
    assert_ne!(*new_key, *key, "with a new private key");
    assert!(encode::key_belongs_to(&renewed, &new_key));
    assert!(!deployment.challenge_record_is_served().await);
    // The stored certificate came from this server, so the order named it.
    let order = log
        .json()
        .into_iter()
        .find(|record| record["message"] == "the order is placed")
        .expect("an order was placed");
    assert_eq!(
        order["replaces"],
        issued.serial.as_str(),
        "the renewal names its predecessor"
    );
    assert_eq!(
        files_in(&deployment.folder()),
        ["fullchain.pem", "privkey.pem"]
    );
}

#[tokio::test]
#[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
async fn validation_failure_fails_the_run_and_leaves_no_challenge_record_behind() {
    // The DNS provider publishes in a DNS server the ACME server does not
    // ask, so the propagation check passes and the validation fails.
    let deployment = Deployment::new(true).await;
    deployment.assert_served_values_are_seen().await;

    let (report, log) = deployment.run(&Selection::default()).await;

    assert_eq!(report.outcome, RunOutcome::Failed);
    let (step, reason) = failure_of(&report, NAME);
    assert_eq!(step, Step::Validate, "{reason}\n{}", log.text());
    assert!(
        reason.starts_with("the ACME server pebble rejected the validation: "),
        "{reason}"
    );
    assert!(!deployment.folder().exists(), "nothing is stored");
    assert!(
        !deployment.challenge_record_is_served().await,
        "the published values are removed although the certificate failed"
    );
    let events: Vec<String> = log
        .json()
        .iter()
        .filter_map(|record| record.get("event")?.as_str().map(str::to_owned))
        .collect();
    assert_eq!(events, ["failure", "run_completed"]);
    let orders = log
        .json()
        .iter()
        .filter(|record| record["message"] == "the order is placed")
        .count();
    assert_eq!(orders, 1, "validation is attempted once per run");
}
