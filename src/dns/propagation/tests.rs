//! Task 6.2: the propagation check against in-process DNS servers.
//!
//! The servers listen on loopback addresses. Several "authoritative
//! nameservers" are several loopback addresses with the same port, because an
//! NS record cannot name a port.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use hickory_proto::op::{MessageType, OpCode};
use hickory_proto::rr::Record;
use hickory_proto::rr::rdata::{A, CNAME, NS, SOA, TXT};
use time::macros::datetime;
use tokio::net::TcpListener;

use super::*;
use crate::logging::{Capture, LogFormat};

const RECORD: &str = "_acme-challenge.example.com";

fn name(text: &str) -> Name {
    parse_name(text).expect("a valid name")
}

/// A question a server received.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    name: String,
    record_type: RecordType,
    recursion: bool,
    over_tcp: bool,
}

#[derive(Default)]
struct State {
    records: Vec<Record>,
    /// Answer over UDP with "truncated", so that the client has to use TCP.
    truncate_udp: bool,
    /// Do not answer at all.
    silent: bool,
    asked: Vec<Asked>,
}

/// A DNS server that answers from a list of records.
#[derive(Clone)]
struct Server {
    address: SocketAddr,
    state: Arc<Mutex<State>>,
}

impl Server {
    /// Starts a server on `ip`. With port 0 any free port is taken.
    async fn start(ip: Ipv4Addr, port: u16) -> std::io::Result<Self> {
        let udp = UdpSocket::bind((ip, port)).await?;
        let address = udp.local_addr()?;
        let tcp = TcpListener::bind(address).await?;
        let server = Self {
            address,
            state: Arc::default(),
        };

        let state = server.state.clone();
        tokio::spawn(async move {
            let mut buffer = vec![0_u8; 4096];
            while let Ok((length, peer)) = udp.recv_from(&mut buffer).await {
                if let Some(answer) = answer(&state, &buffer[..length], false) {
                    let _ = udp.send_to(&answer, peer).await;
                }
            }
        });
        let state = server.state.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = tcp.accept().await {
                let mut length = [0_u8; 2];
                if stream.read_exact(&mut length).await.is_err() {
                    continue;
                }
                let mut question = vec![0_u8; usize::from(u16::from_be_bytes(length))];
                if stream.read_exact(&mut question).await.is_err() {
                    continue;
                }
                if let Some(answer) = answer(&state, &question, true) {
                    let _ = stream.write_all(&(answer.len() as u16).to_be_bytes()).await;
                    let _ = stream.write_all(&answer).await;
                }
            }
        });
        Ok(server)
    }

    /// Starts a server on `ip` on a port that is free for UDP and for TCP.
    async fn start_anywhere(ip: Ipv4Addr) -> Self {
        start_on_one_port(&[ip]).await.pop().expect("one server")
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn add(&self, owner: &str, data: RData) -> &Self {
        self.state()
            .records
            .push(Record::from_rdata(name(owner), 60, data));
        self
    }

    fn txt(&self, owner: &str, values: &[&str]) -> &Self {
        for value in values {
            self.add(owner, RData::TXT(TXT::new(vec![value.to_string()])));
        }
        self
    }

    fn cname(&self, owner: &str, target: &str) -> &Self {
        self.add(owner, RData::CNAME(CNAME(name(target))))
    }

    /// Makes the server know the zone `zone` with the nameservers `hosts`.
    fn zone(&self, zone: &str, hosts: &[(&str, Ipv4Addr)]) -> &Self {
        let soa = SOA::new(
            name(hosts[0].0),
            name("hostmaster.example.com"),
            1,
            3600,
            600,
            86400,
            60,
        );
        self.add(zone, RData::SOA(soa));
        for (host, address) in hosts {
            self.add(zone, RData::NS(NS(name(host))));
            self.add(host, RData::A(A(*address)));
        }
        self
    }

    fn asked(&self) -> Vec<Asked> {
        self.state().asked.clone()
    }

    /// The questions for TXT records the server received.
    fn asked_for_txt(&self) -> Vec<Asked> {
        self.asked()
            .into_iter()
            .filter(|asked| asked.record_type == RecordType::TXT)
            .collect()
    }
}

