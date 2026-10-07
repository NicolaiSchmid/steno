//! The Codex credential store over a temporary home and a stub token
//! endpoint: reading the CLI's file, the refresh windows, the atomic
//! write-back that keeps unknown keys, the re-read on a reused token, the
//! shared refresh for concurrent callers and the redaction of both tokens
//! and the account id.
//! Swift: `CodexCredentialStoreTests`, `CodexCredentialStoreConcurrencyTests`.

mod common;

use std::ffi::{OsStr, OsString};
use std::sync::Arc;

use common::*;
use serde_json::json;
use steno_llm::testing::{StubResponse, scripts};
use steno_llm::{CodexCredentialError, CodexCredentialStore, JwtClaims};

fn minutes(n: i64) -> chrono::TimeDelta {
    chrono::TimeDelta::minutes(n)
}

fn days(n: i64) -> chrono::TimeDelta {
    chrono::TimeDelta::days(n)
}

fn refresh_body(request: &steno_llm::testing::RecordedRequest) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn reads_the_file_the_way_the_cli_writes_it() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default());
    let credentials = home.store().stored().unwrap();
    assert_eq!(
        credentials.access_token,
        CodexHome::access_token(3_600, "plus")
    );
    assert_eq!(credentials.refresh_token, "rt_original");
    assert_eq!(
        credentials.account_id, "acct_stored",
        "the stored id wins over the claim"
    );
    assert_eq!(credentials.email.as_deref(), Some("nicolai@example.com"));
    assert_eq!(credentials.plan_type.as_deref(), Some("plus"));
    assert_eq!(credentials.expires_at, Some(codex_now() + minutes(60)));
    assert_eq!(credentials.last_refresh, Some(codex_now() - minutes(60)));
    assert_eq!(credentials.account_line(), "nicolai@example.com (Plus)");
    // Fresh: no network, no write.
    let before = std::fs::read(home.file()).unwrap();
    assert_eq!(home.store().current().await.unwrap(), credentials);
    assert_eq!(std::fs::read(home.file()).unwrap(), before);
    assert_eq!(home.server.requests().len(), 0);
}

#[tokio::test]
async fn the_debug_form_redacts_both_tokens_and_the_account_id() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default());
    let credentials = home.store().stored().unwrap();
    let debug = format!("{credentials:?}");
    for secret in credentials.secrets() {
        assert!(!debug.contains(&secret), "{debug}");
    }
    assert!(debug.contains("[redacted]"));
    assert!(debug.contains("nicolai@example.com"), "{debug}");
    assert!(debug.contains("plus"), "{debug}");
}

#[tokio::test]
async fn account_id_falls_back_to_the_id_token_claim() {
    let home = CodexHome::new().await;
    home.write(AuthFile {
        account_id: None,
        ..AuthFile::default()
    });
    assert_eq!(home.store().stored().unwrap().account_id, "acct_jwt");
}

