//! The upload over the wire: chunks land on disk at their offsets, the file
//! is verified before the intake gets it, every call is idempotent on the
//! recording id, and an admitted recording is never admitted twice.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::large_futures
)]

mod common;

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{Options, Phone, TestService, chunks, metadata_for, seeded_bytes, sha256};
use steno_core::{AudioFormat, HandoverState, HandoverStateKind, RecordingMetadata};
use steno_handover::configuration::HandoverConfiguration;
use steno_handover::engine::Engine;
use steno_handover::upload::MetadataValidation;
use steno_handover::wire;
use uuid::Uuid;

const CHUNK_SIZE: i64 = 64 * 1024;

fn status(state: HandoverStateKind, received_chunks: Vec<i64>) -> wire::RecordingStatus {
    wire::RecordingStatus {
        state,
        received_chunks,
    }
}

/// The announce answers `complete` with every chunk and opens no partial.
async fn delivered(test: &TestService, phone: &Phone, metadata: &RecordingMetadata) {
    let announced = phone.announce(metadata).await;
    assert_eq!(announced.status, 200);
    let count = MetadataValidation::chunk_count(metadata.byte_count, metadata.chunk_size);
    assert_eq!(
        announced.json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Complete, (0..count).collect())
    );
    assert!(!test.inbox().has_partial(metadata.recording_id));
}

#[tokio::test]
async fn a_torn_chunk_is_not_kept_and_the_resumed_upload_lands_byte_for_byte() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let mut receipts = test.service.receipts();
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(3 * CHUNK_SIZE as usize + 12_345, 42);
    let metadata = metadata_for(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    let parts = chunks(&bytes, CHUNK_SIZE);

    assert_eq!(phone.status(id).await.status, 404, "before the announce");
    let announced = phone.announce(&metadata).await;
    assert_eq!(announced.status, 201);
    assert_eq!(
        announced.json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Receiving, vec![])
    );
    assert_eq!(
        phone.upload(id, 2, &parts[2]).await.status,
        204,
        "any order"
    );

    // Chunk 0 breaks off halfway through its body.
    let torn = test
        .raw_client()
        .exchange(
            "PUT",
            &format!("/v1/recordings/{id}/chunks/0"),
            &[
                ("Authorization", phone.bearer()),
                (wire::CHUNK_HASH_HEADER, STANDARD.encode(sha256(&parts[0]))),
                ("Content-Length", parts[0].len().to_string()),
            ],
            &parts[0][..CHUNK_SIZE as usize / 2],
            Duration::from_millis(50),
            Duration::from_millis(300),
        )
        .await
        .unwrap();
    assert_eq!(torn.status, None, "a torn chunk is not answered");

    let resume = phone.status_of(id).await;
    assert_eq!(resume, status(HandoverStateKind::Receiving, vec![2]));
    for (index, chunk) in parts.iter().enumerate() {
        if !resume.received_chunks.contains(&(index as i64)) {
            assert_eq!(phone.upload(id, index as i64, chunk).await.status, 204);
        }
    }
    assert_eq!(
        phone.upload(id, 1, &parts[1]).await.status,
        204,
        "a duplicate"
    );
    assert_eq!(phone.status_of(id).await.received_chunks, [0, 1, 2, 3]);

    let meeting_id = phone.completed(id).await;
    assert_eq!(
        *test.intake.files.lock().unwrap(),
        [bytes],
        "the bytes the phone sent"
    );
    assert_eq!(
        test.store.handover_receipt(id).unwrap().unwrap().state,
        HandoverState::Complete { meeting_id }
    );
    assert!(
        test.inbox().recording_ids().is_empty(),
        "the inbox is empty"
    );
    let streamed = receipts.borrow_and_update().clone();
    assert_eq!(
        streamed.last().unwrap().state,
        HandoverState::Complete { meeting_id }
    );

    // The phone that lost the 200 asks again: the same meeting, no admission.
    assert_eq!(phone.completed(id).await, meeting_id);
    delivered(&test, &phone, &metadata).await;
    assert_eq!(test.intake.meetings.lock().unwrap().len(), 1);
    test.stop().await;
}

