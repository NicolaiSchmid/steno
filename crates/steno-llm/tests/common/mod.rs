//! Shared harnesses: the fixtures, a client over the stub server on a manual
//! clock with an event recorder, a temporary Codex home, and the golden
//! comparison.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steno_core::{LanguageTag, LlmMessage, LlmRequest, LlmResponseFormat, LlmRole, MeetingExport};
use steno_llm::testing::{ManualClock, StubChatServer};
use steno_llm::{
    CodexCredentialStore, CodexResponsesClient, JwtClaims, LlmClientEvent, LlmEndpoint,
    OpenAiCompatibleClient, RetryPolicy, StructuredOutputMode,
};
use tokio::sync::mpsc;

pub const API_KEY: &str = "sk-test-secret-0123456789";
/// Wall-clock budget for waiting on a sleeper; only spent in the failure
/// case.
pub const SLEEPER_WAIT: Duration = Duration::from_secs(10);

pub fn de() -> LanguageTag {
    LanguageTag::from("de")
}

pub fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Tests/Fixtures/llm")
}

pub fn fixture_text(relative: &str) -> String {
    std::fs::read_to_string(fixtures_root().join(relative))
        .unwrap_or_else(|error| panic!("fixture {relative}: {error}"))
}

pub fn export(name: &str) -> MeetingExport {
    serde_json::from_str(&fixture_text(&format!("transcripts/{name}.json")))
        .unwrap_or_else(|error| panic!("transcript fixture {name}: {error}"))
}

pub fn standup() -> MeetingExport {
    export("denglish-standup")
}

pub fn customer_call() -> MeetingExport {
    export("customer-call-60min")
}

pub fn canned(name: &str) -> String {
    fixture_text(&format!("responses/{name}.json"))
}

pub fn uuid(text: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(text).unwrap()
}

/// `SampleData.uuid(n)` from the Swift fixtures: the number in the last
/// group.
pub fn sample_uuid(number: u32) -> uuid::Uuid {
    uuid(&format!("00000000-0000-0000-0000-{number:012X}"))
}

pub const PERSON_NICOLAI: u32 = 0xA;
pub const PERSON_JEROME: u32 = 0xB;
pub const PERSON_MARA: u32 = 12;
pub const STANDUP_SPEAKER_TWO: u32 = 111;
pub const STANDUP_SPEAKER_THREE: u32 = 112;

/// The golden comparison: `STENO_UPDATE_SNAPSHOTS=1` rewrites the file.
pub fn assert_golden(rendered: &str, relative: &str) {
    let path = fixtures_root().join(relative);
    if std::env::var("STENO_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::write(&path, rendered).unwrap();
        return;
    }
    let expected =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("golden {relative}: {error}"));
    if expected != rendered {
        let diff: Vec<String> = expected
            .lines()
            .zip(rendered.lines())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .take(5)
            .map(|(n, (a, b))| format!("line {}:\n  golden: {a}\n  actual: {b}", n + 1))
            .collect();
        panic!(
            "{relative} differs ({} vs {} lines):\n{}",
            expected.lines().count(),
            rendered.lines().count(),
            diff.join("\n")
        );
    }
}

/// One readable file per request: parameters, then each message.
pub fn render(request: &LlmRequest) -> String {
    let mut lines = vec![
        format!("purpose: {}", request.purpose),
        format!(
            "temperature: {}",
            request
                .temperature
                .map_or("default".to_owned(), |t| format!("{t:?}"))
        ),
        format!(
            "maxTokens: {}",
            request
                .max_tokens
                .map_or("default".to_owned(), |t| t.to_string())
        ),
        format!("responseFormat: {}", describe(&request.response_format)),
    ];
    for message in &request.messages {
        lines.push(String::new());
        lines.push(format!("=== {} ===", message.role.as_str()));
        lines.push(message.content.clone());
    }
    lines.join("\n") + "\n"
}

fn describe(format: &LlmResponseFormat) -> String {
    match format {
        LlmResponseFormat::Text => "text".to_owned(),
        LlmResponseFormat::JsonObject => "json_object".to_owned(),
        LlmResponseFormat::JsonSchema { name, strict, .. } => {
            format!("json_schema {name} strict={strict}")
        }
    }
}