/// Builds the answer of a server to a question, or `None` to stay silent.
fn answer(state: &Mutex<State>, question: &[u8], over_tcp: bool) -> Option<Vec<u8>> {
    let question = Message::from_vec(question).ok()?;
    let asked = question.queries.first()?.clone();
    let mut state = state.lock().unwrap();
    state.asked.push(Asked {
        name: asked.name.to_utf8().trim_end_matches('.').to_lowercase(),
        record_type: asked.query_type,
        recursion: question.metadata.recursion_desired,
        over_tcp,
    });
    if state.silent {
        return None;
    }

    let mut answer = Message::new(question.metadata.id, MessageType::Response, OpCode::Query);
    answer.metadata.authoritative = true;
    answer.metadata.recursion_desired = question.metadata.recursion_desired;
    answer.metadata.recursion_available = true;
    answer.add_query(asked.clone());
    if state.truncate_udp && !over_tcp {
        answer.metadata.truncation = true;
        return answer.to_vec().ok();
    }

    let at = |owner: &Name, record_type: RecordType| -> Vec<Record> {
        state
            .records
            .iter()
            .filter(|record| same_name(&record.name, owner) && record.record_type() == record_type)
            .cloned()
            .collect()
    };
    let exact = at(&asked.name, asked.query_type);
    let alias = at(&asked.name, RecordType::CNAME);
    if !exact.is_empty() {
        answer.add_answers(exact);
    } else if let Some(RData::CNAME(target)) = alias.first().map(|record| record.data.clone()) {
        // Like a resolver: the alias, then what the alias points at.
        answer.add_answers(alias);
        answer.add_answers(at(&target.0, asked.query_type));
    } else {
        // No data: the SOA of the closest enclosing zone, as a server gives it.
        let zone = state
            .records
            .iter()
            .filter(|record| {
                record.record_type() == RecordType::SOA && record.name.zone_of(&asked.name)
            })
            .max_by_key(|record| record.name.num_labels())
            .cloned();
        answer.add_authorities(zone);
        if !state
            .records
            .iter()
            .any(|record| same_name(&record.name, &asked.name))
        {
            answer.metadata.response_code = ResponseCode::NXDomain;
        }
    }
    answer.to_vec().ok()
}

/// A resolver and two authoritative nameservers for `example.com`.
struct World {
    resolver: Server,
    ns1: Server,
    ns2: Server,
    /// A nameserver that is not one of `example.com`'s, for delegation.
    ns3: Server,
    settings: Settings,
}

const NS1: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
const NS2: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 3);
const NS3: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 4);

/// Starts one server on each address, all on the same port.
async fn start_on_one_port(addresses: &[Ipv4Addr]) -> Vec<Server> {
    'attempt: for _ in 0..50 {
        let Ok(first) = Server::start(addresses[0], 0).await else {
            continue;
        };
        let port = first.address.port();
        let mut servers = vec![first];
        for address in &addresses[1..] {
            match Server::start(*address, port).await {
                Ok(server) => servers.push(server),
                Err(_) => continue 'attempt,
            }
        }
        return servers;
    }
    panic!("no port is free on all of {addresses:?}");
}

impl World {
    async fn new(mode: PropagationCheck) -> Self {
        let resolver = Server::start_anywhere(Ipv4Addr::LOCALHOST).await;
        let mut servers = start_on_one_port(&[NS1, NS2, NS3]).await;
        let (ns3, ns2, ns1) = (
            servers.pop().unwrap(),
            servers.pop().unwrap(),
            servers.pop().unwrap(),
        );
        resolver.zone(
            "example.com",
            &[("ns1.example.net", NS1), ("ns2.example.net", NS2)],
        );
        let settings = Settings {
            mode,
            resolvers: vec![resolver.address],
            discovery: vec![resolver.address],
            interval: Duration::from_millis(40),
            timeout: Duration::from_millis(1500),
            nameserver_port: ns1.address.port(),
            query_timeout: Duration::from_millis(400),
        };
        Self {
            resolver,
            ns1,
            ns2,
            ns3,
            settings,
        }
    }
}

