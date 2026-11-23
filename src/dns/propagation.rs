//! The propagation check that runs before the ACME server is asked to validate.
//!
//! After the challenge records are published, the check asks nameservers for
//! them directly and passes only when every one returns every published
//! value. Values beyond those are ignored. It repeats at an interval until it
//! passes or its time limit is reached.
//!
//! Three modes:
//!
//! - `authoritative`: the authoritative nameservers of the zone are found (a
//!   CNAME on the challenge name is followed first) and each is asked without
//!   recursion.
//! - `resolver`: the configured recursive resolvers are asked.
//! - `off`: nothing is asked.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use futures_util::future::join_all;
use hickory_proto::op::{Edns, Message, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::Instant;

use crate::config::{DnsInstance, PropagationCheck, defaults};
use crate::logging::{Level, StepLog};
use crate::{Error, Result};

/// Default time limit for one DNS question, over UDP and again over TCP.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// How many CNAMEs are followed from a challenge name.
const MAX_CNAME_HOPS: usize = 8;

/// The UDP payload size announced to servers. Larger answers come over TCP.
const UDP_PAYLOAD: u16 = 1232;

/// How the check is carried out.
#[derive(Debug, Clone)]
pub struct Settings {
    pub mode: PropagationCheck,
    /// The recursive resolvers asked in `resolver` mode.
    pub resolvers: Vec<SocketAddr>,
    /// The recursive resolvers used in `authoritative` mode to find the
    /// nameservers of a zone.
    pub discovery: Vec<SocketAddr>,
    /// The pause between two checks.
    pub interval: Duration,
    /// How long to keep checking.
    pub timeout: Duration,
    /// The port authoritative nameservers are asked on. 53, except in tests.
    pub nameserver_port: u16,
    /// Time limit for one DNS question, over UDP and again over TCP.
    pub query_timeout: Duration,
}

impl Settings {
    /// The settings of a DNS provider instance, with the resolved
    /// `propagation_timeout` of the certificate.
    pub fn for_instance(instance: &DnsInstance, timeout: Duration) -> Self {
        let system = system_resolvers();
        Self {
            mode: instance.propagation_check,
            resolvers: instance.propagation_resolvers.clone(),
            discovery: if system.is_empty() {
                instance.propagation_resolvers.clone()
            } else {
                system
            },
            interval: instance.propagation_interval,
            timeout,
            nameserver_port: defaults::DNS_PORT,
            query_timeout: QUERY_TIMEOUT,
        }
    }
}

/// The resolvers the machine is configured with, from `/etc/resolv.conf`.
pub fn system_resolvers() -> Vec<SocketAddr> {
    std::fs::read_to_string("/etc/resolv.conf")
        .map(|text| resolvers_from_resolv_conf(&text))
        .unwrap_or_default()
}

fn resolvers_from_resolv_conf(text: &str) -> Vec<SocketAddr> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some("nameserver"))
                .then(|| words.next())
                .flatten()
        })
        // An address may carry a zone index (`fe80::1%eth0`), which is dropped.
        .filter_map(|address| address.split('%').next()?.parse::<IpAddr>().ok())
        .map(|address| SocketAddr::new(address, defaults::DNS_PORT))
        .collect()
}

/// A record whose propagation is checked: its name and the values that were
/// published at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub name: String,
    pub values: Vec<String>,
}

/// A nameserver that did not return every published value of a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// The nameserver, by name and address where known.
    pub nameserver: String,
    pub record: String,
    /// The published values the nameserver did not return.
    pub values: Vec<String>,
    /// Why, when it is more than "not there yet".
    pub problem: Option<String>,
}

impl Missing {
    fn describe(&self) -> String {
        let problem = self
            .problem
            .as_ref()
            .map(|problem| format!(": {problem}"))
            .unwrap_or_default();
        format!(
            "{} does not return {} published value(s) of {}{problem}",
            self.nameserver,
            self.values.len(),
            self.record
        )
    }
}

/// The result of a check that passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirmed {
    /// How long it took from the start of the check until it passed.
    pub elapsed: Duration,
    /// How many times the nameservers were asked.
    pub checks: u32,
}

/// Where a record is really served: the name after following CNAMEs, and the
/// nameservers of the zone that name is in.
#[derive(Debug, Clone)]
struct Target {
    name: Name,
    nameservers: Vec<Nameserver>,
}

#[derive(Debug, Clone)]
struct Nameserver {
    host: String,
    addresses: Vec<SocketAddr>,
}

