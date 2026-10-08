//! The admission ledger (schema v5) over the wire: a recording the computer
//! admitted is answered delivered when it is announced again, whatever
//! happened to its receipt (a revoke, a meeting delete, a line save after
//! the intake's commit, a restart); other bytes under a recording id are a
//! new recording; the same bytes in another split restart the partial; the
//! same bytes from another device take the receipt over; and a late
//! `complete` of replaced bytes never completes the new upload. The intake
//! commits like the real one ([`StoreIntake`]), so the ledger row exists
//! exactly when an admission committed. The design is
//! `.plans/2026-10-08-handover-admission-ledger.md`. Swift:
//! `AdmissionLedgerTests`.

#![allow(clippy::large_futures)]

mod common;

use std::sync::Arc;

use common::{Options, Phone, StoreIntake, TestService, chunks, seeded_bytes};
use steno_core::{HandoverState, HandoverStateKind, RecordingMetadata, Store};
use steno_handover::wire;
use uuid::Uuid;

/// Twice the smallest chunk size, so a resplit can halve it.
const CHUNK_SIZE: i64 = 128 * 1024;

/// A service over `store` whose intake commits like the real one.
async fn service(store: &Arc<Store>, intake: &Arc<StoreIntake>) -> TestService {
    TestService::with(Options {
        chunk_size: CHUNK_SIZE,
        intake: Some(intake.clone()),
        store: Some(store.clone()),
        ..Options::default()
    })
    .await
}

/// A fresh in-memory store and an intake over it.
fn store_and_intake() -> (Arc<Store>, Arc<StoreIntake>) {
    let store = Arc::new(Store::in_memory().unwrap());
    let intake = StoreIntake::new(&store);
    (store, intake)
}

/// Pairs `device_id` again against `test`, as the phone does after an
/// unpair, with the name it had.
async fn pair_again(test: &TestService, phone: &Phone) -> Phone {
    let payload = test.service.begin_pairing();
    let response =
        Phone::try_pair(test, &payload.secret, phone.device_id, &phone.device_name).await;
    assert_eq!(response.status, 200, "pairing again");
    Phone {
        client: test.client(),
        token: response.json::<wire::PairResponse>().token,
        device_id: phone.device_id,
        device_name: phone.device_name.clone(),
    }
}

/// The meeting a `complete` of `recording_id` answered with 200.
async fn completed(phone: &Phone, recording_id: Uuid) -> Uuid {
    let response = phone.complete(recording_id).await;
    assert_eq!(response.status, 200, "complete");
    response.json::<wire::CompleteResponse>().meeting_id
}

/// The announce of `metadata` answers 200 `complete` with every chunk of
/// its split, and opens no file.
async fn delivered(test: &TestService, phone: &Phone, metadata: &RecordingMetadata) {
    let announced = phone.announce(metadata).await;
    assert_eq!(announced.status, 200, "announce");
    let count = (metadata.byte_count + metadata.chunk_size - 1) / metadata.chunk_size;
    assert_eq!(
        announced.json::<wire::RecordingStatus>(),
        wire::RecordingStatus {
            state: HandoverStateKind::Complete,
            received_chunks: (0..count).collect(),
        }
    );
    assert!(
        !test.inbox().has_partial(metadata.recording_id),
        "no partial is opened"
    );
}

/// The metadata of `bytes` under `phone`'s recording `recording_id`.
fn under(phone: &Phone, recording_id: Uuid, bytes: &[u8], chunk_size: i64) -> RecordingMetadata {
    RecordingMetadata {
        recording_id,
        ..phone.metadata(bytes, chunk_size)
    }
}

#[tokio::test]
async fn a_lost_answer_then_an_unpair_is_answered_delivered_after_pairing_again() {
    // The computer admitted the recording, but the 200 of `complete` never
    // reached the phone, so it still holds the row and the file. Then it
    // unpairs, which deletes the receipt with the device. After a restart
    // it pairs again under the same device id and announces the recording:
    // the ledger answers delivered, the phone's `complete` gets the first
    // meeting and deletes its copy, and nothing is admitted twice. A phone
    // under a new device id (an install of the app again) is answered the
    // same once the old one is revoked.
    let (store, intake) = store_and_intake();
    let test = service(&store, &intake).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize + 1, 31);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &bytes).await;
    let meeting_id = completed(&phone, id).await;

    let unpaired = phone
        .client
        .request(
            "DELETE",
            "/v1/pairing",
            &[("Authorization", &phone.bearer())],
            None,
        )
        .await
        .unwrap();
    assert_eq!(unpaired.status, 204);
    assert_eq!(
        store.handover_receipt(id).unwrap(),
        None,
        "the revoke cascades"
    );
    test.stop().await;

    let test = service(&store, &intake).await;
    let phone = pair_again(&test, &phone).await;
    delivered(&test, &phone, &metadata).await;
    assert_eq!(completed(&phone, id).await, meeting_id);
    assert_eq!(intake.meetings(), [meeting_id], "admitted once");

    test.service.revoke(phone.device_id).await.unwrap();
    let reinstalled = Phone::pair(&test).await;
    delivered(&test, &reinstalled, &metadata).await;
    assert_eq!(completed(&reinstalled, id).await, meeting_id);
    assert_eq!(intake.meetings(), [meeting_id]);
    assert_eq!(store.all_meetings().unwrap().len(), 1, "one meeting");
    test.stop().await;
}