#[tokio::test]
async fn missing_file_api_key_login_and_malformed_files_are_told() {
    let home = CodexHome::new().await;
    assert_eq!(
        home.store().stored().unwrap_err(),
        CodexCredentialError::NotSignedIn
    );
    std::fs::write(home.file(), r#"{"OPENAI_API_KEY":"sk-x","tokens":null}"#).unwrap();
    assert_eq!(
        home.store().stored().unwrap_err(),
        CodexCredentialError::ApiKeyLogin
    );
    home.write(AuthFile {
        auth_mode: Some("apikey".to_owned()),
        ..AuthFile::default()
    });
    assert_eq!(
        home.store().stored().unwrap_err(),
        CodexCredentialError::ApiKeyLogin
    );
    std::fs::write(home.file(), "not json").unwrap();
    let error = home.store().stored().unwrap_err();
    assert!(
        matches!(error, CodexCredentialError::Malformed(_)),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        "The Codex sign-in file could not be read."
    );
    assert_eq!(
        CodexCredentialError::NotSignedIn.to_string(),
        "No Codex sign-in found. Run `codex login` in Terminal, then try again."
    );
    assert_eq!(CodexCredentialError::NotSignedIn.detail(), None);
}

#[tokio::test]
async fn refreshes_near_expiry_and_writes_back_preserving_unknown_keys() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(120, "plus")));
    let new_access = CodexHome::access_token(3_600, "pro");
    home.server.enqueue([scripts.token_refresh(
        &new_access,
        Some("rt_rotated"),
        Some(&CodexHome::id_token("nicolai@example.com", "pro")),
    )]);

    let credentials = home.store().current().await.unwrap();
    assert_eq!(credentials.access_token, new_access);
    assert_eq!(credentials.refresh_token, "rt_rotated");
    assert_eq!(credentials.plan_type.as_deref(), Some("pro"));
    assert_eq!(credentials.last_refresh, Some(codex_now()));

    let request = &home.server.requests()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/oauth/token");
    assert_eq!(
        refresh_body(request),
        json!({"grant_type": "refresh_token", "client_id": "app_test", "refresh_token": "rt_original"})
    );
    assert_eq!(
        request.headers.get("user-agent").map(String::as_str),
        Some(steno_llm::USER_AGENT)
    );

    let document = home.document();
    let tokens = document["tokens"].as_object().unwrap();
    assert_eq!(tokens["refresh_token"], "rt_rotated");
    assert_eq!(tokens["access_token"], json!(new_access));
    assert_eq!(tokens["account_id"], "acct_stored", "untouched keys stay");
    assert_eq!(
        document["agent_identity"],
        json!({"keep": true}),
        "unknown keys survive"
    );
    assert_eq!(document["auth_mode"], "chatgpt");
    assert!(document.contains_key("OPENAI_API_KEY"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let entries = file_names(home.directory.path());
    assert_eq!(entries, ["auth.json"], "no temp file left behind");
}

#[tokio::test]
async fn a_stale_file_is_refreshed_even_with_a_live_token() {
    let home = CodexHome::new().await;
    home.write(AuthFile {
        last_refresh: Some(codex_now() - days(9)),
        ..AuthFile::default()
    });
    home.server.enqueue([scripts.token_refresh(
        &CodexHome::access_token(3_600, "plus"),
        Some("rt_2"),
        None,
    )]);
    assert_eq!(home.store().current().await.unwrap().refresh_token, "rt_2");
    assert_eq!(home.server.request_count(), 1);
}

#[tokio::test]
async fn a_spent_refresh_token_means_sign_in_again() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    home.server
        .enqueue([scripts.token_refresh_rejected("refresh_token_expired", 400)]);
    let error = home.store().current().await.unwrap_err();
    let CodexCredentialError::SignInExpired(detail) = &error else {
        panic!("expected SignInExpired, got {error}");
    };
    assert!(detail.contains("refresh_token_expired"));
    assert!(
        !detail.contains("rt_original"),
        "the refresh token never lands in an error"
    );
    assert!(error.to_string().contains("codex login"));
    // The file is untouched by a failed refresh.
    assert_eq!(home.store().stored().unwrap().refresh_token, "rt_original");
}

#[tokio::test]
async fn a_reused_token_rereads_the_file_the_cli_may_have_rotated() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    let file = home.file();
    home.server.respond(Arc::new(move |request| {
        if refresh_body(request)["refresh_token"] == "rt_original" {
            // Simulate the CLI having refreshed in between: it wrote a new
            // token.
            write_auth(
                &file,
                AuthFile::default()
                    .access(&CodexHome::access_token(10, "plus"))
                    .refresh("rt_from_cli"),
            );
            return Some(scripts.token_refresh_rejected("refresh_token_reused", 400));
        }
        Some(scripts.token_refresh(&fresh, Some("rt_after_cli"), None))
    }));
    let credentials = home.store().current().await.unwrap();
    assert_eq!(credentials.refresh_token, "rt_after_cli");
    assert_eq!(home.server.request_count(), 2);
}

#[tokio::test]
async fn a_transient_refresh_failure_is_not_a_sign_in_problem() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    home.server.enqueue([scripts.server_error(503)]);
    let error = home.store().current().await.unwrap_err();
    assert!(
        matches!(error, CodexCredentialError::RefreshFailed(_)),
        "{error}"
    );
}

/// `CODEX_HOME` as given, else `~/.codex`.
fn home_for(codex_home: Option<&str>) -> std::path::PathBuf {
    home_for_os(codex_home.map(OsStr::new))
}

/// [`home_for`] with a value that need not be Unicode.
fn home_for_os(codex_home: Option<&OsStr>) -> std::path::PathBuf {
    CodexCredentialStore::default_home(|name| {
        assert_eq!(name, "CODEX_HOME");
        codex_home.map(OsString::from)
    })
}

#[test]
fn default_home_honours_codex_home() {
    assert_eq!(
        home_for(Some("/tmp/elsewhere")),
        std::path::Path::new("/tmp/elsewhere")
    );
    assert_eq!(home_for(None).file_name().unwrap(), ".codex");
    // A trailing slash, as a shell export often has, does not double up;
    // an empty override is no override.
    let slashed = home_for(Some("/tmp/elsewhere/"));
    assert_eq!(
        CodexCredentialStore::new(slashed).file_path(),
        std::path::Path::new("/tmp/elsewhere/auth.json")
    );
    assert_eq!(home_for(Some("")).file_name().unwrap(), ".codex");
}

