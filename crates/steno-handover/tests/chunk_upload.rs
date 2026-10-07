//! 1 MiB chunks, a little over 3 MiB of seeded random bytes, a mid-chunk
//! disconnect, resume from `GET` status, a duplicate chunk, then complete;
//! bytes on disk equal the source. A hash mismatch is 422 and the partial
//! is gone.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{
    EngineDevice, Phone, TestService, chunks, fake_intake, metadata_for, seeded_bytes, sha256,
};
use steno_core::{AudioFormat, HandoverState, HandoverStateKind, RecordingMetadata};
use steno_handover::upload::MetadataValidation;
use steno_handover::wire;
use uuid::Uuid;

const CHUNK_SIZE: i64 = 1024 * 1024;

fn meeting_id() -> Uuid {
    Uuid::parse_str("0EE71E00-0000-4000-8000-00000000C0DE").unwrap()
}

fn status(state: HandoverStateKind, received_chunks: Vec<i64>) -> wire::RecordingStatus {
    wire::RecordingStatus {
        state,
        received_chunks,
    }
}

#[tokio::test]
async fn upload_survives_disconnect_resumes_skips_duplicates_and_completes() {
    let intake = fake_intake(meeting_id());
    let test = TestService::with_intake(CHUNK_SIZE, intake.clone()).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(3 * CHUNK_SIZE as usize + 12345, 42);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let parts = chunks(&bytes, CHUNK_SIZE);
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[3].len(), 12345);

    let announced = phone.announce(&metadata).await;
    assert_eq!(announced.status, 201);
    assert_eq!(
        announced.json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Receiving, vec![])
    );
    assert_eq!(
        phone
            .upload(metadata.recording_id, 0, &parts[0])
            .await
            .status,
        204
    );

    // Chunk 1 breaks off after 100 KiB: head with the full length, part of
    // the body, then the connection goes away.
    let raw = test.raw_client();
    let interrupted = raw
        .exchange(
            "PUT",
            &format!("/v1/recordings/{}/chunks/1", metadata.recording_id),
            &[
                ("Authorization", phone.bearer()),
                ("Content-Type", "application/octet-stream".to_owned()),
                (wire::CHUNK_HASH_HEADER, STANDARD.encode(sha256(&parts[1]))),
                ("Content-Length", parts[1].len().to_string()),
            ],
            &parts[1][..100 * 1024],
            Duration::from_millis(50),
            Duration::from_millis(300),
        )
        .await
        .unwrap();
    assert_eq!(
        interrupted.status, None,
        "nothing is answered for a torn chunk"
    );

    // Resume from the status the computer reports.
    let resume = phone.status(metadata.recording_id).await;
    assert_eq!(resume.status, 200);
    let resume: wire::RecordingStatus = resume.json();
    assert_eq!(resume, status(HandoverStateKind::Receiving, vec![0]));
    for (index, chunk) in parts.iter().enumerate() {
        if !resume.received_chunks.contains(&(index as i64)) {
            assert_eq!(
                phone
                    .upload(metadata.recording_id, index as i64, chunk)
                    .await
                    .status,
                204
            );
        }
    }
    assert_eq!(
        phone
            .upload(metadata.recording_id, 1, &parts[1])
            .await
            .status,
        204,
        "a duplicate chunk is 204"
    );
    assert_eq!(
        phone
            .status(metadata.recording_id)
            .await
            .json::<wire::RecordingStatus>()
            .received_chunks,
        vec![0, 1, 2, 3]
    );

    let completed = phone.complete(metadata.recording_id).await;
    assert_eq!(completed.status, 200);
    assert_eq!(
        completed.json::<wire::CompleteResponse>().meeting_id,
        meeting_id()
    );

    let admissions = intake.admissions.entries();
    assert_eq!(admissions.len(), 1);
    let admission = &admissions[0];
    assert_eq!(admission.metadata, metadata);
    assert_eq!(admission.device.id, phone.device_id);
    assert_eq!(
        admission.file.extension().and_then(|ext| ext.to_str()),
        Some("m4a")
    );
    assert_eq!(
        std::fs::read(&admission.file).unwrap(),
        bytes,
        "bytes on disk equal the source"
    );

    let receipt = test
        .store
        .handover_receipt(metadata.recording_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.state,
        HandoverState::Complete {
            meeting_id: meeting_id()
        }
    );
    assert_eq!(receipt.device_id, phone.device_id);
    assert_eq!(receipt.byte_count, bytes.len() as i64);
    assert!(!test.inbox().has_partial(metadata.recording_id));
    assert!(!test.inbox().metadata(metadata.recording_id).exists());
    test.stop().await;
}

