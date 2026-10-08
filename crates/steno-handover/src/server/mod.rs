//! The listener: a TCP acceptor that terminates TLS 1.3 with the identity,
//! runs HTTP/1.1 over each connection ([`connection`]) and, when
//! advertising, publishes `_steno._tcp` with the TXT record (`v=1`,
//! `id=<macID>`) through Bonjour, and again when the network changes
//! ([`advertise`]). Loopback only when
//! `advertise` is false; otherwise every IPv4 address is bound, and a
//! connection whose local address is neither loopback nor a LAN address
//! (a VPN tunnel) is closed before the handshake, as the Swift listener's
//! prohibited interface types refuse it. One socket on every address
//! serves an address the computer gains after start on the same port,
//! and a network change closes no connection: the check runs once per
//! connection, at accept. Swift:
//! `Network/HandoverServer.swift`, `Network/ServerMetrics.swift`.

pub mod advertise;
pub mod connection;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

use crate::configuration::HandoverConfiguration;
use crate::engine::RequestHandling;
use crate::identity::{HandoverIdentity, IdentityError};

/// Counters the tests read to prove the auth gate and the body limit did
/// what the plan says: heads seen, body bytes discarded after a rejection,
/// statuses written, connections the server closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub request_heads: u64,
    pub handled_requests: u64,
    pub discarded_body_bytes: u64,
    pub statuses: Vec<u16>,
    pub closed_by_server: u64,
    /// Connections closed because the client stayed silent past the read
    /// timeout (counted in `closed_by_server` too).
    pub timed_out: u64,
    /// Connections closed before the handshake because they arrived on an
    /// address the service does not live on (counted in `closed_by_server`
    /// too).
    pub refused_interface: u64,
}

#[derive(Debug, Default)]
pub struct ServerMetrics {
    state: Mutex<MetricsSnapshot>,
}