/// Waits until the published records are served, as the settings say.
///
/// Returns when the check passed. Fails with a timeout when it has not passed
/// within the time limit; the message and the log name each nameserver that
/// did not return the published values. In `off` mode it returns at once
/// without asking anything.
pub async fn wait_for_propagation(
    settings: &Settings,
    records: &[Published],
    log: &StepLog,
) -> Result<Confirmed> {
    if settings.mode == PropagationCheck::Off {
        log.info("the propagation check is switched off");
        return Ok(Confirmed {
            elapsed: Duration::ZERO,
            checks: 0,
        });
    }
    let started = Instant::now();
    let mut targets: Vec<Option<Target>> = vec![None; records.len()];
    let mut checks = 0;
    loop {
        checks += 1;
        let missing = check_once(settings, records, &mut targets).await;
        let elapsed = started.elapsed();
        if missing.is_empty() {
            log.record(
                Level::Info,
                "the challenge records are served by every nameserver",
            )
            .field("propagation_seconds", seconds(elapsed))
            .field("checks", checks)
            .write();
            return Ok(Confirmed { elapsed, checks });
        }
        if elapsed + settings.interval > settings.timeout {
            let described: Vec<String> = missing.iter().map(Missing::describe).collect();
            let listed: Vec<serde_json::Value> = missing
                .iter()
                .map(|missing| {
                    serde_json::json!({
                        "nameserver": missing.nameserver,
                        "record": missing.record,
                        "values": missing.values,
                        "problem": missing.problem,
                    })
                })
                .collect();
            let reason = format!(
                "the challenge records were not served by every nameserver within {}: {}",
                crate::config::format_duration(settings.timeout),
                described.join("; ")
            );
            log.record(Level::Error, "the propagation check timed out")
                .field("propagation_seconds", seconds(elapsed))
                .field("checks", checks)
                .field("missing", listed)
                .reason(&reason)
                .write();
            return Err(Error::timeout(reason));
        }
        log.record(
            Level::Debug,
            "the challenge records are not served everywhere yet",
        )
        .field(
            "missing",
            missing.iter().map(Missing::describe).collect::<Vec<_>>(),
        )
        .write();
        tokio::time::sleep(settings.interval).await;
    }
}

/// Seconds with one decimal place, as a number.
fn seconds(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 10.0).round() / 10.0
}

/// Asks every nameserver once and returns what is missing.
async fn check_once(
    settings: &Settings,
    records: &[Published],
    targets: &mut [Option<Target>],
) -> Vec<Missing> {
    let mut missing = Vec::new();
    for (record, target) in records.iter().zip(targets.iter_mut()) {
        match settings.mode {
            PropagationCheck::Off => {}
            PropagationCheck::Resolver => {
                missing.extend(check_resolvers(settings, record).await);
            }
            PropagationCheck::Authoritative => {
                // The nameservers of a zone do not change between checks, so
                // they are looked up until that succeeds once.
                if target.is_none() {
                    match discover(settings, &record.name).await {
                        Ok(found) => *target = Some(found),
                        Err(problem) => {
                            missing.push(Missing {
                                nameserver: "the authoritative nameservers".to_owned(),
                                record: record.name.clone(),
                                values: record.values.clone(),
                                problem: Some(format!("they could not be found: {problem}")),
                            });
                            continue;
                        }
                    }
                }
                if let Some(target) = target {
                    missing.extend(check_authoritative(settings, record, target).await);
                }
            }
        }
    }
    missing
}

async fn check_resolvers(settings: &Settings, record: &Published) -> Vec<Missing> {
    let Ok(name) = parse_name(&record.name) else {
        return vec![unusable_name(record)];
    };
    let answers = join_all(settings.resolvers.iter().map(|resolver| {
        let name = name.clone();
        async move {
            (
                resolver,
                query(settings, *resolver, &name, RecordType::TXT, true).await,
            )
        }
    }))
    .await;
    answers
        .into_iter()
        .filter_map(|(resolver, answer)| {
            // A recursive resolver follows CNAMEs itself, so every TXT value
            // in its answer belongs to the record.
            let served = answer.map(|message| txt_values(&message, None));
            compare(record, &resolver.to_string(), served)
        })
        .collect()
}