#[tokio::test]
async fn hash_mismatch_is_422_and_the_partial_is_gone() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 7);
    let wrong_hash = sha256(b"something else");
    let metadata = metadata_for(
        &bytes,
        &phone.device_name,
        Uuid::new_v4(),
        CHUNK_SIZE,
        AudioFormat::M4aAac,
        Some(wrong_hash.clone()),
    );

    phone.upload_all(&metadata, &bytes).await;
    let inbox = test.inbox();
    assert!(inbox.has_partial(metadata.recording_id));

    let completed = phone.complete(metadata.recording_id).await;
    assert_eq!(completed.status, 422);
    assert!(!inbox.has_partial(metadata.recording_id));
    assert_eq!(test.intake.admissions.count(), 0);

    assert_eq!(
        phone
            .status(metadata.recording_id)
            .await
            .json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Failed, vec![])
    );
    let receipt = test
        .store
        .handover_receipt(metadata.recording_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.state,
        HandoverState::Failed("sha256 mismatch".to_owned())
    );
    let parts = chunks(&bytes, CHUNK_SIZE);
    let late = phone.upload(metadata.recording_id, 0, &parts[0]).await;
    assert_eq!(
        late.status, 404,
        "a chunk for the discarded partial asks for a new announce"
    );
    assert!(
        late.json::<wire::Problem>()
            .error
            .contains("announce again")
    );

    // The phone starts over: re-announcing the same id (same metadata) after
    // a failure is a resume, 200 with no chunks.
    let again = phone.announce(&metadata).await;
    assert_eq!(again.status, 200);
    assert!(
        again
            .json::<wire::RecordingStatus>()
            .received_chunks
            .is_empty()
    );
    let fresh = phone.metadata(&bytes, CHUNK_SIZE);
    phone.upload_all(&fresh, &bytes).await;
    assert_eq!(phone.complete(fresh.recording_id).await.status, 200);
    test.stop().await;
}

#[tokio::test]
async fn complete_with_missing_chunks_is_409_with_the_status() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize + 1, 9);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let parts = chunks(&bytes, CHUNK_SIZE);

    assert_eq!(phone.announce(&metadata).await.status, 201);
    assert_eq!(
        phone
            .upload(metadata.recording_id, 2, &parts[2])
            .await
            .status,
        204
    );
    let early = phone.complete(metadata.recording_id).await;
    assert_eq!(early.status, 409);
    assert_eq!(
        early.json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Receiving, vec![2])
    );
    assert_eq!(test.intake.admissions.count(), 0);
    test.stop().await;
}

