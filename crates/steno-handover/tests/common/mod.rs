//! One `HandoverService` on loopback with a minted identity, an in-memory
//! store, an intake that commits like the real one, a settable clock and a
//! temporary inbox; and the phone as the computer sees it, over pinned TLS.

#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::large_futures
)]

use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, TimeZone as _, Utc};
pub use steno_core::content_hash::sha256;
use steno_core::{
    AudioAsset, AudioFormat, AudioLane, AudioRetention, BoundaryResult, HandoverIntake,
    HandoverState, PairedDevice, RecordingMetadata, Store,
};
use steno_handover::pinning::pinned_client_config;
use steno_handover::server::MetricsSnapshot;
use steno_handover::{
    HandoverConfiguration, HandoverIdentity, HandoverService, ListenerState, wire,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use uuid::Uuid;

pub const START: i64 = 1_790_000_000;

pub fn date(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().unwrap()
}

/// The service's one time source; tests move it.
#[derive(Clone)]
pub struct Clock(Arc<Mutex<DateTime<Utc>>>);

impl Clock {
    pub fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }

    pub fn advance(&self, duration: Duration) {
        *self.0.lock().unwrap() += chrono::Duration::from_std(duration).unwrap();
    }

    fn source(&self) -> steno_handover::Clock {
        let clock = self.clone();
        Arc::new(move || clock.now())
    }
}

/// Commits like the real intake (receipt, meeting and asset in one durable
/// write, so the admission ledger holds the bytes), after refusing the first
/// `failures` admissions. Keeps the bytes of every file it was handed.
pub struct Intake {
    store: Arc<Store>,
    failures: Mutex<u32>,
    delay: Duration,
    pub files: Mutex<Vec<Vec<u8>>>,
    pub meetings: Mutex<Vec<Uuid>>,
}

#[steno_core::async_trait]
impl HandoverIntake for Intake {
    async fn admit(
        &self,
        file: &Path,
        metadata: &RecordingMetadata,
        _device: &PairedDevice,
    ) -> BoundaryResult<Uuid> {
        self.files.lock().unwrap().push(std::fs::read(file)?);
        tokio::time::sleep(self.delay).await;
        {
            let mut failures = self.failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err(format!("the pipeline refused {}", file.display()).into());
            }
        }
        let mut receipt = self
            .store
            .handover_receipt(metadata.recording_id)?
            .ok_or("no receipt")?;
        let meeting = steno_core::Meeting {
            id: Uuid::new_v4(),
            ..steno_core::testing::sample_data::meeting()
        };
        let asset = AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: format!("file:///tmp/Audio/{}/master.m4a", meeting.id),
            format: metadata.format,
            lanes: vec![AudioLane::Mixed],
            sidecars_16k: std::collections::BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::KeepForever,
            expires_at: None,
        };
        receipt.state = HandoverState::Complete {
            meeting_id: meeting.id,
        };
        let admitted = self
            .store
            .save_admission_durably(&receipt, &meeting, &asset)?;
        self.meetings.lock().unwrap().push(admitted);
        Ok(admitted)
    }
}

pub struct Options {
    pub chunk_size: i64,
    /// Admissions the intake refuses before it admits.
    pub failures: u32,
    /// How long each admission takes.
    pub delay: Duration,
    pub read_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            chunk_size: 1024 * 1024,
            failures: 0,
            delay: Duration::ZERO,
            read_timeout: Duration::from_secs(30),
        }
    }
}

pub struct TestService {
    pub service: Arc<HandoverService>,
    pub store: Arc<Store>,
    pub intake: Arc<Intake>,
    pub directory: Arc<tempfile::TempDir>,
    pub clock: Clock,
}

impl TestService {
    pub async fn start() -> TestService {
        Self::with(Options::default()).await
    }

    pub async fn with_chunk_size(chunk_size: i64) -> TestService {
        Self::with(Options {
            chunk_size,
            ..Options::default()
        })
        .await
    }

