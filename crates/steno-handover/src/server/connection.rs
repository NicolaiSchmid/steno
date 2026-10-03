//! One accepted connection: HTTP/1.1 over the TLS stream, one request at a
//! time. Per request the route is matched, the auth gate runs on the head
//! (the body is not read while the gate is pending, so an unauthenticated
//! body is never buffered), the body limit is enforced (413 and close), the
//! body is collected and the complete request handed to the engine.
//! Rejections are answered at once with `Connection: close`; what the
//! client still sends is discarded up to a limit, then the connection
//! closes. A body that ends early (a parse error, a client gone mid-body,
//! the read timeout) is not answered: the connection closes, as the Swift
//! handler's `errorCaught` does. A connection that stays silent for the
//! read timeout while the computer waits on the client is closed; the
//! silence is not counted while the engine is handling a request. When the
//! server stops, an idle connection closes at once and one mid-request
//! closes after its response. The drain of a rejected body and the linger
//! after a close run in the connection's own task set, so they end with
//! the connection's slot in the server's set and never outlive `stop`.
//! Swift: `Routing/HTTPHandler.swift`.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use http::{Request, Response, StatusCode, header};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_rustls::server::TlsStream;

use super::ServerMetrics;
use crate::configuration::HandoverConfiguration;
use crate::engine::{AuthOutcome, HandoverRequest, HandoverResponse, RequestHandling};
use crate::route::Route;

/// How long a half-closed connection may linger before it is torn down.
pub const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// How much of a rejected request's body is eaten before the connection
/// closes, so the client reads the status instead of a reset.
pub const REJECTED_BODY_DRAIN: usize = 64 * 1024;

struct Shared {
    engine: Arc<dyn RequestHandling>,
    metrics: Arc<ServerMetrics>,
    configuration: Arc<HandoverConfiguration>,
    /// True from dispatch until the response is written: the silence is the
    /// engine's (a long verify), not the client's, so the read timeout does
    /// not apply.
    handling: AtomicBool,
    /// Set once the server decided to close this connection.
    closing: AtomicBool,
    /// The drain and the linger this connection started; [`serve`] waits
    /// for them, and dropping the set aborts them.
    tasks: Mutex<JoinSet<()>>,
}

impl Shared {
    /// Runs `task` in this connection's set. Nothing runs once the runtime
    /// is gone.
    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            self.tasks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .spawn_on(task, &runtime);
        }
    }
}

/// Serves `tls` until the client goes away, the server closes it, the
/// server stops (`stopping` turns true) or an error ends it; then waits for
/// the drain and the linger, which end on their own within [`CLOSE_GRACE`]
/// or at once when the server stops.
pub async fn serve(
    tls: TlsStream<TcpStream>,
    engine: Arc<dyn RequestHandling>,
    metrics: Arc<ServerMetrics>,
    configuration: Arc<HandoverConfiguration>,
    mut stopping: watch::Receiver<bool>,
) {
    let shared = Arc::new(Shared {
        engine,
        metrics,
        configuration,
        handling: AtomicBool::new(false),
        closing: AtomicBool::new(false),
        tasks: Mutex::new(JoinSet::new()),
    });
    if let Err(error) = run(tls, &shared, &mut stopping).await {
        tracing::debug!(target: "steno::handover", "connection ended: {error}");
    }
    if shared.closing.load(Ordering::SeqCst) {
        shared
            .metrics
            .update(|metrics| metrics.closed_by_server += 1);
    }
    let mut tasks =
        std::mem::take(&mut *shared.tasks.lock().unwrap_or_else(PoisonError::into_inner));
    tokio::select! {
        () = async { while tasks.join_next().await.is_some() {} } => {}
        () = super::stopped(&mut stopping) => {}
    }
}

