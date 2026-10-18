//! The admission ledger over the wire: an admitted recording announced again
//! is answered delivered whatever happened to its receipt; other bytes under
//! its id are a new recording; the same bytes are one meeting.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::sync::Arc;

use common::{Hold, Options, Phone, StoreIntake, TestService, chunks, seeded_bytes};
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

fn store_and_intake() -> (Arc<Store>, Arc<StoreIntake>) {
    let store = Arc::new(Store::in_memory().unwrap());
    let intake = StoreIntake::new(&store);
    (store, intake)
}

/// Pairs `device_id` again against `test` under the name it had.
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

/// The announce of `metadata` answers 200 `complete` and opens no file.
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
    // A phone that lost the 200, unpaired and paired again after a restart is
    // answered delivered with the first meeting, also under a new device id.
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
    // After the user deleted the meeting, a re-announce is answered delivered
    // with its id, also after a restart.
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
    // A line save that put the receipt back to `verifying` after the commit:
    // after a restart the retried `complete` gets the first meeting.
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
    // Another file under an admitted id is a new recording with its own meeting;
    // its `complete` answers 200 only once that meeting committed.
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

        intake.hold_next(Hold::BeforeTheCommit);
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

    // The first file announced again is answered from the ledger.
    delivered(&test, &phone, &metadata).await;
    assert_eq!(completed(&phone, id).await, first_meeting);
    assert_eq!(intake.meetings().len(), 3);
    test.stop().await;
}

#[tokio::test]
async fn another_chunk_size_restarts_an_unfinished_partial() {
    // The same bytes in another split restart the partial under the new split.
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
    // A newer device id announcing the same bytes takes the receipt over; the
    // older device's announce later is answered delivered.
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
async fn a_takeover_during_the_first_admission_completes_with_its_meeting() {
    // The newer device takes the receipt over after the older one's commit;
    // its `complete` answers the first meeting, not a duplicate.
    let (store, intake) = store_and_intake();
    let test = service(&store, &intake).await;
    let older = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize + 3, 41);
    let metadata = older.metadata(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    older.upload_all(&metadata, &bytes).await;
    let newer = Phone::pair(&test).await;

    intake.hold_next(Hold::AfterTheCommit);
    let (first, ()) = tokio::join!(older.complete(id), async {
        intake.admitting().await;
        assert_eq!(newer.announce(&metadata).await.status, 200, "taken over");
        intake.release();
    });
    assert_eq!(first.status, 200);
    let meeting_id = first.json::<wire::CompleteResponse>().meeting_id;
    assert_eq!(
        store.handover_receipt(id).unwrap().unwrap().state,
        HandoverState::Receiving,
        "the takeover's save landed after the commit"
    );
    assert_eq!(newer.complete(id).await.status, 409, "no partial left");

    newer.upload_all(&metadata, &bytes).await;
    assert_eq!(completed(&newer, id).await, meeting_id, "the first meeting");
    assert_eq!(intake.meetings(), [meeting_id], "admitted once");
    assert_eq!(
        store.handover_receipt(id).unwrap().unwrap().state,
        HandoverState::Complete { meeting_id }
    );
    delivered(&test, &older, &metadata).await;
    test.stop().await;
}

#[tokio::test]
async fn admitted_bytes_over_another_devices_unfinished_upload_are_refused() {
    // An admitted file announced again during another device's new recording
    // under the id answers 409, and that upload completes on its own.
    let (store, intake) = store_and_intake();
    let test = service(&store, &intake).await;
    let phone = Phone::pair(&test).await;
    let first = seeded_bytes(2 * CHUNK_SIZE as usize, 42);
    let metadata = phone.metadata(&first, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &first).await;
    let first_meeting = completed(&phone, id).await;

    let other = Phone::pair(&test).await;
    let other_bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 43);
    let theirs = under(&other, id, &other_bytes, CHUNK_SIZE);
    let parts = chunks(&other_bytes, CHUNK_SIZE);
    assert_eq!(other.announce(&theirs).await.status, 201, "a new recording");
    assert_eq!(other.upload(id, 0, &parts[0]).await.status, 204);

    let refused = phone.announce(&metadata).await;
    assert_eq!(refused.status, 409);
    let receipt = store.handover_receipt(id).unwrap().unwrap();
    assert_eq!(
        (
            receipt.device_id,
            receipt.sha256.clone(),
            receipt.state,
            receipt.received_chunks
        ),
        (
            other.device_id,
            theirs.sha256.clone(),
            HandoverState::Receiving,
            vec![0]
        ),
        "the other device's upload stays"
    );
    assert!(test.inbox().has_partial(id), "and so does its partial");

    assert_eq!(other.upload(id, 1, &parts[1]).await.status, 204);
    let their_meeting = completed(&other, id).await;
    assert_ne!(their_meeting, first_meeting);
    assert_eq!(intake.meetings(), [first_meeting, their_meeting]);
    test.stop().await;
}

