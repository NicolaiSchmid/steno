//! Revoking a phone mid-upload: its partial and receipt are gone, every
//! bearer route answers 401 at the gate (the phone's "unpaired" signal),
//! and another phone's upload is untouched. Both ways in: `revoke` from
//! the computer and `DELETE /v1/pairing` from the phone.

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

use common::{Phone, TestService, chunks, seeded_bytes};
use steno_core::RecordingMetadata;
use steno_handover::pairing::DeviceTokens;
use steno_handover::wire;
use uuid::Uuid;

const CHUNK_SIZE: i64 = 256 * 1024;

struct Upload {
    phone: Phone,
    metadata: RecordingMetadata,
    chunks: Vec<Vec<u8>>,
    bytes: Vec<u8>,
}

impl Upload {
    async fn begin(test: &TestService, seed: u64, name: &str) -> Upload {
        let phone = Phone::pair_named(test, name).await;
        let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, seed);
        let metadata = phone.metadata(&bytes, CHUNK_SIZE);
        let chunks = chunks(&bytes, CHUNK_SIZE);
        assert_eq!(phone.announce(&metadata).await.status, 201);
        assert_eq!(
            phone
                .upload(metadata.recording_id, 0, &chunks[0])
                .await
                .status,
            204
        );
        Upload {
            phone,
            metadata,
            chunks,
            bytes,
        }
    }

    fn id(&self) -> Uuid {
        self.metadata.recording_id
    }
}

#[tokio::test]
async fn a_revoke_between_the_gate_read_and_the_touch_stays_a_revoke() {
    // The gate reads the device, yields, then refreshes `last_seen_at`. A
    // revoke that lands in that gap must not be undone by the refresh: the
    // touch is an `UPDATE` of the row that still holds the token, never a
    // save that would re-insert the device and its token hash.
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&test).await;
    let read_at_the_gate = test
        .store
        .paired_device(phone.device_id)
        .unwrap()
        .expect("paired");
    test.advance(Duration::from_secs(
        steno_handover::engine::Engine::LAST_SEEN_RESOLUTION_SECONDS as u64 + 1,
    ));
    test.service.revoke(phone.device_id).await.unwrap();

    let touched = test
        .service
        .engine
        .touch(read_at_the_gate, DeviceTokens::hash(&phone.token))
        .await;
    assert!(
        touched.last_seen_at > Some(test.now),
        "the copy is refreshed"
    );
    assert_eq!(
        test.service.paired_devices().await.unwrap(),
        Vec::new(),
        "the revoked device is not resurrected"
    );
    assert_eq!(
        phone.status(Uuid::new_v4()).await.status,
        401,
        "its token is still unknown"
    );
    test.stop().await;
}

#[tokio::test]
async fn revoke_and_unpair_drop_the_upload_and_make_every_bearer_route_401() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let inbox = test.inbox();
    let a = Upload::begin(&test, 11, "Phone A").await;
    let b = Upload::begin(&test, 12, "Phone B").await;
    assert!(inbox.has_partial(a.id()) && inbox.has_partial(b.id()));
    let mut live: Vec<Uuid> = test
        .service
        .engine
        .receipts_snapshot()
        .iter()
        .map(|r| r.recording_id)
        .collect();
    live.sort();
    let mut expected = vec![a.id(), b.id()];
    expected.sort();
    assert_eq!(live, expected);

    // The computer revokes A.
    test.service.revoke(a.phone.device_id).await.unwrap();

    assert!(!inbox.has_partial(a.id()), "A's partial is discarded");
    assert!(inbox.load_metadata(a.id()).is_none());
    assert!(
        test.store.handover_receipt(a.id()).unwrap().is_none(),
        "the receipt cascades"
    );
    assert_eq!(
        test.service
            .engine
            .receipts_snapshot()
            .iter()
            .map(|r| r.recording_id)
            .collect::<Vec<_>>(),
        vec![b.id()]
    );
    assert_eq!(
        test.service
            .paired_devices()
            .await
            .unwrap()
            .iter()
            .map(|d| d.id)
            .collect::<Vec<_>>(),
        vec![b.phone.device_id]
    );
    assert!(inbox.has_partial(b.id()), "B's upload is untouched");

    // Every bearer route rejects A at the gate; the engine never sees them
    // and the chunk body is not read.
    let handled = test.metrics().handled_requests;
    assert_eq!(a.phone.announce(&a.metadata).await.status, 401);
    assert_eq!(a.phone.status(a.id()).await.status, 401);
    assert_eq!(a.phone.upload(a.id(), 1, &a.chunks[1]).await.status, 401);
    assert_eq!(a.phone.complete(a.id()).await.status, 401);
    assert_eq!(
        a.phone
            .client
            .request(
                "DELETE",
                "/v1/pairing",
                &[("Authorization", &a.phone.bearer())],
                None
            )
            .await
            .unwrap()
            .status,
        401
    );
    assert_eq!(
        test.metrics().handled_requests,
        handled,
        "rejected before the engine"
    );
    // The drain of a rejected body runs on after the answer; give it a moment.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(test.metrics().discarded_body_bytes <= a.chunks[1].len() as u64 + 4096);
    assert_eq!(
        a.phone.client.get("/v1/hello", &[]).await.status,
        200,
        "hello needs no token"
    );
    assert!(
        !inbox.has_partial(a.id()),
        "a rejected announce creates nothing"
    );

    // B continues: a resume from status, the last chunk, complete.
    let resume = b.phone.status(b.id()).await;
    assert_eq!(resume.status, 200);
    assert_eq!(
        resume.json::<wire::RecordingStatus>().received_chunks,
        vec![0]
    );
    assert_eq!(b.phone.upload(b.id(), 1, &b.chunks[1]).await.status, 204);
    assert_eq!(b.phone.complete(b.id()).await.status, 200);
    let admissions = test.intake.admissions.entries();
    assert_eq!(
        admissions.iter().map(|a| a.device.id).collect::<Vec<_>>(),
        vec![b.phone.device_id]
    );
    assert_eq!(std::fs::read(&admissions[0].file).unwrap(), b.bytes);

    // B unpairs itself from the phone side.
    let unpair = b
        .phone
        .client
        .request(
            "DELETE",
            "/v1/pairing",
            &[("Authorization", &b.phone.bearer())],
            None,
        )
        .await
        .unwrap();
    assert_eq!(unpair.status, 204);
    assert!(test.service.paired_devices().await.unwrap().is_empty());
    assert!(test.service.engine.receipts_snapshot().is_empty());
    assert_eq!(b.phone.status(b.id()).await.status, 401);
    assert_eq!(b.phone.complete(b.id()).await.status, 401);
    assert_eq!(b.phone.announce(&b.metadata).await.status, 401);
    test.stop().await;
}

