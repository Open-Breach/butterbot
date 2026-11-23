//! Support for integration tests: starting the ACME test server (Pebble) and
//! its challenge DNS server as child processes.
//!
//! This module is compiled only for tests and with the `test-support`
//! feature. The servers are separate programs:
//!
//! ```text
//! go install github.com/letsencrypt/pebble/v2/cmd/pebble@v2.10.3
//! go install github.com/letsencrypt/pebble/v2/cmd/pebble-challtestsrv@v2.10.3
//! ```
//!
//! They are looked for in the directory named by `BUTTERBOT_TEST_BIN_DIR`,
//! then on `PATH`, then in `~/go/bin`. Tests that need them are marked
//! `#[ignore]` and run with `cargo test -- --include-ignored`.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use serde_json::json;
use tempfile::TempDir;

/// The variable that names the directory with the test servers.
pub const BIN_DIR_VARIABLE: &str = "BUTTERBOT_TEST_BIN_DIR";

/// The name of the challenge DNS server program.
pub const CHALLTESTSRV: &str = "pebble-challtestsrv";

/// The name of the ACME test server program.
pub const PEBBLE: &str = "pebble";

/// Finds a test server program.
///
/// # Panics
///
/// If the program is not installed. The message says how to install it.
pub fn find_program(name: &str) -> PathBuf {
    let mut directories: Vec<PathBuf> = Vec::new();
    directories.extend(std::env::var_os(BIN_DIR_VARIABLE).map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        directories.extend(std::env::split_paths(&path));
    }
    directories.extend(std::env::var_os("HOME").map(|home| PathBuf::from(home).join("go/bin")));
    directories
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| {
            panic!(
                "the test server `{name}` was not found. Install it with \
                 `go install github.com/letsencrypt/pebble/v2/cmd/{name}@v2.10.3`, or set \
                 {BIN_DIR_VARIABLE} to the directory that holds it."
            )
        })
}

/// A loopback address with a port that is free right now, and that this
/// process has not handed out before.
///
/// The port is released again before it is returned, so that a server can
/// take it. Tests run at the same time; without the second condition two of
/// them could be given the same port, and the one whose server lost the port
/// would talk to the server of the other.
pub fn free_address() -> SocketAddr {
    static HANDED_OUT: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

    loop {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("a free port on the loopback interface");
        let address = listener.local_addr().expect("the address of the listener");
        let mut handed_out = HANDED_OUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !handed_out.contains(&address.port()) {
            handed_out.push(address.port());
            return address;
        }
    }
}

/// How many times a test server is started again, on other ports, when it
/// exits before it listens. A port that was free when it was chosen can be
/// taken a moment later.
const START_ATTEMPTS: usize = 5;