#[tokio::test]
async fn chunk_and_metadata_errors_are_answered_without_side_effects() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let other = Phone::pair_named(&test, "Other phone").await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize + 10, 3);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let parts = chunks(&bytes, CHUNK_SIZE);
    let variant = |chunk_size: i64, format: AudioFormat, recording_id: Uuid, data: &[u8]| {
        metadata_for(
            data,
            &phone.device_name,
            recording_id,
            chunk_size,
            format,
            None,
        )
    };

    // Announce problems.
    assert_eq!(
        phone.status(metadata.recording_id).await.status,
        404,
        "status before announce"
    );
    let mismatch = phone
        .client
        .json(
            "PUT",
            &format!("/v1/recordings/{}", Uuid::new_v4()),
            &[("Authorization", &phone.bearer())],
            &metadata,
        )
        .await;
    assert_eq!(mismatch.status, 400, "recordingID differs from the path");
    assert_eq!(
        phone
            .announce(&variant(
                CHUNK_SIZE * 2,
                AudioFormat::M4aAac,
                Uuid::new_v4(),
                &bytes
            ))
            .await
            .status,
        400,
        "chunkSize above the limit"
    );
    assert_eq!(
        phone
            .announce(&variant(
                CHUNK_SIZE,
                AudioFormat::Caf48kFloat32,
                Uuid::new_v4(),
                &bytes
            ))
            .await
            .status,
        400
    );
    let garbage = phone
        .client
        .request(
            "PUT",
            &format!("/v1/recordings/{}", metadata.recording_id),
            &[("Authorization", &phone.bearer())],
            Some(b"{".to_vec()),
        )
        .await
        .unwrap();
    assert_eq!(garbage.status, 400, "unparseable metadata");
    assert_eq!(phone.announce(&metadata).await.status, 201);
    assert_eq!(
        phone.announce(&metadata).await.status,
        200,
        "announcing twice is fine"
    );
    let mut longer = bytes.clone();
    longer.push(1);
    assert_eq!(
        phone
            .announce(&variant(
                CHUNK_SIZE,
                AudioFormat::M4aAac,
                metadata.recording_id,
                &longer
            ))
            .await
            .status,
        409,
        "different metadata under the same id"
    );
    assert_eq!(
        other.announce(&metadata).await.status,
        409,
        "another device's id"
    );
    assert_eq!(
        other.status(metadata.recording_id).await.status,
        404,
        "another device's status"
    );
    assert_eq!(
        other
            .upload(metadata.recording_id, 0, &parts[0])
            .await
            .status,
        404,
        "another device's chunk"
    );
    assert_eq!(
        other.complete(metadata.recording_id).await.status,
        404,
        "another device's complete"
    );

    // Chunk problems.
    assert_eq!(
        phone.upload(Uuid::new_v4(), 0, &parts[0]).await.status,
        404,
        "unknown recording"
    );
    assert_eq!(
        phone
            .upload(metadata.recording_id, 2, &parts[1])
            .await
            .status,
        400,
        "index beyond the chunk count"
    );
    assert_eq!(
        phone
            .upload(metadata.recording_id, 0, &parts[1])
            .await
            .status,
        400,
        "the short last chunk sent as chunk 0"
    );
    assert_eq!(
        phone
            .upload(metadata.recording_id, 1, &parts[0])
            .await
            .status,
        400,
        "a full chunk sent as the short last one"
    );
    assert_eq!(
        phone
            .upload_with(metadata.recording_id, 0, &parts[0], None)
            .await
            .status,
        400,
        "missing hash header"
    );
    assert_eq!(
        phone
            .upload_with(metadata.recording_id, 0, &parts[0], Some(&[1u8; 32]))
            .await
            .status,
        422,
        "hash header disagrees with the body"
    );
    assert_eq!(
        phone
            .upload_with(metadata.recording_id, 0, &parts[0], Some(&[1u8; 31]))
            .await
            .status,
        400,
        "hash header of the wrong length"
    );
    assert!(
        phone
            .status(metadata.recording_id)
            .await
            .json::<wire::RecordingStatus>()
            .received_chunks
            .is_empty(),
        "nothing was recorded"
    );

    assert_eq!(
        phone
            .upload(metadata.recording_id, 0, &parts[0])
            .await
            .status,
        204
    );
    assert_eq!(
        phone
            .upload(metadata.recording_id, 1, &parts[1])
            .await
            .status,
        204
    );
    let completed = phone.complete(metadata.recording_id).await;
    assert_eq!(completed.status, 200);
    let repeated = phone.complete(metadata.recording_id).await;
    assert_eq!(repeated.status, 200);
    assert_eq!(
        repeated.json::<wire::CompleteResponse>().meeting_id,
        completed.json::<wire::CompleteResponse>().meeting_id,
        "completing twice returns the same meeting id"
    );
    assert_eq!(
        phone
            .upload(metadata.recording_id, 1, &parts[1])
            .await
            .status,
        204,
        "a late duplicate after completion is harmless"
    );
    assert_eq!(test.intake.admissions.count(), 1);
    test.stop().await;
}

