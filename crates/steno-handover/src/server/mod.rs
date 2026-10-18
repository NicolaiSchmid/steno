//! The listener: a TCP acceptor that terminates TLS 1.3 with the identity,
//! runs HTTP/1.1 over each connection and, when advertising, publishes the
//! Bonjour record. It serves loopback and LAN addresses only: a connection
//! arriving on any other local address (a VPN tunnel) is closed before the
//! handshake. A fixed port that is taken gives way to one the system chooses.

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

/// Counters the tests read to prove the auth gate and the body limit work:
/// heads seen, body bytes discarded after a rejection, statuses written,
/// connections the server closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub request_heads: u64,
    pub handled_requests: u64,
    pub discarded_body_bytes: u64,
    pub statuses: Vec<u16>,
    pub closed_by_server: u64,
    /// Connections closed for silence past the read timeout (also in
    /// `closed_by_server`).
    pub timed_out: u64,
    /// Connections closed before the handshake for arriving on an address the
    /// service does not live on (also in `closed_by_server`).
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

/// How long [`HandoverServer::stop`] lets connections finish their request
/// before it cuts them.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// How long the accept loop keeps the LAN addresses, so a connection flood
/// costs one interface query a second.
pub const LAN_REFRESH: Duration = Duration::from_secs(1);

/// The LAN addresses a connection is checked against, queried on the
/// blocking pool and kept for `every`. A failed query gives the empty set,
/// which refuses every connection but loopback.
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
    /// Every IPv4 address, each connection checked against `lan`; the record is
    /// published when `publish`.
    Lan { lan: LanAddresses, publish: bool },
}

/// A running listener. [`HandoverServer::stop`] closes it in order; dropping
/// it signals the same and withdraws the record without waiting.
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
    /// The port the Bonjour record carries, `None` when unpublished.
    #[cfg(test)]
    pub(crate) fn advertised_port(&self) -> Option<u16> {
        self.advertiser.as_ref().map(advertise::Advertiser::port)
    }

    /// Binds and starts accepting: every IPv4 interface when advertising (the
    /// phone resolves IPv4 only), otherwise 127.0.0.1.
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

    /// [`HandoverServer::start`] with an explicit reach.
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
        let listener = bind(address, configuration.port).await?;
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

    /// Stops accepting, closes every connection (idle ones at once, busy ones
    /// after their response or after [`STOP_GRACE`] by force) and withdraws the
    /// record; the port is free when it returns. Store writes already in line
    /// still finish, including an admission in flight; if the exit cuts one,
    /// the next launch adopts the copy (at worst a duplicate meeting, never a
    /// lost one).
    pub async fn stop(self) {
        let HandoverServer {
            mut accept_task,
            stop_signal,
            advertiser,
            ..
        } = self;
        stop_signal.send_replace(true);
        // The accept task drains for `STOP_GRACE`; one second more, then abort.
        if tokio::time::timeout(STOP_GRACE + Duration::from_secs(1), &mut accept_task)
            .await
            .is_err()
        {
            accept_task.abort();
            let _ = accept_task.await;
        }
        if let Some(advertiser) = advertiser {
            advertiser.withdraw();
        }
    }
}

/// A listener on `port` at `address`. A fixed port that cannot be bound
/// gives way to a system-chosen one with a warning: Bonjour still finds it,
/// but a firewall opened for the fixed port blocks it.
async fn bind(address: IpAddr, port: u16) -> std::io::Result<TcpListener> {
    let error = match TcpListener::bind(SocketAddr::new(address, port)).await {
        Err(error) if port != 0 => error,
        bound => return bound,
    };
    let listener = TcpListener::bind(SocketAddr::new(address, 0)).await?;
    let chosen = listener.local_addr()?.port();
    tracing::warn!(
        target: "steno::handover",
        "port {port} is not available ({error}), so the phone handover listens on port {chosen} instead, which a firewall opened only for port {port} blocks"
    );
    Ok(listener)
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
        // Stop first, so nothing is accepted once asked; reap finished connections
        // so the set does not grow.
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
            // The handshake gets the read timeout, so a silent client costs the same as
            // one that stops mid-request.
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
    // Nothing new connects while the connections in flight finish.
    drop(listener);
    let drained = async { while connections.join_next().await.is_some() {} };
    let _ = tokio::time::timeout(STOP_GRACE, drained).await;
}

