//! Reads and refreshes `$CODEX_HOME/auth.json` the way the Codex CLI does.

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

/// The ChatGPT sign-in the Codex CLI stored. The `Debug` form redacts both
/// tokens and the account id.
#[derive(Clone, PartialEq, Eq)]
pub struct CodexCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
    pub email: Option<String>,
    /// "plus", "pro", "free", "business", …
    pub plan_type: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
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
    /// "name@example.com (Plus)", "name@example.com", or "your ChatGPT account".
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

/// Why the sign-in could not be used. `Display` is the user-facing sentence;
/// [`detail`](Self::detail) is the redacted technical text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexCredentialError {
    #[error("No Codex sign-in found. Run `codex login` in Terminal, then try again.")]
    NotSignedIn,
    /// An API key login, which the Codex backend does not take.
    #[error(
        "Codex is signed in with an API key, not a ChatGPT account. Pick OpenAI as the service and paste that key instead."
    )]
    ApiKeyLogin,
    #[error("The Codex sign-in file could not be read.")]
    Malformed(String),
    /// The refresh token is spent, expired or revoked.
    #[error("The Codex sign-in has expired. Run `codex login` in Terminal, then try again.")]
    SignInExpired(String),
    #[error("The Codex sign-in could not be refreshed. Check the connection and try again.")]
    RefreshFailed(String),
}

impl CodexCredentialError {
    /// The technical reason, already redacted.
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

/// The clock the store reads for the refresh windows.
pub type Now = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Reads and refreshes `$CODEX_HOME/auth.json` so Steno and the Codex CLI
/// stay signed in together. Every read goes back to the file; a refresh is
/// written back atomically with unknown keys preserved, because refresh
/// tokens rotate. Concurrent callers share one refresh.
///
/// A refresh whose write-back fails keeps the new tokens in memory (the
/// posted refresh token is spent) and writes them again on every later call
/// while the file still holds the spent token or does not parse.
pub struct CodexCredentialStore {
    home: PathBuf,
    http: Result<reqwest::Client, crate::LlmError>,
    token_endpoint: Url,
    client_id: String,
    now: Now,
    /// Held for the length of one refresh. Inside: the refresh token the
    /// endpoint last refused for good, so waiters do not post it again.
    refresh_lock: tokio::sync::Mutex<Option<SpentToken>>,
    /// The refreshed document the file could not take; only touched under
    /// `refresh_lock`, never held across an `.await`.
    kept: std::sync::Mutex<Option<Kept>>,
}

/// Refreshed tokens whose write-back failed. Never `Debug`.
struct Kept {
    auth: AuthFile,
    /// The spent refresh token the file still holds.
    replaces: String,
    /// The temporary the latest failed rename left; removed once a write lands.
    left: Option<PathBuf>,
}

/// Why the write-back failed; never carries a token.
struct WriteFailure {
    detail: String,
    left: Option<PathBuf>,
}

/// A refresh token the endpoint refused for good. Never `Debug`.
struct SpentToken {
    refresh_token: String,
    error: CodexCredentialError,
}

/// The whole document plus what Steno read from it, so unknown keys
/// survive a write-back.
#[derive(Clone)]
struct AuthFile {
    document: Map<String, Value>,
    credentials: CodexCredentials,
}

/// A refresh the token endpoint turned down. Never `Debug`.
struct RefreshRejected {
    refresh_token: String,
    reused: bool,
    permanent: bool,
    /// Already redacted.
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

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(default, rename = "id_token")]
    id: Option<String>,
    #[serde(default, rename = "access_token")]
    access: Option<String>,
    #[serde(default, rename = "refresh_token")]
    refresh: Option<String>,
}