#[tokio::test]
async fn a_deleted_meetings_recording_is_answered_delivered() {
    // The user deleted the meeting, which deletes its receipt; the phone
    // never got the 200 and announces the recording again. The ledger
    // answers delivered with the deleted meeting's id, so the phone deletes
    // its copy instead of uploading it as a new meeting (Nicolai,
    // 2026-10-07). After a restart, the stored `complete` receipt whose
    // meeting is gone still reads as admitted.
    let (store, intake) = store_and_intake();
    let test = service(&store, &intake).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize + 5, 32);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &bytes).await;
    let meeting_id = completed(&phone, id).await;
    store.delete_meeting(meeting_id).unwrap();
    assert_eq!(store.handover_receipt(id).unwrap(), None);
    test.stop().await;

    let test = service(&store, &intake).await;
    let phone = Phone {
        client: test.client(),
        ..phone
    };
    delivered(&test, &phone, &metadata).await;
    assert_eq!(completed(&phone, id).await, meeting_id);
    test.stop().await;

    let test = service(&store, &intake).await;
    let phone = Phone {
        client: test.client(),
        ..phone
    };
    let kept = phone.status(id).await;
    assert_eq!(kept.status, 200);
    assert_eq!(
        kept.json::<wire::RecordingStatus>().state,
        HandoverStateKind::Complete,
        "a complete receipt whose meeting is gone reads as admitted"
    );
    assert_eq!(completed(&phone, id).await, meeting_id);
    assert_eq!(intake.meetings(), [meeting_id], "admitted once");
    assert_eq!(store.all_meetings().unwrap(), []);
    test.stop().await;
}

#[tokio::test]
async fn a_receipt_a_late_save_put_back_reads_admitted_after_a_restart() {
    // A line save asked for before the intake's commit (a chunk, a
    // re-announce) landed after it and put the stored receipt back to
    // `verifying`, and the app stopped before the engine's own `complete`
    // save. After the restart the ledger shows the bytes admitted, so the
    // phone's retried `complete` gets the first meeting, not a second.
    let (store, intake) = store_and_intake();
    let test = service(&store, &intake).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize + 9, 33);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &bytes).await;
    let meeting_id = completed(&phone, id).await;
    test.stop().await;
    let mut put_back = store.handover_receipt(id).unwrap().unwrap();
    put_back.state = HandoverState::Verifying;
    store.save_handover_receipt(&put_back).unwrap();

    let test = service(&store, &intake).await;
    let phone = Phone {
        client: test.client(),
        ..phone
    };
    assert_eq!(completed(&phone, id).await, meeting_id);
    assert_eq!(intake.meetings(), [meeting_id], "admitted once");
    test.stop().await;
}