#[tokio::test]
async fn chunks_in_flight_at_once_all_land_in_the_receipt() {
    // The phone keeps two background tasks going, and after a relaunch the
    // background session may deliver several at once. The engine yields
    // while it saves a receipt, so every concurrent chunk must survive into
    // the same receipt: no lost update, in memory or in the store.
    let intake = fake_intake(meeting_id());
    let chunk_size: i64 = 64 * 1024;
    let test = TestService::with_intake(chunk_size, intake.clone()).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(8 * chunk_size as usize - 77, 5);
    let metadata = phone.metadata(&bytes, chunk_size);
    let parts = chunks(&bytes, chunk_size);
    assert_eq!(parts.len(), 8);
    assert_eq!(phone.announce(&metadata).await.status, 201);

    let mut tasks = Vec::new();
    for (index, chunk) in parts.iter().enumerate().rev() {
        let client = phone.client.clone();
        let token = phone.token.clone();
        let chunk = chunk.clone();
        let recording_id = metadata.recording_id;
        tasks.push(tokio::spawn(async move {
            let phone = Phone {
                client,
                token,
                device_id: Uuid::nil(),
                device_name: String::new(),
            };
            phone
                .upload(recording_id, index as i64, &chunk)
                .await
                .status
        }));
    }
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.unwrap());
    }
    assert_eq!(statuses, vec![204; 8]);
    assert_eq!(
        phone
            .status(metadata.recording_id)
            .await
            .json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Receiving, (0..8).collect())
    );
    let receipt = test
        .store
        .handover_receipt(metadata.recording_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.received_chunks,
        (0..8).collect::<Vec<_>>(),
        "the stored copy lost nothing either"
    );
    assert_eq!(phone.complete(metadata.recording_id).await.status, 200);
    let admission = &intake.admissions.entries()[0];
    assert_eq!(
        std::fs::read(&admission.file).unwrap(),
        bytes,
        "out-of-order chunks land in place"
    );
    test.stop().await;
}

#[tokio::test]
async fn announce_after_complete_reports_complete_with_every_chunk() {
    // The phone's retry after a lost 200 re-announces.
    let intake = fake_intake(meeting_id());
    let test = TestService::with_intake(CHUNK_SIZE, intake.clone()).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize + 1, 6);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    phone.upload_all(&metadata, &bytes).await;
    assert_eq!(phone.complete(metadata.recording_id).await.status, 200);

    let again = phone.announce(&metadata).await;
    assert_eq!(again.status, 200);
    assert_eq!(
        again.json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Complete, vec![0, 1, 2])
    );
    let repeated = phone.complete(metadata.recording_id).await;
    assert_eq!(repeated.status, 200);
    assert_eq!(
        repeated.json::<wire::CompleteResponse>().meeting_id,
        meeting_id()
    );
    assert_eq!(intake.admissions.count(), 1, "no second admission");
    assert!(
        !test.inbox().has_partial(metadata.recording_id),
        "no partial is reopened"
    );
    test.stop().await;
}

#[tokio::test]
async fn a_re_announce_with_other_metadata_is_409_while_receiving_and_once_complete() {
    // A different file under an admitted id is not answered `complete`: the
    // phone would post `complete`, take its 200 and delete a recording the
    // computer does not have. The phone keeps a recording answered 409 and
    // announces it again after its backoff, until the third 409 in a row
    // marks it `failed` with Retry.
    let intake = fake_intake(meeting_id());
    let test = TestService::with_intake(CHUNK_SIZE, intake.clone()).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize + 1, 7);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);

    let mut flipped = bytes.clone();
    flipped[0] ^= 1;
    let mut other_hash = metadata.clone();
    other_hash.sha256 = sha256(&flipped);
    let mut longer = metadata.clone();
    longer.byte_count += 1;
    let mut smaller_chunks = metadata.clone();
    smaller_chunks.chunk_size = CHUNK_SIZE / 2;
    let changed = [
        ("sha256", other_hash),
        ("byteCount", longer),
        ("chunkSize", smaller_chunks),
    ];

    assert_eq!(phone.announce(&metadata).await.status, 201);
    assert_metadata_differs(&phone, &changed, "receiving").await;
    phone.upload_all(&metadata, &bytes).await;
    assert_eq!(phone.complete(metadata.recording_id).await.status, 200);

    assert_metadata_differs(&phone, &changed, "complete").await;
    assert!(
        !test.inbox().has_partial(metadata.recording_id),
        "no partial is reopened"
    );
    let again = phone.announce(&metadata).await;
    assert_eq!(again.status, 200, "the same file is still complete");
    assert_eq!(
        again.json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Complete, vec![0, 1, 2])
    );
    assert_eq!(intake.admissions.count(), 1, "no second admission");
    test.stop().await;
}

/// Announces each `changed` copy of a recording's metadata and expects the
/// 409 that keeps the phone's file; `state` names the receipt's state.
async fn assert_metadata_differs(
    phone: &Phone,
    changed: &[(&str, RecordingMetadata)],
    state: &str,
) {
    for (what, metadata) in changed {
        let refused = phone.announce(metadata).await;
        assert_eq!(refused.status, 409, "{state}: {what}");
        assert_eq!(
            refused.json::<wire::Problem>().error,
            "metadata differs from the first announcement",
            "{state}: {what}"
        );
    }
}