async fn check_authoritative(
    settings: &Settings,
    record: &Published,
    target: &Target,
) -> Vec<Missing> {
    let answers = join_all(target.nameservers.iter().map(|nameserver| async move {
        let mut last_problem = "it has no address".to_owned();
        for address in &nameserver.addresses {
            match query(settings, *address, &target.name, RecordType::TXT, false).await {
                Ok(message) => {
                    let label = format!("{} ({})", nameserver.host, address.ip());
                    return (label, Ok(txt_values(&message, Some(&target.name))));
                }
                Err(problem) => last_problem = problem,
            }
        }
        (nameserver.host.clone(), Err(last_problem))
    }))
    .await;
    answers
        .into_iter()
        .filter_map(|(nameserver, served)| compare(record, &nameserver, served))
        .collect()
}

/// What a nameserver is missing of a record, if anything.
fn compare(
    record: &Published,
    nameserver: &str,
    served: std::result::Result<BTreeSet<String>, String>,
) -> Option<Missing> {
    let (values, problem) = match served {
        Ok(served) => {
            let missing: Vec<String> = record
                .values
                .iter()
                .filter(|value| !served.contains(*value))
                .cloned()
                .collect();
            (missing, None)
        }
        Err(problem) => (record.values.clone(), Some(problem)),
    };
    (!values.is_empty()).then(|| Missing {
        nameserver: nameserver.to_owned(),
        record: record.name.clone(),
        values,
        problem,
    })
}

fn unusable_name(record: &Published) -> Missing {
    Missing {
        nameserver: "any nameserver".to_owned(),
        record: record.name.clone(),
        values: record.values.clone(),
        problem: Some("the record name is not a valid DNS name".to_owned()),
    }
}

/// Finds where a record is served: follows CNAMEs from its name, finds the
/// zone the final name is in, and resolves the zone's nameservers.
async fn discover(settings: &Settings, record: &str) -> std::result::Result<Target, String> {
    let mut name = parse_name(record)?;
    for _ in 0..MAX_CNAME_HOPS {
        let answer = ask_recursive(settings, &name, RecordType::CNAME).await?;
        let target = answer.answers.iter().find_map(|answer| match &answer.data {
            RData::CNAME(target) if same_name(&answer.name, &name) => Some(target.0.clone()),
            _ => None,
        });
        match target {
            Some(target) => name = target,
            None => break,
        }
    }

    let zone = zone_of(settings, &name).await?;
    let answer = ask_recursive(settings, &zone, RecordType::NS).await?;
    let hosts: Vec<Name> = answer
        .answers
        .iter()
        .filter_map(|answer| match &answer.data {
            RData::NS(host) => Some(host.0.clone()),
            _ => None,
        })
        .collect();
    if hosts.is_empty() {
        return Err(format!("the zone {zone} has no nameservers"));
    }

    let mut nameservers = Vec::with_capacity(hosts.len());
    for host in hosts {
        let mut addresses = Vec::new();
        // IPv4 first: it is the family more likely to be reachable.
        for record_type in [RecordType::A, RecordType::AAAA] {
            if let Ok(answer) = ask_recursive(settings, &host, record_type).await {
                addresses.extend(
                    answer
                        .answers
                        .iter()
                        .filter_map(|answer| match &answer.data {
                            RData::A(address) => Some(IpAddr::V4(address.0)),
                            RData::AAAA(address) => Some(IpAddr::V6(address.0)),
                            _ => None,
                        }),
                );
            }
        }
        nameservers.push(Nameserver {
            host: host.to_utf8().trim_end_matches('.').to_owned(),
            addresses: addresses
                .into_iter()
                .map(|address| SocketAddr::new(address, settings.nameserver_port))
                .collect(),
        });
    }
    Ok(Target { name, nameservers })
}

/// The zone a name is in: the owner of the SOA record a resolver returns for
/// it, or, when a resolver returns none, the closest parent that has NS
/// records.
async fn zone_of(settings: &Settings, name: &Name) -> std::result::Result<Name, String> {
    let answer = ask_recursive(settings, name, RecordType::SOA).await?;
    let from_soa = answer
        .answers
        .iter()
        .chain(&answer.authorities)
        .find(|record| matches!(record.data, RData::SOA(_)) && record.name.zone_of(name))
        .map(|record| record.name.clone());
    if let Some(zone) = from_soa {
        return Ok(zone);
    }
    let mut candidate = name.clone();
    while !candidate.is_root() {
        let answer = ask_recursive(settings, &candidate, RecordType::NS).await?;
        let has_nameservers = answer.answers.iter().any(|record| {
            matches!(record.data, RData::NS(_)) && same_name(&record.name, &candidate)
        });
        if has_nameservers {
            return Ok(candidate);
        }
        candidate = candidate.base_name();
    }
    Err(format!("no zone was found for {name}"))
}