#[tokio::test]
async fn other_bytes_under_an_admitted_id_are_taken_in_as_a_new_recording() {
    // The phone announces another file under a recording id the computer
    // admitted (its crash recovery replaced the file). The computer takes
    // it in as a new recording under the same id: a fresh receipt, an
    // empty partial, its own meeting. While its admission has not
    // committed, nothing says `complete`; the phone's `complete` answers
    // 200 only once the new meeting committed, and the ledger then holds
    // both admissions.
    let (store, intake) = store_and_intake();
    let test = service(&store, &intake).await;
    let phone = Phone::pair(&test).await;
    let first = seeded_bytes(2 * CHUNK_SIZE as usize, 34);
    let metadata = phone.metadata(&first, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &first).await;
    let first_meeting = completed(&phone, id).await;

    for (seed, length) in [(35, first.len()), (36, first.len() + 7)] {
        let other = seeded_bytes(length, seed);
        let replaced = under(&phone, id, &other, CHUNK_SIZE);
        let announced = phone.announce(&replaced).await;
        assert_eq!(announced.status, 201, "a new recording");
        assert_eq!(
            announced.json::<wire::RecordingStatus>(),
            wire::RecordingStatus {
                state: HandoverStateKind::Receiving,
                received_chunks: Vec::new(),
            }
        );
        for (index, chunk) in chunks(&other, CHUNK_SIZE).iter().enumerate() {
            assert_eq!(phone.upload(id, index as i64, chunk).await.status, 204);
        }

        intake.hold_next();
        let (answer, ()) = tokio::join!(phone.complete(id), async {
            intake.admitting().await;
            assert_eq!(
                store
                    .admitted_meeting(id, replaced.byte_count, &replaced.sha256)
                    .unwrap(),
                None
            );
            let waiting = phone.status(id).await.json::<wire::RecordingStatus>();
            assert_eq!(
                waiting.state,
                HandoverStateKind::Verifying,
                "not complete yet"
            );
            intake.release();
        });
        assert_eq!(answer.status, 200);
        let meeting_id = answer.json::<wire::CompleteResponse>().meeting_id;
        assert_ne!(meeting_id, first_meeting, "a meeting of its own");
        assert_eq!(
            store
                .admitted_meeting(id, replaced.byte_count, &replaced.sha256)
                .unwrap(),
            Some(meeting_id),
            "committed before the 200"
        );
    }
    assert_eq!(
        store
            .admitted_meeting(id, metadata.byte_count, &metadata.sha256)
            .unwrap(),
        Some(first_meeting),
        "the first admission stays in the ledger"
    );
    assert_eq!(intake.meetings().len(), 3);

    // The first file announced again (a stale request) is answered from
    // the ledger: both were admitted.
    delivered(&test, &phone, &metadata).await;
    assert_eq!(completed(&phone, id).await, first_meeting);
    assert_eq!(intake.meetings().len(), 3);
    test.stop().await;
}

#[tokio::test]
async fn another_chunk_size_restarts_an_unfinished_partial() {
    // The phone announces the same bytes in another split before the
    // receipt is `complete`: the partial starts over under the new split,
    // no chunk listed, and the upload completes in it.
    let (store, intake) = store_and_intake();
    let test = service(&store, &intake).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize + 3, 37);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    assert_eq!(phone.announce(&metadata).await.status, 201);
    let parts = chunks(&bytes, CHUNK_SIZE);
    assert_eq!(phone.upload(id, 0, &parts[0]).await.status, 204);

    let resplit = RecordingMetadata {
        chunk_size: CHUNK_SIZE / 2,
        ..metadata.clone()
    };
    let announced = phone.announce(&resplit).await;
    assert_eq!(announced.status, 200);
    assert_eq!(
        announced.json::<wire::RecordingStatus>(),
        wire::RecordingStatus {
            state: HandoverStateKind::Receiving,
            received_chunks: Vec::new(),
        }
    );
    assert_eq!(
        std::fs::read(test.inbox().partial(id)).unwrap(),
        Vec::<u8>::new(),
        "the partial starts over"
    );
    assert_eq!(test.inbox().load_metadata(id), Some(resplit.clone()));
    let stored = store.handover_receipt(id).unwrap().unwrap();
    assert_eq!(
        (stored.chunk_size, stored.received_chunks),
        (CHUNK_SIZE / 2, vec![])
    );

    for (index, chunk) in chunks(&bytes, CHUNK_SIZE / 2).iter().enumerate() {
        assert_eq!(phone.upload(id, index as i64, chunk).await.status, 204);
    }
    let meeting_id = completed(&phone, id).await;
    assert_eq!(intake.meetings(), [meeting_id]);
    test.stop().await;
}

