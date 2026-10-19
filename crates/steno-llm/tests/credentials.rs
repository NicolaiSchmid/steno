//! The Codex credential store over a temporary home and a stub token
//! endpoint. Steno shares `auth.json` with the Codex CLI and refresh tokens
//! rotate, so a lost write or a clobbered file signs the user out.

#![allow(clippy::type_complexity)] // the case tables spell out their tuples

mod common;

use std::sync::Arc;

use common::*;
use serde_json::json;
use steno_llm::testing::{StubResponse, scripts};
use steno_llm::{CodexCredentialError, CodexCredentialStore, JwtClaims};

fn minutes(n: i64) -> chrono::TimeDelta {
    chrono::TimeDelta::minutes(n)
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
    assert_eq!(credentials.expires_at, Some(codex_now() + minutes(60)));
    assert_eq!(credentials.last_refresh, Some(codex_now() - minutes(60)));
    assert_eq!(credentials.account_line(), "nicolai@example.com (Plus)");
    // Fresh: no network, no write.
    let before = std::fs::read(home.file()).unwrap();
    assert_eq!(home.store().current().await.unwrap(), credentials);
    assert_eq!(std::fs::read(home.file()).unwrap(), before);
    assert_eq!(home.server.request_count(), 0);

    let profile =
        JwtClaims::unsigned_token(&json!({JwtClaims::OPENAI_PROFILE_CLAIM: {"email": "p@q.r"}}));
    assert_eq!(JwtClaims::email(&profile).as_deref(), Some("p@q.r"));
    assert_eq!(JwtClaims::payload("a.!!!.c"), None);
}

#[tokio::test]
async fn sign_ins_steno_cannot_use_are_told_apart() {
    let api_key_login = || r#"{"OPENAI_API_KEY":"sk-x","tokens":null}"#.to_owned();
    let auth = |auth: AuthFile| {
        let file = tempfile::NamedTempFile::new().unwrap();
        write_auth(file.path(), auth);
        std::fs::read_to_string(file.path()).unwrap()
    };
    let malformed = |what: &str| Err(CodexCredentialError::Malformed(what.to_owned()));
    let cases: Vec<(&str, Option<String>, Result<&str, CodexCredentialError>)> = vec![
        ("no file", None, Err(CodexCredentialError::NotSignedIn)),
        (
            "API key",
            Some(api_key_login()),
            Err(CodexCredentialError::ApiKeyLogin),
        ),
        (
            "auth_mode apikey",
            Some(auth(AuthFile {
                auth_mode: Some("apikey".into()),
                ..AuthFile::default()
            })),
            Err(CodexCredentialError::ApiKeyLogin),
        ),
        (
            "not JSON",
            Some("not json".to_owned()),
            malformed("not a JSON object"),
        ),
        (
            "older CLI without auth_mode",
            Some(auth(AuthFile {
                auth_mode: None,
                ..AuthFile::default()
            })),
            Ok("acct_stored"),
        ),
        (
            "account id from the id token",
            Some(auth(AuthFile {
                account_id: None,
                ..AuthFile::default()
            })),
            Ok("acct_jwt"),
        ),
        (
            "empty account id",
            Some(auth(AuthFile {
                account_id: Some(String::new()),
                ..AuthFile::default()
            })),
            Ok("acct_jwt"),
        ),
        (
            "no account id",
            Some(auth(AuthFile {
                id: None,
                account_id: None,
                ..AuthFile::default()
            })),
            malformed("no account id"),
        ),
        (
            "no access token",
            Some(auth(AuthFile::default().access(""))),
            malformed("no access token"),
        ),
        (
            "no refresh token",
            Some(auth(AuthFile::default().refresh(""))),
            malformed("no refresh token"),
        ),
    ];
    for (what, contents, expected) in cases {
        let home = CodexHome::new().await;
        if let Some(contents) = contents {
            std::fs::write(home.file(), contents).unwrap();
        }
        let found = home.store().stored().map(|c| c.account_id);
        assert_eq!(found, expected.map(str::to_owned), "{what}");
    }
    assert_eq!(
        CodexCredentialError::Malformed("x".into()).to_string(),
        "The Codex sign-in file could not be read."
    );

    // An opaque access token has no expiry and is never refreshed by age.
    let home = CodexHome::new().await;
    home.write(AuthFile::default().access("opaque-token"));
    assert_eq!(home.store().current().await.unwrap().expires_at, None);
    assert_eq!(home.server.request_count(), 0);
}