/// Asks the discovery resolvers in turn until one answers.
async fn ask_recursive(
    settings: &Settings,
    name: &Name,
    record_type: RecordType,
) -> std::result::Result<Message, String> {
    let mut last_problem = "no resolver is configured".to_owned();
    for resolver in &settings.discovery {
        match query(settings, *resolver, name, record_type, true).await {
            Ok(answer) => return Ok(answer),
            Err(problem) => last_problem = format!("resolver {resolver}: {problem}"),
        }
    }
    Err(last_problem)
}

fn parse_name(name: &str) -> std::result::Result<Name, String> {
    let mut parsed =
        Name::from_utf8(name).map_err(|error| format!("{name} is not a DNS name: {error}"))?;
    parsed.set_fqdn(true);
    Ok(parsed)
}

fn same_name(left: &Name, right: &Name) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// The TXT values in the answers of a message. With `owner`, only the values
/// of records with that name. A value sent as several strings is joined.
fn txt_values(message: &Message, owner: Option<&Name>) -> BTreeSet<String> {
    message
        .answers
        .iter()
        .filter(|record| owner.is_none_or(|owner| same_name(&record.name, owner)))
        .filter_map(|record| match &record.data {
            RData::TXT(txt) => Some(
                txt.txt_data
                    .iter()
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

/// Asks one server one question: over UDP, and over TCP when the answer does
/// not fit. Every wait is limited by the query timeout of the settings.
async fn query(
    settings: &Settings,
    server: SocketAddr,
    name: &Name,
    record_type: RecordType,
    recursion: bool,
) -> std::result::Result<Message, String> {
    let mut question = Message::query();
    question.add_query(Query::query(name.clone(), record_type));
    question.metadata.recursion_desired = recursion;
    let mut edns = Edns::new();
    edns.set_max_payload(UDP_PAYLOAD);
    question.set_edns(edns);
    let id = question.metadata.id;
    let bytes = question
        .to_vec()
        .map_err(|error| format!("the question cannot be encoded: {error}"))?;

    let limited = |action: &'static str| move |_| format!("no answer ({action} timed out)");
    let answer = tokio::time::timeout(settings.query_timeout, query_udp(server, &bytes, id))
        .await
        .map_err(limited("UDP"))??;
    let answer = if answer.metadata.truncation {
        tokio::time::timeout(settings.query_timeout, query_tcp(server, &bytes))
            .await
            .map_err(limited("TCP"))??
    } else {
        answer
    };
    match answer.metadata.response_code {
        // "No such name" is an answer: the record is not there (yet).
        ResponseCode::NoError | ResponseCode::NXDomain => Ok(answer),
        other => Err(format!("the server answered {other}")),
    }
}

async fn query_udp(
    server: SocketAddr,
    question: &[u8],
    id: u16,
) -> std::result::Result<Message, String> {
    let failed = |error: std::io::Error| format!("UDP: {error}");
    let local: SocketAddr = if server.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        ([0u16; 8], 0).into()
    };
    let socket = UdpSocket::bind(local).await.map_err(failed)?;
    socket.connect(server).await.map_err(failed)?;
    socket.send(question).await.map_err(failed)?;
    let mut buffer = vec![0_u8; 4096];
    loop {
        let length = socket.recv(&mut buffer).await.map_err(failed)?;
        // Anything that is not the answer to this question is skipped.
        match Message::from_vec(&buffer[..length]) {
            Ok(answer) if answer.metadata.id == id => return Ok(answer),
            _ => continue,
        }
    }
}

async fn query_tcp(server: SocketAddr, question: &[u8]) -> std::result::Result<Message, String> {
    let failed = |error: std::io::Error| format!("TCP: {error}");
    let mut stream = TcpStream::connect(server).await.map_err(failed)?;
    let length =
        u16::try_from(question.len()).map_err(|_| "the question is too long".to_owned())?;
    stream
        .write_all(&length.to_be_bytes())
        .await
        .map_err(failed)?;
    stream.write_all(question).await.map_err(failed)?;
    let mut length = [0_u8; 2];
    stream.read_exact(&mut length).await.map_err(failed)?;
    let mut answer = vec![0_u8; usize::from(u16::from_be_bytes(length))];
    stream.read_exact(&mut answer).await.map_err(failed)?;
    Message::from_vec(&answer).map_err(|error| format!("TCP: unreadable answer: {error}"))
}

#[cfg(test)]
mod tests;
