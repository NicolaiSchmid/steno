//! An unreadable row is an error, not a missing row: for a receipt the
//! sweep keeps the files and recording routes answer 500; for a device the
//! gate answers 500, not the 401 the phone takes for a revoke. A temporary
//! table shadowing the real one makes the read fail.

#![allow(clippy::cast_possible_truncation, clippy::large_futures)]

mod common;

use std::sync::Arc;

use common::{EngineDevice, Phone, TestService, chunks, execute_batch, seeded_bytes};
use steno_handover::{HandoverService, wire};
use uuid::Uuid;

const CHUNK_SIZE: i64 = 64 * 1024;

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

    // A restart over the same store and inbox that cannot read the receipts.
    let second = Arc::new(HandoverService::new(
        first.service.configuration.clone(),
        first.store.clone(),
        first.intake.clone(),
        first.service.identity.clone(),
        first.clock.clock(),
    ));
    execute_batch(
        &first.store,
        "CREATE TEMP TABLE handoverReceipt (unreadable INTEGER)",
    );
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
    execute_batch(&first.store, "DROP TABLE temp.handoverReceipt");
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

#[tokio::test]
async fn a_failed_device_read_answers_500_and_keeps_the_phone_paired() {
    let test = TestService::start().await;
    let phone = Phone::pair(&test).await;
    let recording_id = Uuid::new_v4();

    execute_batch(
        &test.store,
        "CREATE TEMP TABLE pairedDevice (unreadable INTEGER)",
    );
    assert_eq!(phone.status(recording_id).await.status, 500, "not 401");

    // Once the store reads again, the same token passes the gate.
    execute_batch(&test.store, "DROP TABLE temp.pairedDevice");
    assert_eq!(phone.status(recording_id).await.status, 404);
    assert_eq!(test.store.paired_devices().unwrap().len(), 1);
    test.stop().await;
}
