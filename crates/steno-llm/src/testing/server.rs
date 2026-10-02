//! A loopback HTTP/1.1 server for tests.
//! Swift: `Sources/StenoLLM/Testing/StubChatServer.swift`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use url::Url;

use super::Responder;
use crate::wire::{self, ChatCompletionRequest, ChatErrorEnvelope, ResponsesRequest};

/// One request the stub server accepted, parsed far enough for assertions.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// Zero-based arrival order.
    pub index: usize,
    pub method: String,
    /// Path and query as sent.
    pub path: String,
    /// Header names lowercased.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    /// The body decoded as a chat completion request, when it is one.
    pub chat: Option<ChatCompletionRequest>,
    /// The body decoded as a Responses API request, when it is one.
    pub responses: Option<ResponsesRequest>,
    /// The `X-Steno-Purpose` header the client sends with every completion.
    pub purpose: Option<String>,
    /// Requests in flight (including this one) when it arrived.
    pub in_flight_on_arrival: usize,
}

impl RecordedRequest {
    #[must_use]
    pub fn authorization(&self) -> Option<&str> {
        self.headers.get("authorization").map(String::as_str)
    }

    #[must_use]
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behaviour {
    /// Write status, headers and body, then close.
    Respond,
    /// Keep the connection open without answering until `stop()`; for
    /// timeout and cancellation tests.
    Hang,
    /// Close the connection without writing anything; a transport error.
    Drop,
}

/// What the stub server answers one request with.
#[derive(Debug, Clone)]
pub struct StubResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub behaviour: Behaviour,
}

impl StubResponse {
    #[must_use]
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        StubResponse {
            status,
            headers: Vec::new(),
            body,
            behaviour: Behaviour::Respond,
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// `value` as JSON with `Content-Type: application/json`.
    #[must_use]
    pub fn json<T: Serialize>(value: &T, status: u16) -> Self {
        StubResponse::new(status, wire::encode(value).unwrap_or_default())
            .with_header("Content-Type", "application/json")
    }

    #[must_use]
    pub fn hang() -> Self {
        StubResponse {
            behaviour: Behaviour::Hang,
            ..StubResponse::new(200, Vec::new())
        }
    }

    #[must_use]
    pub fn drop_connection() -> Self {
        StubResponse {
            behaviour: Behaviour::Drop,
            ..StubResponse::new(200, Vec::new())
        }
    }
}

#[derive(Default)]
struct State {
    queue: VecDeque<StubResponse>,
    responder: Option<Responder>,
    requests: Vec<RecordedRequest>,
    in_flight: usize,
    max_in_flight: usize,
    holding: bool,
    stopped: bool,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    /// Fired on every recorded request and on stop.
    requests_changed: Notify,
    /// Fired on release and on stop.
    hold_changed: Notify,
}

impl Inner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A loopback HTTP/1.1 server for tests: ephemeral port on 127.0.0.1,
/// scripted responses consumed in arrival order, a fallback responder for
/// path-dependent answers, every request recorded with the number of
/// requests in flight on arrival, and a latch that holds responses so tests
/// can observe concurrency. One tokio task per connection; no wall-clock
/// waits anywhere a test can see.
pub struct StubChatServer {
    inner: Arc<Inner>,
    port: u16,
    base_url: Url,
    accept_loop: tokio::task::JoinHandle<()>,
}

impl StubChatServer {
    /// Binds an ephemeral port and starts accepting.
    pub async fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let base_url = Url::parse(&format!("http://127.0.0.1:{port}/v1")).expect("a valid URL");
        let inner = Arc::new(Inner::default());
        let accept_inner = Arc::clone(&inner);
        let accept_loop = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let inner = Arc::clone(&accept_inner);
                tokio::spawn(async move { serve(inner, stream).await });
            }
        });
        Ok(StubChatServer {
            inner,
            port,
            base_url,
            accept_loop,
        })
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `http://127.0.0.1:<port>/v1`.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    // Scripting

    /// Appends responses; the queue is consumed in arrival order.
    pub fn enqueue(&self, responses: impl IntoIterator<Item = StubResponse>) {
        self.inner.state().queue.extend(responses);
    }

    /// Consulted when the queue is empty; `None` falls through to a 404.
    pub fn respond(&self, responder: Responder) {
        self.inner.state().responder = Some(responder);
    }

    /// While held, every request is recorded and then parked until
    /// [`release`](Self::release); in-flight counts stay observable
    /// meanwhile.
    pub fn hold_responses(&self) {
        self.inner.state().holding = true;
    }

    pub fn release(&self) {
        self.inner.state().holding = false;
        self.inner.hold_changed.notify_waiters();
    }

    // Observation

    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.inner.state().requests.clone()
    }

    #[must_use]
    pub fn request_count(&self) -> usize {
        self.inner.state().requests.len()
    }

    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.inner.state().in_flight
    }

    /// The largest number of simultaneously open requests so far.
    #[must_use]
    pub fn max_in_flight(&self) -> usize {
        self.inner.state().max_in_flight
    }

    /// Waits until at least `count` requests have been recorded, or the
    /// server was stopped. Never polls.
    pub async fn received(&self, count: usize) {
        super::wait_until(&self.inner.requests_changed, || {
            let state = self.inner.state();
            state.requests.len() >= count || state.stopped
        })
        .await;
    }

    /// Stops accepting, releases every parked or hanging connection and
    /// wakes every waiter. Idempotent.
    pub fn stop(&self) {
        {
            let mut state = self.inner.state();
            state.stopped = true;
            state.holding = false;
        }
        self.accept_loop.abort();
        self.inner.hold_changed.notify_waiters();
        self.inner.requests_changed.notify_waiters();
    }
}