#[tokio::test]
async fn unpair_mid_upload_discards_the_partial_and_the_receipt() {
    let test = TestService::with_chunk_size(CHUNK_SIZE).await;
    let inbox = test.inbox();
    let upload = Upload::begin(&test, 13, "Phone C").await;
    let mut receipts = test.service.receipts();
    receipts.borrow_and_update();

    let unpair = upload
        .phone
        .client
        .request(
            "DELETE",
            "/v1/pairing",
            &[("Authorization", &upload.phone.bearer())],
            None,
        )
        .await
        .unwrap();
    assert_eq!(unpair.status, 204);
    assert!(!inbox.has_partial(upload.id()));
    assert!(inbox.load_metadata(upload.id()).is_none());
    assert!(test.store.handover_receipt(upload.id()).unwrap().is_none());
    assert!(
        test.store
            .paired_device(upload.phone.device_id)
            .unwrap()
            .is_none()
    );

    // The receipt stream saw the drop.
    assert!(receipts.borrow().is_empty());
    assert_eq!(
        upload
            .phone
            .upload(upload.id(), 1, &upload.chunks[1])
            .await
            .status,
        401
    );
    assert_eq!(test.intake.admissions.count(), 0);
    test.stop().await;
}

#[tokio::test]
async fn a_complete_in_flight_does_not_bring_a_revoked_device_back_into_the_stream() {
    // `complete` read the receipt before the revoke and writes it back
    // after the intake answers; the store refuses the write (the device is
    // gone), and memory must not keep it either, or the receipt stream
    // shows an upload of a revoked phone until restart.
    let chunk_size: i64 = 64 * 1024;
    let meeting_id = Uuid::new_v4();
    let intake = common::ScriptedIntake::gated(meeting_id, false);
    let test = TestService::with(common::Options {
        chunk_size,
        intake: Some(intake.clone() as std::sync::Arc<dyn steno_core::HandoverIntake>),
        start: false,
        ..common::Options::default()
    })
    .await;
    let phone = common::EngineDevice::paired(&test, "Direct iPhone").await;
    let bytes = seeded_bytes(2 * chunk_size as usize, 97);
    let metadata = phone.metadata(&bytes, chunk_size);
    phone.upload_all(&metadata, &bytes).await;
    let id = metadata.recording_id;
    let receipts = test.service.receipts();

    let completing = phone.complete(id);
    let revoking = async {
        intake.admitting().await;
        test.service.revoke(phone.device.id).await.unwrap();
        intake.release();
    };
    let (_, ()) = tokio::join!(completing, revoking);
    assert_eq!(intake.count(), 1, "the revoke landed while the intake ran");

    let owned = |receipts: &[steno_core::HandoverReceipt]| {
        receipts
            .iter()
            .any(|receipt| receipt.device_id == phone.device.id)
    };
    assert!(!owned(&test.service.engine.receipts_snapshot()));
    assert!(
        !owned(&receipts.borrow()),
        "the stream shows no receipt of it"
    );
    assert_eq!(test.store.handover_receipt(id).unwrap(), None);

    // Paired again under the same id, the phone's uploads are live again.
    let payload = test.service.begin_pairing();
    let repaired = common::engine_pair(&test, &payload, phone.device.id, "Direct iPhone").await;
    assert_eq!(repaired.status.as_u16(), 200);
    let again = common::EngineDevice {
        service: test.service.clone(),
        device: test.store.paired_device(phone.device.id).unwrap().unwrap(),
    };
    let bytes = seeded_bytes(chunk_size as usize, 98);
    let metadata = again.metadata(&bytes, chunk_size);
    assert_eq!(again.announce(&metadata).await.status.as_u16(), 201);
    assert!(owned(&test.service.engine.receipts_snapshot()));
    assert!(owned(&receipts.borrow()));
}