/// A `CODEX_HOME` that is not Unicode (a Latin-1 byte on Unix, an
/// unpaired surrogate on Windows) is the home as it is, not `~/.codex`.
#[test]
fn a_codex_home_that_is_not_unicode_is_used_as_it_is() {
    #[cfg(unix)]
    let home = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(b"/tmp/caf\xE9".to_vec())
    };
    #[cfg(windows)]
    let home = {
        use std::os::windows::ffi::OsStringExt;
        let mut wide: Vec<u16> = r"C:\Users\caf".encode_utf16().collect();
        wide.push(0xD800);
        OsString::from_wide(&wide)
    };
    assert!(home.to_str().is_none());
    assert_eq!(home_for_os(Some(&home)), std::path::PathBuf::from(home));
}

/// Older CLI files have no `auth_mode`; tokens alone mean a `ChatGPT` login.
#[tokio::test]
async fn a_file_without_auth_mode_is_a_chatgpt_login() {
    let home = CodexHome::new().await;
    home.write(AuthFile {
        auth_mode: None,
        ..AuthFile::default()
    });
    let credentials = home.store().stored().unwrap();
    assert_eq!(credentials.account_id, "acct_stored");
    assert_eq!(credentials.email.as_deref(), Some("nicolai@example.com"));
    // The mode is compared without regard to case.
    home.write(AuthFile {
        auth_mode: Some("ChatGPT".to_owned()),
        ..AuthFile::default()
    });
    assert_eq!(home.store().stored().unwrap().account_id, "acct_stored");
}

#[tokio::test]
async fn missing_or_empty_tokens_and_no_account_id_are_malformed() {
    let home = CodexHome::new().await;
    home.write(AuthFile {
        id: None,
        account_id: None,
        ..AuthFile::default()
    });
    assert_eq!(
        home.store().stored().unwrap_err(),
        CodexCredentialError::Malformed("no account id".to_owned())
    );
    // An id token without the account claim is as good as none, and an
    // empty stored id does not shadow the claim.
    home.write(AuthFile {
        id: Some(JwtClaims::unsigned_token(&json!({"email": "a@b.c"}))),
        account_id: None,
        ..AuthFile::default()
    });
    assert_eq!(
        home.store().stored().unwrap_err(),
        CodexCredentialError::Malformed("no account id".to_owned())
    );
    home.write(AuthFile {
        account_id: Some(String::new()),
        ..AuthFile::default()
    });
    assert_eq!(home.store().stored().unwrap().account_id, "acct_jwt");
    home.write(AuthFile::default().access(""));
    assert_eq!(
        home.store().stored().unwrap_err(),
        CodexCredentialError::Malformed("no access token".to_owned())
    );
    home.write(AuthFile::default().refresh(""));
    assert_eq!(
        home.store().stored().unwrap_err(),
        CodexCredentialError::Malformed("no refresh token".to_owned())
    );
    // An access token that is not a JWT still works: no expiry, no plan.
    home.write(AuthFile::default().access("opaque-token"));
    let opaque = home.store().stored().unwrap();
    assert_eq!(opaque.expires_at, None);
    assert_eq!(
        opaque.plan_type.as_deref(),
        Some("plus"),
        "from the id token"
    );
    assert_eq!(
        home.store().current().await.unwrap(),
        opaque,
        "never refreshed by expiry"
    );
    assert_eq!(home.server.requests().len(), 0);
}

