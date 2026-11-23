//! The pipeline and the run loop with fake providers.
//!
//! Time is the paused clock of the test runtime, so waits take no real time
//! and their length can be measured exactly.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use time::macros::datetime;

use super::*;
use crate::acme::{Challenge, OrderRequest, PendingOrder};
use crate::clock::FixedClock;
use crate::config::{Config, Format, Layer};
use crate::dns::{RecordHandle, Zone};
use crate::events::{Event, EventBus, EventKind, EventSink, Selection as EventSelection};
use crate::logging::{Capture, Level, LogFormat};
use crate::policy::RenewalWindow;
use crate::store::testing::{issue, issue_with};
use crate::store::{IssuedCert, MemoryStore};
use crate::{Error, Result};

mod events;
mod integration;
mod pipeline;
mod run_loop;
mod status;
mod waits;

const NOW: OffsetDateTime = datetime!(2026-11-23 02:00:00 UTC);
const DAY: Duration = Duration::from_secs(86_400);

/// What happened, in order, across all fakes.
#[derive(Clone, Default)]
struct Journal {
    entries: Arc<Mutex<Vec<String>>>,
}

impl Journal {
    fn add(&self, entry: impl Into<String>) {
        self.entries.lock().unwrap().push(entry.into());
    }

    fn all(&self) -> Vec<String> {
        self.entries.lock().unwrap().clone()
    }

    /// The entries that start with one of `prefixes`.
    fn only(&self, prefixes: &[&str]) -> Vec<String> {
        self.all()
            .into_iter()
            .filter(|entry| prefixes.iter().any(|prefix| entry.starts_with(prefix)))
            .collect()
    }

    fn position(&self, entry: &str) -> usize {
        self.all()
            .iter()
            .position(|recorded| recorded == entry)
            .unwrap_or_else(|| panic!("{entry:?} is not in {:#?}", self.all()))
    }

    fn count(&self, prefix: &str) -> usize {
        self.only(&[prefix]).len()
    }
}

/// A gate that holds whoever reaches it until it is opened.
#[derive(Clone, Default)]
struct Gate {
    reached: Arc<tokio::sync::Notify>,
    open: Arc<tokio::sync::Notify>,
}

impl Gate {
    async fn pass(&self) {
        self.reached.notify_one();
        self.open.notified().await;
    }

    async fn reached(&self) {
        self.reached.notified().await;
    }

    fn open(&self) {
        self.open.notify_one();
    }
}

/// How a fake behaves at one point.
#[derive(Clone, Default)]
enum Behaviour {
    #[default]
    Succeed,
    Fail(Error),
    /// Never answers.
    Hang,
    /// Takes this long.
    Take(Duration),
    /// Waits at the gate.
    Wait(Gate),
}

impl Behaviour {
    async fn act(&self) -> Result<()> {
        match self {
            Self::Succeed => Ok(()),
            Self::Fail(error) => Err(error.clone()),
            Self::Hang => std::future::pending().await,
            Self::Take(duration) => {
                tokio::time::sleep(*duration).await;
                Ok(())
            }
            Self::Wait(gate) => {
                gate.pass().await;
                Ok(())
            }
        }
    }
}

/// A fake ACME server.
#[derive(Default)]
struct FakeIssuer {
    journal: Journal,
    /// The results of the next calls of `order`, before orders succeed.
    order_failures: Mutex<VecDeque<Error>>,
    validate: Mutex<Behaviour>,
    finalize: Mutex<Behaviour>,
    /// The renewal window by serial number; a missing serial is an error.
    windows: Mutex<BTreeMap<String, RenewalWindow>>,
    /// The requests of the orders that were placed.
    requests: Mutex<Vec<OrderRequest>>,
    /// The certificates that were issued.
    issued: Mutex<Vec<IssuedCert>>,
}

impl FakeIssuer {
    fn orders(&self) -> usize {
        self.journal.count("order ")
    }
}

#[async_trait]
impl RenewalInfoSource for FakeIssuer {
    async fn renewal_window(&self, certificate: &StoredCert) -> Result<Option<RenewalWindow>> {
        self.journal
            .add(format!("renewal-info {}", certificate.serial));
        self.windows
            .lock()
            .unwrap()
            .get(&certificate.serial)
            .copied()
            .map(Some)
            .ok_or_else(|| Error::permanent("the server does not know the certificate"))
    }
}