/// Waits until `process` accepts connections at `address`. Returns `false`
/// when the process exited instead.
///
/// # Panics
///
/// If the process keeps running but does not listen within ten seconds.
fn listens(process: &mut Process, what: &str, address: SocketAddr) -> bool {
    let started = Instant::now();
    while TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err() {
        if process.has_exited() {
            return false;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{what} did not start listening at {address}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Something listens at the address. If it is not this process, because
    // the port was taken by someone else in the meantime, this process has
    // failed to bind and is exiting; give it a moment to do so.
    std::thread::sleep(Duration::from_millis(50));
    !process.has_exited()
}

/// A child process that is killed when the value is dropped.
pub struct Process {
    child: Child,
}

impl Process {
    /// Starts `command` with its output discarded.
    ///
    /// # Panics
    ///
    /// If the program cannot be started.
    pub fn start(command: &mut Command) -> Self {
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|error| panic!("{command:?} cannot be started: {error}"));
        Self { child }
    }
}

impl Process {
    /// Whether the process has ended.
    pub fn has_exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A running challenge DNS server.
///
/// It serves the TXT records that are set through its management interface,
/// which is what [`crate::dns::challtestsrv::ChallTestSrvProvider`] does.
pub struct ChallTestSrv {
    /// The address of the management interface.
    pub management: SocketAddr,
    /// The address DNS questions are answered at, over UDP and TCP.
    pub dns: SocketAddr,
    _process: Process,
}

impl ChallTestSrv {
    /// Starts the server on free loopback ports.
    ///
    /// # Panics
    ///
    /// If the program is not installed or does not start.
    pub fn start() -> Self {
        for _ in 0..START_ATTEMPTS {
            let (management, dns) = (free_address(), free_address());
            let mut process = Process::start(
                Command::new(find_program(CHALLTESTSRV))
                    .args(["-management", &management.to_string()])
                    .args(["-dnsserver", &dns.to_string()])
                    .args(["-http01", "", "-https01", "", "-tlsalpn01", "", "-doh", ""]),
            );
            if listens(
                &mut process,
                "the management interface of pebble-challtestsrv",
                management,
            ) && listens(&mut process, "the DNS server of pebble-challtestsrv", dns)
            {
                return Self {
                    management,
                    dns,
                    _process: process,
                };
            }
        }
        panic!("pebble-challtestsrv exited at every start; is it the right program?");
    }

    /// The URL of the management interface.
    pub fn management_url(&self) -> String {
        format!("http://{}", self.management)
    }
}

/// How the ACME test server is set up.
#[derive(Debug, Clone, Default)]
pub struct PebbleOptions {
    /// Require external account binding, and accept this key identifier with
    /// this key (base64url-encoded).
    pub external_account: Option<(String, String)>,
    /// The share of orders, in percent, for which the server reuses an
    /// authorization that is already valid, so that nothing has to be proven.
    pub authorization_reuse: u8,
}

/// A running ACME test server (Pebble).
///
/// It issues certificates from an authority it makes up when it starts, and
/// validates DNS challenges by asking the DNS server it was started with. It
/// offers the certificate profiles `default` (90 days) and `shortlived`
/// (6 days), and renewal information.
pub struct Pebble {
    /// The directory URL to configure as `acme.<name>.directory_url`.
    pub directory_url: String,
    /// The root certificate that signed the certificate of the server's own
    /// HTTPS interface, as PEM. A client has to trust it to talk to the server.
    pub root_pem: String,
    /// The address of the ACME interface.
    pub address: SocketAddr,
    /// The address of the management interface.
    pub management: SocketAddr,
    directory: TempDir,
    _process: Process,
}

impl Pebble {
    /// Starts the server. It asks `dns` to validate challenges.
    ///
    /// # Panics
    ///
    /// If the program is not installed or does not start.
    pub fn start(dns: SocketAddr, options: &PebbleOptions) -> Self {
        for _ in 0..START_ATTEMPTS {
            if let Some(server) = Self::try_start(dns, options) {
                return server;
            }
        }
        panic!("pebble exited at every start; is it the right program?");
    }

    /// Starts the server on freshly chosen ports. `None` when it exited
    /// before it listened, which happens when a port was taken in between.
    fn try_start(dns: SocketAddr, options: &PebbleOptions) -> Option<Self> {
        let directory = TempDir::new().expect("a temporary directory");
        let (address, management) = (free_address(), free_address());

        // The server's own HTTPS certificate, from a root made for this run.
        let root_key = KeyPair::generate().expect("a key");
        let mut root = CertificateParams::new(Vec::<String>::new()).expect("parameters");
        root.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root.distinguished_name
            .push(DnType::CommonName, "Butterbot Test Root");
        let root_certificate = root.self_signed(&root_key).expect("a root certificate");
        let server_key = KeyPair::generate().expect("a key");
        let server = CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .expect("parameters");
        let server_certificate = server
            .signed_by(&server_key, &Issuer::from_params(&root, &root_key))
            .expect("a server certificate");
        let certificate_path = directory.path().join("cert.pem");
        let key_path = directory.path().join("key.pem");
        std::fs::write(&certificate_path, server_certificate.pem()).expect("the certificate file");
        std::fs::write(&key_path, server_key.serialize_pem()).expect("the key file");

        let mut settings = json!({
            "listenAddress": address.to_string(),
            "managementListenAddress": management.to_string(),
            "certificate": certificate_path,
            "privateKey": key_path,
            "httpPort": 5002,
            "tlsPort": 5001,
            "ocspResponderURL": "",
            "externalAccountBindingRequired": options.external_account.is_some(),
            "retryAfter": { "authz": 1, "order": 1 },
            "profiles": {
                "default": {
                    "description": "The profile you know and love",
                    "validityPeriod": 7_776_000
                },
                "shortlived": {
                    "description": "A short-lived cert profile, without actual enforcement",
                    "validityPeriod": 518_400
                }
            }
        });
        if let Some((kid, key)) = &options.external_account {
            settings["externalAccountMACKeys"] = json!({ kid: key });
        }
        let config_path = directory.path().join("pebble-config.json");
        std::fs::write(&config_path, json!({ "pebble": settings }).to_string())
            .expect("the configuration file");

        let mut process = Process::start(
            Command::new(find_program(PEBBLE))
                .arg("-config")
                .arg(&config_path)
                .args(["-dnsserver", &dns.to_string()])
                // Validate at once, do not reject nonces at random, and reuse
                // authorizations only as often as the test asks for.
                .env("PEBBLE_VA_NOSLEEP", "1")
                .env("PEBBLE_WFE_NONCEREJECT", "0")
                .env("PEBBLE_AUTHZREUSE", options.authorization_reuse.to_string()),
        );
        if !listens(&mut process, "the ACME test server", address) {
            return None;
        }
        Some(Self {
            directory_url: format!("https://{address}/dir"),
            root_pem: root_certificate.pem(),
            address,
            management,
            directory,
            _process: process,
        })
    }

    /// Writes the root certificate to a file and returns its path. The file
    /// lives as long as the server.
    pub fn root_file(&self) -> PathBuf {
        let path = self.directory.path().join("root.pem");
        std::fs::write(&path, &self.root_pem).expect("the root certificate file");
        path
    }

    /// An HTTP client that trusts the server.
    pub fn http(&self) -> reqwest::Client {
        crate::http::client_with_roots(std::slice::from_ref(&self.root_pem))
            .expect("an HTTP client that trusts the test root")
    }
}

/// The ACME test server together with the challenge DNS server it validates
/// against.
pub struct AcmeTestServer {
    pub dns: ChallTestSrv,
    pub pebble: Pebble,
}

impl AcmeTestServer {
    /// Starts both servers.
    pub fn start() -> Self {
        Self::start_with(&PebbleOptions::default())
    }

    pub fn start_with(options: &PebbleOptions) -> Self {
        let dns = ChallTestSrv::start();
        let pebble = Pebble::start(dns.dns, options);
        Self { dns, pebble }
    }

    /// Clears the TXT values at `record` through `provider`.
    ///
    /// # Panics
    ///
    /// If the challenge DNS server cannot be reached.
    pub async fn dns_cleared_for(
        &self,
        record: &str,
        provider: &crate::dns::challtestsrv::ChallTestSrvProvider,
    ) {
        use crate::dns::DnsProvider;

        let handle = crate::dns::RecordHandle {
            zone: crate::dns::Zone::new("example.com", "example.com"),
            name: record.to_owned(),
            values: Vec::new(),
            record_ids: Vec::new(),
        };
        provider
            .cleanup(handle)
            .await
            .expect("the record is cleared");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Task 7.1: a trivial test can fetch the server's directory.
    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn directory_of_the_acme_test_server_can_be_fetched() {
        let server = AcmeTestServer::start();

        let response = server
            .pebble
            .http()
            .get(&server.pebble.directory_url)
            .send()
            .await
            .expect("the server answers over HTTPS with its test certificate");
        assert!(response.status().is_success());
        let directory: serde_json::Value = response.json().await.expect("JSON");

        for endpoint in ["newNonce", "newAccount", "newOrder", "renewalInfo"] {
            let url = directory[endpoint]
                .as_str()
                .unwrap_or_else(|| panic!("{endpoint}: {directory}"));
            assert!(
                url.starts_with(&format!("https://{}/", server.pebble.address)),
                "{url}"
            );
        }
        let profiles = directory["meta"]["profiles"]
            .as_object()
            .expect("profiles are offered");
        assert!(profiles.contains_key("default") && profiles.contains_key("shortlived"));
    }

    #[tokio::test]
    #[ignore = "needs pebble and pebble-challtestsrv; see src/testing.rs"]
    async fn acme_test_server_is_not_trusted_without_its_root() {
        let server = AcmeTestServer::start();
        let error = crate::http::client()
            .expect("an HTTP client")
            .get(&server.pebble.directory_url)
            .send()
            .await
            .expect_err("the bundled roots do not include the test root");
        assert!(error.is_connect(), "{error:?}");
    }

    #[test]
    fn free_addresses_are_on_the_loopback_interface() {
        let address = free_address();
        assert!(address.ip().is_loopback() && address.port() != 0);
    }
}
