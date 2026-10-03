//! One `HandoverService` on loopback with a minted identity, an in-memory
//! store, a fake intake, a wall clock the tests advance and a fresh
//! temporary inbox. `advertise` is always false: nothing leaves 127.0.0.1.
//! The clients pin the listener's fingerprint through `pinning`, the Rust
//! form of the phone's `PinnedTrustEvaluator.swift`.

#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures
)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, TimeZone as _, Utc};
use sha2::{Digest as _, Sha256};
use steno_core::testing::FakeHandoverIntake;
use steno_core::{
    AudioFormat, BoundaryResult, HandoverIntake, PairedDevice, RecordingMetadata, Store,
};
use steno_handover::engine::{HandoverRequest, HandoverResponse, Principal, RequestHandling as _};
use steno_handover::pinning::pinned_client_config;
use steno_handover::route::Route;
use steno_handover::server::MetricsSnapshot;
use steno_handover::{
    HandoverConfiguration, HandoverIdentity, HandoverService, ListenerState, wire,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use uuid::Uuid;

pub const START: i64 = 1_790_000_000;

/// The one time source the service reads; tests move it to expire a
/// pairing window or age a receipt.
#[derive(Clone)]
pub struct WallClock(Arc<Mutex<DateTime<Utc>>>);

impl WallClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        WallClock(Arc::new(Mutex::new(start)))
    }

    pub fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }

    pub fn advance(&self, duration: Duration) {
        *self.0.lock().unwrap() += chrono::Duration::from_std(duration).unwrap();
    }

    pub fn clock(&self) -> steno_handover::Clock {
        let clock = self.clone();
        Arc::new(move || clock.now())
    }
}

pub fn date(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().unwrap()
}

/// Core's fake intake, answering `meeting_id` for every admission.
pub fn fake_intake(meeting_id: Uuid) -> Arc<FakeHandoverIntake> {
    Arc::new(FakeHandoverIntake {
        meeting_id: Some(meeting_id),
        ..FakeHandoverIntake::default()
    })
}

/// A `HandoverIntake` that fails the first `failures` admissions and then
/// returns `meeting_id`: the pipeline refusing a file once. `delay` holds
/// each admission open, the way the real intake's copy of a large file
/// does; `admit_once` refuses every admission after the first successful
/// one, the way the real intake fails when a second copy races the first
/// one's removal of the source.
pub struct ScriptedIntake {
    pub meeting_id: Uuid,
    pub delay: Duration,
    pub admit_once: bool,
    pub admissions: Mutex<Vec<PathBuf>>,
    failures_left: Mutex<u32>,
    admitted: Mutex<bool>,
}

/// Carries the file path, as an I/O error from the real intake's copy
/// would; the computer must not echo it to the phone or into the receipt.
#[derive(Debug)]
pub struct Refused(pub PathBuf);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the pipeline refused {}", self.0.display())
    }
}

impl std::error::Error for Refused {}

impl ScriptedIntake {
    pub fn new(meeting_id: Uuid, failures: u32) -> Arc<Self> {
        Self::with_delay(meeting_id, failures, Duration::ZERO, false)
    }

    pub fn with_delay(
        meeting_id: Uuid,
        failures: u32,
        delay: Duration,
        admit_once: bool,
    ) -> Arc<Self> {
        Arc::new(ScriptedIntake {
            meeting_id,
            delay,
            admit_once,
            admissions: Mutex::new(Vec::new()),
            failures_left: Mutex::new(failures),
            admitted: Mutex::new(false),
        })
    }

    pub fn entries(&self) -> Vec<PathBuf> {
        self.admissions.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.admissions.lock().unwrap().len()
    }
}

#[steno_core::async_trait]
impl HandoverIntake for ScriptedIntake {
    async fn admit(
        &self,
        file: &Path,
        _metadata: &RecordingMetadata,
        _device: &PairedDevice,
    ) -> BoundaryResult<Uuid> {
        self.admissions.lock().unwrap().push(file.to_path_buf());
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let refuse = {
            let mut left = self.failures_left.lock().unwrap();
            if *left > 0 {
                *left -= 1;
                true
            } else {
                false
            }
        };
        if refuse {
            return Err(Box::new(Refused(file.to_path_buf())));
        }
        let repeated = {
            let mut done = self.admitted.lock().unwrap();
            let was = *done;
            *done = true;
            was
        };
        if self.admit_once && repeated {
            return Err(Box::new(Refused(file.to_path_buf())));
        }
        Ok(self.meeting_id)
    }
}