impl Drop for StubChatServer {
    fn drop(&mut self) {
        self.stop();
    }
}

struct RawRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

async fn serve(inner: Arc<Inner>, mut stream: TcpStream) {
    let Some(raw) = read_request(&mut stream).await else {
        return;
    };
    let request = record(&inner, raw);
    let response = {
        let mut state = inner.state();
        if let Some(scripted) = state.queue.pop_front() {
            scripted
        } else if let Some(scripted) = state
            .responder
            .as_ref()
            .and_then(|responder| responder(&request))
        {
            scripted
        } else {
            StubResponse::json(
                &ChatErrorEnvelope::message(&format!("no scripted response for {}", request.path)),
                404,
            )
        }
    };
    wait_while(&inner, |state| state.holding && !state.stopped).await;
    match response.behaviour {
        Behaviour::Drop => {
            finish(&inner);
        }
        Behaviour::Hang => {
            wait_while(&inner, |state| !state.stopped).await;
            finish(&inner);
        }
        Behaviour::Respond => {
            finish(&inner);
            let bytes = serialize(&response);
            let _ = stream.write_all(&bytes).await;
            let _ = stream.shutdown().await;
        }
    }
}

async fn wait_while(inner: &Inner, condition: impl Fn(&State) -> bool) {
    super::wait_until(&inner.hold_changed, || !condition(&inner.state())).await;
}

fn record(inner: &Inner, raw: RawRequest) -> RecordedRequest {
    let request = {
        let mut state = inner.state();
        state.in_flight += 1;
        state.max_in_flight = state.max_in_flight.max(state.in_flight);
        let request = RecordedRequest {
            index: state.requests.len(),
            method: raw.method,
            path: raw.path,
            chat: wire::decode(&raw.body).ok(),
            responses: wire::decode(&raw.body).ok(),
            purpose: raw.headers.get("x-steno-purpose").cloned(),
            headers: raw.headers,
            body: raw.body,
            in_flight_on_arrival: state.in_flight,
        };
        state.requests.push(request.clone());
        request
    };
    inner.requests_changed.notify_waiters();
    request
}

fn finish(inner: &Inner) {
    let mut state = inner.state();
    state.in_flight = state.in_flight.saturating_sub(1);
}

async fn read_request(stream: &mut TcpStream) -> Option<RawRequest> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 16_384];
    let header_end = loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position;
        }
        let count = stream.read(&mut chunk).await.ok()?;
        if count == 0 || buffer.len() > 1_048_576 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..count]);
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = header_text.split("\r\n");
    let request_line: Vec<&str> = lines.next()?.split_whitespace().collect();
    if request_line.len() < 2 {
        return None;
    }
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_owned());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let count = stream.read(&mut chunk).await.ok()?;
        if count == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..count]);
    }
    body.truncate(content_length);
    Some(RawRequest {
        method: request_line[0].to_owned(),
        path: request_line[1].to_owned(),
        headers,
        body,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn serialize(response: &StubResponse) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    );
    let mut headers: Vec<(String, String)> = response.headers.clone();
    headers.push(("Content-Length".to_owned(), response.body.len().to_string()));
    headers.push(("Connection".to_owned(), "close".to_owned()));
    headers.sort();
    for (name, value) in headers {
        use std::fmt::Write;
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(&response.body);
    bytes
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}