#[tokio::test]
async fn a_vanished_partial_is_404_on_chunk_and_a_re_announce_starts_over() {
    // The phone's executor answers a 404 on a chunk by re-announcing with
    // an empty chunk set ("The Mac forgot the upload; starting over").
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 8);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let parts = chunks(&bytes, CHUNK_SIZE);
    let inbox = test.inbox();
    assert_eq!(phone.announce(&metadata).await.status, 201);
    assert_eq!(
        phone
            .upload(metadata.recording_id, 0, &parts[0])
            .await
            .status,
        204
    );

    std::fs::remove_file(inbox.partial(metadata.recording_id)).unwrap();
    let lost = phone.upload(metadata.recording_id, 1, &parts[1]).await;
    assert_eq!(lost.status, 404);
    assert!(
        lost.json::<wire::Problem>()
            .error
            .contains("announce again")
    );
    assert_eq!(
        phone
            .upload(metadata.recording_id, 0, &parts[0])
            .await
            .status,
        204,
        "a chunk already recorded is still a harmless duplicate"
    );

    let again = phone.announce(&metadata).await;
    assert_eq!(again.status, 200);
    assert_eq!(
        again.json::<wire::RecordingStatus>(),
        status(HandoverStateKind::Receiving, vec![])
    );
    assert!(inbox.has_partial(metadata.recording_id));
    phone.upload_all(&metadata, &bytes).await;
    assert_eq!(phone.complete(metadata.recording_id).await.status, 200);
    let admission = &test.intake.admissions.entries()[0];
    assert_eq!(std::fs::read(&admission.file).unwrap(), bytes);
    test.stop().await;
}

#[tokio::test]
async fn a_negative_chunk_index_is_400_at_the_engine() {
    // The router never matches a negative index; the engine is driven
    // directly here and must not compute `index * chunk_size` for one.
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let device = EngineDevice::paired(&test, "Engine phone").await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize, 9);
    let metadata = device.metadata(&bytes, CHUNK_SIZE);
    assert_eq!(device.announce(&metadata).await.status, 201);

    for index in [-1, i64::MIN] {
        let response = device.upload(metadata.recording_id, index, &bytes).await;
        assert_eq!(response.status, 400, "index {index}");
        assert_eq!(
            response.decode::<wire::Problem>().unwrap().error,
            "chunk index must be below 1"
        );
    }
    assert!(
        device
            .status(metadata.recording_id)
            .await
            .decode::<wire::RecordingStatus>()
            .unwrap()
            .received_chunks
            .is_empty()
    );
    test.stop().await;
}

#[tokio::test]
async fn a_failed_write_is_500_without_the_inbox_path() {
    // An I/O error names the file, and the inbox lives under the user's
    // home; the phone gets the step that failed and nothing else.
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize, 10);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let inbox = test.inbox();
    assert_eq!(phone.announce(&metadata).await.status, 201);
    // The partial becomes a directory: every write to it fails.
    std::fs::remove_file(inbox.partial(metadata.recording_id)).unwrap();
    std::fs::create_dir(inbox.partial(metadata.recording_id)).unwrap();

    let failed = phone.upload(metadata.recording_id, 0, &bytes).await;
    assert_eq!(failed.status, 500);
    let problem = failed.json::<wire::Problem>().error;
    assert_eq!(problem, "writing the chunk failed on the computer");
    assert!(!problem.contains(inbox.directory.to_str().unwrap()));
    assert!(
        phone
            .status(metadata.recording_id)
            .await
            .json::<wire::RecordingStatus>()
            .received_chunks
            .is_empty(),
        "the failed chunk is not recorded"
    );
    test.stop().await;
}

#[test]
fn chunk_arithmetic_covers_the_short_last_chunk() {
    assert_eq!(MetadataValidation::chunk_count(1, 10), 1);
    assert_eq!(MetadataValidation::chunk_count(10, 10), 1);
    assert_eq!(MetadataValidation::chunk_count(11, 10), 2);
    assert_eq!(MetadataValidation::chunk_length(0, 11, 10), 10);
    assert_eq!(MetadataValidation::chunk_length(1, 11, 10), 1);
    assert_eq!(MetadataValidation::chunk_length(0, 10, 10), 10);
}