#[tokio::test]
async fn a_bad_chunk_is_refused_and_changes_nothing() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let other = Phone::pair(&test).await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize + 10, 3);
    let metadata = metadata_for(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    let parts = chunks(&bytes, CHUNK_SIZE);
    assert_eq!(phone.announce(&metadata).await.status, 201);

    let right = STANDARD.encode(sha256(&parts[0]));
    let wrong = STANDARD.encode(sha256(&parts[1]));
    let cases: [(i64, &[u8], Option<&str>, u16); 6] = [
        (0, &parts[0], Some(wrong.as_str()), 422),
        (0, &parts[0], None, 400),
        (0, &parts[0], Some("not base64"), 400),
        (0, &parts[0][1..], Some(right.as_str()), 400),
        (2, &parts[0], Some(right.as_str()), 400),
        (1, &parts[0], Some(right.as_str()), 400),
    ];
    for (index, body, hash, expected) in cases {
        let answered = phone.upload_with(id, index, body, hash).await;
        assert_eq!(answered.status, expected, "chunk {index}, hash {hash:?}");
    }
    assert_eq!(
        other.upload(id, 0, &parts[0]).await.status,
        404,
        "another phone's recording"
    );
    assert_eq!(other.status(id).await.status, 404);
    assert_eq!(other.complete(id).await.status, 404);
    assert_eq!(phone.status_of(id).await.received_chunks, Vec::<i64>::new());
    assert_eq!(
        std::fs::metadata(test.inbox().partial(id)).unwrap().len(),
        0
    );
    test.stop().await;
}

#[tokio::test]
async fn complete_needs_every_chunk_and_a_file_that_hashes_to_the_announced_value() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 5);
    let parts = chunks(&bytes, CHUNK_SIZE);

    let metadata = metadata_for(&bytes, CHUNK_SIZE);
    assert_eq!(phone.announce(&metadata).await.status, 201);
    assert_eq!(
        phone
            .upload(metadata.recording_id, 1, &parts[1])
            .await
            .status,
        204
    );
    let missing = phone.complete(metadata.recording_id).await;
    assert_eq!(missing.status, 409);
    assert_eq!(
        missing.json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Receiving, vec![1])
    );

    // Every chunk matches its own hash, but the file is not the one announced.
    let lying = RecordingMetadata {
        sha256: sha256(b"other bytes"),
        ..metadata_for(&bytes, CHUNK_SIZE)
    };
    let id = lying.recording_id;
    phone.upload_all(&lying, &bytes).await;
    let mismatch = phone.complete(id).await;
    assert_eq!(mismatch.status, 422);
    assert!(!test.inbox().has_partial(id), "the partial is discarded");
    assert!(!test.inbox().has_verified(id, AudioFormat::M4aAac));
    assert_eq!(
        phone.status_of(id).await,
        status(HandoverStateKind::Failed, vec![])
    );
    assert!(
        test.intake.files.lock().unwrap().is_empty(),
        "nothing reached the intake"
    );

    // The phone starts over with an announce and every chunk.
    phone.upload_all(&lying, &bytes).await;
    assert_eq!(phone.complete(id).await.status, 422);
    test.stop().await;
}

#[tokio::test]
async fn an_intake_failure_keeps_the_verified_file_for_the_retry() {
    let test = TestService::with(Options {
        chunk_size: CHUNK_SIZE,
        failures: 1,
        ..Options::default()
    })
    .await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 77);
    let metadata = metadata_for(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &bytes).await;

    let failed = phone.complete(id).await;
    assert_eq!(failed.status, 500);
    let inbox = test.inbox();
    let problem = failed.json::<wire::Problem>().error;
    assert!(
        !problem.contains(inbox.directory.to_str().unwrap()),
        "no path for the phone: {problem}"
    );
    assert!(
        inbox.has_verified(id, AudioFormat::M4aAac),
        "the verified file waits"
    );
    assert_eq!(inbox.load_metadata(id), Some(metadata.clone()));
    assert_eq!(
        test.store.handover_receipt(id).unwrap().unwrap().state,
        HandoverState::Failed(Engine::INTAKE_REFUSED.to_owned()),
        "no path in the receipt either"
    );
    assert_eq!(
        phone.status_of(id).await,
        status(HandoverStateKind::Failed, vec![0, 1])
    );

    // A re-announce keeps the chunk set; no chunk travels again.
    assert_eq!(phone.announce(&metadata).await.status, 200);
    phone.completed(id).await;
    let files = test.intake.files.lock().unwrap().clone();
    assert_eq!(
        files,
        [bytes.clone(), bytes],
        "the same verified file both times"
    );
    assert!(inbox.recording_ids().is_empty());
    test.stop().await;
}