pub fn schema_format(name: &str) -> LlmResponseFormat {
    LlmResponseFormat::JsonSchema {
        name: name.to_owned(),
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    }
}

pub fn request(purpose: &str, format: LlmResponseFormat) -> LlmRequest {
    LlmRequest {
        messages: vec![
            LlmMessage {
                role: LlmRole::System,
                content: "You are a test.".to_owned(),
            },
            LlmMessage {
                role: LlmRole::User,
                content: "Say hi as JSON.".to_owned(),
            },
        ],
        response_format: format,
        temperature: Some(0.0),
        max_tokens: Some(64),
        purpose: purpose.to_owned(),
    }
}

pub fn json_request() -> LlmRequest {
    request("test", LlmResponseFormat::JsonObject)
}

pub fn text_request() -> LlmRequest {
    request("test", LlmResponseFormat::Text)
}

/// Every event logged for later assertions and streamed once for
/// `drive_retries`.
pub struct EventRecorder {
    log: Arc<Mutex<Vec<LlmClientEvent>>>,
    sender: mpsc::UnboundedSender<LlmClientEvent>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<LlmClientEvent>>>,
}

impl EventRecorder {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        EventRecorder {
            log: Arc::new(Mutex::new(Vec::new())),
            sender,
            receiver: Mutex::new(Some(receiver)),
        }
    }

    pub fn observer(&self) -> steno_llm::Observer {
        let log = Arc::clone(&self.log);
        let sender = self.sender.clone();
        Arc::new(move |event| {
            log.lock().unwrap().push(event.clone());
            let _ = sender.send(event);
        })
    }

    pub fn events(&self) -> Vec<LlmClientEvent> {
        self.log.lock().unwrap().clone()
    }

    pub fn take_receiver(&self) -> mpsc::UnboundedReceiver<LlmClientEvent> {
        self.receiver.lock().unwrap().take().expect("one consumer")
    }

    /// Consumes events in the background and advances the clock by each
    /// announced backoff once the client is asleep.
    pub fn drive_retries(&self, clock: Arc<ManualClock>) -> tokio::task::JoinHandle<()> {
        let mut receiver = self.take_receiver();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                if let LlmClientEvent::Retrying { after, .. } = event {
                    clock.wait_for_sleepers(1, SLEEPER_WAIT).await;
                    clock.advance(after);
                }
            }
        })
    }

    /// The next event matching `predicate`, consuming earlier ones.
    pub async fn next(
        receiver: &mut mpsc::UnboundedReceiver<LlmClientEvent>,
        predicate: impl Fn(&LlmClientEvent) -> bool,
    ) -> Option<LlmClientEvent> {
        while let Some(event) = receiver.recv().await {
            if predicate(&event) {
                return Some(event);
            }
        }
        None
    }
}

/// The mode of every `Request` event, in order.
pub fn request_modes(events: &[LlmClientEvent]) -> Vec<StructuredOutputMode> {
    events
        .iter()
        .filter_map(|event| match event {
            LlmClientEvent::Request { mode, .. } => Some(*mode),
            _ => None,
        })
        .collect()
}

/// The structured output mode only ever walks down the chain: no request
/// goes out under a mode above the one an earlier request was sent with.
pub fn assert_modes_never_go_up(events: &[LlmClientEvent]) {
    let rank = |mode: &StructuredOutputMode| {
        StructuredOutputMode::ALL
            .iter()
            .position(|candidate| candidate == mode)
            .unwrap()
    };
    let modes = request_modes(events);
    let mut reached = 0;
    for mode in &modes {
        assert!(rank(mode) >= reached, "the mode went back up: {modes:?}");
        reached = rank(mode);
    }
}

pub fn is_retrying(event: &LlmClientEvent) -> bool {
    matches!(event, LlmClientEvent::Retrying { .. })
}

pub fn retry_delays(events: &[LlmClientEvent]) -> Vec<Duration> {
    events
        .iter()
        .filter_map(|event| match event {
            LlmClientEvent::Retrying { after, .. } => Some(*after),
            _ => None,
        })
        .collect()
}

/// A stub server, a manual clock, an event log and a client wired to them.
pub struct ClientHarness {
    pub server: StubChatServer,
    pub clock: Arc<ManualClock>,
    pub client: OpenAiCompatibleClient,
    pub endpoint: LlmEndpoint,
    pub recorder: EventRecorder,
}