/// HTTP/1.1 over `tls`; the connection, and with it the stream, is dropped
/// on return, which starts the linger.
async fn run(
    tls: TlsStream<TcpStream>,
    shared: &Arc<Shared>,
    stopping: &mut watch::Receiver<bool>,
) -> hyper::Result<()> {
    let io = TimedStream::new(tls, shared.clone());
    let service = service_fn({
        let shared = shared.clone();
        move |request| {
            let shared = shared.clone();
            async move { handle(shared, request).await }
        }
    });
    let mut connection = std::pin::pin!(
        http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(None)
            .keep_alive(true)
            .serve_connection(TokioIo::new(io), service)
    );
    tokio::select! {
        served = connection.as_mut() => served,
        () = super::stopped(stopping) => {
            // hyper finishes the response in flight, if any, with
            // `Connection: close`, and closes an idle connection now.
            shared.closing.store(true, Ordering::SeqCst);
            connection.as_mut().graceful_shutdown();
            connection.await
        }
    }
}

/// The body ended before its declared end. Returned to hyper as the
/// service's error, which ends the connection without a response.
#[derive(Debug, Error)]
#[error("the request body ended early")]
struct TornBody;

async fn handle(
    shared: Arc<Shared>,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, TornBody> {
    shared.metrics.update(|metrics| metrics.request_heads += 1);
    let (parts, mut body) = request.into_parts();
    let uri = parts
        .uri
        .path_and_query()
        .map_or(parts.uri.path(), http::uri::PathAndQuery::as_str);
    let Some(route) = Route::matches(&parts.method, uri) else {
        return Ok(reject(
            &shared,
            HandoverResponse::problem(StatusCode::NOT_FOUND, "no such route"),
            body,
        ));
    };
    let limit = usize::try_from(route.body_limit(&shared.configuration)).unwrap_or(usize::MAX);
    let declared = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<usize>().ok());
    if declared.is_some_and(|declared| declared > limit) {
        return Ok(respond_and_close(&shared, too_large(limit)));
    }
    let authorization = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let principal = match shared.engine.authenticate(route, authorization).await {
        AuthOutcome::Allowed(principal) => principal,
        AuthOutcome::Rejected(response) => return Ok(reject(&shared, response, body)),
    };

    let mut collected = BytesMut::new();
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            shared.closing.store(true, Ordering::SeqCst);
            return Err(TornBody);
        };
        if let Ok(data) = frame.into_data() {
            collected.extend_from_slice(&data);
            if collected.len() > limit {
                return Ok(respond_and_close(&shared, too_large(limit)));
            }
        }
    }

    let keep_alive = parts.version != http::Version::HTTP_10
        && !parts
            .headers
            .get(header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("close"));
    shared.handling.store(true, Ordering::SeqCst);
    shared
        .metrics
        .update(|metrics| metrics.handled_requests += 1);
    let request = HandoverRequest {
        route,
        principal,
        headers: parts.headers,
        body: collected.freeze(),
    };
    let response = shared.engine.handle(request).await;
    if !keep_alive {
        shared.closing.store(true, Ordering::SeqCst);
    }
    Ok(respond(&shared, response, !keep_alive))
}

fn too_large(limit: usize) -> HandoverResponse {
    HandoverResponse::problem(
        StatusCode::PAYLOAD_TOO_LARGE,
        format!("body limit is {limit} bytes"),
    )
}

/// Answers now, then discards whatever body still arrives (up to
/// [`REJECTED_BODY_DRAIN`]) so the client reads the status instead of a
/// reset, then closes.
fn reject(
    shared: &Shared,
    response: HandoverResponse,
    mut body: Incoming,
) -> Response<Full<Bytes>> {
    // The metrics only: the set holding this task lives in `shared`.
    let metrics = shared.metrics.clone();
    shared.spawn(async move {
        let mut remaining = REJECTED_BODY_DRAIN;
        while let Some(Ok(frame)) = body.frame().await {
            if let Ok(data) = frame.into_data() {
                metrics.update(|metrics| metrics.discarded_body_bytes += data.len() as u64);
                if data.len() > remaining {
                    break;
                }
                remaining -= data.len();
            }
        }
    });
    respond_and_close(shared, response)
}