#[tokio::test]
async fn admitted_bytes_are_answered_from_the_ledger_with_their_first_meeting() {
    let test = TestService::with_chunk_size(2 * CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize + 1, 31);
    let metadata = metadata_for(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &bytes).await;
    let meeting_id = phone.completed(id).await;

    // The phone lost the 200, unpaired (the receipt goes with the device) and
    // paired again after a restart; then the user deleted the meeting.
    assert_eq!(phone.unpair().await.status, 204);
    assert_eq!(test.store.handover_receipt(id).unwrap(), None);
    let test = test.restart(2 * CHUNK_SIZE).await;
    let phone = Phone::pair_as(&test, phone.device_id).await;
    delivered(&test, &phone, &metadata).await;
    assert_eq!(phone.completed(id).await, meeting_id);
    test.store.delete_meeting(meeting_id).unwrap();
    let reinstalled = Phone::pair(&test).await;
    delivered(&test, &reinstalled, &metadata).await;
    assert_eq!(reinstalled.completed(id).await, meeting_id);

    // In another split the same bytes are still delivered.
    let resplit = RecordingMetadata {
        chunk_size: 2 * CHUNK_SIZE,
        ..metadata.clone()
    };
    delivered(&test, &reinstalled, &resplit).await;

    // Other bytes under the id are a new recording with a meeting of its own.
    let other = seeded_bytes(bytes.len(), 32);
    let replaced = RecordingMetadata {
        recording_id: id,
        ..metadata_for(&other, CHUNK_SIZE)
    };
    let announced = reinstalled.announce(&replaced).await;
    assert_eq!(announced.status, 201);
    for (index, chunk) in chunks(&other, CHUNK_SIZE).iter().enumerate() {
        assert_eq!(
            reinstalled.upload(id, index as i64, chunk).await.status,
            204
        );
    }
    assert_ne!(reinstalled.completed(id).await, meeting_id);
    assert_eq!(
        test.intake.meetings.lock().unwrap().len(),
        1,
        "after the restart"
    );
    test.stop().await;
}

/// The phone deletes its copy on a 200 from `complete`, so a `complete`
/// receipt whose meeting never committed must not answer 200.
#[tokio::test]
async fn a_complete_receipt_without_its_meeting_is_admitted_again_after_a_restart() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 80);
    let metadata = metadata_for(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &bytes).await;
    // Handed to the intake: the receipt committed, the meeting did not.
    test.inbox().promote(id, metadata.format).unwrap();
    let mut receipt = test.store.handover_receipt(id).unwrap().unwrap();
    receipt.state = HandoverState::Complete {
        meeting_id: Uuid::new_v4(),
    };
    test.store.save_handover_receipt(&receipt).unwrap();

    let test = test.restart(CHUNK_SIZE).await;
    let phone = phone.to(&test);
    assert!(
        test.inbox().has_verified(id, metadata.format),
        "the sweep keeps the file"
    );
    let announced = phone.announce(&metadata).await;
    assert_eq!(
        announced.json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Receiving, vec![0, 1])
    );
    let meeting_id = phone.completed(id).await;
    assert_ne!(meeting_id, receipt.state.meeting_id().unwrap());
    assert_eq!(*test.intake.files.lock().unwrap(), [bytes]);
    test.stop().await;
}

#[tokio::test]
async fn a_restart_resumes_from_the_stored_receipt_and_sweeps_what_no_upload_needs() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 9);
    let parts = chunks(&bytes, CHUNK_SIZE);
    let resumed = metadata_for(&bytes, CHUNK_SIZE);
    let abandoned = metadata_for(&bytes, CHUNK_SIZE);
    for metadata in [&resumed, &abandoned] {
        assert_eq!(phone.announce(metadata).await.status, 201);
        assert_eq!(
            phone
                .upload(metadata.recording_id, 0, &parts[0])
                .await
                .status,
            204
        );
    }
    let orphan = metadata_for(&bytes, CHUNK_SIZE);
    test.inbox().begin(&orphan).unwrap();

    let test = test.restart(CHUNK_SIZE).await;
    let phone = phone.to(&test);
    assert!(
        !test.inbox().has_partial(orphan.recording_id),
        "no receipt, swept"
    );
    assert_eq!(
        phone.status_of(resumed.recording_id).await.received_chunks,
        [0]
    );
    assert_eq!(
        phone
            .upload(resumed.recording_id, 1, &parts[1])
            .await
            .status,
        204
    );
    phone.completed(resumed.recording_id).await;
    assert_eq!(*test.intake.files.lock().unwrap(), [bytes]);

    // Untouched for two weeks: the partial goes, the receipt stays.
    test.clock.advance(Duration::from_secs(
        Engine::ABANDONED_AFTER_SECONDS as u64 + 60,
    ));
    let test = test.restart(CHUNK_SIZE).await;
    assert!(!test.inbox().has_partial(abandoned.recording_id));
    assert!(
        test.store
            .handover_receipt(abandoned.recording_id)
            .unwrap()
            .is_some()
    );
    test.stop().await;
}

