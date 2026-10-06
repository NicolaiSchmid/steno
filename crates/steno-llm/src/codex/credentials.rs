//! Reads and refreshes `$CODEX_HOME/auth.json` the way the Codex CLI does.
//! Swift: `Sources/StenoLLM/Codex/CodexCredentialStore.swift`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};
use url::Url;

use super::jwt::JwtClaims;
use crate::transport;

/// The ChatGPT sign-in the Codex CLI stored, as far as Steno reads it.
/// Never persisted anywhere but the CLI's own file; never logged: the
/// `Debug` form redacts both tokens and the account id.
#[derive(Clone, PartialEq, Eq)]
pub struct CodexCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
    pub email: Option<String>,
    /// "plus", "pro", "free", "business", …; `None` when the token does not
    /// say.
    pub plan_type: Option<String>,
    /// The access token's `exp` claim; `None` when the token carries none.
    pub expires_at: Option<DateTime<Utc>>,
    /// The file's `last_refresh`; `None` for a file the CLI never refreshed.
    pub last_refresh: Option<DateTime<Utc>>,
}

impl fmt::Debug for CodexCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexCredentials")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("account_id", &"[redacted]")
            .field("email", &self.email)
            .field("plan_type", &self.plan_type)
            .field("expires_at", &self.expires_at)
            .field("last_refresh", &self.last_refresh)
            .finish()
    }
}

impl CodexCredentials {
    /// "name@example.com (Plus)", "name@example.com", or "your ChatGPT
    /// account".
    #[must_use]
    pub fn account_line(&self) -> String {
        let plan = self.plan_type.as_deref().map(|plan| {
            let mut chars = plan.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        });
        match (&self.email, plan) {
            (Some(email), Some(plan)) => format!("{email} ({plan})"),
            (Some(email), None) => email.clone(),
            (None, Some(plan)) => format!("your ChatGPT account ({plan})"),
            (None, None) => "your ChatGPT account".to_owned(),
        }
    }

    /// What must never appear in an error: both tokens and the account id.
    #[must_use]
    pub fn secrets(&self) -> Vec<String> {
        vec![
            self.access_token.clone(),
            self.refresh_token.clone(),
            self.account_id.clone(),
        ]
    }
}

/// Why the sign-in could not be used. The `Display` form is the sentence
/// the user reads and never carries the payload; [`detail`](Self::detail)
/// is the technical text (already redacted) for logs and "Details".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexCredentialError {
    /// No `auth.json`, or one without ChatGPT tokens.
    #[error("No Codex sign-in found. Run `codex login` in Terminal, then try again.")]
    NotSignedIn,
    /// The file holds an API key login, which the Codex backend does not
    /// take; the OpenAI preset of the endpoint provider is the way.
    #[error(
        "Codex is signed in with an API key, not a ChatGPT account. Pick OpenAI as the service and paste that key instead."
    )]
    ApiKeyLogin,
    /// `auth.json` is not JSON, or its token fields have the wrong shape.
    #[error("The Codex sign-in file could not be read.")]
    Malformed(String),
    /// The refresh token is spent, expired or revoked: only `codex login`
    /// helps.
    #[error("The Codex sign-in has expired. Run `codex login` in Terminal, then try again.")]
    SignInExpired(String),
    /// The refresh did not go through for a reason a retry may fix.
    #[error("The Codex sign-in could not be refreshed. Check the connection and try again.")]
    RefreshFailed(String),
}

impl CodexCredentialError {
    /// The technical reason, already redacted; `None` for the two cases
    /// that have none.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            CodexCredentialError::NotSignedIn | CodexCredentialError::ApiKeyLogin => None,
            CodexCredentialError::Malformed(detail)
            | CodexCredentialError::SignInExpired(detail)
            | CodexCredentialError::RefreshFailed(detail) => Some(detail),
        }
    }
}