#[test]
fn the_home_is_codex_home_or_dot_codex() {
    let home = |value: Option<&str>| {
        CodexCredentialStore::default_home(|name| {
            assert_eq!(name, "CODEX_HOME");
            value.map(Into::into)
        })
    };
    assert_eq!(
        home(Some("/tmp/elsewhere")),
        std::path::Path::new("/tmp/elsewhere")
    );
    assert_eq!(home(None).file_name().unwrap(), ".codex");
    assert_eq!(
        home(Some("")).file_name().unwrap(),
        ".codex",
        "empty is no override"
    );
}

/// Refreshed strictly inside five minutes of `exp`, or after more than
/// eight days since `last_refresh`.
#[tokio::test]
async fn the_refresh_windows_are_exact() {
    let days = chrono::TimeDelta::days;
    let cases = [
        (3_600, Some(days(1)), false),
        (300, Some(days(1)), false),
        (299, Some(days(1)), true),
        (-3_600, Some(days(1)), true),
        (3_600, Some(days(8)), false),
        (3_600, Some(days(9)), true),
        (3_600, None, false),
    ];
    for (expires_in, refreshed_ago, refreshes) in cases {
        let home = CodexHome::new().await;
        home.write(AuthFile {
            last_refresh: refreshed_ago.map(|ago| codex_now() - ago),
            ..AuthFile::default().access(&CodexHome::access_token(expires_in, "plus"))
        });
        home.server.enqueue([scripts.token_refresh(
            &CodexHome::access_token(3_600, "plus"),
            Some("rt_2"),
            None,
        )]);
        let credentials = home.store().current().await.unwrap();
        assert_eq!(
            home.server.request_count(),
            usize::from(refreshes),
            "{expires_in} {refreshed_ago:?}"
        );
        assert_eq!(credentials.refresh_token == "rt_2", refreshes);
    }
}

/// The write-back keeps every key Steno does not own, is 0600, leaves no
/// temporary and touches nothing else in `CODEX_HOME`; tokens the endpoint
/// did not rotate stay.
#[tokio::test]
async fn a_refresh_is_written_back_atomically_preserving_unknown_keys() {
    let home = CodexHome::new().await;
    home.write_expiring();
    std::fs::write(home.directory.path().join("config.toml"), "model = \"x\"\n").unwrap();
    let fresh = CodexHome::access_token(3_600, "pro");
    home.server
        .enqueue([scripts.token_refresh(&fresh, Some("rt_rotated"), None)]);

    let credentials = home.store().current().await.unwrap();
    assert_eq!(credentials.access_token, fresh);
    assert_eq!(credentials.refresh_token, "rt_rotated");
    assert_eq!(
        credentials.plan_type.as_deref(),
        Some("plus"),
        "from the kept id token"
    );
    assert_eq!(credentials.last_refresh, Some(codex_now()));
    let request = &home.server.requests()[0];
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/v1/oauth/token")
    );
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        body,
        json!({"grant_type": "refresh_token", "client_id": "app_test", "refresh_token": "rt_original"})
    );

    let document = home.document();
    let tokens = &document["tokens"];
    assert_eq!(tokens["access_token"], json!(fresh));
    assert_eq!(tokens["refresh_token"], "rt_rotated");
    assert_eq!(
        tokens["id_token"],
        json!(CodexHome::id_token("nicolai@example.com", "plus"))
    );
    assert_eq!(tokens["account_id"], "acct_stored");
    assert_eq!(document["agent_identity"], json!({"keep": true}));
    assert_eq!(document["auth_mode"], "chatgpt");
    assert!(document.contains_key("OPENAI_API_KEY"));
    assert_eq!(
        file_names(home.directory.path()),
        ["auth.json", "config.toml"]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(home.store().current().await.unwrap(), credentials);
    assert_eq!(home.server.request_count(), 1);
}