/// Anything that still arrives while the response flushes is dropped; the
/// connection closes once the response is out.
fn respond_and_close(shared: &Shared, response: HandoverResponse) -> Response<Full<Bytes>> {
    shared.closing.store(true, Ordering::SeqCst);
    respond(shared, response, true)
}

fn respond(shared: &Shared, response: HandoverResponse, close: bool) -> Response<Full<Bytes>> {
    shared.handling.store(false, Ordering::SeqCst);
    shared
        .metrics
        .update(|metrics| metrics.statuses.push(response.status.as_u16()));
    let mut builder = Response::builder()
        .status(response.status)
        .header(header::CONTENT_LENGTH, response.body.len());
    for (name, value) in &response.headers {
        builder = builder.header(*name, value);
    }
    if close {
        builder = builder.header(header::CONNECTION, "close");
    }
    builder
        .body(Full::new(response.body))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

/// The TLS stream with the read timeout: a `Sleep` armed when the server
/// waits for the client and cleared by every byte that arrives; not armed
/// while the engine is handling. When the server decided to close, the
/// shutdown half-closes the output and the stream lingers for
/// [`CLOSE_GRACE`] (FIN after the data, so the client reads the status
/// instead of a reset) before it is dropped.
struct TimedStream {
    inner: Option<TlsStream<TcpStream>>,
    shared: Arc<Shared>,
    sleep: Pin<Box<tokio::time::Sleep>>,
    armed: bool,
    shut_down: bool,
}

impl TimedStream {
    fn new(inner: TlsStream<TcpStream>, shared: Arc<Shared>) -> Self {
        TimedStream {
            inner: Some(inner),
            shared,
            sleep: Box::pin(tokio::time::sleep(Duration::from_secs(0))),
            armed: false,
            shut_down: false,
        }
    }

    fn inner(&mut self) -> Pin<&mut TlsStream<TcpStream>> {
        Pin::new(self.inner.as_mut().expect("the stream lives until drop"))
    }
}

impl AsyncRead for TimedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        match self.inner().poll_read(cx, buf) {
            Poll::Ready(result) => {
                if buf.filled().len() > before {
                    self.armed = false;
                }
                Poll::Ready(result)
            }
            Poll::Pending => {
                if self.shared.handling.load(Ordering::SeqCst) {
                    self.armed = false;
                    return Poll::Pending;
                }
                if !self.armed {
                    let deadline = Instant::now() + self.shared.configuration.read_timeout;
                    self.sleep.as_mut().reset(deadline.into());
                    self.armed = true;
                }
                match self.sleep.as_mut().poll(cx) {
                    Poll::Ready(()) => {
                        self.shared.metrics.update(|metrics| metrics.timed_out += 1);
                        self.shared.closing.store(true, Ordering::SeqCst);
                        Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "the client stayed silent past the read timeout",
                        )))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

impl AsyncWrite for TimedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.inner().poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.inner().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let result = self.inner().poll_shutdown(cx);
        if result.is_ready() {
            self.shut_down = true;
        }
        result
    }
}

impl Drop for TimedStream {
    fn drop(&mut self) {
        let Some(stream) = self.inner.take() else {
            return;
        };
        if !self.shut_down {
            return;
        }
        // The output is half-closed (tokio-rustls sent close_notify and
        // shut the write side); keep the input open for the grace period,
        // reading and discarding what the client still sends, so the close
        // that follows carries no unread data and sends no reset.
        self.shared.spawn(async move {
            let mut stream = stream;
            let mut sink = vec![0u8; 16 * 1024];
            let linger = async {
                loop {
                    match tokio::io::AsyncReadExt::read(&mut stream, &mut sink).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            };
            let _ = tokio::time::timeout(CLOSE_GRACE, linger).await;
        });
    }
}