/// The token endpoint's rejection, in either of the two shapes it uses.
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
    /// The OAuth client the Codex CLI refreshes with.
    pub const CODEX_CLIENT_ID: &'static str = "app_EMoamEEZ73f0CkXaXp7hrann";
    pub const DEFAULT_TOKEN_ENDPOINT: &'static str = "https://auth.openai.com/oauth/token";
    pub const EXPIRY_WINDOW: TimeDelta = TimeDelta::minutes(5);
    /// Refresh a file whose `last_refresh` is older than this.
    pub const STALE_AFTER: TimeDelta = TimeDelta::days(8);

    /// Token endpoint codes for a refresh token that will never work again.
    pub const PERMANENT_REFRESH_CODES: [&'static str; 4] = [
        "refresh_token_expired",
        "refresh_token_reused",
        "refresh_token_invalidated",
        "invalid_grant",
    ];

    /// `CODEX_HOME` as `lookup` answers it, else `~/.codex`. An empty value
    /// is no override.
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
            kept: std::sync::Mutex::new(None),
        }
    }

    /// A caller-supplied HTTP client, for a proxy or timeouts. Start it from
    /// [`transport::http_client_builder`], which installs the TLS provider.
    #[must_use]
    pub fn with_http(mut self, http: reqwest::Client) -> Self {
        self.http = Ok(http);
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

    /// What the file says, without touching the network.
    pub fn stored(&self) -> Result<CodexCredentials, CodexCredentialError> {
        Ok(self.read()?.credentials)
    }

    /// Credentials fit to send, refreshed first when near expiry or stale.
    pub async fn current(&self) -> Result<CodexCredentials, CodexCredentialError> {
        self.refresh_unless(|credentials| !self.needs_refresh(credentials))
            .await
    }

    /// The 401 path: the file's credentials if they changed since
    /// `access_token` was read, else one refresh regardless of age.
    pub async fn refreshed_if_still_using(
        &self,
        access_token: &str,
    ) -> Result<CodexCredentials, CodexCredentialError> {
        self.refresh_unless(|credentials| credentials.access_token != access_token)
            .await
    }

    /// The file's credentials when `usable` says so, else a refresh under
    /// the lock, re-reading first since another caller may have refreshed.
    async fn refresh_unless(
        &self,
        usable: impl Fn(&CodexCredentials) -> bool,
    ) -> Result<CodexCredentials, CodexCredentialError> {
        if self.lock_kept().is_none() {
            let file = self.read()?;
            if usable(&file.credentials) {
                return Ok(file.credentials);
            }
        }
        let mut spent = self.refresh_lock.lock().await;
        let latest = self.latest()?;
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

    fn lock_kept(&self) -> std::sync::MutexGuard<'_, Option<Kept>> {
        self.kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The file, or the kept tokens while the file still holds the spent
    /// token they replace (written again first). A file that does not parse
    /// gets the kept tokens for this call; any other change to the file
    /// wins and drops the kept tokens.
    fn latest(&self) -> Result<AuthFile, CodexCredentialError> {
        let document = self.read_document();
        let mut slot = self.lock_kept();
        let Some(mut kept) = slot.take() else {
            return document.and_then(Self::auth_file);
        };
        let mut document = match document {
            Ok(document) if refresh_token_in(&document) == Some(kept.replaces.as_str()) => document,
            Err(CodexCredentialError::Malformed(_)) => return Ok(slot.insert(kept).auth.clone()),
            other => {
                remove_left(kept.left);
                return other.and_then(Self::auth_file);
            }
        };
        for key in ["tokens", "last_refresh"] {
            if let Some(value) = kept.auth.document.get(key) {
                document.insert(key.to_owned(), value.clone());
            }
        }
        // An API-key login that kept the spent tokens: the file wins.
        kept.auth = match Self::auth_file(document) {
            Ok(auth) => auth,
            Err(error) => {
                remove_left(kept.left);
                return Err(error);
            }
        };
        let latest = kept.auth.clone();
        *slot = self.write_kept(kept);
        Ok(latest)
    }

    /// Writes `kept.auth`: `None` once the write lands, else `kept` to retry
    /// on the next call.
    fn write_kept(&self, mut kept: Kept) -> Option<Kept> {
        match self.write(&kept.auth.document) {
            Ok(()) => {
                remove_left(kept.left);
                None
            }
            Err(failure) => {
                tracing::warn!(
                    "{}. Kept in memory; the next call writes it again.",
                    failure.detail
                );
                if let Some(left) = failure.left {
                    remove_left(kept.left.replace(left));
                }
                Some(kept)
            }
        }
    }

    fn read(&self) -> Result<AuthFile, CodexCredentialError> {
        self.read_document().and_then(Self::auth_file)
    }

    fn read_document(&self) -> Result<Map<String, Value>, CodexCredentialError> {
        let bytes = match std::fs::read(self.file_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CodexCredentialError::NotSignedIn);
            }
            Err(error) => return Err(CodexCredentialError::Malformed(error.to_string())),
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(document)) => Ok(document),
            _ => Err(CodexCredentialError::Malformed(
                "not a JSON object".to_owned(),
            )),
        }
    }

    fn auth_file(document: Map<String, Value>) -> Result<AuthFile, CodexCredentialError> {
        let credentials = Self::credentials_in(&document)?;
        Ok(AuthFile {
            document,
            credentials,
        })
    }

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
            // The CLI may have rotated the token; use its credentials if fit
            // to send, else try once more.
            let latest = self.latest()?;
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
        // No client: treated like a dropped connection.
        let http = transport::client(&self.http)
            .map_err(|error| CodexCredentialError::RefreshFailed(error.to_string()))?;
        let response = http
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
            // Decided on the code as sent; only the detail is redacted.
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
            // Redacted before and after the case fold.
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
        self.write_back(file, refreshed)
            .map_err(RefreshFailure::from)
    }

    /// Puts `refreshed` over what the file holds now, so keys the CLI wrote
    /// meanwhile survive. The file decides as in [`Self::latest`].
    fn write_back(
        &self,
        file: AuthFile,
        refreshed: RefreshResponse,
    ) -> Result<CodexCredentials, CodexCredentialError> {
        let current = self.read_document();
        let mut slot = self.lock_kept();
        let expected = match slot.as_ref() {
            Some(kept) => kept.replaces.as_str(),
            None => file.credentials.refresh_token.as_str(),
        };
        let (mut document, write) = match current {
            Ok(mut document) if refresh_token_in(&document) == Some(expected) => {
                if slot.is_some()
                    && let Some(tokens) = file.document.get("tokens")
                {
                    // The kept tokens, so a missing id token stays the kept one.
                    document.insert("tokens".to_owned(), tokens.clone());
                }
                (document, true)
            }
            Err(CodexCredentialError::Malformed(_)) => (file.document, false),
            other => {
                if let Some(earlier) = slot.take() {
                    remove_left(earlier.left);
                }
                return other.and_then(Self::auth_file).map(|auth| auth.credentials);
            }
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
        let auth = Self::auth_file(document)?;
        let credentials = auth.credentials.clone();
        // A failed or skipped write keeps the tokens for the next call.
        let (replaces, left) = match slot.take() {
            Some(earlier) => (earlier.replaces, earlier.left),
            None => (file.credentials.refresh_token, None),
        };
        let kept = Kept {
            auth,
            replaces,
            left,
        };
        *slot = if write {
            self.write_kept(kept)
        } else {
            Some(kept)
        };
        Ok(credentials)
    }

    /// reqwest's message plus every source, redacted.
    fn refresh_failed(error: &reqwest::Error, secrets: &[String]) -> CodexCredentialError {
        CodexCredentialError::RefreshFailed(transport::redact(
            &transport::error_chain(error),
            secrets,
        ))
    }

    /// Atomic write: a 0600 temp file beside the target, synced, renamed
    /// then the folder synced. A failed
    /// rename leaves the temporary for a later write to remove.
    fn write(&self, document: &Map<String, Value>) -> Result<(), WriteFailure> {
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
            file.write_all(text.as_bytes())?;
            // A failed sync still beats the old file, whose token is spent.
            let _ = file.sync_all();
            Ok(())
        });
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temporary);
            return Err(WriteFailure {
                detail: format!("could not write the Codex sign-in file: {error}"),
                left: None,
            });
        }
        // A `codex login` between the read and this rename is replaced; only
        // a file lock would close that window.
        if let Err(error) = std::fs::rename(&temporary, self.file_path()) {
            return Err(WriteFailure {
                detail: format!(
                    "could not replace the Codex sign-in file: {error}; the new sign-in is left in {}",
                    temporary.display()
                ),
                left: Some(temporary),
            });
        }
        // Sync the folder so the rename survives a power loss (not on Windows).
        #[cfg(unix)]
        if let Ok(folder) = std::fs::File::open(&self.home) {
            let _ = folder.sync_all();
        }
        Ok(())
    }
}