/// A refusal for good means sign in again, a hiccup is transient; neither
/// writes the file nor names a token.
#[tokio::test]
async fn token_endpoint_answers_are_classified_without_writing() {
    let expired = |detail: &str| CodexCredentialError::SignInExpired(detail.to_owned());
    let failed = |detail: &str| CodexCredentialError::RefreshFailed(detail.to_owned());
    let nested = |code: &str| {
        StubResponse::new(
            400,
            format!(r#"{{"error":{{"code":"{code}","message":"gone"}}}}"#).into_bytes(),
        )
    };
    let cases = [
        (
            scripts.token_refresh_rejected("refresh_token_expired", 400),
            Some(expired(
                "refresh_token_expired: HTTP 400: The refresh token is refresh_token_expired.",
            )),
        ),
        (
            scripts.token_refresh_rejected("refresh_token_invalidated", 403),
            None,
        ),
        (
            nested("invalid_grant"),
            Some(expired("invalid_grant: HTTP 400: gone")),
        ),
        (
            StubResponse::json(
                &json!({"error": "unauthorized", "error_description": "token rt_original rejected"}),
                401,
            ),
            Some(expired("unauthorized: HTTP 401: token [redacted] rejected")),
        ),
        (
            scripts.server_error(503),
            Some(failed("HTTP 503: The server had an error")),
        ),
        (
            scripts.raw_completion("<html>gateway</html>"),
            Some(failed("undecodable token response")),
        ),
    ];
    for (reply, expected) in cases {
        let home = CodexHome::new().await;
        home.write_expiring();
        let before = std::fs::read(home.file()).unwrap();
        home.server.enqueue([reply]);
        let error = home.store().current().await.unwrap_err();
        match expected {
            Some(expected) => assert_eq!(error, expected),
            None => assert!(
                matches!(error, CodexCredentialError::SignInExpired(_)),
                "{error:?}"
            ),
        }
        assert!(
            !error.to_string().contains("refresh_token"),
            "no wire code on screen"
        );
        assert_eq!(
            std::fs::read(home.file()).unwrap(),
            before,
            "a failed refresh never writes"
        );
    }
    // A secret inside the code is redacted from the detail only: the code
    // as sent still makes the refusal permanent.
    let home = CodexHome::new().await;
    home.write(AuthFile {
        account_id: Some("invalid_grant".to_owned()),
        ..AuthFile::default().access(&CodexHome::access_token(10, "plus"))
    });
    home.server
        .enqueue([scripts.token_refresh_rejected("invalid_grant", 400)]);
    let error = home.store().current().await.unwrap_err();
    let CodexCredentialError::SignInExpired(detail) = &error else {
        panic!("expected SignInExpired, got {error:?}");
    };
    assert!(!detail.contains("invalid_grant"), "{detail}");
    assert!(detail.starts_with("[redacted]: HTTP 400"), "{detail}");

    let home = CodexHome::new().await;
    home.write_expiring();
    home.server.enqueue([StubResponse::drop_connection()]);
    let dropped = home.store().current().await.unwrap_err();
    assert!(
        matches!(dropped, CodexCredentialError::RefreshFailed(_)),
        "{dropped:?}"
    );
}

/// A reused refresh token means the CLI may have rotated the file: its
/// tokens are used when fit to send, refreshed once otherwise, and an
/// unchanged file makes the refusal final.
#[tokio::test]
async fn a_reused_token_rereads_the_file_the_cli_may_have_rotated() {
    let fresh = CodexHome::access_token(3_600, "plus");
    let cases = [
        (
            Some(CodexHome::access_token(3_600, "plus")),
            Ok("rt_from_cli"),
            vec!["rt_original"],
        ),
        (
            Some(CodexHome::access_token(10, "plus")),
            Ok("rt_after_cli"),
            vec!["rt_original", "rt_from_cli"],
        ),
        (None, Err(()), vec!["rt_original"]),
    ];
    for (cli_access, expected, posted) in cases {
        let home = CodexHome::new().await;
        home.write_expiring();
        let file = home.file();
        let reply = fresh.clone();
        home.server.respond(Arc::new(move |request| {
            if request.index > 0 {
                return Some(scripts.token_refresh(&reply, Some("rt_after_cli"), None));
            }
            if let Some(access) = &cli_access {
                write_auth(
                    &file,
                    AuthFile::default().access(access).refresh("rt_from_cli"),
                );
            }
            Some(scripts.token_refresh_rejected("refresh_token_reused", 400))
        }));
        let outcome = home.store().current().await;
        match expected {
            Ok(refresh) => assert_eq!(outcome.unwrap().refresh_token, refresh),
            Err(()) => assert!(matches!(
                outcome,
                Err(CodexCredentialError::SignInExpired(_))
            )),
        }
        assert_eq!(home.posted_refresh_tokens(), posted);
    }
}

/// Waiting callers share one refresh; a refresh token refused for good is
/// posted once and not again until the file holds another one.
#[tokio::test]
async fn a_refusal_for_good_is_posted_once_and_shared() {
    let home = CodexHome::new().await;
    home.write_expiring();
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
    for outcome in [a, b, c, store.current().await] {
        assert!(matches!(
            outcome,
            Err(CodexCredentialError::SignInExpired(_))
        ));
    }
    assert_eq!(home.posted_refresh_tokens(), ["rt_original"]);
    home.write(AuthFile::default().access(&stale).refresh("rt_after_login"));
    let _ = store.current().await;
    assert_eq!(
        home.posted_refresh_tokens(),
        ["rt_original", "rt_after_login"]
    );
}

/// What the CLI does to `auth.json` during the refresh round trip wins:
/// a sign-out, an API key login or a new login is never written over, and
/// keys it added survive the write-back.
#[tokio::test]
async fn the_cli_changing_the_file_during_the_refresh_wins() {
    let new_login = || {
        let file = tempfile::NamedTempFile::new().unwrap();
        write_auth(file.path(), AuthFile::default().refresh("rt_new_login"));
        std::fs::read(file.path()).unwrap()
    };
    let unusable = || {
        let file = tempfile::NamedTempFile::new().unwrap();
        write_auth(
            file.path(),
            AuthFile {
                id: None,
                account_id: None,
                ..AuthFile::default().refresh("rt_edited")
            },
        );
        std::fs::read(file.path()).unwrap()
    };
    let api_key = br#"{"OPENAI_API_KEY":"sk-new","tokens":null}"#.to_vec();
    let cases: Vec<(&str, Option<Vec<u8>>, Result<&str, &str>)> = vec![
        ("codex logout", None, Err("NotSignedIn")),
        ("codex login --api-key", Some(api_key), Err("ApiKeyLogin")),
        ("codex login", Some(new_login()), Ok("rt_new_login")),
        (
            "a login Steno cannot use",
            Some(unusable()),
            Err("Malformed"),
        ),
    ];
    for (what, written, expected) in cases {
        let home = CodexHome::new().await;
        home.write_expiring();
        let file = home.file();
        let during = written.clone();
        home.server.respond(Arc::new(move |_| {
            match &during {
                Some(bytes) => std::fs::write(&file, bytes).unwrap(),
                None => std::fs::remove_file(&file).unwrap(),
            }
            Some(scripts.token_refresh(&CodexHome::access_token(3_600, "plus"), Some("rt_2"), None))
        }));
        let outcome = home.store().current().await.map(|c| c.refresh_token);
        match expected {
            Ok(refresh) => assert_eq!(outcome.unwrap(), refresh, "{what}"),
            Err(kind) => assert!(
                format!("{:?}", outcome.unwrap_err()).starts_with(kind),
                "{what}"
            ),
        }
        match written {
            Some(bytes) => assert_eq!(std::fs::read(home.file()).unwrap(), bytes, "{what}"),
            None => assert!(
                !home.file().exists(),
                "{what}: the write-back recreated auth.json"
            ),
        }
    }

    let home = CodexHome::new().await;
    home.write_expiring();
    let file = home.file();
    home.server.respond(Arc::new(move |_| {
        write_auth(
            &file,
            AuthFile {
                extra: vec![("written_by_cli".to_owned(), json!("yes"))],
                ..AuthFile::default().access(&CodexHome::access_token(10, "plus"))
            },
        );
        Some(scripts.token_refresh(&CodexHome::access_token(3_600, "plus"), Some("rt_2"), None))
    }));
    assert_eq!(home.store().current().await.unwrap().refresh_token, "rt_2");
    assert_eq!(home.document()["written_by_cli"], "yes");
    assert_eq!(home.document()["tokens"]["refresh_token"], "rt_2");
}

/// Turns `auth.json` into a non-empty folder during the first refresh, so
/// the write-back's rename fails, and answers with `rt_2`, later `rt_3`.
fn occupy_the_file_during_the_refresh(home: &CodexHome, access: &str) {
    let file = home.file();
    let access = access.to_owned();
    home.server.respond(Arc::new(move |_| {
        if file.is_dir() {
            return Some(scripts.token_refresh(&access, Some("rt_3"), None));
        }
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("occupied"), b"x").unwrap();
        Some(scripts.token_refresh(&access, Some("rt_2"), None))
    }));
}