    pub async fn with(options: Options) -> TestService {
        let clock = Clock(Arc::new(Mutex::new(date(START))));
        let identity =
            Arc::new(HandoverIdentity::mint("Steno test identity", clock.now()).unwrap());
        Self::over(
            Arc::new(Store::in_memory().unwrap()),
            Arc::new(tempfile::tempdir().unwrap()),
            identity,
            clock,
            &options,
        )
        .await
    }

    async fn over(
        store: Arc<Store>,
        directory: Arc<tempfile::TempDir>,
        identity: Arc<HandoverIdentity>,
        clock: Clock,
        options: &Options,
    ) -> TestService {
        let intake = Arc::new(Intake {
            store: store.clone(),
            failures: Mutex::new(options.failures),
            delay: options.delay,
            files: Mutex::default(),
            meetings: Mutex::default(),
        });
        let configuration = HandoverConfiguration {
            service_name: "Test Mac".to_owned(),
            advertise: false,
            chunk_size: options.chunk_size,
            inbox_directory: directory.path().join("inbox"),
            pairing_window: Duration::from_secs(300),
            port: 0,
            read_timeout: options.read_timeout,
        };
        let service = Arc::new(HandoverService::new(
            configuration,
            store.clone(),
            intake.clone(),
            identity,
            clock.source(),
        ));
        service.start().await.unwrap();
        TestService {
            service,
            store,
            intake,
            directory,
            clock,
        }
    }

    /// The computer after a restart: the same store, inbox, identity and
    /// clock, a new service and an intake that admits.
    pub async fn restart(&self, chunk_size: i64) -> TestService {
        self.stop().await;
        Self::over(
            self.store.clone(),
            self.directory.clone(),
            self.service.identity.clone(),
            self.clock.clone(),
            &Options {
                chunk_size,
                ..Options::default()
            },
        )
        .await
    }

    pub async fn stop(&self) {
        self.service.stop().await;
    }

    pub fn port(&self) -> u16 {
        match self.service.state() {
            ListenerState::Listening { port } => port,
            other => panic!("not listening: {other:?}"),
        }
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        self.service.metrics.snapshot()
    }

    pub fn fingerprint(&self) -> Vec<u8> {
        self.service.identity.fingerprint().to_vec()
    }

    pub fn client(&self) -> LoopbackClient {
        LoopbackClient::new(self.port(), &self.fingerprint())
    }

    pub fn raw_client(&self) -> RawClient {
        RawClient {
            port: self.port(),
            fingerprint: self.fingerprint(),
        }
    }

    pub fn inbox(&self) -> &steno_handover::upload::Inbox {
        &self.service.engine.inbox
    }
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&self.body)))
    }
}

/// The phone's HTTP client, pinning `fingerprint`.
#[derive(Clone)]
pub struct LoopbackClient {
    base: String,
    client: reqwest::Client,
}

impl LoopbackClient {
    pub fn new(port: u16, fingerprint: &[u8]) -> Self {
        let tls = pinned_client_config(fingerprint).unwrap();
        let client = reqwest::Client::builder()
            .use_preconfigured_tls((*tls).clone())
            .timeout(Duration::from_secs(15))
            .no_proxy()
            .build()
            .unwrap();
        LoopbackClient {
            base: format!("https://127.0.0.1:{port}"),
            client,
        }
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Result<Response, reqwest::Error> {
        let mut request = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", self.base));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?.to_vec();
        Ok(Response { status, body })
    }

    pub async fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Response {
        self.request(method, path, headers, body).await.unwrap()
    }
}

pub fn pairing(secret: &[u8]) -> String {
    format!("Pairing {}", STANDARD.encode(secret))
}

/// One response read off a raw connection.
#[derive(Debug, Default)]
pub struct Exchange {
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub closed_by_server: bool,
}

impl Exchange {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub type ClientStream = tokio_rustls::client::TlsStream<TcpStream>;

/// A pinned TLS connection that writes exactly the bytes it is given.
pub struct RawClient {
    pub port: u16,
    pub fingerprint: Vec<u8>,
}

impl RawClient {
    pub async fn connect(&self) -> std::io::Result<ClientStream> {
        let tcp = TcpStream::connect(("127.0.0.1", self.port)).await?;
        let connector = TlsConnector::from(pinned_client_config(&self.fingerprint).unwrap());
        let name = rustls_pki_types::ServerName::try_from("steno.local").unwrap();
        connector.connect(name, tcp).await
    }