#[async_trait]
impl Issuer for FakeIssuer {
    async fn order(&self, request: &OrderRequest) -> Result<Box<dyn PendingOrder>> {
        self.journal
            .add(format!("order {}", request.names.join(",")));
        if let Some(failure) = self.order_failures.lock().unwrap().pop_front() {
            return Err(failure);
        }
        self.requests.lock().unwrap().push(request.clone());
        let challenges = request
            .names
            .iter()
            .map(|name| Challenge {
                dns_name: name.clone(),
                record_name: crate::dns::challenge_record_name(name),
                value: format!("value-for-{name}"),
            })
            .collect();
        Ok(Box::new(FakeOrder {
            journal: self.journal.clone(),
            names: request.names.clone(),
            challenges,
            validate: self.validate.lock().unwrap().clone(),
            finalize: self.finalize.lock().unwrap().clone(),
            issued: {
                let certificate =
                    issue(&request.names.iter().map(String::as_str).collect::<Vec<_>>());
                self.issued.lock().unwrap().push(certificate.clone());
                certificate
            },
        }))
    }
}

struct FakeOrder {
    journal: Journal,
    names: Vec<String>,
    challenges: Vec<Challenge>,
    validate: Behaviour,
    finalize: Behaviour,
    issued: IssuedCert,
}

#[async_trait]
impl PendingOrder for FakeOrder {
    fn challenges(&self) -> &[Challenge] {
        &self.challenges
    }

    async fn validate(&mut self) -> Result<()> {
        self.journal
            .add(format!("validate {}", self.names.join(",")));
        self.validate.act().await
    }

    async fn finalize(&mut self) -> Result<IssuedCert> {
        self.journal
            .add(format!("finalize {}", self.names.join(",")));
        self.finalize.act().await?;
        Ok(self.issued.clone())
    }
}

/// A fake DNS provider.
struct FakeDns {
    journal: Journal,
    zones: Vec<Zone>,
    present: Mutex<Behaviour>,
    cleanup: Mutex<Behaviour>,
    /// The zones that hold a published record right now, with how many.
    busy: Mutex<BTreeMap<String, usize>>,
    /// The largest number of certificates that were in one zone at a time.
    most_in_one_zone: Mutex<usize>,
}

impl FakeDns {
    fn new(journal: &Journal, zones: &[&str]) -> Self {
        Self {
            journal: journal.clone(),
            zones: zones.iter().map(|zone| Zone::new(zone, zone)).collect(),
            present: Mutex::default(),
            cleanup: Mutex::default(),
            busy: Mutex::default(),
            most_in_one_zone: Mutex::new(0),
        }
    }
}

#[async_trait]
impl DnsProvider for FakeDns {
    async fn zones(&self) -> Result<Vec<Zone>> {
        Ok(self.zones.clone())
    }

    async fn present(
        &self,
        zone: &Zone,
        name: &str,
        values: &[String],
        _ttl: u32,
    ) -> Result<RecordHandle> {
        self.journal.add(format!("present {name}"));
        let behaviour = self.present.lock().unwrap().clone();
        behaviour.act().await?;
        {
            let mut busy = self.busy.lock().unwrap();
            let count = busy.entry(zone.name.clone()).or_default();
            *count += 1;
            let mut most = self.most_in_one_zone.lock().unwrap();
            *most = (*most).max(*count);
        }
        Ok(RecordHandle {
            zone: zone.clone(),
            name: name.to_owned(),
            values: values.to_vec(),
            record_ids: Vec::new(),
        })
    }

    async fn cleanup(&self, handle: RecordHandle) -> Result<()> {
        self.journal.add(format!("cleanup {}", handle.name));
        if let Some(count) = self.busy.lock().unwrap().get_mut(&handle.zone.name) {
            *count = count.saturating_sub(1);
        }
        let behaviour = self.cleanup.lock().unwrap().clone();
        behaviour.act().await
    }
}

/// A store that records reads and writes in the journal, and whose calls can
/// be made to behave in a certain way.
struct FakeStore {
    journal: Journal,
    name: String,
    inner: MemoryStore,
    read: Mutex<Behaviour>,
    write: Mutex<Behaviour>,
}

#[async_trait]
impl CertStore for FakeStore {
    async fn current(&self, name: &str, format: Format) -> Result<Option<StoredCert>> {
        self.journal.add(format!("read {} {name}", self.name));
        let behaviour = self.read.lock().unwrap().clone();
        behaviour.act().await?;
        self.inner.current(name, format).await
    }

    async fn put(&self, name: &str, certificate: &IssuedCert, format: Format) -> Result<()> {
        self.journal.add(format!("put {} {name}", self.name));
        let behaviour = self.write.lock().unwrap().clone();
        behaviour.act().await?;
        self.inner.put(name, certificate, format).await
    }
}

/// The fakes of a run, by instance name.
struct Fakes {
    journal: Journal,
    issuers: BTreeMap<String, Arc<FakeIssuer>>,
    dns: BTreeMap<String, Arc<FakeDns>>,
    stores: BTreeMap<String, Arc<FakeStore>>,
    propagation: Mutex<Behaviour>,
    /// Instances that cannot be used, with the reason `check` gives.
    missing: Mutex<BTreeMap<String, Error>>,
    /// ACME servers whose account cannot be set up.
    no_account: Mutex<BTreeMap<String, Error>>,
    /// The largest number of certificates that were in the propagation check
    /// at a time.
    in_propagation: Mutex<(usize, usize)>,
}