/// The posted refresh token is spent, so tokens the file could not take are
/// kept in memory, used, and written once the file holds the spent token
/// again; a refresh meanwhile posts the kept token, never the spent one.
#[tokio::test]
async fn a_failed_write_keeps_the_new_sign_in_and_writes_it_later() {
    let home = CodexHome::new().await;
    home.write_expiring();
    let fresh = CodexHome::access_token(3_600, "plus");
    occupy_the_file_during_the_refresh(&home, &fresh);
    let store = home.store();
    assert_eq!(store.current().await.unwrap().refresh_token, "rt_2");
    assert!(home.file().is_dir(), "nothing was written over");

    std::fs::remove_dir_all(home.file()).unwrap();
    home.write(AuthFile {
        extra: vec![("written_by_cli".to_owned(), json!("yes"))],
        ..AuthFile::default().access(&CodexHome::access_token(10, "plus"))
    });
    let again = store.current().await.unwrap();
    assert_eq!(
        (again.refresh_token.as_str(), again.access_token.as_str()),
        ("rt_2", fresh.as_str())
    );
    assert_eq!(
        home.posted_refresh_tokens(),
        ["rt_original"],
        "the spent token is not posted"
    );
    assert_eq!(home.document()["tokens"]["refresh_token"], "rt_2");
    assert_eq!(home.document()["written_by_cli"], "yes");
    assert_eq!(file_names(home.directory.path()), ["auth.json"]);

    // Still unwritable when the kept tokens expire: the kept token is posted.
    let home = CodexHome::new().await;
    home.write_expiring();
    occupy_the_file_during_the_refresh(&home, &CodexHome::access_token(10, "plus"));
    let store = home.store();
    store.current().await.unwrap();
    assert_eq!(store.current().await.unwrap().refresh_token, "rt_3");
    assert_eq!(home.posted_refresh_tokens(), ["rt_original", "rt_2"]);

    // A half-written file gets the kept tokens and is not written over.
    let home = CodexHome::new().await;
    home.write_expiring();
    occupy_the_file_during_the_refresh(&home, &fresh);
    let store = home.store();
    store.current().await.unwrap();
    std::fs::remove_dir_all(home.file()).unwrap();
    std::fs::write(home.file(), br#"{"tokens": {"refresh_tok"#).unwrap();
    assert_eq!(store.current().await.unwrap().refresh_token, "rt_2");
    assert_eq!(
        std::fs::read(home.file()).unwrap(),
        br#"{"tokens": {"refresh_tok"#
    );
}

/// After a failed write any other change to the file wins over the kept
/// tokens.
#[tokio::test]
async fn after_a_failed_write_a_changed_file_wins() {
    let cases: Vec<(&str, Box<dyn Fn(&CodexHome)>, Result<&str, &str>)> = vec![
        ("codex logout", Box::new(|_| {}), Err("NotSignedIn")),
        (
            "codex login --api-key",
            Box::new(|home| {
                std::fs::write(home.file(), r#"{"OPENAI_API_KEY":"sk-new","tokens":null}"#)
                    .unwrap();
            }),
            Err("ApiKeyLogin"),
        ),
        (
            "auth_mode apikey over the spent token",
            Box::new(|home| {
                home.write(AuthFile {
                    auth_mode: Some("apikey".into()),
                    ..AuthFile::default()
                });
            }),
            Err("ApiKeyLogin"),
        ),
        (
            "codex login",
            Box::new(|home| home.write(AuthFile::default().refresh("rt_new_login"))),
            Ok("rt_new_login"),
        ),
    ];
    for (what, change, expected) in cases {
        let home = CodexHome::new().await;
        home.write_expiring();
        occupy_the_file_during_the_refresh(&home, &CodexHome::access_token(3_600, "plus"));
        let store = home.store();
        store.current().await.unwrap();
        std::fs::remove_dir_all(home.file()).unwrap();
        change(&home);
        let before = std::fs::read(home.file()).ok();
        let outcome = store.current().await.map(|c| c.refresh_token);
        match expected {
            Ok(refresh) => assert_eq!(outcome.unwrap(), refresh, "{what}"),
            Err(kind) => assert!(
                format!("{:?}", outcome.unwrap_err()).starts_with(kind),
                "{what}"
            ),
        }
        assert_eq!(
            std::fs::read(home.file()).ok(),
            before,
            "{what}: left alone"
        );
        assert_eq!(home.server.request_count(), 1, "{what}");
        assert!(
            file_names(home.directory.path())
                .iter()
                .all(|n| n == "auth.json"),
            "{what}"
        );
    }
}