fn published(values: &[&str]) -> Vec<Published> {
    vec![Published {
        name: RECORD.to_owned(),
        values: values.iter().map(|value| value.to_string()).collect(),
    }]
}

fn log() -> (Capture, StepLog) {
    let capture = Capture::new();
    let logger = capture.logger(
        LogFormat::Json,
        Level::Info,
        datetime!(2026-11-23 02:00:00 UTC),
    );
    (capture, logger.step("wildcard-example-com", "propagation"))
}

// "Propagation check against authoritative nameservers", scenario "All nameservers agree".
#[tokio::test]
async fn check_passes_when_every_authoritative_nameserver_returns_all_values() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world.ns1.txt(RECORD, &["value-one", "value-two"]);
    world.ns2.txt(RECORD, &["value-two", "value-one"]);
    let (_, log) = log();

    let confirmed = wait_for_propagation(
        &world.settings,
        &published(&["value-one", "value-two"]),
        &log,
    )
    .await
    .expect("served everywhere");

    assert_eq!(confirmed.checks, 1);
    for nameserver in [&world.ns1, &world.ns2] {
        let asked = nameserver.asked_for_txt();
        assert_eq!(
            asked.len(),
            1,
            "each authoritative nameserver is asked directly"
        );
        assert_eq!(asked[0].name, RECORD);
        assert!(
            !asked[0].recursion,
            "an authoritative nameserver is asked without recursion"
        );
    }
    assert!(
        world.resolver.asked_for_txt().is_empty(),
        "the resolver finds the nameservers; it is not asked for the record"
    );
}

// "Propagation check against authoritative nameservers", scenario "One nameserver lags".
#[tokio::test]
async fn check_is_repeated_while_one_nameserver_lags() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world.ns1.txt(RECORD, &["value-one"]);
    let (capture, log) = log();

    let lagging = world.ns2.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        lagging.txt(RECORD, &["value-one"]);
    });
    let confirmed = wait_for_propagation(&world.settings, &published(&["value-one"]), &log)
        .await
        .expect("served everywhere in the end");

    assert!(
        confirmed.checks > 1,
        "the first check did not pass: {confirmed:?}"
    );
    assert!(
        confirmed.elapsed >= Duration::from_millis(200),
        "{confirmed:?}"
    );
    assert!(
        world.ns2.asked_for_txt().len() > 1,
        "the lagging nameserver was asked again"
    );

    // "Validation diagnostics", scenario "Slow propagation": the time is in the log.
    let records = capture.json();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0]["message"],
        "the challenge records are served by every nameserver"
    );
    assert_eq!(records[0]["certificate"], "wildcard-example-com");
    assert_eq!(records[0]["step"], "propagation");
    let logged = records[0]["propagation_seconds"]
        .as_f64()
        .expect("a number of seconds");
    assert!(
        (logged - confirmed.elapsed.as_secs_f64()).abs() < 0.06,
        "{logged} vs {confirmed:?}"
    );
}

// "Propagation check against authoritative nameservers", scenario "Stale extra value".
#[tokio::test]
async fn values_beyond_the_published_ones_are_ignored() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world
        .ns1
        .txt(RECORD, &["left-over-from-an-earlier-run", "value-one"]);
    world.ns2.txt(RECORD, &["value-one", "from-another-tool"]);
    let (_, log) = log();

    let confirmed = wait_for_propagation(&world.settings, &published(&["value-one"]), &log)
        .await
        .expect("the published value is served");
    assert_eq!(confirmed.checks, 1);
}

// "All values for a name are published together", scenario "Wildcard and apex":
// both values have to be served.
#[tokio::test]
async fn every_published_value_has_to_be_returned() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world.ns1.txt(RECORD, &["for-the-wildcard", "for-the-apex"]);
    world.ns2.txt(RECORD, &["for-the-wildcard"]);
    let (_, log) = log();

    let error = wait_for_propagation(
        &world.settings,
        &published(&["for-the-wildcard", "for-the-apex"]),
        &log,
    )
    .await
    .expect_err("ns2 lacks one of the two values");
    assert!(
        error.to_string().contains(
            "ns2.example.net (127.0.0.3) does not return 1 published value(s) of \
             _acme-challenge.example.com"
        ),
        "{error}"
    );
}