impl ClientHarness {
    pub async fn new() -> Self {
        Self::build(RetryPolicy::default(), Some(API_KEY), |_| {}).await
    }

    pub async fn with_retry(retry: RetryPolicy) -> Self {
        Self::build(retry, Some(API_KEY), |_| {}).await
    }

    pub async fn with_key(api_key: Option<&str>) -> Self {
        Self::build(RetryPolicy::default(), api_key, |_| {}).await
    }

    pub async fn configured(configure: impl FnOnce(&mut LlmEndpoint)) -> Self {
        Self::build(RetryPolicy::default(), Some(API_KEY), configure).await
    }

    pub async fn build(
        retry: RetryPolicy,
        api_key: Option<&str>,
        configure: impl FnOnce(&mut LlmEndpoint),
    ) -> Self {
        let server = StubChatServer::start().await.unwrap();
        let mut endpoint = LlmEndpoint::new(server.base_url().clone(), "stub-model");
        configure(&mut endpoint);
        let clock = ManualClock::new();
        let recorder = EventRecorder::new();
        let client = OpenAiCompatibleClient::new(endpoint.clone(), api_key)
            .with_retry(retry)
            .with_clock(clock.clone())
            .with_observer(recorder.observer());
        ClientHarness {
            server,
            clock,
            client,
            endpoint,
            recorder,
        }
    }

    pub fn events(&self) -> Vec<LlmClientEvent> {
        self.recorder.events()
    }

    pub fn drive_retries(&self) -> tokio::task::JoinHandle<()> {
        self.recorder.drive_retries(self.clock.clone())
    }
}

/// A temporary `CODEX_HOME` with an `auth.json` the tests write, and a stub
/// server standing in for the token endpoint.
pub struct CodexHome {
    pub directory: tempfile::TempDir,
    pub server: StubChatServer,
}

/// A fixed "now" the store reads; the tokens are minted relative to it.
pub fn codex_now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_790_000_000, 0).unwrap()
}

impl CodexHome {
    pub async fn new() -> Self {
        CodexHome {
            directory: tempfile::tempdir().unwrap(),
            server: StubChatServer::start().await.unwrap(),
        }
    }

    pub fn file(&self) -> PathBuf {
        self.directory.path().join("auth.json")
    }

    pub fn store(&self) -> CodexCredentialStore {
        CodexCredentialStore::new(self.directory.path())
            .with_token_endpoint(self.server.base_url().join("/v1/oauth/token").unwrap())
            .with_client_id("app_test")
            .with_now(Arc::new(codex_now))
    }

    /// An access token expiring `expires_in` seconds after `now`.
    pub fn access_token(expires_in: i64, plan: &str) -> String {
        JwtClaims::unsigned_token(&serde_json::json!({
            "exp": codex_now().timestamp() + expires_in,
            JwtClaims::OPENAI_AUTH_CLAIM: {
                "chatgpt_account_id": "acct_jwt",
                "chatgpt_plan_type": plan,
            },
        }))
    }

    pub fn id_token(email: &str, plan: &str) -> String {
        JwtClaims::unsigned_token(&serde_json::json!({
            "email": email,
            JwtClaims::OPENAI_AUTH_CLAIM: {
                "chatgpt_account_id": "acct_jwt",
                "chatgpt_plan_type": plan,
            },
        }))
    }

    /// Writes an `auth.json` like the CLI's, with an extra key Steno does
    /// not know, so the tests can check it survives a write-back.
    pub fn write(&self, auth: AuthFile) {
        write_auth(&self.file(), auth);
    }

    pub fn document(&self) -> serde_json::Map<String, serde_json::Value> {
        match serde_json::from_slice(&std::fs::read(self.file()).unwrap()).unwrap() {
            serde_json::Value::Object(object) => object,
            _ => panic!("not an object"),
        }
    }
}

