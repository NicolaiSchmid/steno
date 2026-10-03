//! A receipt the store cannot read is an error, not a missing receipt: the
//! sweep keeps the upload's files and every recording route answers 500,
//! so a failed read never deletes a resumable upload, nor lets an announce
//! start the recording over (which would overwrite a `complete` receipt
//! and admit the meeting twice). The read fails because a temporary table
//! of the same name shadows `handoverReceipt` on the store's connection.

#![allow(clippy::cast_possible_truncation, clippy::large_futures)]

mod common;

use std::sync::Arc;

use common::{EngineDevice, TestService, chunks, seeded_bytes};
use steno_core::Store;
use steno_handover::{HandoverService, wire};

const CHUNK_SIZE: i64 = 64 * 1024;

fn shadow_the_receipts(store: &Store, shadowed: bool) {
    let sql = if shadowed {
        "CREATE TEMP TABLE handoverReceipt (unreadable INTEGER)"
    } else {
        "DROP TABLE temp.handoverReceipt"
    };
    store
        .write(|transaction| Ok(transaction.execute_batch(sql)?))
        .unwrap();
}

#[tokio::test]
async fn a_failed_receipt_read_keeps_the_upload_and_answers_500() {
    let first = TestService::with(common::Options {
        chunk_size: CHUNK_SIZE,
        start: false,
        ..common::Options::default()
    })
    .await;
    let phone = EngineDevice::paired(&first, "Direct iPhone").await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 83);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let parts = chunks(&bytes, CHUNK_SIZE);
    assert_eq!(phone.announce(&metadata).await.status.as_u16(), 201);
    assert_eq!(
        phone
            .upload(metadata.recording_id, 0, &parts[0])
            .await
            .status
            .as_u16(),
        204
    );
    let id = metadata.recording_id;

    // The computer comes back over the same store and inbox, with nothing
    // in memory, and the store cannot read the receipts.
    let second = Arc::new(HandoverService::new(
        first.service.configuration.clone(),
        first.store.clone(),
        first.intake.clone(),
        first.service.identity.clone(),
        first.clock.clock(),
    ));
    shadow_the_receipts(&first.store, true);
    second.engine.sweep_orphans().await;
    let inbox = &second.engine.inbox;
    assert!(inbox.has_partial(id), "the resumable upload is kept");
    assert!(inbox.load_metadata(id).is_some());

    let resumed = EngineDevice {
        service: second.clone(),
        device: phone.device.clone(),
    };
    assert_eq!(resumed.status(id).await.status.as_u16(), 500);
    assert_eq!(resumed.announce(&metadata).await.status.as_u16(), 500);
    assert_eq!(resumed.upload(id, 1, &parts[1]).await.status.as_u16(), 500);
    assert_eq!(resumed.complete(id).await.status.as_u16(), 500);
    assert!(inbox.has_partial(id), "no route touched the partial");

    // Once the store reads again, the upload resumes where it stood.
    shadow_the_receipts(&first.store, false);
    let status = resumed.status(id).await;
    assert_eq!(status.status.as_u16(), 200);
    assert_eq!(
        status
            .decode::<wire::RecordingStatus>()
            .unwrap()
            .received_chunks,
        vec![0]
    );
    assert_eq!(resumed.upload(id, 1, &parts[1]).await.status.as_u16(), 204);
    assert_eq!(resumed.complete(id).await.status.as_u16(), 200);
}