#[tokio::test]
async fn a_late_complete_of_replaced_bytes_leaves_the_new_upload_unfinished() {
    // A `complete` in the intake when another file is announced under the id
    // leaves the new receipt alone, whether the intake refuses or commits.
    for hold in [Hold::BeforeTheCommit, Hold::AfterTheCommit] {
        let (store, intake) = store_and_intake();
        let test = service(&store, &intake).await;
        let phone = Phone::pair(&test).await;
        let first = seeded_bytes(2 * CHUNK_SIZE as usize, 39);
        let metadata = phone.metadata(&first, CHUNK_SIZE);
        let id = metadata.recording_id;
        phone.upload_all(&metadata, &first).await;
        let other = seeded_bytes(2 * CHUNK_SIZE as usize, 40);
        let replaced = under(&phone, id, &other, CHUNK_SIZE);

        intake.hold_next(hold);
        let (late, ()) = tokio::join!(phone.complete(id), async {
            intake.admitting().await;
            assert_eq!(phone.announce(&replaced).await.status, 201, "{hold:?}");
            intake.release();
        });
        let first_admitted = store
            .admitted_meeting(id, metadata.byte_count, &metadata.sha256)
            .unwrap();
        match hold {
            Hold::BeforeTheCommit => {
                assert_ne!(late.status, 200, "the first file's commit is refused");
                assert_eq!(first_admitted, None);
            }
            Hold::AfterTheCommit => {
                assert_eq!(late.status, 200, "the first file was admitted");
                assert_eq!(
                    first_admitted,
                    Some(late.json::<wire::CompleteResponse>().meeting_id)
                );
            }
        }
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
            "the new upload is unfinished ({hold:?})"
        );
        assert_eq!(
            store
                .admitted_meeting(id, replaced.byte_count, &replaced.sha256)
                .unwrap(),
            None
        );
        let status = phone.status(id).await.json::<wire::RecordingStatus>();
        assert_eq!(status.state, HandoverStateKind::Receiving, "{hold:?}");
        assert_ne!(
            phone.complete(id).await.status,
            200,
            "nothing to admit yet ({hold:?})"
        );

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
}

#[tokio::test]
async fn a_recording_an_older_app_admitted_is_answered_delivered_after_the_backfill() {
    // An older build admitted the recording without a ledger row; the store
    // open backfills it, so the retry is answered delivered.
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
    // The fake intake writes no row; the meeting row is the older intake's.
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

#[tokio::test]
async fn a_resend_after_pair_again_is_delivered_with_the_first_meeting_unchanged() {
    // After Pair again, a renamed meeting's recording sent again is answered
    // delivered and the meeting stays as the user left it.
    let (store, intake) = store_and_intake();
    let secrets = steno_core::testing::InMemorySecretStore::default();
    let record = common::MemoryRecord::default();
    let now = chrono::Utc::now();
    let identity =
        steno_handover::HandoverIdentity::load_or_create(&secrets, &record, &store, "Steno", now)
            .await
            .unwrap();
    let test = service(&store, &intake).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize + 3, 41);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let id = metadata.recording_id;
    phone.upload_all(&metadata, &bytes).await;
    let meeting_id = completed(&phone, id).await;
    assert_eq!(intake.meetings(), [meeting_id]);

    store.rename(meeting_id, "Planning review", now).unwrap();
    let person = steno_core::testing::sample_data::person(0, "Ada");
    store.save_person(&person).unwrap();
    store
        .save_speaker(&steno_core::Speaker {
            id: Uuid::new_v4(),
            meeting_id,
            cluster_label: "Speaker 1".to_owned(),
            assignment: steno_core::SpeakerAssignment::Confirmed {
                person_id: person.id,
            },
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: None,
            cluster_confidence: 0.9,
        })
        .unwrap();
    let meetings = store.all_meetings().unwrap();
    let speakers = store.speakers(meeting_id).unwrap();
    assert_eq!(meetings[0].title, "Planning review");
    test.stop().await;

    let minted =
        steno_handover::HandoverIdentity::mint_anew(&secrets, &record, &store, "Steno", now)
            .await
            .unwrap();
    assert_ne!(
        minted.fingerprint(),
        identity.fingerprint(),
        "a new identity"
    );
    assert!(
        store.paired_devices().unwrap().is_empty(),
        "every phone goes"
    );
    assert_eq!(
        store.handover_receipt(id).unwrap(),
        None,
        "and its receipts"
    );
    assert_eq!(
        store
            .admitted_meeting(id, metadata.byte_count, &metadata.sha256)
            .unwrap(),
        Some(meeting_id),
        "the ledger stays"
    );

    let test = service(&store, &intake).await;
    let phone = pair_again(&test, &phone).await;
    delivered(&test, &phone, &metadata).await;
    assert_eq!(completed(&phone, id).await, meeting_id);
    test.service.revoke(phone.device_id).await.unwrap();
    let reinstalled = Phone::pair(&test).await;
    let resent = under(&reinstalled, id, &bytes, CHUNK_SIZE);
    delivered(&test, &reinstalled, &resent).await;
    assert_eq!(completed(&reinstalled, id).await, meeting_id);

    assert_eq!(intake.meetings(), [meeting_id], "admitted once");
    assert_eq!(store.all_meetings().unwrap(), meetings, "M unchanged");
    assert_eq!(store.speakers(meeting_id).unwrap(), speakers);
    test.stop().await;
}