#[tokio::test]
async fn another_device_takes_over_an_unfinished_upload_of_the_same_bytes() {
    // The receipt belongs to an older device id of the same phone (it
    // paired again while the computer still held the old pairing). The new
    // device announces the same bytes: it takes the receipt over with the
    // chunks received, and its upload completes. The older device's
    // requests then find no receipt of theirs; its announce of the same
    // bytes takes the `complete` receipt back and is answered delivered.
    let (store, intake) = store_and_intake();
    let test = service(&store, &intake).await;
    let older = Phone::pair(&test).await;
    let bytes = seeded_bytes(3 * CHUNK_SIZE as usize, 38);
    let metadata = older.metadata(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    let parts = chunks(&bytes, CHUNK_SIZE);
    assert_eq!(older.announce(&metadata).await.status, 201);
    assert_eq!(older.upload(id, 0, &parts[0]).await.status, 204);

    let newer = Phone::pair(&test).await;
    let taken = newer.announce(&metadata).await;
    assert_eq!(taken.status, 200, "taken over, not 409");
    assert_eq!(
        taken.json::<wire::RecordingStatus>().received_chunks,
        [0],
        "the chunk received stays"
    );
    assert_eq!(
        store.handover_receipt(id).unwrap().unwrap().device_id,
        newer.device_id
    );
    assert_eq!(older.upload(id, 1, &parts[1]).await.status, 404);
    for (index, chunk) in parts.iter().enumerate().skip(1) {
        assert_eq!(newer.upload(id, index as i64, chunk).await.status, 204);
    }
    let meeting_id = completed(&newer, id).await;

    let back = older.announce(&metadata).await;
    assert_eq!(back.status, 200);
    assert_eq!(
        back.json::<wire::RecordingStatus>().state,
        HandoverStateKind::Complete
    );
    assert_eq!(completed(&older, id).await, meeting_id);
    assert_eq!(intake.meetings(), [meeting_id], "admitted once");
    test.stop().await;
}

#[tokio::test]
async fn a_late_complete_of_replaced_bytes_leaves_the_new_upload_unfinished() {
    // The phone's `complete` of the first file is in the intake when it
    // announces another file under the id (a new recording). The intake
    // then refuses the first file, whose receipt is gone, and the
    // `complete` write finds the new upload's receipt in memory and leaves
    // it alone: written into it, the new receipt would read `complete` with
    // the first file's meeting, and the phone would delete a file the
    // computer does not have. The first file's verified copy goes too, so
    // the new upload's `complete` cannot admit it unhashed. The new upload
    // then completes as a meeting of its own.
    let (store, intake) = store_and_intake();
    let test = service(&store, &intake).await;
    let phone = Phone::pair(&test).await;
    let first = seeded_bytes(2 * CHUNK_SIZE as usize, 39);
    let metadata = phone.metadata(&first, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &first).await;
    let other = seeded_bytes(2 * CHUNK_SIZE as usize, 40);
    let replaced = under(&phone, id, &other, CHUNK_SIZE);

    intake.hold_next();
    let (late, ()) = tokio::join!(phone.complete(id), async {
        intake.admitting().await;
        assert_eq!(phone.announce(&replaced).await.status, 201);
        intake.release();
    });
    assert_ne!(late.status, 200, "the first file's commit is refused");
    let receipt = test
        .service
        .engine
        .receipts_snapshot()
        .into_iter()
        .find(|receipt| receipt.recording_id == id)
        .unwrap();
    assert_eq!(
        (receipt.state, receipt.sha256.clone()),
        (HandoverState::Receiving, replaced.sha256.clone()),
        "the new upload is unfinished"
    );
    let stored = store.handover_receipt(id).unwrap().unwrap();
    assert_eq!(
        (stored.state.kind(), stored.sha256),
        (HandoverStateKind::Receiving, replaced.sha256.clone())
    );
    assert_eq!(
        store
            .admitted_meeting(id, replaced.byte_count, &replaced.sha256)
            .unwrap(),
        None
    );
    assert_ne!(phone.complete(id).await.status, 200, "nothing to admit yet");

    phone.upload_all(&replaced, &other).await;
    let meeting_id = completed(&phone, id).await;
    assert_eq!(
        store
            .admitted_meeting(id, replaced.byte_count, &replaced.sha256)
            .unwrap(),
        Some(meeting_id)
    );
    test.stop().await;
}

#[tokio::test]
async fn a_recording_an_older_app_admitted_is_answered_delivered_after_the_backfill() {
    // An older app (the Swift `v0.10.0-rc.2`, or a Rust build before v5)
    // admitted the recording: a meeting and a `complete` receipt, no
    // ledger row. This build's store open backfills the row, so after an
    // unpair and a pairing again the phone's retry is answered delivered,
    // with no second meeting; before it, the retried `complete` is
    // answered from the receipt.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let store = Arc::new(Store::open(&path).unwrap());
    let test = TestService::with(Options {
        chunk_size: CHUNK_SIZE,
        store: Some(store.clone()),
        ..Options::default()
    })
    .await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(CHUNK_SIZE as usize + 11, 41);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &bytes).await;
    // The fake intake writes no row; the engine saves the `complete`
    // receipt itself, and the meeting row is the older intake's.
    let meeting_id = completed(&phone, id).await;
    common::save_admitted_meeting(&store, meeting_id);
    assert_eq!(
        store
            .admitted_meeting(id, metadata.byte_count, &metadata.sha256)
            .unwrap(),
        None,
        "no ledger row yet"
    );
    test.stop().await;
    drop(test);
    drop(store);

    let store = Arc::new(Store::open(&path).unwrap());
    let intake = StoreIntake::new(&store);
    let test = service(&store, &intake).await;
    let phone = Phone {
        client: test.client(),
        ..phone
    };
    assert_eq!(
        completed(&phone, id).await,
        meeting_id,
        "the re-sent complete"
    );
    test.service.revoke(phone.device_id).await.unwrap();
    let phone = pair_again(&test, &phone).await;
    delivered(&test, &phone, &metadata).await;
    assert_eq!(completed(&phone, id).await, meeting_id);
    assert_eq!(intake.meetings(), Vec::<Uuid>::new(), "no second admission");
    assert_eq!(store.all_meetings().unwrap().len(), 1);
    test.stop().await;
}