#[tokio::test]
async fn a_bad_announce_is_400_and_creates_no_file() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(100, 1);
    let metadata = metadata_for(&bytes, CHUNK_SIZE);
    let path = format!("/v1/recordings/{}", Uuid::new_v4());
    let authorization = phone.bearer();
    let headers = [("Authorization", authorization.as_str())];
    let mismatched = serde_json::to_vec(&metadata).unwrap();
    for body in [mismatched, b"{".to_vec()] {
        assert_eq!(
            phone
                .client
                .send("PUT", &path, &headers, Some(body))
                .await
                .status,
            400
        );
    }
    let too_big = RecordingMetadata {
        chunk_size: 2 * CHUNK_SIZE,
        ..metadata.clone()
    };
    assert_eq!(
        phone.announce(&too_big).await.status,
        400,
        "above the computer's chunk size"
    );
    assert!(test.inbox().recording_ids().is_empty());
    test.stop().await;
}

/// One change to the sample metadata.
type Edit<'a> = &'a dyn Fn(&mut RecordingMetadata);

#[test]
fn metadata_validation_holds_every_bound_and_names_the_first_problem() {
    let configuration = HandoverConfiguration {
        chunk_size: 16 * 1024 * 1024,
        ..HandoverConfiguration::default()
    };
    let sample = metadata_for(&[1, 2, 3], MetadataValidation::MIN_CHUNK_SIZE);
    let problem = |edit: Edit| {
        let mut metadata = sample.clone();
        edit(&mut metadata);
        MetadataValidation::problem(&metadata, &configuration)
    };
    let max_bytes = MetadataValidation::MAX_BYTE_COUNT;
    let max_seconds = MetadataValidation::MAX_DURATION_SECONDS;
    let accepted: [Edit; 8] = [
        &|_| {},
        &|m| m.byte_count = 1,
        &|m| m.byte_count = max_bytes,
        &|m| m.chunk_size = configuration.chunk_size,
        &|m| m.duration_seconds = 0.0,
        &|m| m.duration_seconds = max_seconds,
        &|m| m.device_name = format!("  {}\u{200B}", "\u{1F469}\u{200D}\u{1F4BB}".repeat(128)),
        &|m| m.format = AudioFormat::Wav16kInt16,
    ];
    for (index, edit) in accepted.iter().enumerate() {
        assert_eq!(problem(*edit), None, "accepted case {index}");
    }
    let refused: [(Edit, &str); 10] = [
        (&|m| m.byte_count = 0, "byteCount"),
        (&|m| m.byte_count = max_bytes + 1, "byteCount"),
        (
            &|m| m.chunk_size = MetadataValidation::MIN_CHUNK_SIZE - 1,
            "chunkSize",
        ),
        (
            &|m| m.chunk_size = configuration.chunk_size + 1,
            "chunkSize",
        ),
        (&|m| m.sha256 = vec![0; 31], "sha256"),
        (&|m| m.duration_seconds = -0.5, "durationSeconds"),
        (&|m| m.duration_seconds = f64::NAN, "durationSeconds"),
        (&|m| m.device_name = " \u{200B}\n".to_owned(), "deviceName"),
        (&|m| m.device_name = "a".repeat(129), "deviceName"),
        (&|m| m.format = AudioFormat::Caf48kFloat32, "format"),
    ];
    for (edit, field) in refused {
        let found = problem(edit).unwrap_or_default();
        assert!(found.starts_with(field), "{field}: {found}");
    }
    let both = problem(&|m: &mut RecordingMetadata| {
        m.byte_count = 0;
        m.sha256.clear();
    });
    assert!(
        both.unwrap().starts_with("byteCount"),
        "the first problem wins"
    );
    assert_eq!(MetadataValidation::chunk_count(2 * 1024 + 1, 1024), 3);
    assert_eq!(
        MetadataValidation::chunk_length(2, 2 * 1024 + 1, 1024),
        1,
        "a short last chunk"
    );
}