// "Propagation timeout fails without involving the CA", scenario "Record never appears",
// and "Validation diagnostics", scenario "Failed propagation".
#[tokio::test]
async fn check_times_out_and_names_the_nameservers_that_lack_the_values() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world.ns1.txt(RECORD, &["value-one"]);
    let (capture, log) = log();
    let started = Instant::now();

    let error = wait_for_propagation(&world.settings, &published(&["value-one"]), &log)
        .await
        .expect_err("ns2 never serves the value");

    assert_eq!(error.class(), "timeout");
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(1400) && waited < Duration::from_secs(10),
        "{waited:?}"
    );
    assert_eq!(
        error.to_string(),
        "the challenge records were not served by every nameserver within 1s: ns2.example.net \
         (127.0.0.3) does not return 1 published value(s) of _acme-challenge.example.com"
    );

    let records = capture.json();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["level"], "error");
    assert_eq!(records[0]["step"], "propagation");
    assert_eq!(records[0]["reason"], error.to_string());
    assert_eq!(
        records[0]["missing"],
        serde_json::json!([{
            "nameserver": "ns2.example.net (127.0.0.3)",
            "record": "_acme-challenge.example.com",
            "values": ["value-one"],
            "problem": null
        }]),
        "the log names each nameserver that did not return the published values"
    );
}

// "Delegated challenge names", scenario "CNAME delegation".
#[tokio::test]
async fn delegated_name_is_checked_at_the_nameservers_of_the_zone_it_points_into() {
    let world = World::new(PropagationCheck::Authoritative).await;
    let ns3 = &world.ns3;
    world.resolver.cname(RECORD, "example-com.acme.example.net");
    world
        .resolver
        .zone("acme.example.net", &[("ns3.example.org", NS3)]);
    ns3.txt("example-com.acme.example.net", &["value-one"]);
    let (_, log) = log();

    wait_for_propagation(&world.settings, &published(&["value-one"]), &log)
        .await
        .expect("served by the nameserver of the delegated zone");

    let asked = ns3.asked_for_txt();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].name, "example-com.acme.example.net",
        "the name the CNAME points at"
    );
    assert!(!asked[0].recursion);
    assert!(
        world.ns1.asked().is_empty() && world.ns2.asked().is_empty(),
        "the nameservers of the zone the CNAME is in are not asked"
    );
}

#[tokio::test]
async fn delegated_name_that_is_not_served_names_the_delegated_nameserver() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world.resolver.cname(RECORD, "example-com.acme.example.net");
    world
        .resolver
        .zone("acme.example.net", &[("ns3.example.org", NS3)]);
    let settings = Settings {
        timeout: Duration::from_millis(100),
        ..world.settings.clone()
    };
    let (_, log) = log();

    let error = wait_for_propagation(&settings, &published(&["value-one"]), &log)
        .await
        .expect_err("not served");
    assert!(
        error.to_string().contains("ns3.example.org (127.0.0.4)"),
        "{error}"
    );
}

// "Propagation check mode", scenario "Resolver mode".
#[tokio::test]
async fn resolver_mode_asks_the_configured_resolvers_and_no_authoritative_nameserver() {
    let world = World::new(PropagationCheck::Resolver).await;
    let second = Server::start_anywhere(Ipv4Addr::LOCALHOST).await;
    world.resolver.txt(RECORD, &["value-one"]);
    second.txt(RECORD, &["value-one", "something-else"]);
    let settings = Settings {
        resolvers: vec![world.resolver.address, second.address],
        ..world.settings.clone()
    };
    let (_, log) = log();

    let confirmed = wait_for_propagation(&settings, &published(&["value-one"]), &log)
        .await
        .expect("every resolver returns the value");

    assert_eq!(confirmed.checks, 1);
    for resolver in [&world.resolver, &second] {
        let asked = resolver.asked();
        assert_eq!(
            asked.len(),
            1,
            "one question for the record, and none to find nameservers"
        );
        assert_eq!(asked[0].record_type, RecordType::TXT);
        assert!(asked[0].recursion, "a resolver is asked with recursion");
    }
    assert!(world.ns1.asked().is_empty() && world.ns2.asked().is_empty());
}