/// The clock the store reads for the refresh windows; what
/// [`CodexCredentialStore::with_now`] takes.
pub type Now = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Reads and refreshes `$CODEX_HOME/auth.json` the way the Codex CLI does,
/// so the two stay signed in together. Every read goes back to the file
/// (the CLI may have rotated the tokens meanwhile); a refresh is written
/// back atomically over the file's latest contents with every unknown key
/// preserved, because refresh tokens rotate and the CLI would otherwise be
/// signed out. Concurrent callers share one refresh: the second waits for
/// the first and then reads the file it wrote, so two refreshes never spend
/// the same token. Callers never see the file: they get
/// [`CodexCredentials`] and attach the access token themselves.
pub struct CodexCredentialStore {
    home: PathBuf,
    http: reqwest::Client,
    token_endpoint: Url,
    client_id: String,
    now: Now,
    /// Held for the length of one refresh; waiters re-read the file after.
    /// Inside: the refresh token the endpoint last turned down for good,
    /// so a waiter whose re-read shows that same token gets the answer
    /// without posting the dead token again.
    refresh_lock: tokio::sync::Mutex<Option<SpentToken>>,
}

/// A refresh token the endpoint turned down for good, with the error it
/// gave. Never `Debug`: it holds the token.
struct SpentToken {
    refresh_token: String,
    error: CodexCredentialError,
}

/// The whole document plus what Steno read from it, kept so unknown keys
/// survive a write-back.
struct AuthFile {
    document: Map<String, Value>,
    credentials: CodexCredentials,
}

/// A refresh the token endpoint turned down, before it is mapped to the
/// public error: a reused token triggers a re-read, and a permanent refusal
/// is remembered against the token it was for. Never `Debug`.
struct RefreshRejected {
    /// The token the endpoint refused.
    refresh_token: String,
    /// The code was `refresh_token_reused`.
    reused: bool,
    permanent: bool,
    /// The code and the message, already redacted.
    detail: String,
}

enum RefreshFailure {
    Rejected(RefreshRejected),
    Credential(CodexCredentialError),
}

impl From<CodexCredentialError> for RefreshFailure {
    fn from(error: CodexCredentialError) -> Self {
        RefreshFailure::Credential(error)
    }
}

impl From<RefreshRejected> for CodexCredentialError {
    fn from(rejected: RefreshRejected) -> Self {
        if rejected.permanent {
            CodexCredentialError::SignInExpired(rejected.detail)
        } else {
            CodexCredentialError::RefreshFailed(rejected.detail)
        }
    }
}

impl From<RefreshFailure> for CodexCredentialError {
    fn from(failure: RefreshFailure) -> Self {
        match failure {
            RefreshFailure::Rejected(rejected) => rejected.into(),
            RefreshFailure::Credential(error) => error,
        }
    }
}

/// The token endpoint's answer; every field is optional there.
#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(default, rename = "id_token")]
    id: Option<String>,
    #[serde(default, rename = "access_token")]
    access: Option<String>,
    #[serde(default, rename = "refresh_token")]
    refresh: Option<String>,
}

/// The token endpoint's rejection, in the two shapes it uses:
/// `{"error": {"code": …}}` and `{"error": "invalid_grant", "error_code":
/// …, "error_description": …}`.
#[derive(Deserialize, Default)]
struct RefreshError {
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
}

impl RefreshError {
    /// The code as the endpoint wrote it: lowercased for the decisions,
    /// redacted before and after it is lowercased for the detail.
    fn code(&self) -> Option<&str> {
        if let Some(code) = self.error_code.as_deref().filter(|c| !c.is_empty()) {
            return Some(code);
        }
        match &self.error {
            Some(Value::String(code)) if !code.is_empty() => Some(code),
            Some(Value::Object(object)) => object
                .get("code")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty()),
            _ => None,
        }
    }

    fn message(&self) -> Option<&str> {
        if let Some(text) = self.error_description.as_deref().filter(|d| !d.is_empty()) {
            return Some(text);
        }
        match &self.error {
            Some(Value::Object(object)) => object.get("message").and_then(Value::as_str),
            _ => None,
        }
    }
}