/// `needs_refresh` at its edges: no `last_refresh` (a file the CLI wrote
/// at login and never refreshed) is not stale; exactly eight days is not
/// stale; the token is refreshed strictly inside five minutes of `exp`.
#[tokio::test]
async fn a_file_never_refreshed_is_not_stale_and_the_windows_are_exact() {
    let home = CodexHome::new().await;
    home.write(AuthFile {
        last_refresh: None,
        ..AuthFile::default()
    });
    let credentials = home.store().current().await.unwrap();
    assert_eq!(credentials.last_refresh, None);
    assert_eq!(home.server.requests().len(), 0);

    home.write(AuthFile {
        last_refresh: Some(codex_now() - days(8)),
        ..AuthFile::default()
    });
    home.store().current().await.unwrap();
    assert_eq!(
        home.server.requests().len(),
        0,
        "eight days is the limit, not past it"
    );

    home.write(AuthFile::default().access(&CodexHome::access_token(300, "plus")));
    home.store().current().await.unwrap();
    assert_eq!(
        home.server.requests().len(),
        0,
        "five minutes left is enough"
    );

    home.write(AuthFile::default().access(&CodexHome::access_token(299, "plus")));
    home.server.enqueue([scripts.token_refresh(
        &CodexHome::access_token(3_600, "plus"),
        Some("rt_2"),
        None,
    )]);
    assert_eq!(home.store().current().await.unwrap().refresh_token, "rt_2");
    assert_eq!(home.server.request_count(), 1);

    // An already-expired token is refreshed too, not rejected.
    home.write(AuthFile::default().access(&CodexHome::access_token(-3_600, "plus")));
    home.server.enqueue([scripts.token_refresh(
        &CodexHome::access_token(3_600, "plus"),
        Some("rt_3"),
        None,
    )]);
    assert_eq!(home.store().current().await.unwrap().refresh_token, "rt_3");
}

/// The token endpoint may answer without a rotated refresh token or id
/// token: the old ones stay in the file. And a `CODEX_HOME` holds more
/// than `auth.json`: the write-back leaves every other entry alone.
#[tokio::test]
async fn a_refresh_without_rotation_keeps_the_old_tokens_and_touches_only_auth_json() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let config = home.directory.path().join("config.toml");
    let config_bytes = b"model = \"gpt-5.6-terra\"\n";
    std::fs::write(&config, config_bytes).unwrap();
    let sessions = home.directory.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("rollout.jsonl"), "{}").unwrap();
    let fresh = CodexHome::access_token(3_600, "plus");
    home.server
        .enqueue([scripts.token_refresh(&fresh, None, None)]);

    let credentials = home.store().current().await.unwrap();
    assert_eq!(credentials.access_token, fresh);
    assert_eq!(credentials.refresh_token, "rt_original");
    assert_eq!(credentials.email.as_deref(), Some("nicolai@example.com"));
    assert_eq!(credentials.last_refresh, Some(codex_now()));
    let document = home.document();
    let tokens = document["tokens"].as_object().unwrap();
    assert_eq!(tokens["access_token"], json!(fresh));
    assert_eq!(tokens["refresh_token"], "rt_original");
    assert_eq!(
        tokens["id_token"],
        json!(CodexHome::id_token("nicolai@example.com", "plus"))
    );
    assert_eq!(tokens["account_id"], "acct_stored");
    let entries = file_names(home.directory.path());
    assert_eq!(entries, ["auth.json", "config.toml", "sessions"]);
    assert_eq!(std::fs::read(&config).unwrap(), config_bytes);
    let session_entries = file_names(&sessions);
    assert_eq!(session_entries, ["rollout.jsonl"]);
    // The next read sees the written file, so no second refresh.
    assert_eq!(home.store().current().await.unwrap(), credentials);
    assert_eq!(home.server.request_count(), 1);
}

/// The token endpoint's other answers: a 401 without a known code is a dead
/// sign-in, a 200 that is not JSON and a cut connection are transient, and
/// none of them names the refresh token or changes the file.
#[tokio::test]
async fn other_token_endpoint_answers_are_classified_and_redacted() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let before = std::fs::read(home.file()).unwrap();

    home.server.enqueue([StubResponse::json(
        &json!({"error": "unauthorized", "error_description": "token rt_original rejected"}),
        401,
    )]);
    let unauthorized = home.store().current().await.unwrap_err();
    assert_eq!(
        unauthorized,
        CodexCredentialError::SignInExpired(
            "unauthorized: HTTP 401: token [redacted] rejected".to_owned()
        )
    );

    home.server
        .enqueue([scripts.raw_completion("<html>gateway</html>")]);
    assert_eq!(
        home.store().current().await.unwrap_err(),
        CodexCredentialError::RefreshFailed("undecodable token response".to_owned())
    );

    home.server.enqueue([StubResponse::drop_connection()]);
    let dropped = home.store().current().await.unwrap_err();
    let CodexCredentialError::RefreshFailed(message) = &dropped else {
        panic!("expected RefreshFailed, got {dropped}");
    };
    assert!(!message.contains("rt_original"));

    // A permanent code on an unexpected status is still permanent.
    home.server
        .enqueue([scripts.token_refresh_rejected("refresh_token_invalidated", 403)]);
    let invalidated = home.store().current().await.unwrap_err();
    assert!(
        matches!(invalidated, CodexCredentialError::SignInExpired(_)),
        "{invalidated}"
    );
    assert_eq!(
        std::fs::read(home.file()).unwrap(),
        before,
        "a failed refresh never writes"
    );
    assert_eq!(home.server.request_count(), 4);
}

