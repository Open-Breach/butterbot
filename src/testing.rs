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

/// The variable that names the directory with the test servers.
pub const BIN_DIR_VARIABLE: &str = "BUTTERBOT_TEST_BIN_DIR";

/// The name of the challenge DNS server program.
pub const CHALLTESTSRV: &str = "pebble-challtestsrv";

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

/// A loopback address with a port that is free right now.
pub fn free_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port on the loopback interface");
    listener.local_addr().expect("the address of the listener")
}

/// Waits until something accepts connections at `address`.
///
/// # Panics
///
/// If nothing does within ten seconds.
pub fn wait_until_listening(what: &str, address: SocketAddr) {
    let started = Instant::now();
    while TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{what} did not start listening at {address}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
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
        let (management, dns) = (free_address(), free_address());
        let process = Process::start(
            Command::new(find_program(CHALLTESTSRV))
                .args(["-management", &management.to_string()])
                .args(["-dnsserver", &dns.to_string()])
                .args(["-http01", "", "-https01", "", "-tlsalpn01", "", "-doh", ""]),
        );
        wait_until_listening(
            "the management interface of pebble-challtestsrv",
            management,
        );
        wait_until_listening("the DNS server of pebble-challtestsrv", dns);
        Self {
            management,
            dns,
            _process: process,
        }
    }

    /// The URL of the management interface.
    pub fn management_url(&self) -> String {
        format!("http://{}", self.management)
    }
}