pub struct Options {
    pub chunk_size: i64,
    pub intake: Option<Arc<dyn HandoverIntake>>,
    pub read_timeout: Duration,
    pub start: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            chunk_size: 1024 * 1024,
            intake: None,
            read_timeout: Duration::from_secs(30),
            start: true,
        }
    }
}

pub struct TestService {
    pub service: Arc<HandoverService>,
    pub store: Arc<Store>,
    pub intake: Arc<FakeHandoverIntake>,
    pub directory: tempfile::TempDir,
    pub clock: WallClock,
    /// The wall clock as the service read it at start.
    pub now: DateTime<Utc>,
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

    pub async fn with_intake(chunk_size: i64, intake: Arc<dyn HandoverIntake>) -> TestService {
        Self::with(Options {
            chunk_size,
            intake: Some(intake),
            ..Options::default()
        })
        .await
    }

    /// The service, started unless `options.start` is false.
    pub async fn with(options: Options) -> TestService {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let fake = Arc::new(FakeHandoverIntake::default());
        let intake: Arc<dyn HandoverIntake> = options.intake.unwrap_or_else(|| fake.clone());
        let clock = WallClock::new(date(START));
        let configuration = HandoverConfiguration {
            service_name: "Test Mac".to_owned(),
            advertise: false,
            chunk_size: options.chunk_size,
            inbox_directory: directory.path().join("inbox"),
            pairing_window: Duration::from_secs(300),
            port: 0,
            read_timeout: options.read_timeout,
        };
        let identity =
            Arc::new(HandoverIdentity::mint("Steno test identity", clock.now()).unwrap());
        let service = Arc::new(HandoverService::new(
            configuration,
            store.clone(),
            intake,
            identity,
            clock.clock(),
        ));
        if options.start {
            service.start().await.unwrap();
        }
        TestService {
            service,
            store,
            intake: fake,
            directory,
            now: clock.now(),
            clock,
        }
    }

    pub async fn stop(&self) {
        self.service.stop().await;
    }