fn remove_left(path: Option<PathBuf>) {
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
}

fn refresh_token_in(document: &Map<String, Value>) -> Option<&str> {
    document.get("tokens")?.get("refresh_token")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store whose `auth.json` is a non-empty folder, so every rename fails.
    fn store_over_an_occupied_file() -> (tempfile::TempDir, CodexCredentialStore) {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join("auth.json");
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("occupied"), b"x").unwrap();
        let store = CodexCredentialStore::new(home.path());
        (home, store)
    }

    fn document(refresh_token: &str) -> Map<String, Value> {
        let tokens = serde_json::json!({
            "access_token": "at",
            "refresh_token": refresh_token,
            "account_id": "acct",
        });
        Map::from_iter([("tokens".to_owned(), tokens)])
    }

    fn kept(refresh_token: &str) -> Kept {
        Kept {
            auth: CodexCredentialStore::auth_file(document(refresh_token)).unwrap(),
            replaces: "rt_1".to_owned(),
            left: None,
        }
    }

    fn leftovers(home: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(home)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().contains(".auth.json.steno-"))
            .collect()
    }

    /// A write-back whose rename fails keeps the tokens to write again and
    /// leaves one temporary; the write that lands removes it.
    #[test]
    fn a_rename_that_keeps_failing_leaves_only_the_newest_temporary() {
        let (home, store) = store_over_an_occupied_file();
        let first = store.write_kept(kept("rt_2")).expect("the rename fails");
        let first_left = first.left.clone().expect("the temporary stays");
        assert_eq!(leftovers(home.path()), std::slice::from_ref(&first_left));

        let second = store
            .write_kept(Kept {
                auth: kept("rt_3").auth,
                ..first
            })
            .expect("the rename fails");
        let second_left = second.left.clone().expect("the temporary stays");
        assert_ne!(first_left, second_left);
        assert_eq!(leftovers(home.path()), std::slice::from_ref(&second_left));
        assert!(
            std::fs::read_to_string(&second_left)
                .unwrap()
                .contains("rt_3")
        );

        std::fs::remove_dir_all(store.file_path()).unwrap();
        assert!(store.write_kept(second).is_none(), "the write lands");
        assert_eq!(leftovers(home.path()), Vec::<PathBuf>::new());
        assert!(
            std::fs::read_to_string(store.file_path())
                .unwrap()
                .contains("rt_3")
        );
    }
}