/// A reused-token answer when the file has not changed is final: one
/// re-read, no second request.
#[tokio::test]
async fn a_reused_token_with_an_unchanged_file_is_final() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    home.server
        .enqueue([scripts.token_refresh_rejected("refresh_token_reused", 400)]);
    let error = home.store().current().await.unwrap_err();
    let CodexCredentialError::SignInExpired(detail) = &error else {
        panic!("expected SignInExpired, got {error}");
    };
    assert!(detail.contains("refresh_token_reused"));
    assert_eq!(home.server.request_count(), 1);
}

#[test]
fn jwt_claims_decode_base64url_without_padding() {
    let token = CodexHome::id_token("a@b.c", "team");
    assert_eq!(JwtClaims::email(&token).as_deref(), Some("a@b.c"));
    assert_eq!(JwtClaims::plan_type(&token).as_deref(), Some("team"));
    assert_eq!(JwtClaims::account_id(&token).as_deref(), Some("acct_jwt"));
    assert_eq!(JwtClaims::expiry(&token), None);
    assert_eq!(JwtClaims::payload("not.a"), None);
    assert_eq!(JwtClaims::payload("a.!!!.c"), None);
    let profile = JwtClaims::unsigned_token(&json!({
        JwtClaims::OPENAI_PROFILE_CLAIM: {"email": "p@q.r"}
    }));
    assert_eq!(JwtClaims::email(&profile).as_deref(), Some("p@q.r"));
}

/// Two chunks arriving while the token is inside its window must not both
/// spend the same refresh token; the real endpoint rejects the second.
#[tokio::test]
async fn concurrent_callers_share_one_refresh() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    home.server.respond(Arc::new(move |request| {
        Some(if request.index == 0 {
            scripts.token_refresh(&fresh, Some("rt_2"), None)
        } else {
            scripts.token_refresh_rejected("refresh_token_reused", 400)
        })
    }));
    let store = Arc::new(home.store());
    let stale = CodexHome::access_token(10, "plus");
    let (a, b, c) = tokio::join!(
        store.current(),
        store.current(),
        store.refreshed_if_still_using(&stale)
    );
    let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
    assert_eq!(a, b);
    assert_eq!(b, c);
    assert_eq!(a.refresh_token, "rt_2");
    assert_eq!(home.server.request_count(), 1);
    // Once done, the next caller reads the file and needs nothing.
    assert_eq!(
        store.current().await.unwrap().access_token,
        CodexHome::access_token(3_600, "plus")
    );
    assert_eq!(home.server.request_count(), 1);
}

/// `codex logout` during the round trip removes the file; the write-back
/// must not recreate it with the new tokens. A file that is merely
/// unreadable at that moment keeps the copy read before as its base.
#[tokio::test]
async fn a_sign_out_during_the_refresh_is_not_undone_by_the_write_back() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    let file = home.file();
    home.server.respond(Arc::new(move |_| {
        std::fs::remove_file(&file).unwrap();
        Some(scripts.token_refresh(&fresh, Some("rt_2"), None))
    }));
    assert_eq!(
        home.store().current().await.unwrap_err(),
        CodexCredentialError::NotSignedIn
    );
    assert!(!home.file().exists(), "the write-back recreated auth.json");
    assert_eq!(home.server.request_count(), 1);

    let second = CodexHome::new().await;
    second.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    let file = second.file();
    second.server.respond(Arc::new(move |_| {
        std::fs::write(&file, b"{half-written").unwrap();
        Some(scripts.token_refresh(&fresh, Some("rt_2"), None))
    }));
    let credentials = second.store().current().await.unwrap();
    assert_eq!(credentials.refresh_token, "rt_2");
    assert_eq!(second.document()["agent_identity"], json!({"keep": true}));
}

