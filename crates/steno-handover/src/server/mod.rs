//! The listener: a TCP acceptor that terminates TLS 1.3 with the identity,
//! runs HTTP/1.1 over each connection ([`connection`]) and, when
//! advertising, publishes `_steno._tcp` with the TXT record (`v=1`,
//! `id=<macID>`) through Bonjour ([`advertise`]). Loopback only when
//! `advertise` is false. Swift: `Network/HandoverServer.swift`,
//! `Network/ServerMetrics.swift`.

pub mod advertise;
pub mod connection;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

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

/// A running listener. [`HandoverServer::stop`] closes it in order;
/// dropping it sends the same signal without waiting.
pub struct HandoverServer {
    pub port: u16,
    accept_task: JoinHandle<()>,
    shutdown: watch::Sender<bool>,
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
    /// (the phone resolves IPv4 only); otherwise 127.0.0.1.
    pub async fn start(
        configuration: &HandoverConfiguration,
        identity: &HandoverIdentity,
        engine: Arc<dyn RequestHandling>,
        metrics: Arc<ServerMetrics>,
    ) -> Result<HandoverServer, ServerError> {
        let acceptor = TlsAcceptor::from(identity.server_config()?);
        let address = if configuration.advertise {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        let listener = TcpListener::bind(SocketAddr::new(address, configuration.port)).await?;
        let port = listener.local_addr()?.port();
        if port == 0 {
            return Err(ServerError::NoPort);
        }
        let advertiser = if configuration.advertise {
            Some(advertise::Advertiser::publish(
                &configuration.service_name,
                identity.mac_id(),
                port,
            )?)
        } else {
            None
        };
        let configuration = Arc::new(configuration.clone());
        let (shutdown, stopping) = watch::channel(false);
        let accept_task = tokio::spawn(accept_loop(
            listener,
            acceptor,
            engine,
            metrics,
            configuration,
            stopping,
        ));
        Ok(HandoverServer {
            port,
            accept_task,
            shutdown,
            advertiser,
        })
    }

    /// Stops accepting, closes every connection (an idle one at once, one
    /// mid-request after its response, any still busy after
    /// [`STOP_GRACE`] by force) and withdraws the Bonjour record. Swift:
    /// `group.shutdownGracefully()` closes the child channels.
    pub async fn stop(self) {
        let HandoverServer {
            mut accept_task,
            shutdown,
            advertiser,
            ..
        } = self;
        shutdown.send_replace(true);
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
    mut stopping: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            () = stopped(&mut stopping) => break,
            // Reap finished connections so the set does not grow for the
            // life of the listener.
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
        };
        let (stream, _) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(target: "steno::handover", "accept failed: {error}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
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