/// Writes `auth` to `path` the way the CLI would (compact JSON).
pub fn write_auth(path: &std::path::Path, auth: AuthFile) {
    {
        let mut tokens = serde_json::Map::new();
        tokens.insert("access_token".to_owned(), auth.access.into());
        tokens.insert("refresh_token".to_owned(), auth.refresh.into());
        if let Some(id) = auth.id {
            tokens.insert("id_token".to_owned(), id.into());
        }
        if let Some(account_id) = auth.account_id {
            tokens.insert("account_id".to_owned(), account_id.into());
        }
        let mut document = serde_json::Map::new();
        document.insert("OPENAI_API_KEY".to_owned(), serde_json::Value::Null);
        document.insert("tokens".to_owned(), tokens.into());
        if let Some(mode) = auth.auth_mode {
            document.insert("auth_mode".to_owned(), mode.into());
        }
        if let Some(last_refresh) = auth.last_refresh {
            document.insert(
                "last_refresh".to_owned(),
                steno_core::json::format_date(last_refresh).into(),
            );
        }
        for (key, value) in auth.extra {
            document.insert(key, value);
        }
        std::fs::write(
            path,
            serde_json::to_vec(&serde_json::Value::Object(document)).unwrap(),
        )
        .unwrap();
    }
}

/// What `CodexHome::write` puts in the file; `Default` is the CLI's file
/// after a fresh `ChatGPT` login an hour ago.
pub struct AuthFile {
    pub access: String,
    pub refresh: String,
    pub id: Option<String>,
    pub account_id: Option<String>,
    pub last_refresh: Option<chrono::DateTime<chrono::Utc>>,
    pub auth_mode: Option<String>,
    pub extra: Vec<(String, serde_json::Value)>,
}

impl Default for AuthFile {
    fn default() -> Self {
        AuthFile {
            access: CodexHome::access_token(3_600, "plus"),
            refresh: "rt_original".to_owned(),
            id: Some(CodexHome::id_token("nicolai@example.com", "plus")),
            account_id: Some("acct_stored".to_owned()),
            last_refresh: Some(codex_now() - chrono::TimeDelta::hours(1)),
            auth_mode: Some("chatgpt".to_owned()),
            extra: vec![(
                "agent_identity".to_owned(),
                serde_json::json!({"keep": true}),
            )],
        }
    }
}

impl AuthFile {
    pub fn access(mut self, access: &str) -> Self {
        access.clone_into(&mut self.access);
        self
    }

    pub fn refresh(mut self, refresh: &str) -> Self {
        refresh.clone_into(&mut self.refresh);
        self
    }
}

/// A `CodexHome` (auth file plus token endpoint), a second stub server as
/// the Codex backend, a manual clock and the client wired to them.
pub struct CodexHarness {
    pub home: CodexHome,
    pub backend: StubChatServer,
    pub clock: Arc<ManualClock>,
    pub client: CodexResponsesClient,
    pub endpoint: LlmEndpoint,
    pub recorder: EventRecorder,
}

impl CodexHarness {
    pub async fn new() -> Self {
        Self::build(RetryPolicy::default(), "gpt-stub", |_| {}).await
    }

    pub async fn with_retry(retry: RetryPolicy) -> Self {
        Self::build(retry, "gpt-stub", |_| {}).await
    }

    pub async fn build(
        retry: RetryPolicy,
        model: &str,
        configure: impl FnOnce(&mut LlmEndpoint),
    ) -> Self {
        let home = CodexHome::new().await;
        home.write(AuthFile::default());
        let backend = StubChatServer::start().await.unwrap();
        let mut endpoint = LlmEndpoint::codex(model, 200_000);
        endpoint.base_url = backend.base_url().clone();
        configure(&mut endpoint);
        let clock = ManualClock::new();
        let recorder = EventRecorder::new();
        let client = CodexResponsesClient::new(endpoint.clone(), Arc::new(home.store()))
            .with_retry(retry)
            .with_clock(clock.clone())
            .with_observer(recorder.observer());
        CodexHarness {
            home,
            backend,
            clock,
            client,
            endpoint,
            recorder,
        }
    }

    pub fn events(&self) -> Vec<LlmClientEvent> {
        self.recorder.events()
    }

    pub fn drive_retries(&self) -> tokio::task::JoinHandle<()> {
        self.recorder.drive_retries(self.clock.clone())
    }
}

/// The error a boundary returned, downcast to `T`.
pub fn downcast<T: std::error::Error + Clone + 'static>(error: &steno_core::BoxError) -> T {
    error
        .downcast_ref::<T>()
        .unwrap_or_else(|| panic!("expected {}, got {error}", std::any::type_name::<T>()))
        .clone()
}