#[tokio::test]
async fn resolver_mode_fails_when_one_resolver_lacks_a_value() {
    let world = World::new(PropagationCheck::Resolver).await;
    let second = Server::start_anywhere(Ipv4Addr::LOCALHOST).await;
    world.resolver.txt(RECORD, &["value-one"]);
    let settings = Settings {
        resolvers: vec![world.resolver.address, second.address],
        timeout: Duration::from_millis(150),
        ..world.settings.clone()
    };
    let (_, log) = log();

    let error = wait_for_propagation(&settings, &published(&["value-one"]), &log)
        .await
        .expect_err("the second resolver lacks the value");
    assert!(
        error.to_string().contains(&second.address.to_string()),
        "{error}"
    );
    assert!(
        !error
            .to_string()
            .contains(&format!("{} does not", world.resolver.address)),
        "{error}"
    );
}

#[tokio::test]
async fn resolver_mode_follows_a_delegated_name_through_the_resolver() {
    let world = World::new(PropagationCheck::Resolver).await;
    world.resolver.cname(RECORD, "example-com.acme.example.net");
    world
        .resolver
        .txt("example-com.acme.example.net", &["value-one"]);
    let (_, log) = log();

    wait_for_propagation(&world.settings, &published(&["value-one"]), &log)
        .await
        .expect("the resolver returns the value behind the CNAME");
}

// "Propagation check mode", scenario "Check switched off".
#[tokio::test]
async fn off_mode_sends_no_dns_query() {
    let world = World::new(PropagationCheck::Off).await;
    let (capture, log) = log();
    let started = Instant::now();

    let confirmed = wait_for_propagation(&world.settings, &published(&["value-one"]), &log)
        .await
        .expect("nothing is checked");

    assert_eq!(
        confirmed,
        Confirmed {
            elapsed: Duration::ZERO,
            checks: 0
        }
    );
    assert!(
        started.elapsed() < Duration::from_millis(40),
        "it returns at once"
    );
    for server in [&world.resolver, &world.ns1, &world.ns2] {
        assert!(server.asked().is_empty(), "no DNS query from the program");
    }
    assert_eq!(
        capture.json()[0]["message"],
        "the propagation check is switched off"
    );
}

#[tokio::test]
async fn answer_that_does_not_fit_udp_is_fetched_over_tcp() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world.ns1.txt(RECORD, &["value-one"]);
    world.ns2.txt(RECORD, &["value-one"]);
    world.ns2.state().truncate_udp = true;
    let (_, log) = log();

    wait_for_propagation(&world.settings, &published(&["value-one"]), &log)
        .await
        .expect("the truncated answer is fetched over TCP");

    let asked = world.ns2.asked_for_txt();
    assert_eq!(
        asked.iter().map(|asked| asked.over_tcp).collect::<Vec<_>>(),
        [false, true]
    );
}

#[tokio::test]
async fn nameserver_that_does_not_answer_counts_as_not_serving() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world.ns1.txt(RECORD, &["value-one"]);
    world.ns2.txt(RECORD, &["value-one"]);
    world.ns2.state().silent = true;
    let settings = Settings {
        timeout: Duration::from_millis(300),
        query_timeout: Duration::from_millis(100),
        ..world.settings.clone()
    };
    let (_, log) = log();

    let error = wait_for_propagation(&settings, &published(&["value-one"]), &log)
        .await
        .expect_err("ns2 does not answer");
    assert!(
        error.to_string().contains(
            "ns2.example.net does not return 1 published value(s) of \
             _acme-challenge.example.com: no answer (UDP timed out)"
        ),
        "{error}"
    );
}

#[tokio::test]
async fn nameservers_that_cannot_be_found_fail_the_check() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world.resolver.state().silent = true;
    let settings = Settings {
        timeout: Duration::from_millis(200),
        query_timeout: Duration::from_millis(60),
        ..world.settings.clone()
    };
    let (_, log) = log();

    let error = wait_for_propagation(&settings, &published(&["value-one"]), &log)
        .await
        .expect_err("the resolver does not answer");
    assert!(
        error.to_string().contains("they could not be found"),
        "{error}"
    );
    assert!(world.ns1.asked().is_empty());
}