    pub fn advance(&self, duration: Duration) {
        self.clock.advance(duration);
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

    pub fn client_pinning(&self, fingerprint: &[u8]) -> LoopbackClient {
        LoopbackClient::new(self.port(), fingerprint)
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

// Loopback client: the phone as far as the computer can tell

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "body is not {}: {error}: {}",
                std::any::type_name::<T>(),
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

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
            .redirect(reqwest::redirect::Policy::none())
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
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .header("Accept", "application/json");
        let mut has_content_type = false;
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("content-type") {
                has_content_type = true;
            }
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            if !has_content_type {
                request = request.header("Content-Type", "application/json");
            }
            request = request.body(body);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let body = response.bytes().await?.to_vec();
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    pub async fn get(&self, path: &str, headers: &[(&str, &str)]) -> Response {
        self.request("GET", path, headers, None).await.unwrap()
    }

    pub async fn json<T: serde::Serialize>(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &T,
    ) -> Response {
        self.request(
            method,
            path,
            headers,
            Some(serde_json::to_vec(body).unwrap()),
        )
        .await
        .unwrap()
    }
}

pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

pub fn pairing(secret: &[u8]) -> String {
    format!("Pairing {}", STANDARD.encode(secret))
}

// Raw client: for the limit tests, where the assertion is about the
// connection itself

#[derive(Default)]
pub struct Exchange {
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub closed_by_server: bool,
}

impl Exchange {
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

pub type ClientStream = tokio_rustls::client::TlsStream<TcpStream>;

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

    /// Sends exactly the head and body it is given and reads one response.
    /// Returns once a full response has been read and the server either
    /// closed or `close_grace` passed; a response that never arrives within
    /// `timeout` returns whatever was read.
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

    /// Sends `bytes` as they are and then stays silent. True when the server
    /// closed the connection within `timeout`.
    pub async fn hold_open(&self, bytes: &[u8], timeout: Duration) -> std::io::Result<bool> {
        let mut stream = self.connect().await?;
        stream.write_all(bytes).await?;
        stream.flush().await?;
        Ok(read_until(&mut stream, timeout, &mut Vec::new(), |_| false).await)
    }
}

/// Appends what arrives to `received` until `done` says so, the server
/// closes (true) or `timeout` passes.
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
        if remaining.is_zero() {
            return false;
        }
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

/// One complete HTTP/1.1 response (status, headers, body by
/// `Content-Length`), or `None` while it is still incomplete.
pub fn parse_response(bytes: &[u8]) -> Option<Exchange> {
    let end = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&bytes[..end]).ok()?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next()?;
    let status: u16 = status_line.split(' ').nth(1)?.parse().ok()?;
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

// The phone

/// A paired phone in the tests: the bearer from `/v1/pair` and the
/// recording calls, over `LoopbackClient`.
pub struct Phone {
    pub client: LoopbackClient,
    pub token: String,
    pub device_id: Uuid,
    pub device_name: String,
}

impl Phone {
    /// `POST /v1/pair` with `secret`, whatever the computer answers.
    pub async fn try_pair(
        test: &TestService,
        secret: &[u8],
        device_id: Uuid,
        device_name: &str,
    ) -> Response {
        test.client()
            .json(
                "POST",
                "/v1/pair",
                &[("Authorization", &pairing(secret))],
                &wire::PairRequest {
                    device_id,
                    device_name: device_name.to_owned(),
                },
            )
            .await
    }

    /// Opens a window and pairs a fresh device against the running service.
    pub async fn pair(test: &TestService) -> Phone {
        Self::pair_named(test, "Test iPhone").await
    }

    pub async fn pair_named(test: &TestService, device_name: &str) -> Phone {
        let payload = test.service.begin_pairing();
        Self::pair_with(test, &payload, device_name).await
    }

    /// Pairs a fresh device with the secret of `payload`, the window the
    /// caller opened.
    pub async fn pair_with(
        test: &TestService,
        payload: &steno_handover::PairingPayload,
        device_name: &str,
    ) -> Phone {
        let device_id = Uuid::new_v4();
        let response = Self::try_pair(test, &payload.secret, device_id, device_name).await;
        assert_eq!(response.status, 200, "pairing failed");
        let pair: wire::PairResponse = response.json();
        Phone {
            client: test.client(),
            token: pair.token,
            device_id,
            device_name: device_name.to_owned(),
        }
    }

    pub fn bearer(&self) -> String {
        bearer(&self.token)
    }

    pub async fn announce(&self, metadata: &RecordingMetadata) -> Response {
        self.client
            .json(
                "PUT",
                &format!("/v1/recordings/{}", metadata.recording_id),
                &[("Authorization", &self.bearer())],
                metadata,
            )
            .await
    }

    pub async fn status(&self, recording_id: Uuid) -> Response {
        self.client
            .get(
                &format!("/v1/recordings/{recording_id}"),
                &[("Authorization", &self.bearer())],
            )
            .await
    }

    /// Uploads one chunk with the hash header `UploadSession.swift` sets; a
    /// different `declared_hash` simulates a corrupt body.
    pub async fn upload(&self, recording_id: Uuid, index: i64, bytes: &[u8]) -> Response {
        self.upload_with(recording_id, index, bytes, Some(&sha256(bytes)))
            .await
    }

    pub async fn upload_with(
        &self,
        recording_id: Uuid,
        index: i64,
        bytes: &[u8],
        declared_hash: Option<&[u8]>,
    ) -> Response {
        let authorization = self.bearer();
        let mut headers: Vec<(&str, &str)> = vec![
            ("Authorization", &authorization),
            ("Content-Type", "application/octet-stream"),
        ];
        let encoded = declared_hash.map(|hash| STANDARD.encode(hash));
        if let Some(encoded) = &encoded {
            headers.push((wire::CHUNK_HASH_HEADER, encoded));
        }
        self.client
            .request(
                "PUT",
                &format!("/v1/recordings/{recording_id}/chunks/{index}"),
                &headers,
                Some(bytes.to_vec()),
            )
            .await
            .unwrap()
    }

    pub async fn complete(&self, recording_id: Uuid) -> Response {
        self.client
            .request(
                "POST",
                &format!("/v1/recordings/{recording_id}/complete"),
                &[("Authorization", &self.bearer())],
                None,
            )
            .await
            .unwrap()
    }

    /// Announces and uploads every chunk of `bytes`.
    pub async fn upload_all(&self, metadata: &RecordingMetadata, bytes: &[u8]) {
        let announced = self.announce(metadata).await;
        assert!(
            announced.status == 201 || announced.status == 200,
            "announce: {}",
            announced.status
        );
        for (index, chunk) in chunks(bytes, metadata.chunk_size).iter().enumerate() {
            let response = self
                .upload(metadata.recording_id, index as i64, chunk)
                .await;
            assert_eq!(response.status, 204, "chunk {index}");
        }
    }

    /// Metadata for `bytes` as the phone would declare it.
    pub fn metadata(&self, bytes: &[u8], chunk_size: i64) -> RecordingMetadata {
        metadata_for(
            bytes,
            &self.device_name,
            Uuid::new_v4(),
            chunk_size,
            AudioFormat::M4aAac,
            None,
        )
    }
}

pub fn metadata_for(
    bytes: &[u8],
    device_name: &str,
    recording_id: Uuid,
    chunk_size: i64,
    format: AudioFormat,
    sha256_override: Option<Vec<u8>>,
) -> RecordingMetadata {
    RecordingMetadata {
        recording_id,
        started_at: date(1_789_990_000),
        duration_seconds: 61.5,
        byte_count: bytes.len() as i64,
        sha256: sha256_override.unwrap_or_else(|| sha256(bytes)),
        chunk_size,
        format,
        device_name: device_name.to_owned(),
    }
}

pub fn sha256(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

pub fn chunks(bytes: &[u8], size: i64) -> Vec<Vec<u8>> {
    bytes
        .chunks(usize::try_from(size).unwrap())
        .map(<[u8]>::to_vec)
        .collect()
}

/// Deterministic pseudo-random bytes (`SplitMix64`), so a failing test can
/// name what it sent.
pub fn seeded_bytes(count: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut data = Vec::with_capacity(count + 8);
    while data.len() < count {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        data.extend_from_slice(&z.to_le_bytes());
    }
    data.truncate(count);
    data
}

// The engine driven directly, without the listener

/// A paired device's recording calls, straight into the engine.
pub struct EngineDevice {
    pub service: Arc<HandoverService>,
    pub device: PairedDevice,
}

impl EngineDevice {
    /// Opens a window, pairs and returns the paired device's view.
    pub async fn paired(test: &TestService, device_name: &str) -> EngineDevice {
        let _ = test.service.begin_pairing();
        let device_id = Uuid::new_v4();
        let response = engine_pair(test, device_id, device_name).await;
        assert_eq!(response.status, 200);
        let device = test.store.paired_device(device_id).unwrap().unwrap();
        EngineDevice {
            service: test.service.clone(),
            device,
        }
    }

    pub async fn announce(&self, metadata: &RecordingMetadata) -> HandoverResponse {
        self.handle(
            HandoverRequest::new(
                Route::Announce(metadata.recording_id),
                Principal::Device(self.device.clone()),
            )
            .with_body(serde_json::to_vec(metadata).unwrap()),
        )
        .await
    }

    pub async fn status(&self, recording_id: Uuid) -> HandoverResponse {
        self.handle(HandoverRequest::new(
            Route::Status(recording_id),
            Principal::Device(self.device.clone()),
        ))
        .await
    }

    pub async fn upload(&self, recording_id: Uuid, index: i64, bytes: &[u8]) -> HandoverResponse {
        self.handle(
            HandoverRequest::new(
                Route::Chunk(recording_id, index),
                Principal::Device(self.device.clone()),
            )
            .with_body(bytes.to_vec())
            .with_header(wire::CHUNK_HASH_HEADER, &STANDARD.encode(sha256(bytes))),
        )
        .await
    }

    pub async fn complete(&self, recording_id: Uuid) -> HandoverResponse {
        self.handle(HandoverRequest::new(
            Route::Complete(recording_id),
            Principal::Device(self.device.clone()),
        ))
        .await
    }

    pub async fn upload_all(&self, metadata: &RecordingMetadata, bytes: &[u8]) {
        let announced = self.announce(metadata).await;
        assert!(announced.status.as_u16() == 201 || announced.status.as_u16() == 200);
        for (index, chunk) in chunks(bytes, metadata.chunk_size).iter().enumerate() {
            let response = self
                .upload(metadata.recording_id, index as i64, chunk)
                .await;
            assert_eq!(response.status.as_u16(), 204, "chunk {index}");
        }
    }

    pub fn metadata(&self, bytes: &[u8], chunk_size: i64) -> RecordingMetadata {
        metadata_for(
            bytes,
            &self.device.name,
            Uuid::new_v4(),
            chunk_size,
            AudioFormat::M4aAac,
            None,
        )
    }

    async fn handle(&self, request: HandoverRequest) -> HandoverResponse {
        self.service.engine.handle(request).await
    }
}

/// `POST /v1/pair` past the gate, as a request whose secret matched.
pub async fn engine_pair(
    test: &TestService,
    device_id: Uuid,
    device_name: &str,
) -> HandoverResponse {
    test.service
        .engine
        .handle(
            HandoverRequest::new(Route::Pair, Principal::Pairing).with_body(
                serde_json::to_vec(&wire::PairRequest {
                    device_id,
                    device_name: device_name.to_owned(),
                })
                .unwrap(),
            ),
        )
        .await
}

pub async fn engine_hello(test: &TestService) -> HandoverResponse {
    test.service
        .engine
        .handle(HandoverRequest::new(Route::Hello, Principal::Anonymous))
        .await
}
