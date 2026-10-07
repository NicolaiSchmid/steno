//! A computer without trusted root certificates (a minimal Linux install)
//! cannot build the HTTP client. The clients are still made without a
//! panic, so the app starts and records and only the summaries fail; the
//! OpenAI-compatible client is called here and reports the reason, the
//! Codex client and its credential store are only made. Linux reads the
//! roots only from `SSL_CERT_FILE` and `SSL_CERT_DIR` when either is set,
//! so the test runs itself again with both pointing at an empty file and
//! folder.
#![cfg(all(unix, not(target_vendor = "apple")))]

use steno_core::{LlmRequest, LlmResponseFormat};
use steno_llm::{
    CodexCredentialStore, CodexResponsesClient, LlmEndpoint, LlmError, OpenAiCompatibleClient,
};
use url::Url;

const CHILD: &str = "STENO_LLM_TEST_NO_CERTIFICATES";
const NAME: &str = "without_ca_certificates_the_clients_fail_their_calls_instead_of_panicking";

#[tokio::test]
async fn without_ca_certificates_the_clients_fail_their_calls_instead_of_panicking() {
    if std::env::var_os(CHILD).is_some() {
        child().await;
        return;
    }
    let roots = tempfile::tempdir().unwrap();
    let empty = roots.path().join("empty.pem");
    std::fs::write(&empty, b"").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("SSL_CERT_FILE", &empty)
        .env("SSL_CERT_DIR", roots.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("no-certificates child passed"), "{stdout}");
}

async fn child() {
    let expected = LlmError::HttpClientUnavailable(
        "no trusted root certificates were found on this computer".into(),
    );
    assert_eq!(
        steno_llm::transport::default_http_client().unwrap_err(),
        expected
    );
    let endpoint = LlmEndpoint::new(Url::parse("http://127.0.0.1:9/v1").unwrap(), "m");
    let client = OpenAiCompatibleClient::new(endpoint, None);
    let request = LlmRequest {
        messages: Vec::new(),
        response_format: LlmResponseFormat::Text,
        temperature: None,
        max_tokens: None,
        purpose: "test".to_owned(),
    };
    assert_eq!(client.complete_llm(&request).await.unwrap_err(), expected);
    let home = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(CodexCredentialStore::new(home.path()));
    // Made without a panic; a call would first need a sign-in file.
    let _codex = CodexResponsesClient::new(LlmEndpoint::codex("m", 32_000), store);
    println!("no-certificates child passed");
}