impl CodexCredentialStore {
    /// The OAuth client the Codex CLI refreshes with; the token endpoint
    /// accepts refresh tokens for it only.
    pub const CODEX_CLIENT_ID: &'static str = "app_EMoamEEZ73f0CkXaXp7hrann";
    pub const DEFAULT_TOKEN_ENDPOINT: &'static str = "https://auth.openai.com/oauth/token";
    /// The CLI refreshes an access token this close to its `exp`.
    pub const EXPIRY_WINDOW: TimeDelta = TimeDelta::minutes(5);
    /// The CLI refreshes a file whose `last_refresh` is older than this even
    /// when the access token has not expired.
    pub const STALE_AFTER: TimeDelta = TimeDelta::days(8);

    /// The error codes the token endpoint uses for a refresh token that
    /// will never work again.
    pub const PERMANENT_REFRESH_CODES: [&'static str; 4] = [
        "refresh_token_expired",
        "refresh_token_reused",
        "refresh_token_invalidated",
        "invalid_grant",
    ];

    /// `CODEX_HOME` as `lookup` answers it (the process environment:
    /// `|name| std::env::var_os(name)`), else `~/.codex`. A path that is not
    /// Unicode is used as it is; an empty one is no override.
    #[must_use]
    pub fn default_home(lookup: impl Fn(&str) -> Option<OsString>) -> PathBuf {
        if let Some(home) = lookup("CODEX_HOME").filter(|home| !home.is_empty()) {
            return PathBuf::from(home);
        }
        std::env::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".codex")
    }

    /// The store over `home` with the real token endpoint and clock.
    ///
    /// ```
    /// use steno_llm::CodexCredentialStore;
    ///
    /// let home = CodexCredentialStore::default_home(|name| std::env::var_os(name));
    /// let store = CodexCredentialStore::new(&home);
    /// assert_eq!(store.file_path(), home.join("auth.json"));
    /// ```
    #[must_use]
    pub fn new(home: impl Into<PathBuf>) -> Self {
        CodexCredentialStore {
            home: home.into(),
            http: transport::default_http_client(),
            token_endpoint: Url::parse(Self::DEFAULT_TOKEN_ENDPOINT).expect("a literal URL"),
            client_id: Self::CODEX_CLIENT_ID.to_owned(),
            now: Arc::new(Utc::now),
            refresh_lock: tokio::sync::Mutex::new(None),
        }
    }

    /// An HTTP client of the caller's, for a proxy or for connect and read
    /// timeouts. Its overall `ClientBuilder::timeout` does not apply: the
    /// refresh carries its own 30-second timeout, and reqwest prefers that.
    /// Start it from [`transport::http_client_builder`]: reqwest comes
    /// without a TLS provider here, and that builder installs one.
    #[must_use]
    pub fn with_http(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    #[must_use]
    pub fn with_token_endpoint(mut self, endpoint: Url) -> Self {
        self.token_endpoint = endpoint;
        self
    }

    #[must_use]
    pub fn with_client_id(mut self, client_id: &str) -> Self {
        client_id.clone_into(&mut self.client_id);
        self
    }

    #[must_use]
    pub fn with_now(mut self, now: Now) -> Self {
        self.now = now;
        self
    }

    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    #[must_use]
    pub fn file_path(&self) -> PathBuf {
        self.home.join("auth.json")
    }

    /// What the file says, without touching the network. For the consent
    /// card and the status row.
    pub fn stored(&self) -> Result<CodexCredentials, CodexCredentialError> {
        Ok(self.read()?.credentials)
    }

    /// Credentials fit to send: the file's tokens, refreshed first when the
    /// access token is within [`Self::EXPIRY_WINDOW`] of its expiry or the
    /// file is older than [`Self::STALE_AFTER`].
    pub async fn current(&self) -> Result<CodexCredentials, CodexCredentialError> {
        self.refresh_unless(|credentials| !self.needs_refresh(credentials))
            .await
    }

    /// The 401 path: the file's credentials when they have changed since
    /// `access_token` was read (the CLI rotated them; no network), else one
    /// refresh regardless of age.
    pub async fn refreshed_if_still_using(
        &self,
        access_token: &str,
    ) -> Result<CodexCredentials, CodexCredentialError> {
        self.refresh_unless(|credentials| credentials.access_token != access_token)
            .await
    }

    /// The file's credentials when `usable` says so, else a refresh under
    /// the lock, re-reading first: another caller may have refreshed while
    /// this caller waited, and the file is the truth. A caller whose
    /// re-read shows the token the previous refresh was refused for good
    /// gets that refusal back without a round trip; only a token the CLI
    /// rotated since is tried.
    async fn refresh_unless(
        &self,
        usable: impl Fn(&CodexCredentials) -> bool,
    ) -> Result<CodexCredentials, CodexCredentialError> {
        let file = self.read()?;
        if usable(&file.credentials) {
            return Ok(file.credentials);
        }
        let mut spent = self.refresh_lock.lock().await;
        let latest = self.read()?;
        if usable(&latest.credentials) {
            return Ok(latest.credentials);
        }
        if let Some(token) = spent.as_ref()
            && token.refresh_token == latest.credentials.refresh_token
        {
            return Err(token.error.clone());
        }
        match self.refresh_rereading_on_reuse(latest, &usable).await {
            Ok(credentials) => {
                *spent = None;
                Ok(credentials)
            }
            Err(failure) => {
                let refused_for_good = match &failure {
                    RefreshFailure::Rejected(rejected) if rejected.permanent => {
                        Some(rejected.refresh_token.clone())
                    }
                    _ => None,
                };
                let error = CodexCredentialError::from(failure);
                if let Some(refresh_token) = refused_for_good {
                    *spent = Some(SpentToken {
                        refresh_token,
                        error: error.clone(),
                    });
                }
                Err(error)
            }
        }
    }

    /// Whether the access token is inside its expiry window or the file is
    /// stale.
    #[must_use]
    pub fn needs_refresh(&self, credentials: &CodexCredentials) -> bool {
        let now = (self.now)();
        if let Some(expires_at) = credentials.expires_at
            && expires_at - now < Self::EXPIRY_WINDOW
        {
            return true;
        }
        if let Some(last_refresh) = credentials.last_refresh
            && now - last_refresh > Self::STALE_AFTER
        {
            return true;
        }
        false
    }

    // File

    fn read(&self) -> Result<AuthFile, CodexCredentialError> {
        let path = self.file_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CodexCredentialError::NotSignedIn);
            }
            Err(error) => return Err(CodexCredentialError::Malformed(error.to_string())),
        };
        let Ok(Value::Object(document)) = serde_json::from_slice::<Value>(&bytes) else {
            return Err(CodexCredentialError::Malformed(
                "not a JSON object".to_owned(),
            ));
        };
        let credentials = Self::credentials_in(&document)?;
        Ok(AuthFile {
            document,
            credentials,
        })
    }

    /// The credentials an `auth.json` document holds.
    pub fn credentials_in(
        document: &Map<String, Value>,
    ) -> Result<CodexCredentials, CodexCredentialError> {
        let Some(Value::Object(tokens)) = document.get("tokens") else {
            if document
                .get("OPENAI_API_KEY")
                .and_then(Value::as_str)
                .is_some_and(|key| !key.is_empty())
            {
                return Err(CodexCredentialError::ApiKeyLogin);
            }
            return Err(CodexCredentialError::NotSignedIn);
        };
        if let Some(mode) = document.get("auth_mode").and_then(Value::as_str)
            && mode.to_lowercase() != "chatgpt"
        {
            return Err(CodexCredentialError::ApiKeyLogin);
        }
        let non_empty = |key: &str| {
            tokens
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        };
        let access_token = non_empty("access_token")
            .ok_or_else(|| CodexCredentialError::Malformed("no access token".to_owned()))?;
        let refresh_token = non_empty("refresh_token")
            .ok_or_else(|| CodexCredentialError::Malformed("no refresh token".to_owned()))?;
        let id_token = tokens.get("id_token").and_then(Value::as_str);
        let account_id = non_empty("account_id")
            .map(str::to_owned)
            .or_else(|| id_token.and_then(JwtClaims::account_id))
            .ok_or_else(|| CodexCredentialError::Malformed("no account id".to_owned()))?;
        // RFC 3339 with or without fractional seconds, as chrono writes it.
        let last_refresh = document
            .get("last_refresh")
            .and_then(Value::as_str)
            .and_then(steno_core::json::parse_date);
        Ok(CodexCredentials {
            access_token: access_token.to_owned(),
            refresh_token: refresh_token.to_owned(),
            account_id,
            email: id_token
                .and_then(JwtClaims::email)
                .or_else(|| JwtClaims::email(access_token)),
            plan_type: id_token
                .and_then(JwtClaims::plan_type)
                .or_else(|| JwtClaims::plan_type(access_token)),
            expires_at: JwtClaims::expiry(access_token),
            last_refresh,
        })
    }

    // Refresh

    async fn refresh_rereading_on_reuse(
        &self,
        file: AuthFile,
        usable: &impl Fn(&CodexCredentials) -> bool,
    ) -> Result<CodexCredentials, RefreshFailure> {
        let rejected = match self.refresh_once(file).await {
            Ok(credentials) => return Ok(credentials),
            Err(RefreshFailure::Credential(error)) => return Err(error.into()),
            Err(RefreshFailure::Rejected(rejected)) => rejected,
        };
        if rejected.reused {
            // The CLI may have rotated the token since this read; its file
            // is the truth. One more read: its credentials when they are
            // already fit to send (refreshing them would spend the CLI's
            // fresh token for nothing), else one more try, then the
            // failure stands.
            let latest = self.read()?;
            if latest.credentials.refresh_token != rejected.refresh_token {
                if usable(&latest.credentials) {
                    return Ok(latest.credentials);
                }
                return self.refresh_once(latest).await;
            }
        }
        Err(RefreshFailure::Rejected(rejected))
    }

    async fn refresh_once(&self, file: AuthFile) -> Result<CodexCredentials, RefreshFailure> {
        let secrets = file.credentials.secrets();
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": self.client_id,
            "refresh_token": file.credentials.refresh_token,
        });
        let response = self
            .http
            .post(self.token_endpoint.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, crate::USER_AGENT)
            .timeout(std::time::Duration::from_secs(30))
            .body(serde_json::to_vec(&body).unwrap_or_default())
            .send()
            .await
            .map_err(|error| Self::refresh_failed(&error, &secrets))?;
        let status = response.status().as_u16();
        let data = response
            .bytes()
            .await
            .map_err(|error| Self::refresh_failed(&error, &secrets))?;
        if !(200..300).contains(&status) {
            let rejection: RefreshError = serde_json::from_slice(&data).unwrap_or_default();
            // Decided on the code as sent, so a secret that happens to
            // occur in it cannot turn a permanent refusal into a temporary
            // one; only the detail is redacted.
            let sent = rejection.code();
            let code = sent.map(str::to_lowercase);
            let permanent = status == 401
                || code
                    .as_deref()
                    .is_some_and(|code| Self::PERMANENT_REFRESH_CODES.contains(&code));
            let message = match rejection.message() {
                Some(message) => transport::redact(message, &secrets),
                None => transport::redacted_prefix(&String::from_utf8_lossy(&data), &secrets, 300),
            };
            // Redacted before the case fold, which could leave a folded
            // copy of a secret, and after it, which could rebuild one from
            // an upper-case echo.
            let detail = match sent {
                Some(sent) => format!(
                    "{}: HTTP {status}: {message}",
                    transport::redact(&transport::redact(sent, &secrets).to_lowercase(), &secrets)
                ),
                None => format!("HTTP {status}: {message}"),
            };
            return Err(RefreshFailure::Rejected(RefreshRejected {
                refresh_token: file.credentials.refresh_token,
                reused: code.as_deref() == Some("refresh_token_reused"),
                permanent,
                detail,
            }));
        }
        let refreshed: RefreshResponse = serde_json::from_slice(&data).map_err(|_| {
            CodexCredentialError::RefreshFailed("undecodable token response".to_owned())
        })?;
        // Overlay the new tokens on what the file holds now, not on the copy
        // read before the round trip: the CLI may have written other keys
        // meanwhile, and those must survive. A file that changed hands
        // meanwhile is left alone and the new tokens are dropped: one the
        // user signed out of (`codex logout`) or switched to an API key
        // (`codex login --api-key`) keeps that answer, and one whose
        // refresh token is no longer the one posted (the CLI or a new
        // `codex login` wrote it) is returned as it stands. A file that is
        // merely unreadable at that moment (half-written by the CLI) falls
        // back to the copy read before.
        let mut document = match self.read() {
            Ok(latest) if latest.credentials.refresh_token != file.credentials.refresh_token => {
                return Ok(latest.credentials);
            }
            Ok(latest) => latest.document,
            Err(
                error @ (CodexCredentialError::NotSignedIn | CodexCredentialError::ApiKeyLogin),
            ) => {
                return Err(error.into());
            }
            Err(_) => file.document,
        };
        let mut tokens = match document.get("tokens") {
            Some(Value::Object(existing)) => existing.clone(),
            _ => Map::new(),
        };
        for (key, value) in [
            ("access_token", refreshed.access),
            ("refresh_token", refreshed.refresh),
            ("id_token", refreshed.id),
        ] {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                tokens.insert(key.to_owned(), Value::String(value));
            }
        }
        document.insert("tokens".to_owned(), Value::Object(tokens));
        document.insert(
            "last_refresh".to_owned(),
            Value::String(steno_core::json::format_date((self.now)())),
        );
        self.write(&document)?;
        Ok(Self::credentials_in(&document)?)
    }

    /// reqwest's message plus every source, redacted, as the clients
    /// report a transport failure.
    fn refresh_failed(error: &reqwest::Error, secrets: &[String]) -> CodexCredentialError {
        CodexCredentialError::RefreshFailed(transport::redact(
            &transport::error_chain(error),
            secrets,
        ))
    }

    /// Temp file beside the target with mode 0600, then `rename`: readers
    /// see the old or the new file, never a partial one, and the mode never
    /// opens up on the way.
    fn write(&self, document: &Map<String, Value>) -> Result<(), CodexCredentialError> {
        let text = crate::wire::swift_pretty(&Value::Object(document.clone()));
        let temporary = self
            .home
            .join(format!(".auth.json.steno-{}", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options.open(&temporary).and_then(|mut file| {
            use std::io::Write;
            file.write_all(text.as_bytes())
        });
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
            return Err(CodexCredentialError::RefreshFailed(
                "could not write the sign-in file".to_owned(),
            ));
        }
        if let Err(error) = std::fs::rename(&temporary, self.file_path()) {
            // The posted refresh token is spent by now, so a temporary file
            // that stays behind holds the only live tokens: name it, never
            // what it holds.
            let left = match std::fs::remove_file(&temporary) {
                Err(left) if left.kind() != std::io::ErrorKind::NotFound => {
                    format!("; the new sign-in is left in {}", temporary.display())
                }
                _ => String::new(),
            };
            let detail = format!("could not replace the sign-in file: {error}{left}");
            return Err(CodexCredentialError::RefreshFailed(detail));
        }
        Ok(())
    }
}
