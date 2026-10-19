//! Shared harnesses: the fixtures, a client over the stub server on a manual
//! clock with an event log, a temporary Codex home, and the golden
//! comparison.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steno_core::{LanguageTag, LlmMessage, LlmRequest, LlmResponseFormat, LlmRole, MeetingExport};
use steno_llm::testing::{ManualClock, STALL_DEADLINE, StubChatServer};
use steno_llm::{
    CodexCredentialStore, CodexResponsesClient, JwtClaims, LlmClientEvent, LlmEndpoint,
    OpenAiCompatibleClient, RetryPolicy,
};

pub const API_KEY: &str = "sk-test-secret-0123456789";

pub fn de() -> LanguageTag {
    LanguageTag::from("de")
}

pub fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

pub fn fixture_text(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Tests/Fixtures/llm");
    std::fs::read_to_string(path.join(relative))
        .unwrap_or_else(|error| panic!("fixture {relative}: {error}"))
}

fn export(name: &str) -> MeetingExport {
    serde_json::from_str(&fixture_text(&format!("transcripts/{name}.json"))).unwrap()
}

/// 24 German segments, three speakers, the `daily-standup` template.
pub fn standup() -> MeetingExport {
    export("denglish-standup")
}

/// 900 segments over an hour; "Me" on the mic lane.
pub fn customer_call() -> MeetingExport {
    export("customer-call-60min")
}

pub fn canned(name: &str) -> String {
    fixture_text(&format!("responses/{name}.json"))
}

/// The fixtures' sample UUID `number`: the number in the last group.
pub fn sample_uuid(number: u32) -> uuid::Uuid {
    uuid::Uuid::parse_str(&format!("00000000-0000-0000-0000-{number:012X}")).unwrap()
}

pub const PERSON_NICOLAI: u32 = 0xA;
pub const PERSON_JEROME: u32 = 0xB;
pub const PERSON_MARA: u32 = 12;
pub const STANDUP_SPEAKER_TWO: u32 = 111;
pub const STANDUP_SPEAKER_THREE: u32 = 112;

/// The golden comparison: `STENO_UPDATE_SNAPSHOTS=1` rewrites the file.
pub fn assert_golden(rendered: &str, relative: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/llm")
        .join(relative);
    if std::env::var("STENO_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::write(&path, rendered).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    if let Some((line, (golden, actual))) = expected
        .lines()
        .zip(rendered.lines())
        .enumerate()
        .find(|(_, (a, b))| a != b)
    {
        panic!(
            "{relative} differs at line {}:\n  golden: {golden}\n  actual: {actual}",
            line + 1
        );
    }
    assert_eq!(expected, rendered, "{relative} differs in length");
}

/// One readable file per request: parameters, then each message.
pub fn render(request: &LlmRequest) -> String {
    let format = match &request.response_format {
        LlmResponseFormat::Text => "text".to_owned(),
        LlmResponseFormat::JsonObject => "json_object".to_owned(),
        LlmResponseFormat::JsonSchema { name, strict, .. } => {
            format!("json_schema {name} strict={strict}")
        }
    };
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
        format!("responseFormat: {format}"),
    ];
    for message in &request.messages {
        lines.push(String::new());
        lines.push(format!("=== {} ===", message.role.as_str()));
        lines.push(message.content.clone());
    }
    lines.join("\n") + "\n"
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

/// Every event a client reported.
#[derive(Clone, Default)]
pub struct EventLog(Arc<Mutex<Vec<LlmClientEvent>>>);

impl EventLog {
    pub fn observer(&self) -> steno_llm::Observer {
        let log = Arc::clone(&self.0);
        Arc::new(move |event| log.lock().unwrap().push(event))
    }

    pub fn events(&self) -> Vec<LlmClientEvent> {
        self.0.lock().unwrap().clone()
    }

    pub fn retry_delays(&self) -> Vec<Duration> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                LlmClientEvent::Retrying { after, .. } => Some(after),
                _ => None,
            })
            .collect()
    }

    /// Advances `clock` through every backoff in the background: whenever
    /// a new `Retrying` event is logged and the client sleeps, by its delay.
    pub fn drive_retries(&self, clock: Arc<ManualClock>) -> tokio::task::JoinHandle<()> {
        let log = self.clone();
        tokio::spawn(async move {
            let mut driven = 0;
            loop {
                let delays = log.retry_delays();
                if let Some(delay) = delays.get(driven) {
                    clock.wait_for_sleepers(1, STALL_DEADLINE).await;
                    clock.advance(*delay);
                    driven += 1;
                } else {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        })
    }
}

/// A stub server, a manual clock, an event log and a client wired to them.
pub struct ClientHarness {
    pub server: StubChatServer,
    pub clock: Arc<ManualClock>,
    pub client: OpenAiCompatibleClient,
    pub endpoint: LlmEndpoint,
    pub log: EventLog,
}

impl ClientHarness {
    pub async fn new() -> Self {
        Self::build(RetryPolicy::default(), Some(API_KEY), |_| {}).await
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
        let log = EventLog::default();
        let client = OpenAiCompatibleClient::new(endpoint.clone(), api_key)
            .with_retry(retry)
            .with_clock(clock.clone())
            .with_observer(log.observer());
        ClientHarness {
            server,
            clock,
            client,
            endpoint,
            log,
        }
    }

    pub fn drive_retries(&self) -> tokio::task::JoinHandle<()> {
        self.log.drive_retries(self.clock.clone())
    }
}

/// A fixed "now" the store reads; the tokens are minted relative to it.
pub fn codex_now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_790_000_000, 0).unwrap()
}

