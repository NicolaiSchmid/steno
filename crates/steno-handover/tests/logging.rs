//! What the handover logs through `tracing` never carries a credential:
//! not the pairing secret, not the bearer token, through a pairing, an
//! upload and a failure that logs at error level. The capture is a
//! subscriber of its own; the crate has no `tracing-subscriber`.

#![allow(clippy::large_futures)]

mod common;

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{Phone, TestService, seeded_bytes};
use steno_handover::base64url;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata};
use uuid::Uuid;

const CHUNK_SIZE: i64 = 256 * 1024;

/// Every event as one line: target, message and every field in `Debug`.
#[derive(Default, Clone)]
struct Capture(Arc<Mutex<Vec<String>>>);

impl Capture {
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct Line(String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, " {}={value:?}", field.name());
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut line = Line(event.metadata().target().to_owned());
        event.record(&mut line);
        self.0.lock().unwrap().push(line.0);
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[tokio::test]
async fn no_credential_reaches_the_log() {
    let capture = Capture::default();
    let _default = tracing::subscriber::set_default(capture.clone());
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;

    // Pairing, with one rejected attempt, then an upload whose write
    // fails: the error path is the one that logs the most.
    let payload = test.service.begin_pairing();
    let rejected = Phone::try_pair(&test, &[0u8; 32], Uuid::new_v4(), "Intruder").await;
    assert_eq!(rejected.status, 403);
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(usize::try_from(CHUNK_SIZE).unwrap(), 44);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    assert_eq!(phone.announce(&metadata).await.status, 201);
    let inbox = test.inbox();
    std::fs::remove_file(inbox.partial(metadata.recording_id)).unwrap();
    std::fs::create_dir(inbox.partial(metadata.recording_id)).unwrap();
    assert_eq!(
        phone.upload(metadata.recording_id, 0, &bytes).await.status,
        500
    );
    test.stop().await;

    let lines = capture.lines();
    assert!(
        lines.iter().any(|line| line.contains("writing the chunk")),
        "the failed write was logged: {lines:?}"
    );
    let secrets = [
        ("the secret, base64url", base64url::encode(&payload.secret)),
        ("the secret, base64", STANDARD.encode(&payload.secret)),
        ("the secret, bytes", format!("{:?}", payload.secret)),
        ("the bearer token", phone.token.clone()),
    ];
    for line in &lines {
        for (what, secret) in &secrets {
            assert!(!line.contains(secret.as_str()), "{what} in the log: {line}");
        }
    }
}