#[async_trait]
impl Providers for Fakes {
    fn check(&self, entry: &Resolved<'_>) -> Result<()> {
        let missing = self.missing.lock().unwrap();
        match missing
            .get(&entry.certificate.dns)
            .or_else(|| missing.get(&entry.certificate.acme))
        {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    async fn renewal_info(&self, acme: &str, _log: &StepLog) -> Option<Arc<dyn RenewalInfoSource>> {
        self.journal.add(format!("renewal-info-source {acme}"));
        let issuer: Arc<dyn RenewalInfoSource> = self.issuers.get(acme)?.clone();
        Some(issuer)
    }

    async fn issuer(&self, acme: &str, _log: &StepLog) -> Result<Arc<dyn Issuer>> {
        self.journal.add(format!("account {acme}"));
        if let Some(error) = self.no_account.lock().unwrap().get(acme) {
            return Err(error.clone());
        }
        let issuer: Arc<dyn Issuer> = self.issuers[acme].clone();
        Ok(issuer)
    }

    fn dns(&self, dns: &str) -> Result<Arc<dyn DnsProvider>> {
        let provider: Arc<dyn DnsProvider> = self.dns[dns].clone();
        Ok(provider)
    }

    fn store(&self, store: &str) -> Result<Arc<dyn CertStore>> {
        let store: Arc<dyn CertStore> = self.stores[store].clone();
        Ok(store)
    }

    async fn confirm_propagation(
        &self,
        _dns: &str,
        _timeout: Duration,
        records: &[Published],
        _log: &StepLog,
    ) -> Result<Confirmed> {
        let names: Vec<&str> = records.iter().map(|record| record.name.as_str()).collect();
        self.journal.add(format!("propagation {}", names.join(",")));
        {
            let mut counts = self.in_propagation.lock().unwrap();
            counts.0 += 1;
            counts.1 = counts.1.max(counts.0);
        }
        let behaviour = self.propagation.lock().unwrap().clone();
        let result = behaviour.act().await;
        self.in_propagation.lock().unwrap().0 -= 1;
        result.map(|()| Confirmed {
            elapsed: Duration::ZERO,
            checks: 1,
        })
    }
}

/// A sink that keeps the events it receives.
#[derive(Default)]
struct Collected {
    events: Mutex<Vec<Event>>,
}

#[async_trait]
impl EventSink for Collected {
    async fn deliver(&self, event: &Event) -> Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

/// A run with fakes, ready to go.
struct Harness {
    config: Config,
    fakes: Arc<Fakes>,
    context: Arc<RunContext>,
    events: EventBus,
    collected: Arc<Collected>,
    log: Capture,
    journal: Journal,
}

/// The providers every test configuration has: the ACME server `le`, the DNS
/// providers `dns` and `other-dns`, and the stores `one` and `two`. Waits are
/// zero unless a test sets them.
const PROVIDERS: &str = r#"
[run]
parallel = 4

[acme.le]
directory_url = "https://acme.example.com/directory"

[acme.le-no-ari]
directory_url = "https://acme.example.net/directory"
ari = false

[dns.dns]
type = "cloudflare"
settle = "0s"

[dns.other-dns]
type = "cloudflare"
settle = "0s"

[dns.slow-dns]
type = "cloudflare"

[store.one]
type = "directory"
path = "/one"

[store.two]
type = "directory"
path = "/two"
"#;

/// A certificate entry for `<name>.example.com` in the store `one`.
fn entry(name: &str) -> String {
    entry_with(name, "le", "dns", &["one"], "")
}

/// A certificate entry for `<name>.<zone>` in the store `one`.
fn entry_in_zone(name: &str, dns: &str, zone: &str) -> String {
    format!(
        "\n[[certificate]]\nname = \"{name}\"\nnames = [\"{name}.{zone}\"]\nacme = \"le\"\n\
         dns = \"{dns}\"\nstores = [\"one\"]\n"
    )
}

fn entry_with(name: &str, acme: &str, dns: &str, stores: &[&str], extra: &str) -> String {
    let stores: Vec<String> = stores.iter().map(|store| format!("{store:?}")).collect();
    format!(
        "\n[[certificate]]\nname = \"{name}\"\nnames = [\"{name}.example.com\"]\nacme = \"{acme}\"\n\
         dns = \"{dns}\"\nstores = [{}]\n{extra}\n",
        stores.join(", ")
    )
}

impl Harness {
    fn new(entries: &[String]) -> Self {
        Self::with_providers(PROVIDERS, entries)
    }

    fn with_providers(providers: &str, entries: &[String]) -> Self {
        let text = format!("{providers}{}", entries.concat());
        let config = Config::from_layers([Layer::from_toml("test.toml", &text).expect("TOML")])
            .unwrap_or_else(|error| panic!("{error}"));
        let journal = Journal::default();
        let issuers = config
            .acme
            .keys()
            .map(|name| {
                let issuer = FakeIssuer {
                    journal: journal.clone(),
                    ..FakeIssuer::default()
                };
                (name.clone(), Arc::new(issuer))
            })
            .collect();
        let zones = [
            "example.com",
            "example.org",
            "z1.example.net",
            "z2.example.net",
            "z3.example.net",
            "z4.example.net",
            "z5.example.net",
            "z6.example.net",
        ];
        let dns = config
            .dns
            .keys()
            .map(|name| (name.clone(), Arc::new(FakeDns::new(&journal, &zones))))
            .collect();
        let stores = config
            .stores
            .keys()
            .map(|name| {
                let store = FakeStore {
                    journal: journal.clone(),
                    name: name.clone(),
                    inner: MemoryStore::new(),
                    read: Mutex::default(),
                    write: Mutex::default(),
                };
                (name.clone(), Arc::new(store))
            })
            .collect();
        let fakes = Arc::new(Fakes {
            journal: journal.clone(),
            issuers,
            dns,
            stores,
            propagation: Mutex::default(),
            missing: Mutex::default(),
            no_account: Mutex::default(),
            in_propagation: Mutex::new((0, 0)),
        });
        let log = Capture::new();
        let logger = log.logger(LogFormat::Json, Level::Debug, NOW);
        let collected = Arc::new(Collected::default());
        let mut events = EventBus::new(logger.clone());
        events.register("collector", EventSelection::All, collected.clone());
        let context = Arc::new(RunContext {
            providers: fakes.clone(),
            clock: Arc::new(FixedClock(NOW)),
            logger,
            shutdown: Shutdown::new(),
            limits: Limits::default(),
            min_reissue: config.run.min_reissue,
            retry_wait_max: config.run.retry_wait_max,
            zone_locks: ZoneLocks::default(),
        });
        Self {
            config,
            fakes,
            context,
            events,
            collected,
            log,
            journal,
        }
    }

    async fn run(&self) -> RunReport {
        self.run_selected(&Selection::default()).await
    }

    async fn run_selected(&self, selection: &Selection) -> RunReport {
        run(&self.context, &self.config, selection, &self.events).await
    }

    fn issuer(&self) -> &FakeIssuer {
        &self.fakes.issuers["le"]
    }

    fn dns(&self) -> &FakeDns {
        &self.fakes.dns["dns"]
    }

    fn store(&self, name: &str) -> &FakeStore {
        &self.fakes.stores[name]
    }

    /// Puts a certificate for `<name>.example.com` that expires in
    /// `remaining_days` days into `store`, and returns it.
    async fn holds(&self, store: &str, name: &str, remaining_days: u32) -> IssuedCert {
        let certificate = issue_with(
            &[&format!("{name}.example.com")],
            crate::config::KeyType::EcdsaP256,
            NOW - DAY * (90 - remaining_days),
            NOW + DAY * remaining_days,
        );
        self.store(store)
            .inner
            .put(name, &certificate, Format::Pem)
            .await
            .expect("stored");
        certificate
    }

    fn events(&self) -> Vec<Event> {
        self.collected.events.lock().unwrap().clone()
    }

    fn event_kinds(&self) -> Vec<EventKind> {
        self.events().iter().map(Event::kind).collect()
    }

    /// The log records of a certificate at a level, as (step, message).
    fn records(&self, certificate: &str, level: &str) -> Vec<(String, String)> {
        self.log
            .json()
            .iter()
            .filter(|record| {
                record
                    .get("certificate")
                    .is_some_and(|name| name == certificate)
            })
            // An event has a record of its own; these are the ordinary ones.
            .filter(|record| !record.contains_key("event"))
            .filter(|record| record["level"] == level)
            .map(|record| {
                (
                    record["step"].as_str().unwrap_or_default().to_owned(),
                    record["message"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }
}

fn outcome_of<'a>(report: &'a RunReport, name: &str) -> &'a CertificateOutcome {
    &report
        .certificates
        .iter()
        .find(|certificate| certificate.name == name)
        .unwrap_or_else(|| panic!("{name} was evaluated"))
        .outcome
}

fn is_renewed(report: &RunReport, name: &str) -> bool {
    matches!(outcome_of(report, name), CertificateOutcome::Renewed { .. })
}

fn failure_of(report: &RunReport, name: &str) -> (Step, String) {
    match outcome_of(report, name) {
        CertificateOutcome::Failed { step, reason } => (*step, reason.clone()),
        other => panic!("{name} did not fail: {other:?}"),
    }
}