/// A temporary `CODEX_HOME` and a stub token endpoint.
pub struct CodexHome {
    pub directory: tempfile::TempDir,
    pub server: StubChatServer,
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

    pub fn write(&self, auth: AuthFile) {
        write_auth(&self.file(), auth);
    }

    /// The default sign-in with an access token inside the expiry window.
    pub fn write_expiring(&self) {
        self.write(AuthFile::default().access(&Self::access_token(10, "plus")));
    }

    pub fn document(&self) -> serde_json::Map<String, serde_json::Value> {
        match serde_json::from_slice(&std::fs::read(self.file()).unwrap()).unwrap() {
            serde_json::Value::Object(object) => object,
            _ => panic!("not an object"),
        }
    }

    /// The refresh tokens the token endpoint was sent, in order.
    pub fn posted_refresh_tokens(&self) -> Vec<String> {
        self.server
            .requests()
            .iter()
            .map(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                body["refresh_token"].as_str().unwrap().to_owned()
            })
            .collect()
    }
}

/// 4 096 characters of a body that is not the answer.
pub const BODY_CUT: usize = 4_096;
/// 500 characters of an error body that is not an envelope.
pub const ERROR_CUT: usize = 500;
/// 300 characters of a refusal from the token endpoint.
pub const REFRESH_CUT: usize = 300;

/// The names in `directory`, sorted.
pub fn file_names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Writes `auth` to `path` the way the CLI would (compact JSON).
pub fn write_auth(path: &Path, auth: AuthFile) {
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
    std::fs::write(path, serde_json::to_vec(&document).unwrap()).unwrap();
}

/// What `CodexHome::write` puts in the file; `Default` is a fresh `ChatGPT`
/// login an hour ago with an unknown key that must survive a write-back.
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

/// A `CodexHome`, a stub Codex backend, a manual clock and a client.
pub struct CodexHarness {
    pub home: CodexHome,
    pub backend: StubChatServer,
    pub clock: Arc<ManualClock>,
    pub client: CodexResponsesClient,
    pub endpoint: LlmEndpoint,
    pub log: EventLog,
}

impl CodexHarness {
    pub async fn new() -> Self {
        Self::with_retry(RetryPolicy::default()).await
    }

    pub async fn with_retry(retry: RetryPolicy) -> Self {
        let home = CodexHome::new().await;
        home.write(AuthFile::default());
        let backend = StubChatServer::start().await.unwrap();
        let mut endpoint = LlmEndpoint::codex("gpt-stub", 200_000);
        endpoint.base_url = backend.base_url().clone();
        let clock = ManualClock::new();
        let log = EventLog::default();
        let client = CodexResponsesClient::new(endpoint.clone(), Arc::new(home.store()))
            .with_retry(retry)
            .with_clock(clock.clone())
            .with_observer(log.observer());
        CodexHarness {
            home,
            backend,
            clock,
            client,
            endpoint,
            log,
        }
    }

    pub fn drive_retries(&self) -> tokio::task::JoinHandle<()> {
        self.log.drive_retries(self.clock.clone())
    }

    /// The `Authorization` header of every backend request, in order.
    pub fn authorizations(&self) -> Vec<String> {
        self.backend
            .requests()
            .iter()
            .map(|r| r.authorization().unwrap_or_default().to_owned())
            .collect()
    }
}

pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// The error a boundary returned, downcast to `T`.
pub fn downcast<T: std::error::Error + Clone + 'static>(error: &steno_core::BoxError) -> T {
    error
        .downcast_ref::<T>()
        .unwrap_or_else(|| panic!("expected {}, got {error}", std::any::type_name::<T>()))
        .clone()
}