/// Whether `stream` arrived on loopback or on one of `lan`; a connection
/// over a tunnel is not served.
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

    #[tokio::test]
    async fn the_lan_addresses_are_read_once_per_refresh_and_a_failed_read_serves_none() {
        let mut kept = LanAddresses::new(counted, Duration::from_secs(3600));
        assert_eq!(kept.current().await, &[Ipv4Addr::new(192, 168, 1, 20)]);
        assert_eq!(kept.current().await, &[Ipv4Addr::new(192, 168, 1, 20)]);
        assert_eq!(QUERIES.load(Ordering::SeqCst), 1, "one query");
        let mut panicking = LanAddresses::new(|| panic!("the system did not say"), LAN_REFRESH);
        assert!(panicking.current().await.is_empty(), "fails closed");
    }

    /// A non-loopback IPv4 address of this host that delivers a connection to
    /// this process (the macOS firewall can hold one back), or why there is
    /// none. CI sets `STENO_REQUIRE_LAN_TEST` on Linux and Windows, where a
    /// skip fails.
    async fn host_address() -> Result<Ipv4Addr, String> {
        let interfaces = if_addrs::get_if_addrs().unwrap_or_default();
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
        let Some(&(_, address)) = candidates.first() else {
            return Err("this host has no non-loopback IPv4 address".to_owned());
        };
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let both = async {
            let (connected, accepted) =
                tokio::join!(TcpStream::connect((address, port)), listener.accept());
            connected.is_ok() && accepted.is_ok()
        };
        match tokio::time::timeout(Duration::from_secs(3), both).await {
            Ok(true) => Ok(address),
            _ => Err(format!("{address} delivers no inbound connection here")),
        }
    }

    fn skip(reason: &str) {
        assert!(
            std::env::var_os("STENO_REQUIRE_LAN_TEST").is_none(),
            "the interface test must not skip here: {reason}"
        );
        println!("SKIPPED: {reason}");
    }

    fn configuration() -> HandoverConfiguration {
        HandoverConfiguration {
            inbox_directory: std::env::temp_dir().join("steno-handover-server-tests"),
            read_timeout: Duration::from_secs(20),
            advertise: false,
            ..HandoverConfiguration::default()
        }
    }

    fn identity() -> Arc<HandoverIdentity> {
        Arc::new(HandoverIdentity::mint("Steno test", Utc::now()).unwrap())
    }

    /// A listener on every IPv4 address serving loopback and `lan`, read
    /// again after `every`.
    async fn serve(
        configuration: &HandoverConfiguration,
        identity: &Arc<HandoverIdentity>,
        lan: fn() -> Vec<Ipv4Addr>,
        every: Duration,
        publish: bool,
    ) -> (HandoverServer, Arc<ServerMetrics>) {
        let engine = Engine::new(
            configuration.clone(),
            identity.clone(),
            Arc::new(Store::in_memory().unwrap()),
            Arc::new(FakeHandoverIntake::default()),
            watch::channel(Vec::new()).0,
            Arc::new(Utc::now),
        );
        let metrics = Arc::new(ServerMetrics::default());
        let reach = Reach::Lan {
            lan: LanAddresses::new(lan, every),
            publish,
        };
        let server =
            HandoverServer::start_with(configuration, identity, engine, metrics.clone(), reach)
                .await
                .unwrap();
        (server, metrics)
    }

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

    /// `GET /v1/hello` on `tls`, kept alive; true for a 200.
    async fn hello(tls: &mut tokio_rustls::client::TlsStream<TcpStream>) -> bool {
        tls.write_all(b"GET /v1/hello HTTP/1.1\r\nHost: steno\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        let mut buffer = [0u8; 1024];
        // The hello's JSON body ends the response.
        while !response.ends_with(b"}") {
            let read = tls.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0, "closed mid-response");
            response.extend_from_slice(&buffer[..read]);
        }
        response.starts_with(b"HTTP/1.1 200")
    }

    static HOST: OnceLock<Ipv4Addr> = OnceLock::new();

    fn the_host() -> Vec<Ipv4Addr> {
        HOST.get().copied().into_iter().collect()
    }

    #[tokio::test]
    async fn the_lan_is_served_and_a_connection_outside_it_is_closed_before_any_tls_byte() {
        let address = match host_address().await {
            Ok(address) => address,
            Err(reason) => return skip(&reason),
        };
        HOST.set(address).unwrap();
        let identity = identity();

        // Only loopback is served; the host's own address stands in for a tunnel.
        let (outside, metrics) =
            serve(&configuration(), &identity, Vec::new, LAN_REFRESH, false).await;
        let mut refused = TcpStream::connect((address, outside.port)).await.unwrap();
        let mut received = Vec::new();
        let _ = within(refused.read_to_end(&mut received)).await;
        assert!(received.is_empty(), "no TLS byte: {received:?}");
        assert_eq!(metrics.snapshot().refused_interface, 1);
        let mut on_loopback = within(connect(&identity, Ipv4Addr::LOCALHOST, outside.port))
            .await
            .unwrap();
        assert!(within(hello(&mut on_loopback)).await, "loopback is served");
        outside.stop().await;

        let (lan, metrics) = serve(&configuration(), &identity, the_host, LAN_REFRESH, false).await;
        let mut tls = within(connect(&identity, address, lan.port)).await.unwrap();
        assert!(within(hello(&mut tls)).await, "the LAN is served");
        assert_eq!(metrics.snapshot().refused_interface, 0);
        lan.stop().await;
    }

    static JOINING: OnceLock<Ipv4Addr> = OnceLock::new();
    static JOINED: AtomicBool = AtomicBool::new(false);

    fn the_host_once_joined() -> Vec<Ipv4Addr> {
        if JOINED.load(Ordering::SeqCst) {
            JOINING.get().copied().into_iter().collect()
        } else {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn an_address_gained_after_start_is_served_and_the_listener_keeps_an_open_connection_when_it_leaves()
     {
        let address = match host_address().await {
            Ok(address) => address,
            Err(reason) => return skip(&reason),
        };
        JOINING.set(address).unwrap();
        let identity = identity();
        let (server, metrics) = serve(
            &configuration(),
            &identity,
            the_host_once_joined,
            Duration::ZERO,
            false,
        )
        .await;
        assert!(
            within(connect(&identity, address, server.port))
                .await
                .is_err()
        );

        JOINED.store(true, Ordering::SeqCst);
        let mut joined = within(connect(&identity, address, server.port))
            .await
            .expect("a handshake on the gained address");
        assert!(within(hello(&mut joined)).await);

        // The address leaves: the open connection is still answered, a new one
        // is refused.
        JOINED.store(false, Ordering::SeqCst);
        assert!(within(hello(&mut joined)).await);
        assert!(
            within(connect(&identity, address, server.port))
                .await
                .is_err()
        );
        assert_eq!(metrics.snapshot().refused_interface, 2);
        drop(joined);
        server.stop().await;
    }

    /// The Bonjour record carries the port bound, so a phone still finds a
    /// listener whose fixed port was taken.
    #[tokio::test]
    async fn a_fixed_port_another_program_holds_gives_way_and_the_record_carries_the_port_bound() {
        let held = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let taken = held.local_addr().unwrap().port();
        let configuration = HandoverConfiguration {
            service_name: "Steno port test".to_owned(),
            port: taken,
            ..configuration()
        };
        let identity = identity();
        let (server, _) = serve(&configuration, &identity, Vec::new, LAN_REFRESH, true).await;
        assert_ne!(server.port, taken);
        assert_eq!(server.advertised_port(), Some(server.port));
        let mut tls = within(connect(&identity, Ipv4Addr::LOCALHOST, server.port))
            .await
            .unwrap();
        assert!(within(hello(&mut tls)).await);
        drop(tls);
        server.stop().await;
        drop(held);
    }
}