impl ServerMetrics {
    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn update(&self, change: impl FnOnce(&mut MetricsSnapshot)) {
        change(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// How long [`HandoverServer::stop`] waits for the connections in flight
/// to finish their request before it cuts them.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// How long the accept loop keeps the LAN addresses before it reads them
/// again: a connection flood costs one interface query a second, not one
/// per connection.
pub const LAN_REFRESH: Duration = Duration::from_secs(1);

/// The LAN addresses a connection is checked against: `query` on the
/// blocking pool, kept for `every`. A query that fails or panics gives the
/// empty set, which refuses every connection but loopback.
pub(crate) struct LanAddresses {
    query: fn() -> Vec<Ipv4Addr>,
    every: Duration,
    read_at: Option<Instant>,
    addresses: Vec<Ipv4Addr>,
}

impl LanAddresses {
    pub(crate) fn new(query: fn() -> Vec<Ipv4Addr>, every: Duration) -> Self {
        LanAddresses {
            query,
            every,
            read_at: None,
            addresses: Vec::new(),
        }
    }

    /// The computer's ([`advertise::current_lan_addresses`]).
    fn system() -> Self {
        Self::new(advertise::current_lan_addresses, LAN_REFRESH)
    }

    async fn current(&mut self) -> &[Ipv4Addr] {
        if self
            .read_at
            .is_none_or(|read_at| read_at.elapsed() >= self.every)
        {
            self.addresses = tokio::task::spawn_blocking(self.query)
                .await
                .unwrap_or_default();
            self.read_at = Some(Instant::now());
        }
        &self.addresses
    }
}

/// What the listener binds and whom it serves.
pub(crate) enum Reach {
    /// 127.0.0.1 only.
    Loopback,
    /// Every IPv4 address, each connection checked against `lan`; the
    /// Bonjour record is published when `publish`.
    Lan { lan: LanAddresses, publish: bool },
}

/// A running listener. [`HandoverServer::stop`] closes it in order;
/// dropping it sends the same signal without waiting.
pub struct HandoverServer {
    pub port: u16,
    accept_task: JoinHandle<()>,
    stop_signal: watch::Sender<bool>,
    advertiser: Option<advertise::Advertiser>,
}

impl std::fmt::Debug for HandoverServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandoverServer")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl HandoverServer {
    /// Binds and starts accepting. Advertising binds every IPv4 interface
    /// (the phone resolves IPv4 only) and publishes the LAN addresses;
    /// otherwise 127.0.0.1.
    pub async fn start(
        configuration: &HandoverConfiguration,
        identity: &HandoverIdentity,
        engine: Arc<dyn RequestHandling>,
        metrics: Arc<ServerMetrics>,
    ) -> Result<HandoverServer, ServerError> {
        let reach = if configuration.advertise {
            Reach::Lan {
                lan: LanAddresses::system(),
                publish: true,
            }
        } else {
            Reach::Loopback
        };
        Self::start_with(configuration, identity, engine, metrics, reach).await
    }

    /// [`HandoverServer::start`] with the reach given, not derived from
    /// `configuration.advertise`.
    pub(crate) async fn start_with(
        configuration: &HandoverConfiguration,
        identity: &HandoverIdentity,
        engine: Arc<dyn RequestHandling>,
        metrics: Arc<ServerMetrics>,
        reach: Reach,
    ) -> Result<HandoverServer, ServerError> {
        let acceptor = TlsAcceptor::from(identity.server_config()?);
        let (address, lan, publish) = match reach {
            Reach::Loopback => (IpAddr::V4(Ipv4Addr::LOCALHOST), None, false),
            Reach::Lan { lan, publish } => (IpAddr::V4(Ipv4Addr::UNSPECIFIED), Some(lan), publish),
        };
        let listener = TcpListener::bind(SocketAddr::new(address, configuration.port)).await?;
        let port = listener.local_addr()?.port();
        if port == 0 {
            return Err(ServerError::NoPort);
        }
        let advertiser = if publish {
            Some(advertise::Advertiser::publish(
                &configuration.service_name,
                identity.mac_id(),
                port,
            )?)
        } else {
            None
        };
        let configuration = Arc::new(configuration.clone());
        let (stop_signal, stopping) = watch::channel(false);
        let accept_task = tokio::spawn(accept_loop(
            listener,
            acceptor,
            engine,
            metrics,
            configuration,
            lan,
            stopping,
        ));
        Ok(HandoverServer {
            port,
            accept_task,
            stop_signal,
            advertiser,
        })
    }

    /// Stops accepting, closes every connection (an idle one, or one
    /// lingering after its last response, at once; one mid-request after
    /// its response; any still busy after [`STOP_GRACE`] by force) and
    /// withdraws the Bonjour record. No task of the listener outlives it,
    /// except store writes already in line, which finish unless the runtime
    /// shuts down first.
    /// Swift: `HandoverServer.stop` (`group.shutdownGracefully()` closes the
    /// child channels).
    pub async fn stop(self) {
        let HandoverServer {
            mut accept_task,
            stop_signal,
            advertiser,
            ..
        } = self;
        stop_signal.send_replace(true);
        // The accept task drains for STOP_GRACE; one second more before it
        // is aborted outright.
        if tokio::time::timeout(STOP_GRACE + Duration::from_secs(1), &mut accept_task)
            .await
            .is_err()
        {
            accept_task.abort();
        }
        if let Some(advertiser) = advertiser {
            advertiser.withdraw();
        }
    }
}

/// Resolves once the server is stopping, also when the server was dropped.
pub(crate) async fn stopped(stopping: &mut watch::Receiver<bool>) {
    let _ = stopping.wait_for(|stopping| *stopping).await;
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    engine: Arc<dyn RequestHandling>,
    metrics: Arc<ServerMetrics>,
    configuration: Arc<HandoverConfiguration>,
    mut lan: Option<LanAddresses>,
    mut stopping: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    loop {
        // Stop first, so nothing is accepted once it is asked for; then reap
        // finished connections before the next accept, so the set does not
        // grow for the life of the listener, nor under a connection flood.
        let accepted = tokio::select! {
            biased;
            () = stopped(&mut stopping) => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            accepted = listener.accept() => accepted,
        };
        let (stream, _) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(target: "steno::handover", "accept failed: {error}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let served = match &mut lan {
            Some(lan) => on_a_served_network(&stream, lan.current().await),
            None => true,
        };
        if !served {
            metrics.update(|metrics| {
                metrics.refused_interface += 1;
                metrics.closed_by_server += 1;
            });
            continue;
        }
        let _ = stream.set_nodelay(true);
        let acceptor = acceptor.clone();
        let engine = engine.clone();
        let metrics = metrics.clone();
        let configuration = configuration.clone();
        let mut stopping = stopping.clone();
        connections.spawn(async move {
            // The handshake gets the read timeout: a client that connects
            // and says nothing costs the same as one that stops mid-request.
            let handshake =
                tokio::time::timeout(configuration.read_timeout, acceptor.accept(stream));
            let handshake = tokio::select! {
                handshake = handshake => handshake,
                () = stopped(&mut stopping) => {
                    metrics.update(|metrics| metrics.closed_by_server += 1);
                    return;
                }
            };
            match handshake {
                Ok(Ok(tls)) => {
                    connection::serve(tls, engine, metrics, configuration, stopping).await;
                }
                Ok(Err(error)) => {
                    tracing::debug!(target: "steno::handover", "TLS handshake failed: {error}");
                }
                Err(_) => {
                    metrics.update(|metrics| {
                        metrics.timed_out += 1;
                        metrics.closed_by_server += 1;
                    });
                }
            }
        });
    }
    // The listener is dropped here: nothing new connects while the
    // connections in flight finish. What is still busy after the grace is
    // aborted with the set.
    drop(listener);
    let drained = async { while connections.join_next().await.is_some() {} };
    let _ = tokio::time::timeout(STOP_GRACE, drained).await;
}

/// Whether `stream` arrived on loopback or on one of `lan`; a connection
/// over a tunnel is not served. `lan` is at most [`LAN_REFRESH`] old, so a
/// network change after start is followed.
fn on_a_served_network(stream: &tokio::net::TcpStream, lan: &[Ipv4Addr]) -> bool {
    stream
        .local_addr()
        .is_ok_and(|local| advertise::accepts_local_address(local.ip(), lan))
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("binding the listener: {0}")]
    Bind(#[from] std::io::Error),
    #[error("the listener reported no port")]
    NoPort,
    #[error("advertising the service: {0}")]
    Advertise(#[from] mdns_sd::Error),
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Utc;
    use rustls_pki_types::ServerName;
    use steno_core::Store;
    use steno_core::testing::FakeHandoverIntake;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;
    use tokio_rustls::TlsConnector;

    use super::*;
    use crate::engine::Engine;
    use crate::pinning::pinned_client_config;

    static QUERIES: AtomicUsize = AtomicUsize::new(0);

    fn counted() -> Vec<Ipv4Addr> {
        QUERIES.fetch_add(1, Ordering::SeqCst);
        vec![Ipv4Addr::new(192, 168, 1, 20)]
    }

    fn no_lan() -> Vec<Ipv4Addr> {
        Vec::new()
    }

    #[tokio::test]
    async fn the_lan_addresses_are_read_once_per_refresh_and_a_failed_read_serves_none() {
        let mut kept = LanAddresses::new(counted, Duration::from_secs(3600));
        let before = QUERIES.load(Ordering::SeqCst);
        assert_eq!(kept.current().await, &[Ipv4Addr::new(192, 168, 1, 20)]);
        assert_eq!(kept.current().await, &[Ipv4Addr::new(192, 168, 1, 20)]);
        assert_eq!(QUERIES.load(Ordering::SeqCst) - before, 1, "one query");

        let mut panicking = LanAddresses::new(|| panic!("the system did not say"), LAN_REFRESH);
        assert!(panicking.current().await.is_empty(), "fails closed");
    }

    static READS: AtomicUsize = AtomicUsize::new(0);

    /// Answers twice, then panics: the system that stops saying.
    fn answers_twice() -> Vec<Ipv4Addr> {
        assert!(
            READS.fetch_add(1, Ordering::SeqCst) < 2,
            "the system stopped saying"
        );
        vec![Ipv4Addr::new(192, 168, 1, 20)]
    }

    #[tokio::test]
    async fn with_no_refresh_interval_each_connection_reads_again_and_a_failed_read_keeps_nothing()
    {
        let mut fresh = LanAddresses::new(answers_twice, Duration::ZERO);
        assert_eq!(fresh.current().await, &[Ipv4Addr::new(192, 168, 1, 20)]);
        assert_eq!(fresh.current().await, &[Ipv4Addr::new(192, 168, 1, 20)]);
        assert_eq!(READS.load(Ordering::SeqCst), 2, "read again");
        assert!(fresh.current().await.is_empty(), "not the stale set");
    }

    /// A non-loopback IPv4 address of this host, an interface that is up
    /// first.
    fn host_address() -> Option<Ipv4Addr> {
        let interfaces = if_addrs::get_if_addrs().ok()?;
        let mut candidates: Vec<_> = interfaces
            .iter()
            .filter_map(|interface| match &interface.addr {
                if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() => {
                    Some((!interface.is_oper_up(), v4.ip))
                }
                _ => None,
            })
            .collect();
        candidates.sort_unstable();
        candidates.first().map(|&(_, address)| address)
    }

    /// Whether a connection to `address` reaches an `accept` in this
    /// process. An application firewall (macOS) completes the handshake
    /// and then holds the connection back from an unsigned binary.
    async fn reaches_this_process(address: Ipv4Addr) -> bool {
        let Ok(listener) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
            return false;
        };
        let Ok(local) = listener.local_addr() else {
            return false;
        };
        let both = async {
            let (connected, accepted) = tokio::join!(
                TcpStream::connect((address, local.port())),
                listener.accept()
            );
            connected.is_ok() && accepted.is_ok()
        };
        tokio::time::timeout(Duration::from_secs(3), both)
            .await
            .unwrap_or(false)
    }

    /// The interface test's host address, as the LAN query of its second
    /// server.
    static HOST: OnceLock<Ipv4Addr> = OnceLock::new();

    fn the_host() -> Vec<Ipv4Addr> {
        HOST.get().copied().into_iter().collect()
    }

    /// Reports a test this host cannot run on stdout. CI sets
    /// `STENO_REQUIRE_LAN_TEST` on Linux and Windows, so a skip there fails.
    fn skip(reason: &str) {
        let required = std::env::var_os("STENO_REQUIRE_LAN_TEST").is_some();
        if let Err(message) = may_skip(required, reason) {
            panic!("{message}");
        }
        println!("SKIPPED: {reason}");
    }

    /// Whether the interface test may skip for `reason`: only where the run
    /// does not require it.
    fn may_skip(required: bool, reason: &str) -> Result<(), String> {
        if required {
            Err(format!("the interface test must not skip here: {reason}"))
        } else {
            Ok(())
        }
    }

    #[test]
    fn a_skip_fails_where_the_run_requires_the_interface_test() {
        let refused = may_skip(true, "no LAN address").unwrap_err();
        assert!(refused.contains("no LAN address"), "{refused}");
        assert_eq!(may_skip(false, "no LAN address"), Ok(()));
    }

    /// A listener on every IPv4 address that serves loopback and `lan`,
    /// read again after `every`.
    async fn serve(
        configuration: &HandoverConfiguration,
        identity: &Arc<HandoverIdentity>,
        lan: fn() -> Vec<Ipv4Addr>,
        every: Duration,
    ) -> (HandoverServer, Arc<ServerMetrics>) {
        let engine = Arc::new(Engine::new(
            configuration.clone(),
            identity.clone(),
            Arc::new(Store::in_memory().unwrap()),
            Arc::new(FakeHandoverIntake::default()),
            watch::channel(Vec::new()).0,
            Arc::new(Utc::now),
        ));
        let metrics = Arc::new(ServerMetrics::default());
        let reach = Reach::Lan {
            lan: LanAddresses::new(lan, every),
            publish: false,
        };
        let server =
            HandoverServer::start_with(configuration, identity, engine, metrics.clone(), reach)
                .await
                .unwrap();
        (server, metrics)
    }

    #[tokio::test]
    async fn the_lan_is_served_and_a_connection_outside_it_is_closed_before_any_tls_byte() {
        let Some(address) = host_address() else {
            return skip("this host has no non-loopback IPv4 address");
        };
        if !reaches_this_process(address).await {
            return skip(&format!(
                "{address} delivers no inbound connection here (a firewall)"
            ));
        }
        HOST.set(address).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let configuration = HandoverConfiguration {
            inbox_directory: directory.path().join("inbox"),
            read_timeout: Duration::from_secs(20),
            ..HandoverConfiguration::default()
        };
        let identity = Arc::new(HandoverIdentity::mint("Steno test", Utc::now()).unwrap());

        // Only loopback is served: the host's own address stands in for a
        // tunnel's.
        let (outside, metrics) = serve(&configuration, &identity, no_lan, LAN_REFRESH).await;
        let mut refused = TcpStream::connect((address, outside.port)).await.unwrap();
        let mut received = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), refused.read_to_end(&mut received))
            .await
            .expect("closed long before the read timeout");
        assert!(
            read.is_ok() || received.is_empty(),
            "closed or reset: {read:?}"
        );
        assert!(received.is_empty(), "no TLS byte: {received:?}");
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.refused_interface, 1);
        assert_eq!(snapshot.closed_by_server, 1);