    /// Sends the head and `body` as given, reads one response for up to
    /// `timeout`, then waits up to `close_grace` for the server's close.
    pub async fn exchange(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &[u8],
        close_grace: Duration,
        timeout: Duration,
    ) -> std::io::Result<Exchange> {
        let mut stream = self.connect().await?;
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
        for (name, value) in headers {
            let _ = write!(head, "{name}: {value}\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body).await?;
        stream.flush().await?;
        let mut received = Vec::new();
        let complete = |bytes: &[u8]| parse_response(bytes).is_some();
        let mut closed = read_until(&mut stream, timeout, &mut received, complete).await;
        if !closed {
            closed = read_until(&mut stream, close_grace, &mut received, |_| false).await;
        }
        let mut exchange = parse_response(&received).unwrap_or_default();
        exchange.closed_by_server = closed;
        Ok(exchange)
    }
}

/// Appends what arrives to `received` until `done` (false), a close (true)
/// or `timeout` (false).
pub async fn read_until(
    stream: &mut ClientStream,
    timeout: Duration,
    received: &mut Vec<u8>,
    done: impl Fn(&[u8]) -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.read(&mut buffer)).await {
            Ok(Ok(0) | Err(_)) => return true,
            Ok(Ok(count)) => {
                received.extend_from_slice(&buffer[..count]);
                if done(received) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
}

/// One complete HTTP/1.1 response, or `None` while it is incomplete.
pub fn parse_response(bytes: &[u8]) -> Option<Exchange> {
    let end = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&bytes[..end]).ok()?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let body = &bytes[end + 4..];
    (body.len() >= length).then(|| Exchange {
        status: Some(status),
        headers,
        body: body[..length].to_vec(),
        closed_by_server: false,
    })
}

/// A paired phone.
pub struct Phone {
    pub client: LoopbackClient,
    pub token: String,
    pub device_id: Uuid,
}

impl Phone {
    /// `POST /v1/pair` with `secret`, whatever the computer answers.
    pub async fn try_pair(
        test: &TestService,
        secret: &[u8],
        device_id: Uuid,
        device_name: &str,
    ) -> Response {
        let body = wire::PairRequest {
            device_id,
            device_name: device_name.to_owned(),
        };
        test.client()
            .send(
                "POST",
                "/v1/pair",
                &[("Authorization", &pairing(secret))],
                Some(serde_json::to_vec(&body).unwrap()),
            )
            .await
    }

    /// Opens a window and pairs a new phone.
    pub async fn pair(test: &TestService) -> Phone {
        Self::pair_as(test, Uuid::new_v4()).await
    }

    /// Opens a window and pairs `device_id`.
    pub async fn pair_as(test: &TestService, device_id: Uuid) -> Phone {
        let payload = test.service.begin_pairing();
        let response = Self::try_pair(test, &payload.secret, device_id, "Test iPhone").await;
        assert_eq!(response.status, 200, "pairing failed");
        Phone {
            client: test.client(),
            token: response.json::<wire::PairResponse>().token,
            device_id,
        }
    }

    /// The same phone talking to `test`, e.g. after a restart.
    pub fn to(&self, test: &TestService) -> Phone {
        Phone {
            client: test.client(),
            token: self.token.clone(),
            device_id: self.device_id,
        }
    }

    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    async fn call(&self, method: &str, path: &str, body: Option<Vec<u8>>) -> Response {
        self.client
            .send(method, path, &[("Authorization", &self.bearer())], body)
            .await
    }

    pub async fn announce(&self, metadata: &RecordingMetadata) -> Response {
        let body = serde_json::to_vec(metadata).unwrap();
        let path = format!("/v1/recordings/{}", metadata.recording_id);
        self.call("PUT", &path, Some(body)).await
    }

    pub async fn status(&self, recording_id: Uuid) -> Response {
        self.call("GET", &format!("/v1/recordings/{recording_id}"), None)
            .await
    }

    pub async fn status_of(&self, recording_id: Uuid) -> wire::RecordingStatus {
        let response = self.status(recording_id).await;
        assert_eq!(response.status, 200);
        response.json()
    }

    pub async fn upload(&self, recording_id: Uuid, index: i64, bytes: &[u8]) -> Response {
        self.upload_with(
            recording_id,
            index,
            bytes,
            Some(&STANDARD.encode(sha256(bytes))),
        )
        .await
    }

    /// One chunk with `hash` as its hash header, or none.
    pub async fn upload_with(
        &self,
        recording_id: Uuid,
        index: i64,
        bytes: &[u8],
        hash: Option<&str>,
    ) -> Response {
        let authorization = self.bearer();
        let mut headers = vec![("Authorization", authorization.as_str())];
        if let Some(hash) = hash {
            headers.push((wire::CHUNK_HASH_HEADER, hash));
        }
        let path = format!("/v1/recordings/{recording_id}/chunks/{index}");
        self.client
            .send("PUT", &path, &headers, Some(bytes.to_vec()))
            .await
    }

    pub async fn complete(&self, recording_id: Uuid) -> Response {
        let path = format!("/v1/recordings/{recording_id}/complete");
        self.call("POST", &path, None).await
    }

    /// The meeting a `complete` answered with 200.
    pub async fn completed(&self, recording_id: Uuid) -> Uuid {
        let response = self.complete(recording_id).await;
        assert_eq!(response.status, 200, "complete");
        response.json::<wire::CompleteResponse>().meeting_id
    }

    pub async fn unpair(&self) -> Response {
        self.call("DELETE", "/v1/pairing", None).await
    }

    /// Announces `metadata` and uploads every chunk of `bytes`.
    pub async fn upload_all(&self, metadata: &RecordingMetadata, bytes: &[u8]) {
        let announced = self.announce(metadata).await;
        assert!([200, 201].contains(&announced.status), "announce");
        for (index, chunk) in chunks(bytes, metadata.chunk_size).iter().enumerate() {
            let response = self
                .upload(metadata.recording_id, index as i64, chunk)
                .await;
            assert_eq!(response.status, 204, "chunk {index}");
        }
    }
}

/// The metadata the phone announces for `bytes`.
pub fn metadata_for(bytes: &[u8], chunk_size: i64) -> RecordingMetadata {
    RecordingMetadata {
        recording_id: Uuid::new_v4(),
        started_at: date(1_789_990_000),
        duration_seconds: 61.5,
        byte_count: bytes.len() as i64,
        sha256: sha256(bytes),
        chunk_size,
        format: AudioFormat::M4aAac,
        device_name: "Test iPhone".to_owned(),
    }
}

pub fn chunks(bytes: &[u8], size: i64) -> Vec<Vec<u8>> {
    bytes
        .chunks(usize::try_from(size).unwrap())
        .map(<[u8]>::to_vec)
        .collect()
}

/// Deterministic pseudo-random bytes (`SplitMix64`).
pub fn seeded_bytes(count: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut data = Vec::with_capacity(count + 8);
    while data.len() < count {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        data.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    data.truncate(count);
    data
}

/// Runs `sql` on the store's connection, e.g. a temporary table that makes
/// reads fail.
pub fn execute_batch(store: &Store, sql: &str) {
    store
        .write(|transaction| Ok(transaction.execute_batch(sql)?))
        .unwrap();
}