/// Once the endpoint has turned a refresh token down for good, the callers
/// that waited for that refresh get the same answer from the lock, not from
/// the network: the dead token is posted once. Only a token the CLI
/// rotated since is tried again.
#[tokio::test]
async fn a_permanent_refusal_is_shared_with_the_waiting_callers() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    home.server.respond(Arc::new(|_| {
        Some(scripts.token_refresh_rejected("refresh_token_expired", 400))
    }));
    let store = Arc::new(home.store());
    let stale = CodexHome::access_token(10, "plus");
    let (a, b, c) = tokio::join!(
        store.current(),
        store.current(),
        store.refreshed_if_still_using(&stale)
    );
    for outcome in [a, b, c] {
        let error = outcome.unwrap_err();
        assert!(
            matches!(error, CodexCredentialError::SignInExpired(_)),
            "{error}"
        );
    }
    assert_eq!(home.server.request_count(), 1, "one POST for three callers");
    // A later caller over the same file: still no round trip.
    assert!(matches!(
        store.current().await.unwrap_err(),
        CodexCredentialError::SignInExpired(_)
    ));
    assert_eq!(home.server.request_count(), 1);
    // The CLI signs in again: the new token is tried.
    home.write(
        AuthFile::default()
            .access(&CodexHome::access_token(10, "plus"))
            .refresh("rt_after_login"),
    );
    let _ = store.current().await;
    assert_eq!(home.server.request_count(), 2);
    assert_eq!(
        refresh_body(&home.server.requests()[1])["refresh_token"],
        "rt_after_login"
    );
}

/// `{"error": {"code": …}}`, the shape the endpoint uses beside the flat
/// one: a permanent code on a 400 is final, and a reused code triggers the
/// re-read.
#[tokio::test]
async fn nested_error_codes_are_permanent_and_trigger_the_reread() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    home.server.enqueue([StubResponse::new(
        400,
        br#"{"error":{"code":"refresh_token_expired","message":"gone"}}"#.to_vec(),
    )
    .with_header("Content-Type", "application/json")]);
    let error = home.store().current().await.unwrap_err();
    let CodexCredentialError::SignInExpired(detail) = &error else {
        panic!("expected SignInExpired, got {error}");
    };
    assert!(
        detail.starts_with("refresh_token_expired: HTTP 400: gone"),
        "{detail}"
    );
    assert!(
        !error.to_string().contains("refresh_token"),
        "no wire code on screen"
    );

    let second = CodexHome::new().await;
    second.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    let file = second.file();
    second.server.respond(Arc::new(move |request| {
        if refresh_body(request)["refresh_token"] == "rt_original" {
            write_auth(
                &file,
                AuthFile::default()
                    .access(&CodexHome::access_token(10, "plus"))
                    .refresh("rt_from_cli"),
            );
            return Some(
                StubResponse::new(
                    400,
                    br#"{"error":{"code":"refresh_token_reused"}}"#.to_vec(),
                )
                .with_header("Content-Type", "application/json"),
            );
        }
        Some(scripts.token_refresh(&fresh, Some("rt_after_cli"), None))
    }));
    assert_eq!(
        second.store().current().await.unwrap().refresh_token,
        "rt_after_cli"
    );
    assert_eq!(second.server.request_count(), 2);
}

/// A token the endpoint echoes inside its error code never reaches the
/// detail either.
#[tokio::test]
async fn error_codes_are_redacted_too() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    home.server.enqueue([
        StubResponse::new(401, br#"{"error":"bad rt_original"}"#.to_vec())
            .with_header("Content-Type", "application/json"),
    ]);
    let error = home.store().current().await.unwrap_err();
    let detail = error.detail().unwrap_or("");
    assert!(!detail.contains("rt_original"));
    assert!(detail.contains("[redacted]"));
}

/// The permanent and reused decisions read the code as sent: an account id
/// that happens to occur in it is redacted from the detail only.
#[tokio::test]
async fn a_secret_inside_the_error_code_changes_no_decision() {
    for code in ["invalid_grant", "refresh_token_reused"] {
        let home = CodexHome::new().await;
        let mut auth = AuthFile::default().access(&CodexHome::access_token(10, "plus"));
        auth.account_id = Some(code.to_owned());
        home.write(auth);
        home.server
            .enqueue([scripts.token_refresh_rejected(code, 400)]);
        let error = home.store().current().await.unwrap_err();
        let CodexCredentialError::SignInExpired(detail) = &error else {
            panic!("{code}: expected SignInExpired, got {error:?}");
        };
        assert!(!detail.contains(code), "{detail}");
        assert!(detail.starts_with("[redacted]: HTTP 400"), "{detail}");
        assert_eq!(home.server.request_count(), 1, "{code}");
    }
}

/// A refresh that cannot reach the token endpoint names the cause, not only
/// reqwest's "error sending request".
#[tokio::test]
async fn a_refresh_that_cannot_connect_names_the_cause() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    // Port 1 (tcpmux) is closed on every runner: the connection is refused.
    let store = home
        .store()
        .with_token_endpoint(url::Url::parse("http://127.0.0.1:1/oauth/token").unwrap());
    let error = store.current().await.unwrap_err();
    let CodexCredentialError::RefreshFailed(detail) = &error else {
        panic!("expected RefreshFailed, got {error:?}");
    };
    assert!(detail.starts_with("error sending request"), "{detail}");
    assert!(detail.contains("Connect"), "the cause is kept: {detail}");
}