        // Loopback is served: the server waits for the client's hello.
        let mut on_loopback = TcpStream::connect((Ipv4Addr::LOCALHOST, outside.port))
            .await
            .unwrap();
        let mut byte = [0u8; 1];
        let waited =
            tokio::time::timeout(Duration::from_millis(300), on_loopback.read(&mut byte)).await;
        assert!(waited.is_err(), "a loopback connection stays open");
        assert_eq!(metrics.snapshot().refused_interface, 1);
        outside.stop().await;

        // The host's address is the LAN: the handshake completes and a
        // request is answered.
        let (lan, metrics) = serve(&configuration, &identity, the_host, LAN_REFRESH).await;
        let exchange = async {
            let mut tls = connect(&identity, address, lan.port).await?;
            tls.write_all(b"GET /v1/hello HTTP/1.1\r\nHost: steno\r\nConnection: close\r\n\r\n")
                .await?;
            let mut response = Vec::new();
            tls.read_to_end(&mut response).await?;
            std::io::Result::Ok(response)
        };
        let response = within(exchange)
            .await
            .expect("a TLS exchange on the LAN address");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(metrics.snapshot().refused_interface, 0);
        lan.stop().await;
    }

    /// The host address of the network test below, and whether the
    /// computer is on that network yet.
    static JOINING: OnceLock<Ipv4Addr> = OnceLock::new();
    static JOINED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn the_host_once_joined() -> Vec<Ipv4Addr> {
        if JOINED.load(Ordering::SeqCst) {
            JOINING.get().copied().into_iter().collect()
        } else {
            Vec::new()
        }
    }

    /// `future`'s output, which comes long before the read timeout.
    async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .expect("answered long before the read timeout")
    }

    /// A pinned TLS connection to `address` on `port`.
    async fn connect(
        identity: &HandoverIdentity,
        address: Ipv4Addr,
        port: u16,
    ) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let connector = TlsConnector::from(pinned_client_config(&identity.fingerprint()).unwrap());
        let tcp = TcpStream::connect((address, port)).await?;
        connector
            .connect(ServerName::from(IpAddr::V4(address)), tcp)
            .await
    }

    /// `GET /v1/hello` on `tls`, kept alive: the status line, after the
    /// whole response is read.
    async fn hello(
        tls: &mut tokio_rustls::client::TlsStream<TcpStream>,
    ) -> std::io::Result<String> {
        tls.write_all(b"GET /v1/hello HTTP/1.1\r\nHost: steno\r\n\r\n")
            .await?;
        let mut response = Vec::new();
        let head_end = loop {
            if let Some(end) = response.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
            let mut buffer = [0u8; 1024];
            let read = tls.read(&mut buffer).await?;
            if read == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            response.extend_from_slice(&buffer[..read]);
        };
        let head = String::from_utf8_lossy(&response[..head_end]).into_owned();
        let length: usize = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
            .unwrap_or(0);
        let mut body = vec![0u8; (head_end + length).saturating_sub(response.len())];
        tls.read_exact(&mut body).await?;
        Ok(head.lines().next().unwrap_or_default().to_owned())
    }

    #[tokio::test]
    async fn an_address_gained_after_start_is_served_and_a_change_cuts_no_connection() {
        let Some(address) = host_address() else {
            return skip("this host has no non-loopback IPv4 address");
        };
        if !reaches_this_process(address).await {
            return skip(&format!(
                "{address} delivers no inbound connection here (a firewall)"
            ));
        }
        JOINING.set(address).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let configuration = HandoverConfiguration {
            inbox_directory: directory.path().join("inbox"),
            read_timeout: Duration::from_secs(20),
            ..HandoverConfiguration::default()
        };
        let identity = Arc::new(HandoverIdentity::mint("Steno test", Utc::now()).unwrap());
        let (server, metrics) = serve(
            &configuration,
            &identity,
            the_host_once_joined,
            Duration::ZERO,
        )
        .await;

        // Not on the network yet: refused before the handshake.
        let refused = within(connect(&identity, address, server.port)).await;
        assert!(refused.is_err(), "no handshake off the LAN");
        assert_eq!(metrics.snapshot().refused_interface, 1);

        // The computer joins: the same socket, on the same port, serves the
        // new address.
        JOINED.store(true, Ordering::SeqCst);
        let mut joined = within(connect(&identity, address, server.port))
            .await
            .expect("a handshake on the gained address");
        let status = within(hello(&mut joined)).await.unwrap();
        assert!(status.starts_with("HTTP/1.1 200"), "{status}");

        // The address leaves the LAN: the connection already open is still
        // answered, and only a new one is refused.
        JOINED.store(false, Ordering::SeqCst);
        let status = within(hello(&mut joined)).await.unwrap();
        assert!(status.starts_with("HTTP/1.1 200"), "{status}");
        let refused = within(connect(&identity, address, server.port)).await;
        assert!(refused.is_err(), "no handshake once the address left");
        assert_eq!(metrics.snapshot().refused_interface, 2);
        drop(joined);
        server.stop().await;
    }
}