#[tokio::test]
async fn zone_is_found_by_walking_up_when_the_resolver_returns_no_soa() {
    let world = World::new(PropagationCheck::Authoritative).await;
    world
        .resolver
        .state()
        .records
        .retain(|record| record.record_type() != RecordType::SOA);
    world.ns1.txt(RECORD, &["value-one"]);
    world.ns2.txt(RECORD, &["value-one"]);
    let (_, log) = log();

    wait_for_propagation(&world.settings, &published(&["value-one"]), &log)
        .await
        .expect("the zone is found through its NS records");

    let ns_questions: Vec<String> = world
        .resolver
        .asked()
        .into_iter()
        .filter(|asked| asked.record_type == RecordType::NS)
        .map(|asked| asked.name)
        .collect();
    assert_eq!(ns_questions, [RECORD, "example.com", "example.com"]);
}

#[tokio::test]
async fn several_records_are_all_checked() {
    let world = World::new(PropagationCheck::Authoritative).await;
    let other = "_acme-challenge.www.example.com";
    world
        .ns1
        .txt(RECORD, &["value-one"])
        .txt(other, &["value-two"]);
    world.ns2.txt(RECORD, &["value-one"]);
    let records = vec![
        Published {
            name: RECORD.to_owned(),
            values: vec!["value-one".to_owned()],
        },
        Published {
            name: other.to_owned(),
            values: vec!["value-two".to_owned()],
        },
    ];
    let settings = Settings {
        timeout: Duration::from_millis(100),
        ..world.settings.clone()
    };
    let (_, log) = log();

    let error = wait_for_propagation(&settings, &records, &log)
        .await
        .expect_err("one record lags");
    assert!(
        error
            .to_string()
            .contains("_acme-challenge.www.example.com"),
        "{error}"
    );
    assert!(
        !error.to_string().contains("of _acme-challenge.example.com"),
        "{error}"
    );

    world.ns2.txt(other, &["value-two"]);
    wait_for_propagation(&settings, &records, &log)
        .await
        .expect("both records are served");
}

#[test]
fn system_resolvers_are_read_from_resolv_conf() {
    let text = "# comment\nsearch example.com\nnameserver 10.0.0.10\nnameserver  2001:db8::1 \n\
                nameserver fe80::1%eth0\nnameserver not-an-address\noptions ndots:5\n";
    assert_eq!(
        resolvers_from_resolv_conf(text),
        [
            "10.0.0.10:53".parse::<SocketAddr>().unwrap(),
            "[2001:db8::1]:53".parse().unwrap(),
            "[fe80::1]:53".parse().unwrap(),
        ]
    );
    assert!(resolvers_from_resolv_conf("").is_empty());
}

#[test]
fn settings_of_an_instance_use_its_resolvers_interval_and_mode() {
    let config = crate::config::Config::from_layers([crate::config::Layer::from_toml(
        "test.toml",
        "[dns.d]\ntype = \"cloudflare\"\npropagation_check = \"resolver\"\n\
         propagation_resolvers = [\"192.0.2.1\", \"192.0.2.2:5353\"]\npropagation_interval = \"3s\"\n",
    )
    .unwrap()])
    .unwrap();
    let settings = Settings::for_instance(&config.dns["d"], Duration::from_secs(120));
    assert_eq!(settings.mode, PropagationCheck::Resolver);
    assert_eq!(
        settings.resolvers,
        [
            "192.0.2.1:53".parse::<SocketAddr>().unwrap(),
            "192.0.2.2:5353".parse().unwrap()
        ]
    );
    assert_eq!(settings.interval, Duration::from_secs(3));
    assert_eq!(settings.timeout, Duration::from_secs(120));
    assert_eq!(settings.nameserver_port, 53);
    assert_eq!(settings.query_timeout, QUERY_TIMEOUT);
    assert!(!settings.discovery.is_empty());
}