/// Answers the refresh with `access` and `rt_2` after turning `auth.json`
/// into a non-empty directory, so the rename of the write-back fails.
#[cfg(unix)]
fn occupy_the_file_during_the_refresh(home: &CodexHome, access: &str) {
    let file = home.file();
    let access = access.to_owned();
    home.server.respond(Arc::new(move |_| {
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("occupied"), b"x").unwrap();
        Some(scripts.token_refresh(&access, Some("rt_2"), None))
    }));
}

/// A rename that fails still answers with the new tokens, which the store
/// keeps in memory, and leaves the temporary file with them in it in case
/// the app quits first. Once the file can be written again, the next
/// call writes the kept tokens, removes the temporary and posts nothing:
/// the posted refresh token is spent.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_rename_keeps_the_new_sign_in_and_writes_it_on_the_next_call() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let original = std::fs::read(home.file()).unwrap();
    let fresh = CodexHome::access_token(3_600, "plus");
    occupy_the_file_during_the_refresh(&home, &fresh);
    let store = home.store();
    let credentials = store.current().await.unwrap();
    assert_eq!(credentials.access_token, fresh);
    assert_eq!(credentials.refresh_token, "rt_2");
    let entries = file_names(home.directory.path());
    assert_eq!(entries.len(), 2, "{entries:?}");
    let temporary = entries
        .iter()
        .find(|name| name.starts_with(".auth.json.steno-"))
        .expect("the temporary file stays");
    let kept: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.directory.path().join(temporary)).unwrap())
            .unwrap();
    assert_eq!(kept["tokens"]["refresh_token"], "rt_2");
    assert_eq!(kept["tokens"]["access_token"], json!(fresh));

    // The file is back as it was, with the spent token.
    std::fs::remove_dir_all(home.file()).unwrap();
    std::fs::write(home.file(), &original).unwrap();
    let again = store.current().await.unwrap();
    assert_eq!(again.refresh_token, "rt_2");
    assert_eq!(
        home.server.request_count(),
        1,
        "the spent token is not posted"
    );
    assert_eq!(home.document()["tokens"]["refresh_token"], "rt_2");
    assert_eq!(home.document()["agent_identity"], json!({"keep": true}));
    assert_eq!(file_names(home.directory.path()), ["auth.json"]);
}

/// A sign-in file that cannot be written at all (the folder is read-only)
/// leaves no temporary behind; the new tokens are answered and kept in
/// memory, and once the folder is writable the next call writes them and
/// posts nothing.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_write_keeps_the_new_sign_in_and_writes_it_on_the_next_call() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let directory = home.directory.path().to_owned();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::File::create(directory.join("probe")).is_ok() {
        // Root writes past the mode; nothing to test.
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let fresh = CodexHome::access_token(3_600, "plus");
    home.server
        .enqueue([scripts.token_refresh(&fresh, Some("rt_2"), None)]);
    let store = home.store();
    let credentials = store.current().await.unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(credentials.access_token, fresh);
    assert_eq!(credentials.refresh_token, "rt_2");
    assert_eq!(
        home.document()["tokens"]["refresh_token"],
        "rt_original",
        "nothing was written"
    );
    assert_eq!(file_names(&directory), ["auth.json"]);

    let again = store.current().await.unwrap();
    assert_eq!(again.access_token, fresh);
    assert_eq!(
        home.server.request_count(),
        1,
        "the spent token is not posted"
    );
    assert_eq!(home.document()["tokens"]["refresh_token"], "rt_2");
    assert_eq!(home.document()["tokens"]["access_token"], json!(fresh));
}

/// A file the user changed after a failed write-back (a new `codex
/// login`) wins over the tokens kept in memory, and the temporary they
/// left goes.
#[cfg(unix)]
#[tokio::test]
async fn a_new_login_after_a_failed_write_wins_over_the_kept_sign_in() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    occupy_the_file_during_the_refresh(&home, &fresh);
    let store = home.store();
    assert_eq!(store.current().await.unwrap().refresh_token, "rt_2");
    std::fs::remove_dir_all(home.file()).unwrap();
    home.write(AuthFile::default().refresh("rt_new_login"));
    assert_eq!(store.current().await.unwrap().refresh_token, "rt_new_login");
    assert_eq!(home.server.request_count(), 1);
    assert_eq!(file_names(home.directory.path()), ["auth.json"]);
}

/// What the CLI wrote during the round trip survives the write-back.
#[tokio::test]
async fn write_back_overlays_the_files_latest_contents() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    let fresh_for_responder = fresh.clone();
    let file = home.file();
    home.server.respond(Arc::new(move |_| {
        write_auth(
            &file,
            AuthFile {
                extra: vec![
                    ("written_by_cli".to_owned(), json!("yes")),
                    ("agent_identity".to_owned(), json!({"keep": true})),
                ],
                ..AuthFile::default().access(&CodexHome::access_token(10, "plus"))
            },
        );
        Some(scripts.token_refresh(&fresh_for_responder, Some("rt_2"), None))
    }));
    assert_eq!(home.store().current().await.unwrap().refresh_token, "rt_2");
    let document = home.document();
    assert_eq!(document["written_by_cli"], "yes");
    assert_eq!(document["agent_identity"], json!({"keep": true}));
    assert_eq!(document["tokens"]["access_token"], json!(fresh));
}

/// A 401 after the CLI rotated the file: the file wins, no network.
#[tokio::test]
async fn the_unauthorized_path_trusts_a_rotated_file() {
    let home = CodexHome::new().await;
    let rotated = CodexHome::access_token(3_600, "pro");
    home.write(AuthFile::default().access(&rotated).refresh("rt_cli"));
    let credentials = home
        .store()
        .refreshed_if_still_using("stale-token")
        .await
        .unwrap();
    assert_eq!(credentials.access_token, rotated);
    assert_eq!(credentials.refresh_token, "rt_cli");
    assert_eq!(home.server.requests().len(), 0);
}

/// `codex login --api-key` during the round trip: the write-back must not
/// put the old sign-in tokens over the new key.
#[tokio::test]
async fn an_api_key_login_during_the_refresh_is_not_overwritten() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    let file = home.file();
    let api_key_login = br#"{"OPENAI_API_KEY":"sk-new","tokens":null}"#;
    home.server.respond(Arc::new(move |_| {
        std::fs::write(&file, api_key_login).unwrap();
        Some(scripts.token_refresh(&fresh, Some("rt_2"), None))
    }));
    assert_eq!(
        home.store().current().await.unwrap_err(),
        CodexCredentialError::ApiKeyLogin
    );
    assert_eq!(std::fs::read(home.file()).unwrap(), api_key_login);
}

/// A new `codex login` (or a CLI refresh) during the round trip wrote a
/// different refresh token: that file is the answer, left as it stands.
#[tokio::test]
async fn a_new_login_during_the_refresh_is_returned_and_left_alone() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let fresh = CodexHome::access_token(3_600, "plus");
    let from_login = CodexHome::access_token(3_600, "pro");
    let file = home.file();
    let from_login_for_responder = from_login.clone();
    home.server.respond(Arc::new(move |_| {
        write_auth(
            &file,
            AuthFile::default()
                .access(&from_login_for_responder)
                .refresh("rt_new_login"),
        );
        Some(scripts.token_refresh(&fresh, Some("rt_2"), None))
    }));
    let credentials = home.store().current().await.unwrap();
    assert_eq!(credentials.refresh_token, "rt_new_login");
    assert_eq!(credentials.access_token, from_login);
    let stored = home.store().stored().unwrap();
    assert_eq!(stored, credentials, "no write-back over the new login");
    assert_eq!(stored.last_refresh, Some(codex_now() - minutes(60)));
    assert_eq!(home.server.request_count(), 1);
}

/// A reused-token answer when the CLI has already written a token fit to
/// send: that token is used, not spent on a second refresh.
#[tokio::test]
async fn a_reused_token_takes_the_clis_usable_tokens_without_refreshing_them() {
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access(&CodexHome::access_token(10, "plus")));
    let from_cli = CodexHome::access_token(3_600, "plus");
    let file = home.file();
    let from_cli_for_responder = from_cli.clone();
    home.server.respond(Arc::new(move |_| {
        write_auth(
            &file,
            AuthFile::default()
                .access(&from_cli_for_responder)
                .refresh("rt_from_cli"),
        );
        Some(scripts.token_refresh_rejected("refresh_token_reused", 400))
    }));
    let credentials = home.store().current().await.unwrap();
    assert_eq!(credentials.refresh_token, "rt_from_cli");
    assert_eq!(credentials.access_token, from_cli);
    assert_eq!(
        home.server.request_count(),
        1,
        "the CLI's token is not spent"
    );
}
